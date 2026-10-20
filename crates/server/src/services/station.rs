//! In-station services: `station` (item bits, location search, ship names, the
//! paintshop library), `charmgr` (public character info, bounties, stations),
//! `shipSvc`, `lookupSvc`, `billMgr`, `slash`, and machoNet's cluster
//! statistics for the map.

use marshal::Value;
use proto::gpcs::Call;

use super::{user_error, Args, CallResult, Session};
use crate::world::Station;
use crate::{db, rows, Server};

fn current_station<'a>(server: &'a Server, session: &Session) -> Option<&'a Station> {
    let id = session.attrs.int("stationid")? as i32;
    server.world.stations.get(&id)
}

/// eve.SetStationItemBits: util.Row(['hangarGraphicID', 'ownerID', 'itemID', 'serviceMask'], bits).
pub fn station_item_bits(st: &Station) -> Value {
    Value::List(vec![
        Value::Int(st.hangar_graphic_id),
        Value::Int(st.corporation_id),
        Value::Int(st.station_id),
        Value::Int(st.service_mask),
    ])
}

/// corp.GetPublicStationInfo: reads corporationID and raceID.
pub fn public_station_info(server: &Server, st: &Station) -> Value {
    rows::row(
        &[
            "stationID",
            "stationName",
            "stationTypeID",
            "corporationID",
            "solarSystemID",
            "constellationID",
            "regionID",
            "raceID",
        ],
        vec![
            Value::Int(st.station_id),
            db::text_value(&st.name),
            Value::Int(st.type_id),
            Value::Int(st.corporation_id),
            Value::Int(st.solar_system_id),
            Value::Int(st.constellation_id),
            Value::Int(st.region_id),
            Value::Int(
                server
                    .world
                    .station_race
                    .get(&st.type_id)
                    .copied()
                    .unwrap_or(1),
            ),
        ],
    )
}

const PUBLIC_HEADER: [&str; 15] = [
    "characterID",
    "characterName",
    "charName",
    "gender",
    "bloodlineID",
    "bloodlineName",
    "raceID",
    "raceName",
    "corporationID",
    "ancestryID",
    "securityStatus",
    "bounty",
    "description",
    "createDateTime",
    "typeID",
];

/// charmgr.GetPublicInfo(charID): the character sheet, show-info and the
/// member window read characterName/charName, gender, raceID/raceName,
/// bloodlineID/bloodlineName; GetPublicInfo2 adds the bounty.
async fn char_public_info(server: &Server, char_id: i32) -> anyhow::Result<Value> {
    let c = server.pool.get().await?;
    let r = c
        .query_opt(
            r#"SELECT ch."characterID", ch."characterName", ch."characterName", ch."gender", ch."bloodlineID", b."bloodlineName",
                      ch."raceID", r."raceName", ch."corporationID", ch."ancestryID", ch."securityStatus", ch."bounty", '',
                      ch."created", 1
               FROM game.characters ch
               LEFT JOIN static.bloodlines b USING ("bloodlineID") LEFT JOIN static.races r ON r."raceID" = ch."raceID"
               WHERE ch."characterID" = $1
               UNION ALL
               -- NPC agents (and the brokers they double as).
               SELECT a."agentID", a."agentName", a."agentName", 1, a."bloodlineID", b."bloodlineName", a."raceID", r."raceName",
                      a."corporationID", NULL::integer, 0.0::float8, 0.0::float8,
                      'An agent of ' || coalesce(n."corporationName", 'a corporation') || '.', now(), 1
               FROM static.agents a
               LEFT JOIN static.bloodlines b USING ("bloodlineID") LEFT JOIN static.races r ON r."raceID" = a."raceID"
               LEFT JOIN static.npc_corporations n ON n."corporationID" = a."corporationID"
               WHERE a."agentID" = $1"#,
            &[&char_id],
        )
        .await?;
    Ok(match r {
        Some(r) => rows::row(&PUBLIC_HEADER, db::row_values(&r)?),
        None => Value::None,
    })
}

