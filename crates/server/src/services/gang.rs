//! `gangsta`: gangs (parklife/gang.py, the comtool gang channel, the people
//! menus). A gang lives as long as its members are connected, so it is kept
//! in memory. Membership is the session's `gangid`, leadership its `gangrole`
//! (const.gangLeaderRole).
//!
//! Events: OnGangInvite(leaderID, gangID) to the invitee; OnGangJoin(charID)
//! and OnGangLeave(charID) to every member including the subject (a member
//! seeing itself join re-reads GetGangInfoForChar); OnGangLeader(leaderID).

use std::collections::HashMap;
use std::sync::Mutex;

use marshal::Value;
use proto::gpcs::Call;

use super::{user_error, Args, CallResult, Session};
use crate::Server;

#[derive(Debug, Default, Clone)]
pub struct Gang {
    pub leader: Option<i32>,
    pub members: Vec<i32>,
    pub invited: Vec<i32>,
}

#[derive(Default)]
pub struct Gangs {
    gangs: Mutex<HashMap<i64, Gang>>,
    next: Mutex<i64>,
}

impl Gangs {
    fn with<R>(&self, f: impl FnOnce(&mut HashMap<i64, Gang>) -> R) -> R {
        f(&mut self.gangs.lock().unwrap())
    }

    fn new_id(&self) -> i64 {
        let mut n = self.next.lock().unwrap();
        *n += 1;
        900_000_000 + *n
    }

    pub fn members(&self, gang_id: i64) -> Vec<i32> {
        self.with(|g| {
            g.get(&gang_id)
                .map(|g| g.members.clone())
                .unwrap_or_default()
        })
    }
}

fn leader_role(server: &Server) -> Value {
    Value::Int(server.world.constant("gangLeaderRole"))
}

/// Applies a gang session change to a character's connections. My own
/// session changes in place so the reply follows the notification.
fn set_membership(
    server: &Server,
    session: &mut Session,
    char_id: i32,
    gang: Option<i64>,
    leader: bool,
) {
    let changes = vec![
        ("gangid", Value::from(gang)),
        (
            "gangrole",
            if leader {
                leader_role(server)
            } else {
                Value::None
            },
        ),
    ];
    if session.char_id() == Some(char_id) {
        session.change(server, &changes);
    } else {
        for (_, c) in server
            .registry
            .select(Some(session.client_id), |c| c.char_id == Some(char_id))
        {
            c.change(changes.clone());
        }
    }
}

fn tell(server: &Server, members: &[i32], name: &str, args: Vec<Value>) {
    super::notify_chars(server, members, name, args);
}

/// Removes `char_id` from its gang; the gang disbands when one member is left.
fn leave(server: &Server, session: &mut Session, gang_id: i64, char_id: i32) {
    let Some(gang) = server.gangs.with(|g| {
        let gang = g.get_mut(&gang_id)?;
        gang.members.retain(|&m| m != char_id);
        if gang.leader == Some(char_id) {
            gang.leader = None;
        }
        Some(gang.clone())
    }) else {
        return;
    };
    let mut told: Vec<i32> = gang.members.clone();
    told.push(char_id);
    set_membership(server, session, char_id, None, false);
    tell(server, &told, "OnGangLeave", vec![Value::Int(char_id)]);
    if gang.members.len() <= 1 {
        server.gangs.with(|g| g.remove(&gang_id));
        for m in gang.members {
            set_membership(server, session, m, None, false);
            tell(server, &[m], "OnGangLeave", vec![Value::Int(m)]);
        }
    }
}

