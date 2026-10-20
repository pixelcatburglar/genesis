//! Skills: the bound `skillMgr` (environment/godma.py GetSkillHandler, the
//! skill sheet and item menus) and dogmaIM's prerequisite lookups.
//!
//! A skill is an item in the character (flagSkill) with the item attributes
//! skillLevel and skillPoints; its type carries skillTimeConstant (the rank)
//! and primary/secondaryAttribute. The client's formulas (godma.py
//! GetSPForLevel, GetSPM) decide the numbers: reaching level L+1 takes
//! rank*250 * 2^(2.5*L) skill points (rank*250 for level 1), gained at
//! primary + secondary/2 points a minute. One skill trains at a time
//! (flagSkillInTraining); `tick` completes training when it is due.

use std::time::{Duration, SystemTime};

use marshal::Value;
use proto::gpcs::Call;

use super::inventory::{self, Item};
use super::{user_error, Args, CallResult, Session};
use crate::{db, Server};

fn attr(server: &Server, name: &str) -> i32 {
    server
        .world
        .attribute(name)
        .unwrap_or_else(|| panic!("attribute {name} missing"))
}

/// The skill points the client shows as spHi for `level`: needed to reach level+1.
pub fn sp_for_level(rank: f64, level: i32) -> f64 {
    let stc = rank * 250.0;
    match level {
        l if l <= 0 => stc,
        l => stc * 2f64.powf(2.5 * l as f64),
    }
}

async fn item_attr(server: &Server, item_id: i64, attribute: i32) -> anyhow::Result<Option<f64>> {
    db::scalar(
        &server.pool,
        r#"SELECT "value" FROM game.item_attributes WHERE "itemID" = $1 AND "attributeID" = $2"#,
        &[&item_id, &attribute],
    )
    .await
}

async fn type_attr(server: &Server, type_id: i32, attribute: i32) -> anyhow::Result<Option<f64>> {
    db::scalar(
        &server.pool,
        r#"SELECT "value" FROM static.dgmtypeattribs WHERE "typeID" = $1 AND "attributeID" = $2"#,
        &[&type_id, &attribute],
    )
    .await
}

async fn set_item_attr(
    server: &Server,
    item_id: i64,
    attribute: i32,
    value: f64,
) -> anyhow::Result<()> {
    db::exec(
        &server.pool,
        r#"INSERT INTO game.item_attributes ("itemID", "attributeID", "value") VALUES ($1, $2, $3)
           ON CONFLICT ("itemID", "attributeID") DO UPDATE SET "value" = $3"#,
        &[&item_id, &attribute, &value],
    )
    .await?;
    Ok(())
}

/// The trained level of a skill type, 0 if the character lacks it.
pub async fn skill_level(server: &Server, char_id: i32, type_id: i32) -> anyhow::Result<i32> {
    let v: Option<f64> = db::scalar(
        &server.pool,
        r#"SELECT max(a."value") FROM game.items i JOIN game.item_attributes a ON a."itemID" = i."itemID" AND a."attributeID" = $3
           WHERE i."locationID" = $1 AND i."typeID" = $2"#,
        &[&(char_id as i64), &type_id, &attr(server, "skillLevel")],
    )
    .await?
    .flatten();
    Ok(v.unwrap_or(0.0) as i32)
}

/// Skill points a minute for a skill (godma.py GetSPM).
async fn sp_per_minute(server: &Server, char_id: i32, type_id: i32) -> anyhow::Result<f64> {
    let c = server.pool.get().await?;
    let r = c
        .query_one(r#"SELECT "charisma", "intelligence", "memory", "perception", "willpower" FROM game.characters WHERE "characterID" = $1"#, &[&char_id])
        .await?;
    let value_of = |attr_id: Option<f64>| -> f64 {
        let Some(id) = attr_id.map(|v| v as i32) else {
            return 0.0;
        };
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
            if server.world.attribute(name) == Some(id) {
                return r.get::<_, i32>(i) as f64;
            }
        }
        0.0
    };
    let primary = value_of(type_attr(server, type_id, attr(server, "primaryAttribute")).await?);
    let secondary = value_of(type_attr(server, type_id, attr(server, "secondaryAttribute")).await?);
    Ok((primary + secondary / 2.0).max(1.0))
}

/// Tells the owner a dogma attribute of an item changed (godma
/// OnModuleAttributeChange(ownerID, itemID, attributeName, time, new, old)).
pub fn attribute_changed(
    server: &Server,
    owner: i32,
    item_id: i64,
    name: &str,
    new: f64,
    old: f64,
) {
    super::notify_chars(
        server,
        &[owner],
        "OnModuleAttributeChange",
        vec![
            Value::Int(owner),
            Value::from(item_id),
            Value::str(name),
            Value::Long(crate::statics::blue_time_now()),
            Value::Float(new),
            Value::Float(old),
        ],
    );
}