/// station.FindLocation(name, flag, groupID) / lookupSvc.LookupStations(name):
/// stations by name prefix (medical.py reads locationID, locationName,
/// parentLocationID; uix.SearchStation reads stationID, stationName).
async fn find_stations(server: &Server, name: &str, exact: bool) -> anyhow::Result<Value> {
    let needle = name.to_lowercase();
    let mut lines: Vec<Vec<Value>> = server
        .world
        .stations
        .values()
        .filter(|s| {
            let n = s.name.to_lowercase();
            if exact {
                n == needle
            } else {
                n.starts_with(&needle) || n.contains(&needle)
            }
        })
        .map(|s| {
            vec![
                Value::Int(s.station_id),
                db::text_value(&s.name),
                Value::Int(s.solar_system_id),
                Value::Int(s.station_id),
                db::text_value(&s.name),
                Value::Int(s.type_id),
            ]
        })
        .collect();
    lines.sort_by(|a, b| a[1].as_text().cmp(&b[1].as_text()));
    lines.truncate(100);
    Ok(rows::rowset(
        &[
            "locationID",
            "locationName",
            "parentLocationID",
            "stationID",
            "stationName",
            "typeID",
        ],
        lines,
    ))
}

const APPEARANCE_COLUMNS: [&str; 12] = [
    "layer1Shape",
    "layer1Color",
    "layer2Shape",
    "layer2Color",
    "layer3Shape",
    "layer3Color",
    "shipColor1",
    "shipColor2",
    "tag",
    "tagColor",
    "tagFont",
    "shipID",
];

