//! `repair`: the station repair shop (ui/station/repairshop/base_repairshop.py).
//!
//! GetDamageReport(shipID).quote lists the ship and what is fitted to it as
//! KeyVal(itemID, typeID, damage, maxHealth, costToRepairOneUnitOfDamage).
//! A ship's damage is its armour plus hull damage (the "armorDamage" and
//! "damage" item attributes combat keeps); RepairModule / RepairShip spend an
//! amount of ISK on repairs, armour first.

use marshal::Value;
use proto::gpcs::Call;

use super::inventory::{self, Item};
use super::{user_error, wallet, Args, CallResult, Session};
use crate::{db, rows, Server};

/// TEMPLATE DEFAULT ISK per hit point repaired.
const COST_PER_UNIT: i64 = 10;

async fn attr(server: &Server, item_id: i64, name: &str) -> anyhow::Result<f64> {
    let Some(a) = server.world.attribute(name) else { return Ok(0.0) };
    Ok(db::scalar::<f64>(
        &server.pool,
        r#"SELECT "value" FROM game.item_attributes WHERE "itemID" = $1 AND "attributeID" = $2"#,
        &[&item_id, &a],
    )
    .await?
    .unwrap_or(0.0))
}

async fn set_attr(server: &Server, item_id: i64, name: &str, value: f64) -> anyhow::Result<()> {
    let Some(a) = server.world.attribute(name) else { return Ok(()) };
    db::exec(
        &server.pool,
        r#"INSERT INTO game.item_attributes VALUES ($1, $2, $3) ON CONFLICT ("itemID", "attributeID") DO UPDATE SET "value" = $3"#,
        &[&item_id, &a, &value],
    )
    .await?;
    Ok(())
}

/// (armour damage, armour max, hull damage, hull max) of an item.
async fn health(server: &Server, it: &Item) -> anyhow::Result<(f64, f64, f64, f64)> {
    let ty = |n: &'static str| inventory::type_attribute(server, it.type_id, n);
    Ok((
        attr(server, it.item_id, "armorDamage").await?,
        ty("armorHP").await?.unwrap_or(0.0),
        attr(server, it.item_id, "damage").await?,
        ty("hp").await?.unwrap_or(0.0),
    ))
}

/// Items the caller may repair: in their hangar here, or fitted to such a ship.
async fn repairable(server: &Server, session: &Session, item_id: i64) -> anyhow::Result<Option<Item>> {
    let me = session.char_id().unwrap_or(0);
    let station = session.attrs.int("stationid").unwrap_or(-1);
    let Some(it) = inventory::item(server, item_id).await? else { return Ok(None) };
    if it.owner_id != me {
        return Ok(None);
    }
    if it.location_id == station {
        return Ok(Some(it));
    }
    let parent = inventory::item(server, it.location_id).await?;
    Ok(parent.filter(|p| p.location_id == station && p.owner_id == me).map(|_| it))
}

/// Repairs up to `budget` ISK worth of damage; returns what it cost.
async fn repair(server: &Server, it: &Item, budget: i64) -> anyhow::Result<i64> {
    let (mut armor, _, mut hull, _) = health(server, it).await?;
    let mut units = (budget / COST_PER_UNIT).max(0) as f64;
    let mut spent = 0.0;
    for (value, name) in [(&mut armor, "armorDamage"), (&mut hull, "damage")] {
        let fix = value.ceil().min(units);
        if fix > 0.0 {
            *value = (*value - fix).max(0.0);
            units -= fix;
            spent += fix;
            set_attr(server, it.item_id, name, *value).await?;
        }
    }
    Ok(spent as i64 * COST_PER_UNIT)
}

async fn charge(server: &Server, session: &Session, cost: i64) -> CallResult {
    let me = session.char_id().unwrap_or(0);
    let station = session.attrs.int("stationid").unwrap_or(0) as i32;
    let landlord = server.world.stations.get(&station).map(|s| s.corporation_id).unwrap_or(0);
    wallet::transfer(server, me, landlord, cost as f64, "refRepairBill", Some(station as i64), "").await?;
    Ok(Value::None)
}

pub async fn dispatch(server: &Server, session: &mut Session, method: &str, call: &Call) -> CallResult {
    let a = Args::of(call);
    let w = &server.world;
    match method {
        "GetDamageReport" => {
            let Some(ship) = repairable(server, session, a.int(0).unwrap_or(0)).await? else {
                return Ok(rows::keyval(vec![("quote", Value::List(vec![]))]));
            };
            let (s0, s1) = (w.flag("SlotFirst"), w.flag("SlotLast"));
            let mut items = vec![ship.clone()];
            items.extend(
                inventory::items_where(server, r#""locationID" = $1 AND "flag" BETWEEN $2 AND $3"#, &[&ship.item_id, &s0, &s1]).await?,
            );
            let mut quote = Vec::new();
            for it in items {
                let (armor, armor_max, hull, hull_max) = health(server, &it).await?;
                quote.push(rows::keyval(vec![
                    ("itemID", Value::from(it.item_id)),
                    ("typeID", Value::Int(it.type_id)),
                    ("damage", Value::Float(armor + hull)),
                    ("maxHealth", Value::Float(armor_max + hull_max)),
                    ("costToRepairOneUnitOfDamage", Value::from(COST_PER_UNIT)),
                ]));
            }
            Ok(rows::keyval(vec![("quote", Value::List(quote))]))
        }
        // RepairModule(itemID, iskToSpend)
        "RepairModule" => {
            let Some(it) = repairable(server, session, a.int(0).unwrap_or(0)).await? else { return Ok(Value::None) };
            let budget = a.float(1).unwrap_or(0.0) as i64;
            check_funds(server, session, budget).await?;
            let cost = repair(server, &it, budget).await?;
            charge(server, session, cost).await
        }
        // RepairShip(shipID, iskToSpend): the ship, then its modules.
        "RepairShip" => {
            let Some(ship) = repairable(server, session, a.int(0).unwrap_or(0)).await? else { return Ok(Value::None) };
            let mut budget = a.float(1).unwrap_or(0.0) as i64;
            check_funds(server, session, budget).await?;
            let (s0, s1) = (w.flag("SlotFirst"), w.flag("SlotLast"));
            let mut items = vec![ship.clone()];
            items.extend(
                inventory::items_where(server, r#""locationID" = $1 AND "flag" BETWEEN $2 AND $3"#, &[&ship.item_id, &s0, &s1]).await?,
            );
            let mut total = 0;
            for it in items {
                let cost = repair(server, &it, budget).await?;
                budget -= cost;
                total += cost;
            }
            charge(server, session, total).await
        }
        // DamageModule(itemID, units): the shop's test button.
        "DamageModule" => {
            let Some(it) = repairable(server, session, a.int(0).unwrap_or(0)).await? else { return Ok(Value::None) };
            let (_, _, hull, hull_max) = health(server, &it).await?;
            let units = a.float(1).unwrap_or(1.0);
            let hull = if hull_max > 0.0 { (hull + units).min(hull_max) } else { hull + units };
            set_attr(server, it.item_id, "damage", hull).await?;
            Ok(Value::None)
        }
        m => super::unimplemented("repair", m, call),
    }
}

async fn check_funds(server: &Server, session: &Session, amount: i64) -> Result<(), super::CallError> {
    let have = wallet::balance(server, session.char_id().unwrap_or(0)).await?;
    if have < amount as f64 {
        return Err(user_error("NotEnoughMoney", vec![("balance", Value::Float(have)), ("amount", Value::from(amount))]));
    }
    Ok(())
}
