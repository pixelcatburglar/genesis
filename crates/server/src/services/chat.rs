//! Chat (ui/shared/comtool/base.py). Channel types: 0 custom, 1 location,
//! 2 corp, 3 gang. A channel binds as chatx[(channelID, channelType,
//! channelBindExtra, channelName)]; its List() returns member rows
//! (charID, corpID, corpRole, name) and Speak() fans out OnChatSpeak to the
//! other members (the speaker's client echoes its own line). Location, corp
//! and gang membership follow the session; custom channels are joined by
//! listing them (comtool CreateChannel/JoinChannel) and left with Leave.

use std::collections::HashMap;
use std::sync::Mutex;

use marshal::Value;
use proto::gpcs::Call;

use super::{Args, Bound, CallResult, Session};
use crate::registry::Client;
use crate::{db, rows, Server};

const CHANNEL_CUSTOM: i64 = 0;
const CHANNEL_LOCATION: i64 = 1;
const CHANNEL_CORP: i64 = 2;
const CHANNEL_GANG: i64 = 3;
const MEMBER_HEADER: [&str; 4] = ["charID", "corpID", "corpRole", "name"];
const CHANNEL_HEADER: [&str; 5] = [
    "channelID",
    "channelType",
    "channelName",
    "active",
    "nodeID",
];

/// Custom channels by name, created on first use.
#[derive(Default)]
pub struct Channels {
    by_name: Mutex<HashMap<String, i64>>,
}

impl Channels {
    fn id(&self, name: &str) -> i64 {
        let mut m = self.by_name.lock().unwrap();
        let next = 1_500_000_000 + m.len() as i64;
        *m.entry(name.to_lowercase()).or_insert(next)
    }
}

fn channel_line(id: i64, kind: i64, name: Value) -> Vec<Value> {
    vec![
        Value::from(id),
        Value::from(kind),
        name,
        Value::Int(1),
        Value::None,
    ]
}

/// chatx service calls.
pub fn service(server: &Server, session: &mut Session, method: &str, call: &Call) -> CallResult {
    match method {
        // comtool __XyzzyInitialize: rows read for channelID, channelType,
        // channelName, nodeID, active; AddField appends to each line in place.
        "GetChannels" => {
            let mut lines = Vec::new();
            if let Some(loc) = session.attrs.int("locationid") {
                lines.push(channel_line(loc, CHANNEL_LOCATION, Value::from(loc)));
            }
            if let Some(corp) = session.attrs.int("corpid") {
                lines.push(channel_line(corp, CHANNEL_CORP, Value::from(corp)));
            }
            if let Some(gang) = session.attrs.int("gangid") {
                lines.push(channel_line(gang, CHANNEL_GANG, Value::from(gang)));
            }
            Ok(rows::rowset(&CHANNEL_HEADER, lines))
        }
        // EstablishChannel(name): a custom channel row (comtool CreateChannel joins it).
        "EstablishChannel" => {
            let name = Args::of(call).text(0).unwrap_or_default();
            let id = server.chat.id(&name);
            Ok(rows::row(
                &CHANNEL_HEADER,
                channel_line(id, CHANNEL_CUSTOM, db::text_value(&name)),
            ))
        }
        m => super::unimplemented("chatx", m, call),
    }
}

fn channel_key(bound: &Bound) -> (i64, i64) {
    let p = bound.params.as_seq().unwrap_or_default();
    (
        p.first().and_then(Value::as_int).unwrap_or(0),
        p.get(1).and_then(Value::as_int).unwrap_or(0),
    )
}

fn member_of(c: &Client, channel_id: i64, channel_type: i64) -> bool {
    c.char_id.is_some()
        && match channel_type {
            CHANNEL_LOCATION => c.location_id == Some(channel_id),
            CHANNEL_CORP => c.corp_id.map(i64::from) == Some(channel_id),
            CHANNEL_GANG => c.gang_id == Some(channel_id),
            CHANNEL_CUSTOM => c.channels.contains(&channel_id),
            _ => false,
        }
}

