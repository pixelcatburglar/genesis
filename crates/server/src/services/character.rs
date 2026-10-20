//! The `character` service: the creation wizard, character select and logon.
//! Client side: ui/login/{ccutil,steps,charcreationbase,charsel}.py, ui/util/photo.py.

use anyhow::{anyhow, Context};
use marshal::Value;
use proto::gpcs::Call;

use super::{user_error, CallResult, Session};
use crate::{db, rows, Server};

pub async fn dispatch(
    server: &Server,
    session: &mut Session,
    method: &str,
    call: &Call,
) -> CallResult {
    let args = call.args();
    match method {
        "GetCharCreationInfo" => Ok(server.world.creation.info.clone()),
        "GetAppearanceInfo" => Ok(server.world.creation.appearance_info.clone()),
        // steps.py Race.Select: a Jove key. None of ours are valid.
        "ValidateJoveCode" => Err(user_error("InvalidJoveCode", vec![("code", args.first().cloned().unwrap_or(Value::None))])),
        "GetCharacterInfo" => get_character_info(server, session).await,
        "GetCharacterAppearanceList" => get_appearance_list(server, args).await,
        "CreateCharacter" => create_character(server, session, args).await,
        "DeleteCharacter" => delete_character(server, session, args).await,
        "SelectCharacterID" => select_character(server, session, args).await,
        "GetHomeStation" => home_station(server, session.char_id().unwrap_or(0)).await,
        // medical.py: rows read for stationID - here, my school's station and my corporation's HQ.
        "GetPotentialHomeStations" => {
            let me = session.char_id().unwrap_or(0);
            let c = server.pool.get().await.map_err(anyhow::Error::from)?;
            let mut ids: Vec<i32> = c
                .query(
                    r#"SELECT s."stationID" FROM game.characters ch JOIN static.cc_schools s USING ("schoolID") WHERE ch."characterID" = $1
                       UNION SELECT c."stationID" FROM game.characters ch JOIN game.corporations c USING ("corporationID")
                         WHERE ch."characterID" = $1 AND c."stationID" IS NOT NULL"#,
                    &[&me],
                )
                .await
                .map_err(anyhow::Error::from)?
                .iter()
                .map(|r| r.get(0))
                .collect();
            if let Some(st) = session.attrs.int("stationid") {
                ids.insert(0, st as i32);
            }
            ids.dedup();
            Ok(crate::rows::rowset(&["stationID"], ids.into_iter().map(|i| vec![Value::Int(i)]).collect()))
        }
        // SetHomeStation(stationID): a clone contract costs const.costCloneContract.
        "SetHomeStation" => {
            let me = session.char_id().unwrap_or(0);
            let station = int_arg(args, 0, "stationID")? as i32;
            let Some(st) = server.world.stations.get(&station) else { return Ok(Value::None) };
            let cost = server.world.constant("costCloneContract") as f64;
            super::wallet::transfer(server, me, st.corporation_id, cost, "refCloneActivation", Some(station as i64), "").await?;
            db::exec(&server.pool, r#"UPDATE game.characters SET "homeStationID" = $2 WHERE "characterID" = $1"#, &[&me, &station])
                .await?;
            Ok(Value::None)
        }
        // map.py STARMODE_VISITED: solarSystemID, visits, lastDateTime.
        "GetSolarSystemVisits" => Ok(db::rowset(
            &server.pool,
            r#"SELECT "solarSystemID", "visits", "lastDateTime" FROM game.system_visits WHERE "characterID" = $1"#,
            &[&session.char_id().unwrap_or(0)],
        )
        .await?),
        // [(solarSystemID, stations)]
        "GetStationCount" => {
            let mut counts: std::collections::BTreeMap<i32, i32> = Default::default();
            for st in server.world.stations.values() {
                *counts.entry(st.solar_system_id).or_default() += 1;
            }
            Ok(Value::List(counts.into_iter().map(|(s, n)| Value::tuple([Value::Int(s), Value::Int(n)])).collect()))
        }
        // GetHistory(kind): [(solarSystemID, events in the last hour)] for the
        // map's jumps (4), ship kills (26) and pod kills (9) modes.
        "GetHistory" => {
            let kind = int_arg(args, 0, "kind")? as i32;
            let c = server.pool.get().await.map_err(anyhow::Error::from)?;
            let rs = c
                .query(
                    r#"SELECT "solarSystemID", count(*)::int FROM game.system_events
                       WHERE "kind" = $1 AND "at" > now() - interval '1 hour' GROUP BY 1"#,
                    &[&kind],
                )
                .await
                .map_err(anyhow::Error::from)?;
            Ok(Value::List(rs.iter().map(|r| Value::tuple([Value::Int(r.get(0)), Value::Int(r.get(1))])).collect()))
        }
        _ => super::unimplemented("character", method, call),
    }
}

/// The station a character's clone waits in: the contracted one, else the school's.
pub async fn home_station(server: &Server, char_id: i32) -> CallResult {
    let st: Option<i32> = db::scalar(
        &server.pool,
        r#"SELECT coalesce(ch."homeStationID", s."stationID") FROM game.characters ch
           JOIN static.cc_schools s USING ("schoolID") WHERE ch."characterID" = $1"#,
        &[&char_id],
    )
    .await?;
    Ok(Value::from(st))
}

/// Map history: a character arriving in a system (visits) and a jump event.
pub async fn record_arrival(
    server: &Server,
    char_id: i32,
    system: i32,
    jumped: bool,
) -> anyhow::Result<()> {
    db::exec(
        &server.pool,
        r#"INSERT INTO game.system_visits ("characterID", "solarSystemID") VALUES ($1, $2)
           ON CONFLICT ("characterID", "solarSystemID") DO UPDATE SET "visits" = game.system_visits."visits" + 1,
               "lastDateTime" = now()"#,
        &[&char_id, &system],
    )
    .await?;
    if jumped {
        db::exec(
            &server.pool,
            r#"INSERT INTO game.system_events ("solarSystemID", "kind") VALUES ($1, 4)"#,
            &[&system],
        )
        .await?;
    }
    Ok(())
}

fn int_arg(args: &[Value], i: usize, what: &str) -> anyhow::Result<i64> {
    args.get(i)
        .and_then(Value::as_int)
        .ok_or_else(|| anyhow!("argument {i} ({what}) is not an int: {:?}", args.get(i)))
}

/// charsel.LoadCharacters: rows with characterID, characterName, gender,
/// bloodlineID, corporationID, security and hierarchy =
/// (stationID, solarsystemID, constellationID, regionID).
async fn get_character_info(server: &Server, session: &Session) -> CallResult {
    let c = server.pool.get().await.map_err(anyhow::Error::from)?;
    let rows_ = c
        .query(
            r#"SELECT "characterID", "characterName", "gender", "bloodlineID", "corporationID", "securityStatus",
                      "stationID", "solarSystemID", "constellationID", "regionID"
               FROM game.characters WHERE "userID" = $1 AND "deleted" IS NULL ORDER BY "characterID" LIMIT 3"#,
            &[&(session.user_id.unwrap_or(0) as i32)],
        )
        .await
        .map_err(anyhow::Error::from)?;
    let lines = rows_
        .iter()
        .map(|r| {
            vec![
                Value::Int(r.get(0)),
                crate::db::text_value(r.get(1)),
                Value::Int(r.get(2)),
                Value::Int(r.get(3)),
                Value::Int(r.get(4)),
                Value::Float(r.get(5)),
                Value::tuple([
                    r.get::<_, Option<i32>>(6)
                        .map(Value::Int)
                        .unwrap_or(Value::None),
                    Value::Int(r.get(7)),
                    Value::Int(r.get(8)),
                    Value::Int(r.get(9)),
                ]),
            ]
        })
        .collect();
    Ok(rows::rowset(
        &[
            "characterID",
            "characterName",
            "gender",
            "bloodlineID",
            "corporationID",
            "security",
            "hierarchy",
        ],
        lines,
    ))
}

/// photo.PrimeCharAppearance: one single-row util.Rowset per requested ID, in
/// request order. A hole would shift every later portrait, so unknown IDs get
/// an empty-dict row rather than being skipped.
async fn get_appearance_list(server: &Server, args: &[Value]) -> CallResult {
    let ids: Vec<i32> = args
        .first()
        .and_then(Value::as_seq)
        .map(|s| {
            s.iter()
                .filter_map(Value::as_int)
                .map(|i| i as i32)
                .collect()
        })
        .unwrap_or_default();
    let c = server.pool.get().await.map_err(anyhow::Error::from)?;
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let rs = c
            .query(
                r#"SELECT "key", "intValue", "floatValue", "textValue" FROM game.character_appearance
                   WHERE "characterID" = $1 ORDER BY "key""#,
                &[&id],
            )
            .await
            .map_err(anyhow::Error::from)?;
        let header: Vec<String> = rs.iter().map(|r| r.get::<_, String>(0)).collect();
        let line: Vec<Value> = rs
            .iter()
            .map(|r| {
                if let Some(i) = r.get::<_, Option<i64>>(1) {
                    Value::from(i)
                } else if let Some(f) = r.get::<_, Option<f64>>(2) {
                    Value::Float(f)
                } else {
                    r.get::<_, Option<&str>>(3)
                        .map(crate::db::text_value)
                        .unwrap_or(Value::None)
                }
            })
            .collect();
        let header: Vec<&str> = header.iter().map(String::as_str).collect();
        out.push(rows::rowset(&header, vec![line]));
    }
    Ok(Value::List(out))
}

