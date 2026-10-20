//! Dogma: `dogmaIM.GetAttributeTypes` and the bound dogma location
//! (environment/godma.py StateManager).
//!
//! The client does not evaluate effects; it shows the attribute values the
//! server reports. Each info record is a util.KeyVal with invItem (inventory
//! row), attributes {attributeID: value}, effects {effectID: isDefault},
//! activeEffects {effectID: ...} and time (blue time).

use std::collections::BTreeMap;

use marshal::Value;
use proto::gpcs::Call;

use super::inventory::{self, Item};
use super::{CallResult, Session};
use crate::{rows, statics, Server};

/// godma.InitializeData: `attributesByName[name].attributeID`, `.values()`
/// with header/lines, FilterRowset on chargeRechargeTimeID - a
/// util.IndexRowset keyed on attributeName over every dgmattribs column.
pub async fn attribute_types(server: &Server) -> CallResult {
    let c = server.pool.get().await?;
    let stmt = c
        .prepare(r#"SELECT * FROM static.dgmattribs ORDER BY "attributeID""#)
        .await?;
    let rs = c.query(&stmt, &[]).await?;
    let header: Vec<Value> = stmt
        .columns()
        .iter()
        .map(|c| Value::str(c.name()))
        .collect();
    let name_idx = stmt
        .columns()
        .iter()
        .position(|c| c.name() == "attributeName")
        .unwrap();
    let mut items = Vec::new();
    for r in &rs {
        let line = crate::db::row_values(r)?;
        items.push((line[name_idx].clone(), Value::List(line)));
    }
    Ok(Value::instance(
        "util.IndexRowset",
        Value::dict([
            (Value::str("items"), Value::Dict(items)),
            (Value::str("header"), Value::List(header)),
            (Value::str("idName"), Value::str("attributeName")),
        ]),
    ))
}

/// Attribute values of an item: its type's attributes, then per-item
/// overrides, then the charged attributes at full (capacitor, shields).
async fn attributes(
    server: &Server,
    item: &Item,
    char_attrs: Option<[i32; 5]>,
) -> anyhow::Result<BTreeMap<i32, f64>> {
    let c = server.pool.get().await?;
    let mut out = BTreeMap::new();
    for r in c
        .query(
            r#"SELECT "attributeID", "value" FROM static.dgmtypeattribs WHERE "typeID" = $1"#,
            &[&item.type_id],
        )
        .await?
    {
        out.insert(r.get::<_, i32>(0), r.get::<_, f64>(1));
    }
    for r in c
        .query(
            r#"SELECT "attributeID", "value" FROM game.item_attributes WHERE "itemID" = $1"#,
            &[&item.item_id],
        )
        .await?
    {
        out.insert(r.get::<_, i32>(0), r.get::<_, f64>(1));
    }
    let w = &server.world;
    for (charged, max) in [
        ("charge", "capacitorCapacity"),
        ("shieldCharge", "shieldCapacity"),
    ] {
        if let (Some(a), Some(m)) = (w.attribute(charged), w.attribute(max)) {
            if let Some(&v) = out.get(&m) {
                out.entry(a).or_insert(v);
            }
        }
    }
    if let Some(attrs) = char_attrs {
        for (i, name) in [
            "charisma",
            "intelligence",
            "memory",
            "perception",
            "willpower",
        ]
        .iter()
        .enumerate()
        {
            if let Some(a) = w.attribute(name) {
                out.insert(a, attrs[i] as f64);
            }
        }
    }
    Ok(out)
}

async fn effects(server: &Server, type_id: i32) -> anyhow::Result<Vec<(i32, i32)>> {
    let c = server.pool.get().await?;
    Ok(c.query(
        r#"SELECT "effectID", "isDefault" FROM static.dgmtypeeffects WHERE "typeID" = $1"#,
        &[&type_id],
    )
    .await?
    .iter()
    .map(|r| (r.get(0), r.get(1)))
    .collect())
}

async fn info(server: &Server, item: &Item, char_attrs: Option<[i32; 5]>) -> anyhow::Result<Value> {
    let attrs = attributes(server, item, char_attrs).await?;
    let effects = effects(server, item.type_id).await?;
    // Fitted modules are online (2003 modules go online when fitted; only GMs
    // toggle them, shipmodulebutton.py).
    let w = &server.world;
    let fitted =
        (w.flag("SlotFirst")..=w.flag("SlotLast")).contains(&item.flag) && item.singleton != 0;
    let online = super::combat::online_effect_id(server).await?;
    let active: Vec<(Value, Value)> = match online {
        Some(id) if fitted && effects.iter().any(|(e, _)| *e == id) => {
            vec![(
                Value::Int(id),
                Value::tuple([
                    Value::from(item.item_id),
                    Value::Int(id),
                    Value::Long(statics::blue_time_now()),
                ]),
            )]
        }
        _ => Vec::new(),
    };
    Ok(rows::keyval(vec![
        ("invItem", item.row(server)),
        ("itemID", Value::from(item.item_id)),
        (
            "attributes",
            Value::Dict(
                attrs
                    .into_iter()
                    .map(|(k, v)| (Value::Int(k), Value::Float(v)))
                    .collect(),
            ),
        ),
        (
            "effects",
            Value::Dict(
                effects
                    .into_iter()
                    .map(|(e, d)| (Value::Int(e), Value::Int(d)))
                    .collect(),
            ),
        ),
        ("activeEffects", Value::Dict(active)),
        ("time", Value::Long(statics::blue_time_now())),
    ]))
}

async fn char_attributes(server: &Server, char_id: i32) -> anyhow::Result<[i32; 5]> {
    let c = server.pool.get().await?;
    let r = c
        .query_one(
            r#"SELECT "charisma", "intelligence", "memory", "perception", "willpower"
               FROM game.characters WHERE "characterID" = $1"#,
            &[&char_id],
        )
        .await?;
    Ok([r.get(0), r.get(1), r.get(2), r.get(3), r.get(4)])
}

/// dogmaIM[(locationID, groupID)] methods.
pub async fn location(server: &Server, session: &mut Session, call: &Call) -> CallResult {
    let char_id = session.char_id().unwrap_or(0);
    match call.method() {
        // {itemID: info} for the ship and everything in it.
        "ShipGetInfo" => {
            let Some(ship_id) = session.attrs.int("shipid") else {
                return Ok(Value::Dict(Vec::new()));
            };
            let mut out = Vec::new();
            if let Some(ship) = inventory::item(server, ship_id).await? {
                out.push((Value::from(ship.item_id), info(server, &ship, None).await?));
                for it in
                    inventory::items_where(server, r#""locationID" = $1"#, &[&ship_id]).await?
                {
                    out.push((Value::from(it.item_id), info(server, &it, None).await?));
                }
            }
            Ok(Value::Dict(out))
        }
        // {itemID: info} for the character and its skills/implants.
        "CharGetInfo" => {
            let mut out = Vec::new();
            if let Some(ch) = inventory::character_item(server, char_id).await? {
                let attrs = char_attributes(server, char_id).await?;
                out.push((
                    Value::from(ch.item_id),
                    info(server, &ch, Some(attrs)).await?,
                ));
                for it in
                    inventory::items_where(server, r#""locationID" = $1"#, &[&(char_id as i64)])
                        .await?
                {
                    out.push((Value::from(it.item_id), info(server, &it, None).await?));
                }
            }
            Ok(Value::Dict(out))
        }
        "ItemGetInfo" => {
            let id = call.args().first().and_then(Value::as_int).unwrap_or(0);
            if id == char_id as i64 {
                let ch = inventory::character_item(server, char_id)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("no character"))?;
                return Ok(info(server, &ch, Some(char_attributes(server, char_id).await?)).await?);
            }
            match inventory::item(server, id).await? {
                Some(it) => Ok(info(server, &it, None).await?),
                None => Ok(Value::None),
            }
        }
        m => super::unimplemented("dogmaIM", m, call),
    }
}