async fn set_flag(server: &Server, item: &Item, flag: i32) -> anyhow::Result<Item> {
    db::exec(
        &server.pool,
        r#"UPDATE game.items SET "flag" = $2 WHERE "itemID" = $1"#,
        &[&item.item_id, &flag],
    )
    .await?;
    let mut new = item.clone();
    new.flag = flag;
    inventory::notify_change(server, &new, vec![("ixFlag", Value::Int(item.flag))]);
    Ok(new)
}

/// Stops training: banks the points earned so far.
async fn stop_training(server: &Server, char_id: i32, silent: bool) -> anyhow::Result<()> {
    let c = server.pool.get().await?;
    let Some(t) = c
        .query_opt(
            r#"DELETE FROM game.skill_training WHERE "characterID" = $1
               RETURNING "itemID", "startTime", "endTime", "startSP", "targetSP""#,
            &[&char_id],
        )
        .await?
    else {
        return Ok(());
    };
    drop(c);
    let item_id: i64 = t.get(0);
    let (start, end): (SystemTime, SystemTime) = (t.get(1), t.get(2));
    let (start_sp, target_sp): (f64, f64) = (t.get(3), t.get(4));
    let total = end
        .duration_since(start)
        .unwrap_or_default()
        .as_secs_f64()
        .max(1.0);
    let done = SystemTime::now()
        .duration_since(start)
        .unwrap_or_default()
        .as_secs_f64()
        .min(total);
    let sp = start_sp + (target_sp - start_sp) * done / total;
    set_item_attr(server, item_id, attr(server, "skillPoints"), sp).await?;
    attribute_changed(server, char_id, item_id, "skillPoints", sp, start_sp);
    if let Some(item) = inventory::item(server, item_id).await? {
        set_flag(server, &item, server.world.flag("Skill")).await?;
    }
    super::notify_chars(
        server,
        &[char_id],
        "OnSkillTrainingStopped",
        vec![Value::from(item_id), Value::Int(silent as i32)],
    );
    Ok(())
}

/// Finishes one training: the level goes up and the points reach the mark.
async fn complete(
    server: &Server,
    char_id: i32,
    item_id: i64,
    target_sp: f64,
    target_level: i32,
) -> anyhow::Result<()> {
    let level_attr = attr(server, "skillLevel");
    let old_level = item_attr(server, item_id, level_attr).await?.unwrap_or(0.0);
    let old_sp = item_attr(server, item_id, attr(server, "skillPoints"))
        .await?
        .unwrap_or(0.0);
    set_item_attr(server, item_id, level_attr, target_level as f64).await?;
    set_item_attr(server, item_id, attr(server, "skillPoints"), target_sp).await?;
    if let Some(item) = inventory::item(server, item_id).await? {
        set_flag(server, &item, server.world.flag("Skill")).await?;
    }
    attribute_changed(server, char_id, item_id, "skillPoints", target_sp, old_sp);
    attribute_changed(
        server,
        char_id,
        item_id,
        "skillLevel",
        target_level as f64,
        old_level,
    );
    super::notify_chars(
        server,
        &[char_id],
        "OnSkillTrained",
        vec![Value::from(item_id)],
    );
    Ok(())
}

/// Completes every training that is due (run periodically).
pub async fn tick(server: &Server) -> anyhow::Result<()> {
    let c = server.pool.get().await?;
    let due = c
        .query(
            r#"DELETE FROM game.skill_training WHERE "endTime" <= now()
               RETURNING "characterID", "itemID", "targetSP", "targetLevel""#,
            &[],
        )
        .await?;
    drop(c);
    for r in due {
        complete(server, r.get(0), r.get(1), r.get(2), r.get(3)).await?;
    }
    Ok(())
}

pub async fn run_ticker(server: std::sync::Arc<Server>) {
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        if let Err(e) = tick(&server).await {
            tracing::warn!("skill training tick: {e:#}");
        }
        if let Err(e) = super::factory::tick(&server).await {
            tracing::warn!("factory tick: {e:#}");
        }
        if let Err(e) = super::broker::tick(&server).await {
            tracing::warn!("broker tick: {e:#}");
        }
    }
}