fn member_line(c: &Client) -> Vec<Value> {
    vec![
        Value::from(c.char_id.map(i64::from)),
        Value::from(c.corp_id.map(i64::from)),
        Value::Int(0),
        db::text_value(&c.char_name),
    ]
}

fn tell_others(
    server: &Server,
    me: i64,
    channel_id: i64,
    channel_type: i64,
    name: &str,
    args: Vec<Value>,
) {
    for (id, other) in server
        .registry
        .select(Some(me), |c| member_of(c, channel_id, channel_type))
    {
        other.send(crate::conn::event(
            server.config.node_id,
            id,
            name,
            args.clone(),
        ));
    }
}

/// chatx[(channelID, channelType, extra, name)] methods.
pub async fn channel(
    server: &Server,
    session: &mut Session,
    bound: &Bound,
    call: &Call,
) -> CallResult {
    let (channel_id, channel_type) = channel_key(bound);
    let char_id = session.char_id().map(i64::from);
    match call.method() {
        "List" => {
            if channel_type == CHANNEL_CUSTOM {
                server.registry.update(session.client_id, |c| {
                    if !c.channels.contains(&channel_id) {
                        c.channels.push(channel_id);
                    }
                });
            }
            let members = server
                .registry
                .select(None, |c| member_of(c, channel_id, channel_type));
            if let Some((_, me)) = members.iter().find(|(id, _)| *id == session.client_id) {
                let info = rows::row(&MEMBER_HEADER, member_line(me));
                let hint = Value::List(vec![
                    Value::from(char_id),
                    db::text_value(&me.char_name),
                    Value::Int(1),
                ]);
                tell_others(
                    server,
                    session.client_id,
                    channel_id,
                    channel_type,
                    "OnChatJoin",
                    vec![
                        Value::from(channel_type),
                        Value::from(channel_id),
                        info,
                        hint,
                    ],
                );
            }
            Ok(rows::rowset(
                &MEMBER_HEADER,
                members.iter().map(|(_, c)| member_line(c)).collect(),
            ))
        }
        "Speak" => {
            let msg = call.args().first().cloned().unwrap_or(Value::str(""));
            tell_others(
                server,
                session.client_id,
                channel_id,
                channel_type,
                "OnChatSpeak",
                vec![
                    Value::from(channel_type),
                    Value::from(channel_id),
                    Value::from(char_id),
                    msg,
                ],
            );
            Ok(Value::None)
        }
        "Typing" => {
            tell_others(
                server,
                session.client_id,
                channel_id,
                channel_type,
                "OnChatTyping",
                vec![
                    Value::from(channel_type),
                    Value::from(channel_id),
                    Value::from(char_id),
                ],
            );
            Ok(Value::None)
        }
        "Leave" | "DeActivate" => {
            tell_others(
                server,
                session.client_id,
                channel_id,
                channel_type,
                "OnChatLeave",
                vec![
                    Value::from(channel_type),
                    Value::from(channel_id),
                    Value::from(char_id),
                ],
            );
            if channel_type == CHANNEL_CUSTOM && call.method() == "Leave" {
                server.registry.update(session.client_id, |c| {
                    c.channels.retain(|&id| id != channel_id)
                });
            }
            Ok(Value::None)
        }
        "Activate" => Ok(Value::None),
        // InviteCharacter(charID, channelID): comtool.ChatInvite(invitorID,
        // channel) on the invitee, a server-to-client call.
        "InviteCharacter" => {
            let who = Args::of(call).i32(0).unwrap_or(0);
            let p = bound.params.as_seq().unwrap_or_default();
            let channel = rows::row(
                &CHANNEL_HEADER,
                channel_line(
                    channel_id,
                    channel_type,
                    p.get(3).cloned().unwrap_or(Value::from(channel_id)),
                ),
            );
            for (id, c) in server.registry.select(None, |c| c.char_id == Some(who)) {
                c.send(crate::conn::client_call(
                    server.config.node_id,
                    id,
                    "comtool",
                    "ChatInvite",
                    vec![Value::from(char_id), channel.clone()],
                ));
            }
            Ok(Value::None)
        }
        m => super::unimplemented("chatx", m, call),
    }
}