/// ccutil.ValidateName plus uniqueness.
fn validate_name(name: &str) -> Result<(), String> {
    if name.len() < 4 {
        return Err("Name must have a minimum of 4 characters.".into());
    }
    if name.len() > 16 {
        return Err("Name can't exceed 16 characters.".into());
    }
    if name.matches(' ').count() > 1 {
        return Err("Name can't have more than one space.".into());
    }
    if name.starts_with(' ') || name.ends_with(' ') {
        return Err("Name can't start or end with a space.".into());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == ' ' || c == '\'' || c == '-')
    {
        return Err(
            "Name can only contain letters, digits, spaces, apostrophes and hyphens.".into(),
        );
    }
    Ok(())
}

/// charcreationbase.CreateCharacter sends 14 positional args:
/// (name, bloodlineID, genderID, ancestryID, schoolID, departmentID, fieldID,
///  specialityID, int, cha, per, mem, wil, appearance) - the attribute args are
/// the wizard's bonus points only, and there is no raceID.
async fn create_character(server: &Server, session: &Session, args: &[Value]) -> CallResult {
    let name = args
        .first()
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    let invalid =
        |reason: &str| user_error("CharCreationInvalid", vec![("reason", Value::str(reason))]);
    if let Err(reason) = validate_name(&name) {
        return Err(user_error(
            "CharNameInvalid",
            vec![("reason", Value::str(reason))],
        ));
    }
    let bloodline_id = int_arg(args, 1, "bloodlineID")? as i32;
    let gender = int_arg(args, 2, "genderID")? as i32;
    let ancestry_id = int_arg(args, 3, "ancestryID")? as i32;
    let school_id = int_arg(args, 4, "schoolID")? as i32;
    let department_id = int_arg(args, 5, "departmentID")? as i32;
    let field_id = int_arg(args, 6, "fieldID")? as i32;
    let speciality_id = int_arg(args, 7, "specialityID")? as i32;
    // int, cha, per, mem, wil -> our Charisma, Intelligence, Memory, Perception, Willpower order
    let bonus_in =
        [8, 9, 10, 11, 12].map(|i| args.get(i).and_then(Value::as_int).unwrap_or(0) as i32);
    let bonus = [
        bonus_in[1],
        bonus_in[0],
        bonus_in[3],
        bonus_in[2],
        bonus_in[4],
    ];
    let appearance = args.get(13).cloned().unwrap_or(Value::None);

    let cr = &server.world.creation;
    let bl = cr
        .bloodlines
        .get(&bloodline_id)
        .ok_or_else(|| invalid("Unknown bloodline."))?;
    if !(0..=1).contains(&gender) {
        return Err(invalid("Unknown gender."));
    }
    // steps.py: 5 points to distribute, at most 3 on one attribute.
    if bonus.iter().any(|&b| !(0..=3).contains(&b)) || bonus.iter().sum::<i32>() > 5 {
        return Err(invalid("Invalid attribute point distribution."));
    }
    let (anc_bl, post, anc_skills) = cr
        .ancestries
        .get(&ancestry_id)
        .ok_or_else(|| invalid("Unknown ancestry."))?;
    let (school_race, corp_id, station_id) = *cr
        .schools
        .get(&school_id)
        .ok_or_else(|| invalid("Unknown school."))?;
    let (dept_school, dept_skills) = cr
        .departments
        .get(&department_id)
        .ok_or_else(|| invalid("Unknown department."))?;
    let (field_dept, field_skills, field_equip) = cr
        .fields
        .get(&field_id)
        .ok_or_else(|| invalid("Unknown field."))?;
    let (spec_field, spec_skills, spec_equip) = cr
        .specialities
        .get(&speciality_id)
        .ok_or_else(|| invalid("Unknown speciality."))?;
    if *anc_bl != bloodline_id
        || school_race != bl.race_id
        || *dept_school != school_id
        || *field_dept != department_id
        || *spec_field != field_id
    {
        return Err(invalid("Those choices do not belong together."));
    }
    let station = server
        .world
        .stations
        .get(&station_id)
        .context("school station missing")?;
    let attrs: Vec<i32> = (0..5).map(|i| bl.base[i] + post[i] + bonus[i]).collect();

    // Skills: one level per time a skill is granted (charcreationbase
    // RefreshAcquired counts repeats as levels).
    let mut skill_levels: Vec<(i32, i32)> = Vec::new();
    let granted = cr
        .race_skill
        .get(&bl.race_id)
        .into_iter()
        .chain(&bl.skills)
        .chain(anc_skills)
        .chain(dept_skills)
        .chain(field_skills)
        .chain(spec_skills);
    for &s in granted {
        match skill_levels.iter_mut().find(|(t, _)| *t == s) {
            Some((_, lvl)) => *lvl = (*lvl + 1).min(5),
            None => skill_levels.push((s, 1)),
        }
    }

    let mut client = server.pool.get().await.map_err(anyhow::Error::from)?;
    let tx = client.transaction().await.map_err(anyhow::Error::from)?;
    let user_id = session.user_id.unwrap_or(0) as i32;
    let count: i64 = tx
        .query_one(
            r#"SELECT count(*) FROM game.characters WHERE "userID" = $1 AND "deleted" IS NULL"#,
            &[&user_id],
        )
        .await
        .map_err(anyhow::Error::from)?
        .get(0);
    if count >= 3 {
        return Err(user_error("CharSlotsFull", vec![]));
    }
    let taken = tx
        .query_opt(
            r#"SELECT 1 FROM game.characters WHERE lower("characterName") = lower($1) AND "deleted" IS NULL"#,
            &[&name],
        )
        .await
        .map_err(anyhow::Error::from)?;
    if taken.is_some() {
        return Err(user_error(
            "CharNameTaken",
            vec![("name", Value::str(&name))],
        ));
    }
    let char_id: i32 = tx
        .query_one(
            r#"INSERT INTO game.characters ("userID", "characterName", "bloodlineID", "raceID", "gender", "ancestryID",
                   "schoolID", "departmentID", "fieldID", "specialityID", "charisma", "intelligence", "memory",
                   "perception", "willpower", "corporationID", "stationID", "solarSystemID", "constellationID",
                   "regionID", "balance", "homeStationID")
               VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$17)
               RETURNING "characterID""#,
            &[
                &user_id, &name, &bloodline_id, &bl.race_id, &gender, &ancestry_id, &school_id, &department_id,
                &field_id, &speciality_id, &attrs[0], &attrs[1], &attrs[2], &attrs[3], &attrs[4], &corp_id,
                &station_id, &station.solar_system_id, &station.constellation_id, &station.region_id,
                &STARTING_BALANCE,
            ],
        )
        .await
        .map_err(anyhow::Error::from)?
        .get(0);

    // Appearance dict, typed per value.
    if let Value::Dict(pairs) = &appearance {
        for (k, v) in pairs {
            let Some(key) = k.as_str() else { continue };
            let (i, f, t): (Option<i64>, Option<f64>, Option<String>) = match v {
                Value::Int(_) | Value::Long(_) => (v.as_int(), None, None),
                Value::Float(x) => (None, Some(*x), None),
                Value::None => continue,
                other => (None, None, other.as_str().map(str::to_owned)),
            };
            tx.execute(
                r#"INSERT INTO game.character_appearance VALUES ($1, $2, $3, $4, $5)"#,
                &[&char_id, &key, &i, &f, &t],
            )
            .await
            .map_err(anyhow::Error::from)?;
        }
    }

    let flag = |n: &str| server.world.flag(n);
    let item = |type_id: i32, location: i64, flag: i32, singleton: i32, qty: i32| {
        (type_id, char_id, location, flag, singleton, qty)
    };
    let mut items = Vec::new();
    for (skill, _) in &skill_levels {
        items.push(item(*skill, char_id as i64, flag("Skill"), 1, 1));
    }
    for (type_id, qty) in field_equip.iter().chain(spec_equip.iter()) {
        items.push(item(*type_id, station_id as i64, flag("Hangar"), 0, *qty));
    }
    let mut ship_id: Option<i64> = None;
    for (n, (type_id, owner, location, flag, singleton, qty)) in items.into_iter().enumerate() {
        let id: i64 = tx
            .query_one(
                r#"INSERT INTO game.items ("typeID", "ownerID", "locationID", "flag", "singleton", "quantity")
                   VALUES ($1, $2, $3, $4, $5, $6) RETURNING "itemID""#,
                &[&type_id, &owner, &location, &flag, &singleton, &qty],
            )
            .await
            .map_err(anyhow::Error::from)?
            .get(0);
        if let Some(&(_, level)) = skill_levels.get(n) {
            let sp = skill_points(level) as f64;
            for (attr, value) in [
                (server.world.attribute("skillLevel"), level as f64),
                (server.world.attribute("skillPoints"), sp),
            ] {
                if let Some(attr) = attr {
                    tx.execute(
                        r#"INSERT INTO game.item_attributes VALUES ($1, $2, $3)"#,
                        &[&id, &attr, &value],
                    )
                    .await
                    .map_err(anyhow::Error::from)?;
                }
            }
        }
    }
    // The rookie ship, assembled in the station hangar, as the active ship.
    if let Some(ship_type) = bl.ship_type_id {
        let id: i64 = tx
            .query_one(
                r#"INSERT INTO game.items ("typeID", "ownerID", "locationID", "flag", "singleton", "quantity", "itemName")
                   VALUES ($1, $2, $3, $4, 1, 1, $5) RETURNING "itemID""#,
                &[&ship_type, &char_id, &(station_id as i64), &flag("Hangar"), &format!("{name}'s ship")],
            )
            .await
            .map_err(anyhow::Error::from)?
            .get(0);
        ship_id = Some(id);
        // TEMPLATE DEFAULT starter kit: the race's light gun and a mining
        // laser in the high slots, a load of ammunition in the cargo hold.
        let race = bl.race_id;
        let (gun, ammo) = match race {
            r if Some(&r) == server.world.races.get("Amarr") => ("LightPulseLaserI", None),
            r if Some(&r) == server.world.races.get("Minmatar") => {
                ("LightAutocannonI", Some("PhasedPlasmaS"))
            }
            _ => ("LightRailgunI", Some("AntimatterChargeS")),
        };
        let fit = [
            (gun, flag("HiSlot0"), 1, 1),
            ("MinerI", flag("HiSlot1"), 1, 1),
        ];
        for (type_name, slot, singleton, qty) in fit
            .iter()
            .copied()
            .chain(ammo.map(|a| (a, flag("Cargo"), 0, 200)))
        {
            tx.execute(
                r#"INSERT INTO game.items ("typeID", "ownerID", "locationID", "flag", "singleton", "quantity")
                   VALUES ($1, $2, $3, $4, $5, $6)"#,
                &[&server.world.type_id(type_name), &char_id, &id, &slot, &singleton, &qty],
            )
            .await
            .map_err(anyhow::Error::from)?;
        }
    }
    tx.execute(
        r#"UPDATE game.characters SET "shipID" = $2 WHERE "characterID" = $1"#,
        &[&char_id, &ship_id],
    )
    .await
    .map_err(anyhow::Error::from)?;
    tx.commit().await.map_err(anyhow::Error::from)?;
    tracing::info!(
        char_id,
        name,
        station = station.name,
        skills = skill_levels.len(),
        "character created"
    );
    Ok(Value::Int(char_id))
}