/// {skillTypeID: level} the type requires (requiredSkillN / requiredSkillNLevel).
pub async fn required_skills(server: &Server, type_id: i32) -> anyhow::Result<Value> {
    let mut out = Vec::new();
    for n in 1..=3 {
        let skill = type_attr(server, type_id, attr(server, &format!("requiredSkill{n}"))).await?;
        let level = type_attr(
            server,
            type_id,
            attr(server, &format!("requiredSkill{n}Level")),
        )
        .await?;
        if let Some(s) = skill.filter(|&s| s > 0.0) {
            out.push((
                Value::Int(s as i32),
                Value::Int(level.unwrap_or(1.0) as i32),
            ));
        }
    }
    Ok(Value::Dict(out))
}

/// skillMgr[(locationID, groupID)] methods.
pub async fn skill_mgr(server: &Server, session: &mut Session, call: &Call) -> CallResult {
    let a = Args::of(call);
    let me = session.char_id().unwrap_or(0);
    match call.method() {
        // factories.py: `const.typeIndustry in skills` - keyed by skill typeID.
        "CharGetSkills" => {
            let skills = inventory::items_where(
                server,
                r#""locationID" = $1 AND "flag" IN ($2, $3)"#,
                &[
                    &(me as i64),
                    &server.world.flag("Skill"),
                    &server.world.flag("SkillInTraining"),
                ],
            )
            .await?;
            let mut out = Vec::new();
            for s in skills {
                let level = item_attr(server, s.item_id, attr(server, "skillLevel"))
                    .await?
                    .unwrap_or(0.0);
                out.push((Value::Int(s.type_id), Value::Int(level as i32)));
            }
            Ok(Value::Dict(out))
        }
        // CharStartTrainingSkill(itemID): one at a time; the old one stops.
        "CharStartTrainingSkill" => {
            let item_id = a.int(0).unwrap_or(0);
            let Some(item) = inventory::item(server, item_id)
                .await?
                .filter(|i| i.location_id == me as i64)
            else {
                return Err(user_error(
                    "CustomInfo",
                    vec![("info", Value::str("That skill is not yours."))],
                ));
            };
            let level = item_attr(server, item_id, attr(server, "skillLevel"))
                .await?
                .unwrap_or(0.0) as i32;
            if level >= 5 {
                return Err(user_error(
                    "CustomInfo",
                    vec![("info", Value::str("That skill is fully trained."))],
                ));
            }
            stop_training(server, me, true).await?;
            let rank = type_attr(server, item.type_id, attr(server, "skillTimeConstant"))
                .await?
                .unwrap_or(1.0);
            let sp = item_attr(server, item_id, attr(server, "skillPoints"))
                .await?
                .unwrap_or(0.0);
            let target = sp_for_level(rank, level);
            let minutes =
                ((target - sp).max(0.0)) / sp_per_minute(server, me, item.type_id).await?;
            let end = SystemTime::now() + Duration::from_secs_f64(minutes * 60.0);
            db::exec(
                &server.pool,
                r#"INSERT INTO game.skill_training ("characterID", "itemID", "endTime", "startSP", "targetSP", "targetLevel")
                   VALUES ($1, $2, $3, $4, $5, $6)"#,
                &[&me, &item_id, &end, &sp, &target, &(level + 1)],
            )
            .await?;
            set_flag(server, &item, server.world.flag("SkillInTraining")).await?;
            super::notify_chars(
                server,
                &[me],
                "OnSkillStartTraining",
                vec![Value::from(item_id), Value::Long(db::blue_time(end))],
            );
            Ok(Value::None)
        }
        "GetEndOfTraining" => {
            let end: Option<SystemTime> = db::scalar(
                &server.pool,
                r#"SELECT "endTime" FROM game.skill_training WHERE "characterID" = $1 AND "itemID" = $2"#,
                &[&me, &a.int(0).unwrap_or(0)],
            )
            .await?;
            Ok(end
                .map(|e| Value::Long(db::blue_time(e)))
                .unwrap_or(Value::None))
        }
        // Plug an implant / take a booster: the item moves into the character.
        "CharAddImplant" | "CharAddBooster" => {
            let item_id = a.int(0).unwrap_or(0);
            let Some(item) = inventory::item(server, item_id)
                .await?
                .filter(|i| i.owner_id == me)
            else {
                return Ok(Value::None);
            };
            let flag = server.world.flag(if call.method() == "CharAddImplant" {
                "Implant"
            } else {
                "Booster"
            });
            let item = inventory::split_one(server, &item).await?;
            inventory::move_item(server, &item, me as i64, flag).await?;
            Ok(Value::None)
        }
        "RemoveImplantFromCharacter" => {
            let item_id = a.int(0).unwrap_or(0);
            if let Some(item) = inventory::item(server, item_id)
                .await?
                .filter(|i| i.location_id == me as i64)
            {
                inventory::delete_item(server, &item).await?;
            }
            Ok(Value::None)
        }
        m => super::unimplemented("skillMgr", m, call),
    }
}
