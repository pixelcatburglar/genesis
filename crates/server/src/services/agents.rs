//! `newBreedAgents`: talking to NPC agents (ui/station/agents/agents.py) and
//! the journal's mission list (journal.py, missionentry.py).
//!
//! Interact(agentID, command, cookie, argument) returns (dialogue, public,
//! cookie). The agent says message 'agents::<command>[A]' (or
//! 'agents::<dialogue>[A]' when dialogue is a string); each response key in
//! a dialogue list is offered as 'agents::<key>[C]'; keys starting '.quit.'
//! end the conversation. `public` holds the %(...)s props of those texts.
//!
//! TEMPLATE DEFAULT missions: carry a load of cargo to a nearby station, or
//! bring ore to the agent. Rewards scale with the agent's level.

use marshal::Value;
use proto::gpcs::Call;

use super::{inventory, user_error, Args, CallResult, Session};
use crate::{db, Server};

/// cfg.py format types for canned-message props.
const LOCID: i32 = 3;
const AMT: i32 = 18;
const TYPEIDANDQUANTITY: i32 = 24;

#[derive(Debug, Clone)]
struct Agent {
    id: i32,
    name: String,
    corp: i32,
    station: i32,
    level: i32,
}

async fn agent(server: &Server, id: i32) -> anyhow::Result<Option<Agent>> {
    let c = server.pool.get().await?;
    Ok(c.query_opt(
        r#"SELECT "agentID", "agentName", "corporationID", "stationID", "level" FROM static.agents WHERE "agentID" = $1"#,
        &[&id],
    )
    .await?
    .map(|r| Agent { id: r.get(0), name: r.get(1), corp: r.get(2), station: r.get(3), level: r.get(4) }))
}

#[derive(Debug, Clone)]
struct Mission {
    id: i32,
    kind: String,
    status: String,
    type_id: i32,
    quantity: i32,
    destination: i32,
    reward: f64,
    created: std::time::SystemTime,
    expires: std::time::SystemTime,
}

async fn mission(server: &Server, agent_id: i32, char_id: i32) -> anyhow::Result<Option<Mission>> {
    let c = server.pool.get().await?;
    Ok(c.query_opt(
        r#"SELECT "missionID", "kind", "status", "typeID", "quantity", "destinationID", "reward", "created", "expires"
           FROM game.agent_missions WHERE "agentID" = $1 AND "characterID" = $2 AND "status" IN ('offered', 'accepted')
             AND "expires" > now() ORDER BY "missionID" DESC LIMIT 1"#,
        &[&agent_id, &char_id],
    )
    .await?
    .map(|r| Mission {
        id: r.get(0),
        kind: r.get(1),
        status: r.get(2),
        type_id: r.get(3),
        quantity: r.get(4),
        destination: r.get(5),
        reward: r.get(6),
        created: r.get(7),
        expires: r.get(8),
    }))
}

fn type_name(server: &Server, type_id: i32) -> String {
    server
        .world
        .type_info(type_id)
        .map(|t| t.name.clone())
        .unwrap_or_default()
}

fn station_name(server: &Server, station: i32) -> String {
    server
        .world
        .stations
        .get(&station)
        .map(|s| s.name.clone())
        .unwrap_or_default()
}

/// The mission's texts for the dialogue and the journal.
fn props(server: &Server, a: &Agent, m: &Mission) -> Vec<(Value, Value)> {
    let what = type_name(server, m.type_id);
    let (text, howto) = match m.kind.as_str() {
        "courier" => (
            format!(
                "I need {} units of {} taken to {}.",
                m.quantity,
                what,
                station_name(server, m.destination)
            ),
            format!(
                "Take the cargo from your hangar here to {}, then tell me it is done.",
                station_name(server, m.destination)
            ),
        ),
        _ => (
            format!(
                "We are short of raw materials: bring me {} units of {}.",
                m.quantity, what
            ),
            format!(
                "Put {} units of {} in your hangar at {}, then tell me.",
                m.quantity,
                what,
                station_name(server, a.station)
            ),
        ),
    };
    vec![
        (Value::str("mission.text"), db::text_value(&text)),
        (Value::str("mission.howto"), db::text_value(&howto)),
        (
            Value::str("mission.reward"),
            Value::tuple([Value::Int(AMT), Value::Float(m.reward)]),
        ),
        (
            Value::str("name"),
            db::text_value(&format!(
                "{} for {}",
                if m.kind == "courier" {
                    "Delivery"
                } else {
                    "Supply"
                },
                a.name
            )),
        ),
    ]
}