/// TEMPLATE DEFAULT starting wallet.
const STARTING_BALANCE: f64 = 5000.0;

/// Skill points to reach `level` of a rank-1 skill: 250 * 2^(2.5 * (level - 1)).
fn skill_points(level: i32) -> i64 {
    if level <= 0 {
        0
    } else {
        (250.0 * 2f64.powf(2.5 * (level - 1) as f64)).ceil() as i64
    }
}

/// charsel.Terminate: a falsy return means success; a truthy one is applied to eve.Message.
async fn delete_character(server: &Server, session: &Session, args: &[Value]) -> CallResult {
    let char_id = int_arg(args, 0, "charID")? as i32;
    let c = server.pool.get().await.map_err(anyhow::Error::from)?;
    let n = c
        .execute(
            r#"UPDATE game.characters SET "deleted" = now() WHERE "characterID" = $1 AND "userID" = $2 AND "deleted" IS NULL"#,
            &[&char_id, &(session.user_id.unwrap_or(0) as i32)],
        )
        .await
        .map_err(anyhow::Error::from)?;
    if n == 0 {
        return Ok(Value::tuple([
            Value::str("CustomInfo"),
            Value::dict([(Value::str("info"), Value::str("No such character."))]),
        ]));
    }
    tracing::info!(char_id, "character deleted");
    Ok(Value::None)
}

