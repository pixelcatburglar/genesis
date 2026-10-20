//! `slash`: GM commands typed in chat (comtool channel.py/regioninput.py send
//! lines starting with '/' when session.role & ROLE_SLASH, i.e. ROLE_GML |
//! ROLE_LEGIONEER). The reply is printed as 'slash result: ...'.

use marshal::Value;

use super::{CallResult, Session};
use crate::{db, Server};

const ROLE_GML: i64 = 8;
const ROLE_LEGIONEER: i64 = 262_144;

const HELP: &str = "/create <typeID or type name> [quantity] | /giveisk <amount> [character] | /tr <station name or ID> | /online | /help";

pub async fn command(server: &Server, session: &mut Session, line: &str) -> CallResult {
    if session.role & (ROLE_GML | ROLE_LEGIONEER) == 0 {
        return Ok(Value::str("Permission denied."));
    }
    let me = session.char_id().unwrap_or(0);
    let mut words = line.trim_start_matches('/').split_whitespace();
    let cmd = words.next().unwrap_or("").to_lowercase();
    let rest: Vec<&str> = words.collect();
    let reply = match cmd.as_str() {
        "help" | "" => HELP.to_owned(),
        "online" => format!(
            "{} pilots online",
            server.registry.select(None, |c| c.char_id.is_some()).len()
        ),
        // /create <type> [qty]: into the hangar (or cargo in space).
        "create" => {
            let (what, qty) = match rest.last().and_then(|q| q.parse::<i32>().ok()) {
                Some(q) if rest.len() > 1 => (rest[..rest.len() - 1].join(" "), q),
                _ => (rest.join(" "), 1),
            };
            let type_id = match what.parse::<i32>() {
                Ok(id) if server.world.types.contains_key(&id) => Some(id),
                _ => server
                    .world
                    .types
                    .iter()
                    .find(|(_, t)| t.name.eq_ignore_ascii_case(&what))
                    .map(|(id, _)| *id),
            };
            match type_id {
                None => format!("No type '{what}'."),
                Some(t) => {
                    let (location, flag) = match session.attrs.int("stationid") {
                        Some(st) => (st, server.world.flag("Hangar")),
                        None => (
                            session.attrs.int("shipid").unwrap_or(0),
                            server.world.flag("Cargo"),
                        ),
                    };
                    let singleton = server
                        .world
                        .type_info(t)
                        .is_some_and(|i| [server.world.category_id("Ship"), server.world.category_id("Blueprint")].contains(&i.category_id));
                    let item = super::inventory::create_item(
                        server,
                        t,
                        me,
                        location,
                        flag,
                        qty.max(1),
                        singleton,
                    )
                    .await?;
                    super::inventory::notify_new(server, &item);
                    format!(
                        "Created {} x {} ({}).",
                        qty.max(1),
                        server
                            .world
                            .type_info(t)
                            .map(|i| i.name.as_str())
                            .unwrap_or("?"),
                        item.item_id
                    )
                }
            }
        }
        // /giveisk <amount> [character name]
        "giveisk" => {
            let amount: f64 = rest.first().and_then(|a| a.parse().ok()).unwrap_or(0.0);
            let target = if rest.len() > 1 {
                db::scalar::<i32>(
                    &server.pool,
                    r#"SELECT "characterID" FROM game.characters WHERE lower("characterName") = lower($1) AND "deleted" IS NULL"#,
                    &[&rest[1..].join(" ")],
                )
                .await?
            } else {
                Some(me)
            };
            match target {
                None => "No such character.".to_owned(),
                Some(t) => {
                    super::wallet::transfer(
                        server,
                        1,
                        t,
                        amount,
                        "refGMCashTransfer",
                        Some(me as i64),
                        "GM",
                    )
                    .await?;
                    format!("Gave {amount} ISK.")
                }
            }
        }
        // /tr <station>: docked teleport between stations.
        "tr" => {
            let what = rest.join(" ");
            let station = match what.parse::<i32>() {
                Ok(id) => server.world.stations.get(&id),
                Err(_) => server
                    .world
                    .stations
                    .values()
                    .find(|s| s.name.to_lowercase().contains(&what.to_lowercase())),
            };
            match (station, session.attrs.int("stationid")) {
                (None, _) => format!("No station '{what}'."),
                (Some(_), None) => "Dock first.".to_owned(),
                (Some(st), Some(_)) => {
                    let ship = session.attrs.int("shipid");
                    db::exec(
                        &server.pool,
                        r#"UPDATE game.characters SET "stationID" = $2, "solarSystemID" = $3, "constellationID" = $4, "regionID" = $5
                           WHERE "characterID" = $1"#,
                        &[&me, &st.station_id, &st.solar_system_id, &st.constellation_id, &st.region_id],
                    )
                    .await?;
                    if let Some(ship) = ship {
                        db::exec(
                            &server.pool,
                            r#"UPDATE game.items SET "locationID" = $2 WHERE "itemID" = $1"#,
                            &[&ship, &(st.station_id as i64)],
                        )
                        .await?;
                    }
                    session.change(
                        server,
                        &[
                            ("stationid", Value::Int(st.station_id)),
                            ("locationid", Value::Int(st.station_id)),
                            ("solarsystemid2", Value::Int(st.solar_system_id)),
                            ("constellationid", Value::Int(st.constellation_id)),
                            ("regionid", Value::Int(st.region_id)),
                        ],
                    );
                    format!("Moved to {}.", st.name)
                }
            }
        }
        other => format!("Unknown command /{other}. {HELP}"),
    };
    Ok(Value::from(reply))
}
