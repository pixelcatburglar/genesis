//! `reprocessing`: refining ore and recycling manufactured items into
//! minerals at a station (ui/station/reprocessingplant/base_reprocessingplant.py).
//!
//! Only whole portions (portionSize units) are processed. Of what a portion
//! holds (static.type_materials) the station recovers its efficiency share
//! (more with the Refining skill), keeps a tax of that, and the rest is lost:
//! the quote's client / station / unrecoverable columns.

use marshal::Value;
use proto::gpcs::Call;

use super::inventory;
use super::{factory, skills, user_error, Args, CallResult, Session};
use crate::{db, rows, Server};

/// TEMPLATE DEFAULT: base station efficiency, +2% per Refining level.
const BASE_EFFICIENCY: f64 = 0.5;
const REFINING_BONUS: f64 = 0.02;
/// TEMPLATE DEFAULT share of the recovered minerals the station keeps.
const STATION_TAX: f64 = 0.05;

struct Quote {
    item: inventory::Item,
    to_process: i32,
    /// (mineral, client, station, unrecoverable)
    lines: Vec<(i32, i64, i64, i64)>,
}

async fn portion(server: &Server, type_id: i32) -> anyhow::Result<i32> {
    Ok(db::scalar::<i32>(&server.pool, r#"SELECT "portionSize" FROM static.types WHERE "typeID" = $1"#, &[&type_id])
        .await?
        .unwrap_or(1)
        .max(1))
}

async fn quote(server: &Server, session: &Session, item_id: i64) -> anyhow::Result<Option<Quote>> {
    let me = session.char_id().unwrap_or(0);
    let Some(item) = inventory::item(server, item_id).await? else { return Ok(None) };
    let station = session.attrs.int("stationid").unwrap_or(0);
    if item.owner_id != me || item.location_id != station || item.flag != server.world.flag("Hangar") {
        return Ok(None);
    }
    let mats = factory::materials(server, item.type_id).await?;
    if mats.is_empty() {
        return Ok(None);
    }
    let portion = portion(server, item.type_id).await?;
    let portions = item.quantity / portion;
    if portions == 0 {
        return Ok(None);
    }
    let refining = skills::skill_level(server, me, server.world.type_id("Refining")).await?;
    let efficiency = (BASE_EFFICIENCY + REFINING_BONUS * refining as f64).min(1.0);
    let lines = mats
        .into_iter()
        .map(|(m, q)| {
            let total = q as i64 * portions as i64;
            let recovered = (total as f64 * efficiency).floor() as i64;
            let tax = (recovered as f64 * STATION_TAX).floor() as i64;
            (m, recovered - tax, tax, total - recovered)
        })
        .collect();
    Ok(Some(Quote { item, to_process: portions * portion, lines }))
}

pub async fn dispatch(server: &Server, session: &mut Session, method: &str, call: &Call) -> CallResult {
    let a = Args::of(call);
    let w = &server.world;
    match method {
        // {typeID: KeyVal(isRecyclable, isRefinable)}; ore is refined, other
        // types with materials are recycled.
        "GetOptionsForItemTypes" => {
            let ids: Vec<i32> = match a.pos.first() {
                Some(Value::Dict(d)) => d.iter().filter_map(|(k, _)| k.as_int()).map(|t| t as i32).collect(),
                _ => a.ints(0).into_iter().map(|t| t as i32).collect(),
            };
            let mut out = Vec::new();
            for t in ids {
                let has = !factory::materials(server, t).await?.is_empty();
                let ore = w.type_info(t).is_some_and(|ti| Some(ti.group_id) == w.group_id("Asteroid"));
                out.push((
                    Value::Int(t),
                    rows::keyval(vec![("isRecyclable", Value::from(has && !ore)), ("isRefinable", Value::from(has && ore))]),
                ));
            }
            Ok(Value::Dict(out))
        }
        // KeyVal(quantityToProcess, leftOvers, recoverables=[KeyVal(typeID, client, station, unrecoverable)])
        "GetQuote" => {
            let Some(q) = quote(server, session, a.int(0).unwrap_or(0)).await? else { return Ok(Value::None) };
            Ok(rows::keyval(vec![
                ("quantityToProcess", Value::Int(q.to_process)),
                ("leftOvers", Value::Int(q.item.quantity - q.to_process)),
                (
                    "recoverables",
                    Value::List(
                        q.lines
                            .iter()
                            .map(|(m, c, s, u)| {
                                rows::keyval(vec![
                                    ("typeID", Value::Int(*m)),
                                    ("client", Value::from(*c)),
                                    ("station", Value::from(*s)),
                                    ("unrecoverable", Value::from(*u)),
                                ])
                            })
                            .collect(),
                    ),
                ),
            ]))
        }
        "Reprocess" => {
            let item_id = a.int(0).unwrap_or(0);
            if Some(item_id) == session.attrs.int("shipid") {
                return Err(user_error("CustomInfo", vec![("info", Value::str("You can not recycle your active ship."))]));
            }
            let Some(q) = quote(server, session, item_id).await? else {
                return Err(user_error("CustomInfo", vec![("info", Value::str("That item can not be reprocessed."))]));
            };
            // Contents of a recycled container or ship drop into the hangar.
            for inner in inventory::items_where(server, r#""locationID" = $1"#, &[&q.item.item_id]).await? {
                inventory::stack_into(server, &inner, q.item.location_id, w.flag("Hangar")).await?;
            }
            if q.to_process >= q.item.quantity {
                inventory::delete_item(server, &q.item).await?;
            } else {
                inventory::set_quantity(server, &q.item, q.item.quantity - q.to_process).await?;
            }
            for (m, got, _, _) in q.lines {
                if got <= 0 {
                    continue;
                }
                inventory::give(server, m, q.item.owner_id, q.item.location_id, w.flag("Hangar"), got as i32).await?;
            }
            Ok(Value::None)
        }
        m => super::unimplemented("reprocessing", m, call),
    }
}