pub async fn dispatch(
    server: &Server,
    session: &mut Session,
    service: &str,
    method: &str,
    call: &Call,
) -> CallResult {
    let a = Args::of(call);
    let me = session.char_id().unwrap_or(0);
    match (service, method) {
        ("station", "GetStationItemBits") => Ok(current_station(server, session)
            .map(station_item_bits)
            .unwrap_or(Value::None)),
        ("station", "FindLocation") => {
            Ok(find_stations(server, &a.text(0).unwrap_or_default(), a.int(1) == Some(1)).await?)
        }
        ("lookupSvc", "LookupStations") => {
            Ok(find_stations(server, &a.text(0).unwrap_or_default(), false).await?)
        }
        // SetShipName(shipID, name): the location name of the ship item.
        ("station", "SetShipName") => {
            let name: String = a.text(1).unwrap_or_default().chars().take(20).collect();
            db::exec(
                &server.pool,
                r#"UPDATE game.items SET "itemName" = $3 WHERE "itemID" = $1 AND "ownerID" = $2"#,
                &[&a.int(0).unwrap_or(0), &me, &name],
            )
            .await?;
            Ok(Value::None)
        }
        // [(graphicID, path)] / [(graphicID, path, label)]
        ("station", "Paintshop_GetLayerShapes") | ("station", "Paintshop_GetLayerColors") => {
            let kind = if method.ends_with("Shapes") {
                "shape"
            } else {
                "color"
            };
            let c = server.pool.get().await?;
            let rs = c
                .query(r#"SELECT "graphicID", "path", "label" FROM static.logo_library WHERE "kind" = $1 ORDER BY "graphicID""#, &[&kind])
                .await?;
            Ok(Value::List(
                rs.iter()
                    .map(|r| {
                        let mut t = vec![Value::Int(r.get(0)), db::text_value(r.get(1))];
                        if kind == "color" {
                            t.push(db::text_value(
                                &r.get::<_, Option<String>>(2).unwrap_or_default(),
                            ));
                        }
                        Value::Tuple(t)
                    })
                    .collect(),
            ))
        }
        // Paintshop_SetShipAppearance(shipID, layer1Shape, layer1Color, ..., tag, tagColor, tagFont)
        ("station", "Paintshop_SetShipAppearance") => {
            let ship = a.int(0).unwrap_or(0);
            let owner: Option<i32> = db::scalar(
                &server.pool,
                r#"SELECT "ownerID" FROM game.items WHERE "itemID" = $1"#,
                &[&ship],
            )
            .await?;
            if owner != Some(me) {
                return Err(user_error(
                    "CustomInfo",
                    vec![("info", Value::str("That is not your ship."))],
                ));
            }
            let ints: Vec<Option<i32>> = (1..9).map(|i| a.i32(i)).collect();
            db::exec(
                &server.pool,
                r#"INSERT INTO game.ship_appearance VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
                   ON CONFLICT ("shipID") DO UPDATE SET "layer1Shape" = $2, "layer1Color" = $3, "layer2Shape" = $4,
                       "layer2Color" = $5, "layer3Shape" = $6, "layer3Color" = $7, "shipColor1" = $8, "shipColor2" = $9,
                       "tag" = $10, "tagColor" = $11, "tagFont" = $12"#,
                &[&ship, &ints[0], &ints[1], &ints[2], &ints[3], &ints[4], &ints[5], &ints[6], &ints[7], &a.text(9), &a.i32(10), &a.i32(11)],
            )
            .await?;
            Ok(Value::None)
        }
        // A Rowset-like {header, lines: [line]} (base_paintshop reads header and
        // lines[0]; ship.py LoadAppearance passes appearance[0] to
        // util.SetShipAppearance), or None for an unpainted ship.
        ("shipSvc", "GetShipAppearance") => {
            let ship = a.int(0).unwrap_or(0);
            let c = server.pool.get().await?;
            let r = c
                .query_opt(
                    r#"SELECT "layer1Shape", "layer1Color", "layer2Shape", "layer2Color", "layer3Shape", "layer3Color",
                              "shipColor1", "shipColor2", "tag", "tagColor", "tagFont", "shipID"
                       FROM game.ship_appearance WHERE "shipID" = $1"#,
                    &[&ship],
                )
                .await?;
            Ok(match r {
                Some(r) => rows::rowset(&APPEARANCE_COLUMNS, vec![db::row_values(&r)?]),
                None => Value::None,
            })
        }
        ("charmgr", "GetPublicInfo") => Ok(char_public_info(server, a.i32(0).unwrap_or(0)).await?),
        ("charmgr", "GetPublicInfo2") => {
            let mut out = Vec::new();
            for id in a.ints(0) {
                out.push(char_public_info(server, id as i32).await?);
            }
            Ok(Value::List(out))
        }
        // AddToBounty(charID, amount): paid to CONCORD's pool on the target.
        ("charmgr", "AddToBounty") => {
            let target = a.i32(0).unwrap_or(0);
            let amount = a.float(1).unwrap_or(0.0);
            if target == me || amount <= 0.0 {
                return Ok(Value::None);
            }
            super::wallet::transfer(
                server,
                me,
                server.world.constant("ownerCONCORD"),
                amount,
                "refBounty",
                Some(target as i64),
                "",
            )
            .await?;
            db::exec(
                &server.pool,
                r#"UPDATE game.characters SET "bounty" = "bounty" + $2 WHERE "characterID" = $1"#,
                &[&target, &amount],
            )
            .await?;
            Ok(Value::None)
        }
        // base_missions BrowseBounties: characterID, ownerName, bounty, online.
        ("charmgr", "GetTopBounties") => {
            let c = server.pool.get().await?;
            let rs = c
                .query(
                    r#"SELECT "characterID", "characterName", "bounty" FROM game.characters
                       WHERE "bounty" > 0 AND "deleted" IS NULL ORDER BY "bounty" DESC LIMIT 50"#,
                    &[],
                )
                .await?;
            let lines = rs
                .iter()
                .map(|r| {
                    let id: i32 = r.get(0);
                    vec![
                        Value::Int(id),
                        db::text_value(r.get(1)),
                        Value::Float(r.get(2)),
                        Value::from(super::is_online(server, id)),
                    ]
                })
                .collect();
            Ok(rows::rowset(
                &["characterID", "ownerName", "bounty", "online"],
                lines,
            ))
        }
        // medical.py station picker: itemID (station), locationID (system) of
        // every station holding my things, and this one.
        ("charmgr", "GetStationList") => {
            let c = server.pool.get().await?;
            let mut ids: Vec<i32> = c
                .query(r#"SELECT DISTINCT "locationID"::int FROM game.items WHERE "ownerID" = $1 AND "locationID" BETWEEN 60000000 AND 69999999"#, &[&me])
                .await?
                .iter()
                .map(|r| r.get(0))
                .collect();
            if let Some(st) = session.attrs.int("stationid") {
                if !ids.contains(&(st as i32)) {
                    ids.push(st as i32);
                }
            }
            let lines = ids
                .into_iter()
                .filter_map(|id| server.world.stations.get(&id))
                .map(|s| {
                    vec![
                        Value::Int(s.station_id),
                        Value::Int(s.solar_system_id),
                        Value::Int(s.type_id),
                    ]
                })
                .collect();
            Ok(rows::rowset(&["itemID", "locationID", "typeID"], lines))
        }
        ("billMgr", "GetBillTypes") => {
            let c = server.pool.get().await?;
            let rs = c
                .query(
                    r#"SELECT "billTypeID", "billTypeName" FROM static.billtypes ORDER BY 1"#,
                    &[],
                )
                .await?;
            Ok(rows::rowset(
                &["billTypeID", "billTypeName"],
                rows::lines(&rs)?,
            ))
        }
        // Payable: bills owed by me / my corporation; receivable: owed to us.
        ("billMgr", "CharGetBills")
        | ("billMgr", "GetCorporationBills")
        | ("billMgr", "CharGetBillsReceivable")
        | ("billMgr", "GetCorporationBillsReceivable") => {
            let owner = if method.contains("Corporation") {
                session.attrs.int("corpid").unwrap_or(0) as i32
            } else {
                me
            };
            let side = if method.ends_with("Receivable") {
                "creditorID"
            } else {
                "debtorID"
            };
            Ok(db::rowset(
                &server.pool,
                &format!(
                    r#"SELECT "billID", "billTypeID", "debtorID", "creditorID", "amount", "dueDateTime", "interest", "externalID"
                       FROM game.bills WHERE "{side}" = $1 AND "paid" = 0 ORDER BY "dueDateTime""#
                ),
                &[&owner],
            )
            .await?)
        }
        ("billMgr", "CharPayBill") | ("billMgr", "PayCorporationBill") => {
            let payer = if method == "PayCorporationBill" {
                session.attrs.int("corpid").unwrap_or(0) as i32
            } else {
                me
            };
            let bill = a.i32(0).unwrap_or(0);
            let c = server.pool.get().await?;
            let Some(r) = c
                .query_opt(
                    r#"SELECT "creditorID", "amount" + "interest" FROM game.bills WHERE "billID" = $1 AND "debtorID" = $2 AND "paid" = 0"#,
                    &[&bill, &payer],
                )
                .await?
            else {
                return Ok(Value::None);
            };
            drop(c);
            super::wallet::transfer(
                server,
                payer,
                r.get(0),
                r.get(1),
                "refCorporationPayment",
                Some(bill as i64),
                "Bill",
            )
            .await?;
            db::exec(
                &server.pool,
                r#"UPDATE game.bills SET "paid" = 1 WHERE "billID" = $1"#,
                &[&bill],
            )
            .await?;
            Ok(Value::None)
        }
        ("slash", "SlashCmd") => {
            super::slash::command(server, session, &a.text(0).unwrap_or_default()).await
        }
        // map.py STARMODE_PLAYERCOUNT: {sessionAttribute: {value: count}}.
        ("machoNet", "GetClusterSessionStatistics") => {
            let mut per: std::collections::BTreeMap<
                &'static str,
                std::collections::BTreeMap<i64, i32>,
            > = Default::default();
            for (_, c) in server.registry.select(None, |c| c.char_id.is_some()) {
                if let Some(ss) = c.solar_system_id {
                    *per.entry("solarsystemid")
                        .or_default()
                        .entry(ss)
                        .or_default() += 1;
                }
                if let Some(loc) = c.location_id {
                    let key = if (60_000_000..70_000_000).contains(&loc) {
                        "stationid"
                    } else {
                        "locationid"
                    };
                    *per.entry(key).or_default().entry(loc).or_default() += 1;
                }
            }
            Ok(Value::Dict(
                per.into_iter()
                    .map(|(k, m)| {
                        (
                            Value::str(k),
                            Value::Dict(
                                m.into_iter()
                                    .map(|(v, n)| (Value::from(v), Value::Int(n)))
                                    .collect(),
                            ),
                        )
                    })
                    .collect(),
            ))
        }
        _ => super::unimplemented(service, method, call),
    }
}