pub async fn dispatch(
    server: &Server,
    session: &mut Session,
    method: &str,
    call: &Call,
) -> CallResult {
    let a = Args::of(call);
    let me = session.char_id().unwrap_or(0);
    let my_gang = session.attrs.int("gangid");
    let gang = my_gang.and_then(|id| server.gangs.with(|g| g.get(&id).cloned()));
    let i_lead = gang.as_ref().is_some_and(|g| g.leader == Some(me));
    match method {
        // (gangID, leaderID, [(charID, state)], invitations)
        "GetGangInfoForChar" => Ok(match (my_gang, gang) {
            (Some(id), Some(g)) => Value::tuple([
                Value::from(id),
                Value::from(g.leader),
                Value::List(
                    g.members
                        .iter()
                        .map(|&m| Value::tuple([Value::Int(m), Value::str("Active")]))
                        .collect(),
                ),
                Value::List(g.invited.iter().map(|&m| Value::Int(m)).collect()),
            ]),
            _ => Value::tuple([
                Value::None,
                Value::None,
                Value::List(vec![]),
                Value::List(vec![]),
            ]),
        }),
        // Inviting with no gang founds one, led by the inviter.
        "InviteCharacter" => {
            let who = a.i32(0).unwrap_or(0);
            if who == me {
                return Ok(Value::None);
            }
            let gang_id = match my_gang {
                Some(id) if gang.is_some() => {
                    if !i_lead {
                        return Err(user_error("GangNotLeader", vec![]));
                    }
                    id
                }
                _ => {
                    let id = server.gangs.new_id();
                    server.gangs.with(|g| {
                        g.insert(
                            id,
                            Gang {
                                leader: Some(me),
                                members: vec![me],
                                invited: vec![],
                            },
                        )
                    });
                    set_membership(server, session, me, Some(id), true);
                    tell(server, &[me], "OnGangJoin", vec![Value::Int(me)]);
                    id
                }
            };
            server.gangs.with(|g| {
                if let Some(g) = g.get_mut(&gang_id) {
                    if !g.invited.contains(&who) {
                        g.invited.push(who);
                    }
                }
            });
            tell(
                server,
                &[who],
                "OnGangInvite",
                vec![Value::Int(me), Value::from(gang_id)],
            );
            Ok(Value::None)
        }
        "AcceptInvitation" => {
            let gang_id = a.int(0).unwrap_or(0);
            let joined = server.gangs.with(|g| {
                let g = g.get_mut(&gang_id)?;
                if !g.invited.contains(&me) {
                    return None;
                }
                g.invited.retain(|&m| m != me);
                g.members.push(me);
                Some(g.members.clone())
            });
            let Some(members) = joined else {
                return Err(user_error("GangInvitationExpired", vec![]));
            };
            if let Some(old) = my_gang {
                if old != gang_id {
                    leave(server, session, old, me);
                }
            }
            set_membership(server, session, me, Some(gang_id), false);
            tell(server, &members, "OnGangJoin", vec![Value::Int(me)]);
            Ok(Value::None)
        }
        "RejectInvitation" => {
            let gang_id = a.int(0).unwrap_or(0);
            server.gangs.with(|g| {
                if let Some(g) = g.get_mut(&gang_id) {
                    g.invited.retain(|&m| m != me);
                }
            });
            Ok(Value::None)
        }
        "LeaveGang" => {
            if let Some(id) = my_gang {
                leave(server, session, id, me);
            }
            Ok(Value::None)
        }
        "KickMember" => {
            let who = a.i32(0).unwrap_or(0);
            match my_gang {
                Some(id) if i_lead => leave(server, session, id, who),
                _ => return Err(user_error("GangNotLeader", vec![])),
            }
            Ok(Value::None)
        }
        "DisbandGang" => {
            let Some(id) = my_gang.filter(|_| i_lead) else {
                return Err(user_error("GangNotLeader", vec![]));
            };
            let members = server
                .gangs
                .with(|g| g.remove(&id))
                .map(|g| g.members)
                .unwrap_or_default();
            for m in &members {
                set_membership(server, session, *m, None, false);
            }
            for m in &members {
                tell(server, &members, "OnGangLeave", vec![Value::Int(*m)]);
            }
            Ok(Value::None)
        }
        "MakeLeader" => {
            let who = a.i32(0).unwrap_or(0);
            let Some(id) = my_gang.filter(|_| i_lead) else {
                return Err(user_error("GangNotLeader", vec![]));
            };
            let members = server.gangs.with(|g| {
                let g = g.get_mut(&id)?;
                if !g.members.contains(&who) {
                    return None;
                }
                g.leader = Some(who);
                Some(g.members.clone())
            });
            if let Some(members) = members {
                set_membership(server, session, me, Some(id), false);
                set_membership(server, session, who, Some(id), true);
                tell(server, &members, "OnGangLeader", vec![Value::Int(who)]);
            }
            Ok(Value::None)
        }
        "ResignLeadership" => {
            if let Some(id) = my_gang.filter(|_| i_lead) {
                let members = server.gangs.with(|g| {
                    let g = g.get_mut(&id)?;
                    g.leader = None;
                    Some(g.members.clone())
                });
                set_membership(server, session, me, Some(id), false);
                tell(
                    server,
                    &members.unwrap_or_default(),
                    "OnGangLeader",
                    vec![Value::None],
                );
            }
            Ok(Value::None)
        }
        "ClaimLeadership" => {
            let Some(id) = my_gang else {
                return Ok(Value::None);
            };
            let members = server.gangs.with(|g| {
                let g = g.get_mut(&id)?;
                if g.leader.is_some() {
                    return None;
                }
                g.leader = Some(me);
                Some(g.members.clone())
            });
            match members {
                Some(members) => {
                    set_membership(server, session, me, Some(id), true);
                    tell(server, &members, "OnGangLeader", vec![Value::Int(me)]);
                    Ok(Value::None)
                }
                None => Err(user_error("GangAlreadyHasLeader", vec![])),
            }
        }
        m => super::unimplemented("gangsta", m, call),
    }
}

/// A disconnecting member leaves its gang.
pub fn on_disconnect(server: &Server, session: &mut Session) {
    if let (Some(id), Some(me)) = (session.attrs.int("gangid"), session.char_id()) {
        leave(server, session, id, me);
    }
}