/// Logs a character on: the session change carries the character's location
/// verbatim (the client derives nothing), and gameui.ProcessSessionChange
/// then brings up the station or space scene.
async fn select_character(server: &Server, session: &mut Session, args: &[Value]) -> CallResult {
    let char_id = int_arg(args, 0, "charID")? as i32;
    if session.attrs.int("charid").is_some() {
        // machoNet closes the transport on a charid change with a non-None old value.
        return Err(user_error(
            "CustomInfo",
            vec![("info", Value::str("A character is already selected."))],
        ));
    }
    let c = server.pool.get().await.map_err(anyhow::Error::from)?;
    let r = c
        .query_opt(
            r#"SELECT "corporationID", "stationID", "solarSystemID", "constellationID", "regionID", "shipID",
                      "characterName", "corporationRoles"
               FROM game.characters WHERE "characterID" = $1 AND "userID" = $2 AND "deleted" IS NULL"#,
            &[&char_id, &(session.user_id.unwrap_or(0) as i32)],
        )
        .await
        .map_err(anyhow::Error::from)?
        .ok_or_else(|| anyhow!("character {char_id} is not on this account"))?;
    let station: Option<i32> = r.get(1);
    let mut changes: Vec<(&'static str, Value)> = vec![
        ("charid", Value::Int(char_id)),
        ("corpid", Value::Int(r.get(0))),
        // const.corpRole* bits; the corp window checks corpRoleDirector & co. locally.
        ("corprole", Value::from(r.get::<_, i64>(7))),
        ("constellationid", Value::Int(r.get(3))),
        ("regionid", Value::Int(r.get(4))),
        (
            "shipid",
            r.get::<_, Option<i64>>(5)
                .map(Value::from)
                .unwrap_or(Value::None),
        ),
    ];
    match station {
        Some(st) => changes.extend([
            ("stationid", Value::Int(st)),
            ("solarsystemid2", Value::Int(r.get(2))),
            ("solarsystemid", Value::None),
            ("locationid", Value::Int(st)),
        ]),
        None => changes.extend([
            ("stationid", Value::None),
            ("solarsystemid2", Value::Int(r.get(2))),
            ("solarsystemid", Value::Int(r.get(2))),
            ("locationid", Value::Int(r.get(2))),
        ]),
    }
    let name: String = r.get(6);
    server
        .registry
        .update(session.client_id, |c| c.char_name = name.clone());
    session.change(server, &changes);
    tracing::info!(char_id, name, ?station, "character selected");
    Ok(Value::None)
}