/// A new offer: courier to a station within a few jumps, or ore to the agent.
async fn offer(server: &Server, a: &Agent, char_id: i32) -> anyhow::Result<Mission> {
    let w = &server.world;
    let seed =
        (crate::statics::blue_time_now() / 10_000_000) as u64 ^ (char_id as u64) << 7 ^ a.id as u64;
    let st = w.stations.get(&a.station).cloned();
    let courier = seed % 2 == 0;
    let (kind, type_id, quantity, destination) = if courier {
        // A station in the same constellation, else the same region.
        let near: Vec<i32> = w
            .stations
            .values()
            .filter(|s| {
                st.as_ref().is_some_and(|me| {
                    s.station_id != me.station_id && s.constellation_id == me.constellation_id
                })
            })
            .map(|s| s.station_id)
            .collect();
        let near = if near.is_empty() {
            w.stations
                .values()
                .filter(|s| {
                    st.as_ref().is_some_and(|me| {
                        s.station_id != me.station_id && s.region_id == me.region_id
                    })
                })
                .map(|s| s.station_id)
                .collect()
        } else {
            near
        };
        let dest = near
            .get((seed as usize / 3) % near.len().max(1))
            .copied()
            .unwrap_or(a.station);
        (
            "courier",
            w.type_id("Tourists"),
            5 + (seed % 20) as i32,
            dest,
        )
    } else {
        let c = server.pool.get().await?;
        let ores: Vec<i32> = c
            .query(r#"SELECT t."typeID" FROM static.types t JOIN static.groups g USING ("groupID") WHERE g."groupName" = 'Asteroid' ORDER BY 1 LIMIT 5"#, &[])
            .await?
            .iter()
            .map(|r| r.get(0))
            .collect();
        let ore = ores
            .get((seed as usize / 5) % ores.len().max(1))
            .copied()
            .unwrap_or(w.type_id("Tourists"));
        ("mining", ore, 100 * (1 + (seed % 5) as i32), a.station)
    };
    let reward = (15_000.0 + (seed % 15_000) as f64) * a.level as f64;
    let c = server.pool.get().await?;
    let r = c
        .query_one(
            r#"INSERT INTO game.agent_missions ("agentID", "characterID", "kind", "status", "typeID", "quantity", "destinationID", "reward", "expires")
               VALUES ($1, $2, $3, 'offered', $4, $5, $6, $7, now() + interval '7 days') RETURNING "missionID", "created", "expires""#,
            &[&a.id, &char_id, &kind, &type_id, &quantity, &destination, &reward],
        )
        .await?;
    Ok(Mission {
        id: r.get(0),
        kind: kind.to_owned(),
        status: "offered".into(),
        type_id,
        quantity,
        destination,
        reward,
        created: r.get(1),
        expires: r.get(2),
    })
}

/// Units of a type a character holds at a station.
async fn held(server: &Server, char_id: i32, station: i32, type_id: i32) -> anyhow::Result<i64> {
    Ok(db::scalar::<i64>(
        &server.pool,
        r#"SELECT coalesce(sum("quantity"), 0)::bigint FROM game.items WHERE "ownerID" = $1 AND "locationID" = $2 AND "typeID" = $3"#,
        &[&char_id, &(station as i64), &type_id],
    )
    .await?
    .unwrap_or(0))
}

/// Takes `qty` units of a type out of a hangar.
async fn consume(
    server: &Server,
    char_id: i32,
    station: i32,
    type_id: i32,
    mut qty: i64,
) -> anyhow::Result<()> {
    for it in inventory::items_where(
        server,
        r#""ownerID" = $1 AND "locationID" = $2 AND "typeID" = $3"#,
        &[&char_id, &(station as i64), &type_id],
    )
    .await?
    {
        if qty <= 0 {
            break;
        }
        if it.quantity as i64 <= qty {
            qty -= it.quantity as i64;
            inventory::delete_item(server, &it).await?;
        } else {
            inventory::set_quantity(server, &it, it.quantity - qty as i32).await?;
            qty = 0;
        }
    }
    Ok(())
}

async fn adjust_standing(server: &Server, from: i32, to: i32, delta: f64) -> anyhow::Result<()> {
    db::exec(
        &server.pool,
        r#"INSERT INTO game.standings ("fromID", "toID", "standing") VALUES ($1, $2, $3)
           ON CONFLICT ("fromID", "toID") DO UPDATE SET "standing" = least(10, greatest(-10, game.standings."standing" + $3))"#,
        &[&from, &to, &delta],
    )
    .await?;
    Ok(())
}

fn reply(dialogue: Value, public: Vec<(Value, Value)>) -> Value {
    Value::tuple([dialogue, Value::Dict(public), Value::None])
}

fn keys(k: &[&str]) -> Value {
    Value::List(k.iter().map(|k| Value::str(k)).collect())
}

pub async fn dispatch(
    server: &Server,
    session: &mut Session,
    method: &str,
    call: &Call,
) -> CallResult {
    let a = Args::of(call);
    let me = session.char_id().unwrap_or(0);
    match method {
        // Agents here and agents I work for: charID, corpID, corpTypeID,
        // raceID, bloodlineID, stationID (agents.py, lobby.py).
        "GetMyAgents" => {
            let station = session
                .attrs
                .int("stationid")
                .map(|s| s as i32)
                .unwrap_or(0);
            let corp_type = server.world.type_id("Corporation");
            Ok(db::rowset(
                &server.pool,
                &format!(
                    r#"SELECT a."agentID" AS "charID", a."corporationID" AS "corpID", {corp_type} AS "corpTypeID", a."raceID",
                              a."bloodlineID", a."stationID", a."level", a."agentName"
                       FROM static.agents a
                       WHERE a."stationID" = $1 OR a."agentID" IN
                           (SELECT "agentID" FROM game.agent_missions WHERE "characterID" = $2 AND "status" = 'accepted')
                       ORDER BY a."agentID""#
                ),
                &[&station, &me],
            )
            .await?)
        }
        // {(agentID, missionID, startTime): (None, None, details, briefingKey, helpKey)}
        "GetMyMissionStatus" => {
            let c = server.pool.get().await?;
            let rs = c
                .query(
                    r#"SELECT "agentID" FROM game.agent_missions WHERE "characterID" = $1 AND "status" = 'accepted' AND "expires" > now()"#,
                    &[&me],
                )
                .await?;
            drop(c);
            let mut out = Vec::new();
            for r in rs {
                let Some(ag) = agent(server, r.get(0)).await? else {
                    continue;
                };
                let Some(m) = mission(server, ag.id, me).await? else {
                    continue;
                };
                let mut details = props(server, &ag, &m);
                let days = m
                    .expires
                    .duration_since(std::time::SystemTime::now())
                    .map(|d| d.as_secs() / 86400)
                    .unwrap_or(0);
                details.push((
                    Value::str("expirationText"),
                    Value::from(format!("{days} days")),
                ));
                // missionentry.py: cfg.FormatConvert(*details['items.<x>.<n>.typeandquantity'])
                details.push((
                    Value::str("items.fetch.1.typeandquantity"),
                    Value::tuple([
                        Value::Int(TYPEIDANDQUANTITY),
                        Value::Int(m.type_id),
                        Value::Int(m.quantity),
                    ]),
                ));
                details.push((
                    Value::str("items.fetch.1.location"),
                    Value::tuple([Value::Int(LOCID), Value::Int(m.destination)]),
                ));
                details.push((
                    Value::str("items.reward.1.typeandquantity"),
                    Value::tuple([
                        Value::Int(TYPEIDANDQUANTITY),
                        Value::Int(server.world.type_id("Credits")),
                        Value::Int(m.reward as i32),
                    ]),
                ));
                out.push((
                    Value::tuple([
                        Value::Int(ag.id),
                        Value::Int(m.id),
                        Value::Long(db::blue_time(m.created)),
                    ]),
                    Value::tuple([
                        Value::None,
                        Value::None,
                        Value::Dict(details),
                        Value::str("agents::mission::journal::briefing"),
                        Value::str("agents::mission::journal::help"),
                    ]),
                ));
            }
            Ok(Value::Dict(out))
        }
        // Interact(agentID, command, cookie, argument)
        "Interact" => {
            let Some(ag) = agent(server, a.i32(0).unwrap_or(0)).await? else {
                return Err(user_error(
                    "CustomInfo",
                    vec![("info", Value::str("That agent is not available."))],
                ));
            };
            let command = a.text(1).unwrap_or_else(|| "root".into());
            let current = mission(server, ag.id, me).await?;
            let accepted = current.as_ref().filter(|m| m.status == "accepted");
            let mprops = |m: &Mission| props(server, &ag, m);
            Ok(match command.as_str() {
                "root" => match accepted {
                    Some(m) => reply(keys(&["mission::status", ".quit.root"]), mprops(m)),
                    None => reply(keys(&["mission::ask", ".quit.root"]), vec![]),
                },
                "root::remote" => match accepted {
                    Some(m) => reply(keys(&["mission::status", ".quit.root"]), mprops(m)),
                    None => reply(Value::List(vec![]), vec![]),
                },
                "mission::ask" => {
                    if let Some(m) = accepted {
                        reply(Value::str("mission::busy"), mprops(m))
                    } else {
                        let m = match current {
                            Some(m) => m,
                            None => offer(server, &ag, me).await?,
                        };
                        reply(keys(&["mission::accept", "mission::decline"]), mprops(&m))
                    }
                }
                "mission::accept" => {
                    let Some(m) = current.filter(|m| m.status == "offered") else {
                        return Ok(reply(Value::List(vec![]), vec![]));
                    };
                    db::exec(&server.pool, r#"UPDATE game.agent_missions SET "status" = 'accepted' WHERE "missionID" = $1"#, &[&m.id]).await?;
                    if m.kind == "courier" {
                        let w = &server.world;
                        let cargo = inventory::create_item(
                            server,
                            m.type_id,
                            me,
                            ag.station as i64,
                            w.flag("Hangar"),
                            m.quantity,
                            false,
                        )
                        .await?;
                        inventory::notify_new(server, &cargo);
                    }
                    reply(Value::List(vec![]), mprops(&m))
                }
                "mission::decline" => {
                    if let Some(m) = current.filter(|m| m.status == "offered") {
                        db::exec(
                            &server.pool,
                            r#"DELETE FROM game.agent_missions WHERE "missionID" = $1"#,
                            &[&m.id],
                        )
                        .await?;
                    }
                    reply(Value::List(vec![]), vec![])
                }
                "mission::status" => match accepted {
                    Some(m) => reply(
                        keys(&["mission::complete", "mission::quit", ".quit.root"]),
                        mprops(m),
                    ),
                    None => reply(Value::List(vec![]), vec![]),
                },
                "mission::complete" => {
                    let Some(m) = accepted.cloned() else {
                        return Ok(reply(Value::List(vec![]), vec![]));
                    };
                    if held(server, me, m.destination, m.type_id).await? < m.quantity as i64 {
                        return Ok(reply(Value::str("mission::notdone"), mprops(&m)));
                    }
                    consume(server, me, m.destination, m.type_id, m.quantity as i64).await?;
                    db::exec(&server.pool, r#"UPDATE game.agent_missions SET "status" = 'done' WHERE "missionID" = $1"#, &[&m.id]).await?;
                    super::wallet::transfer(
                        server,
                        ag.corp,
                        me,
                        m.reward,
                        "refMissionReward",
                        Some(ag.id as i64),
                        "",
                    )
                    .await?;
                    adjust_standing(server, ag.corp, me, 0.1 * ag.level as f64).await?;
                    adjust_standing(server, ag.id, me, 0.2).await?;
                    super::notify_chars(
                        server,
                        &[me],
                        "OnAgentMissionChange",
                        vec![Value::str("completed"), Value::Int(m.id)],
                    );
                    reply(Value::List(vec![]), mprops(&m))
                }
                "mission::quit" => {
                    if let Some(m) = accepted {
                        db::exec(
                            &server.pool,
                            r#"DELETE FROM game.agent_missions WHERE "missionID" = $1"#,
                            &[&m.id],
                        )
                        .await?;
                        adjust_standing(server, ag.corp, me, -0.05).await?;
                        super::notify_chars(
                            server,
                            &[me],
                            "OnAgentMissionChange",
                            vec![Value::str("quit"), Value::Int(m.id)],
                        );
                    }
                    reply(Value::List(vec![]), vec![])
                }
                _ => reply(Value::List(vec![]), vec![]),
            })
        }
        m => super::unimplemented("newBreedAgents", m, call),
    }
}
