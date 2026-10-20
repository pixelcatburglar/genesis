//! The `config` service: lazy record fetchers behind cfg's Recordsets
//! (sys/cfg.py Recordset._Prime) and the unit table for show-info.
//!
//! Fetchers return `(header, rows)` with the bulk table's header. A key the
//! reply does not contain is remembered client-side in `knownLuzers` and never
//! asked again this session, and a later Get() raises RecordNotFound, so every
//! ID the server hands out must resolve here.

use marshal::Value;
use proto::gpcs::Call;

use super::{CallResult, Session};
use crate::{db, rows, Server};

pub async fn dispatch(
    server: &Server,
    session: &mut Session,
    method: &str,
    call: &Call,
) -> CallResult {
    let args = call.args();
    let ids = || -> Vec<i64> {
        match args.first() {
            Some(Value::List(v)) | Some(Value::Tuple(v)) => {
                v.iter().filter_map(Value::as_int).collect()
            }
            Some(v) => v.as_int().into_iter().collect(),
            None => Vec::new(),
        }
    };
    match method {
        "GetMultiOwnersEx" | "GetOwnersEx" => owners(server, &ids()).await,
        "GetMultiLocationsEx" | "GetLocationsEx" => locations(server, &ids()).await,
        "GetMultiInvTypesEx" | "GetInvTypesEx" => by_ids(server, "types", "typeID", &ids()).await,
        "GetMultiInvGroupsEx" | "GetInvGroupsEx" => {
            by_ids(server, "groups", "groupID", &ids()).await
        }
        "GetMultiInvCategoriesEx" | "GetInvCategoriesEx" => {
            by_ids(server, "categories", "categoryID", &ids()).await
        }
        "GetMultiGraphicsEx" | "GetGraphicsEx" => {
            by_ids(server, "graphics", "graphicID", &ids()).await
        }
        "GetMultiBlueprintTypeEx" | "GetBlueprintTypeEx" => {
            by_ids(server, "bptypes", "blueprintTypeID", &ids()).await
        }
        "GetUnits" => units(server).await,
        // GetMapObjects(id, wantRegions, wantConstellations, wantSystems, wantItems)
        "GetMapObjects" => {
            let flag = |i: usize| args.get(i).and_then(Value::as_int).unwrap_or(0) != 0;
            let id = args.first().and_then(Value::as_int).unwrap_or(0);
            if flag(4) {
                return Ok(system_items(server, id).await?);
            }
            let groups: Vec<i32> = [(1, 3), (2, 4), (3, 5)].iter().filter(|(i, _)| flag(*i)).map(|(_, g)| *g).collect();
            map_below(server, id, &groups).await
        }
        // A solar system's contents (map.py GetSolarSystem, infosvc orbit tree).
        "GetMap" | "GetSolarSystemCelestials" => {
            Ok(system_items(server, args.first().and_then(Value::as_int).unwrap_or(0)).await?)
        }
        // [(ctype, fromRegion, fromConstellation, fromSystem, stargateID,
        //   celestialID, toSystem, toConstellation, toRegion)] for jumps out of id.
        "GetMapConnections" => connections(server, args.first().and_then(Value::as_int).unwrap_or(0)).await,
        "GetMapLandmarks" => Ok(db::rowset(
            &server.pool,
            r#"SELECT "landmarkID", "landmarkName", "description", "radius", "x", "y", "z", "importance"
               FROM static.map_landmarks ORDER BY 1"#,
            &[],
        )
        .await?),
        // SetMapLandmarks([(landmarkID, x, y, z, radius)]): the map's landmark editor.
        "SetMapLandmarks" => {
            if session.role & ROLE_GML == 0 {
                return Err(super::user_error("CustomInfo", vec![("info", Value::str("Only game masters can move landmarks."))]));
            }
            for lm in args.first().and_then(Value::as_seq).unwrap_or(&[]) {
                let f = |i: usize| lm.as_seq().and_then(|t| t.get(i)).and_then(Value::as_float).unwrap_or(0.0);
                let id = lm.as_seq().and_then(|t| t.first()).and_then(Value::as_int).unwrap_or(0) as i32;
                db::exec(
                    &server.pool,
                    r#"UPDATE static.map_landmarks SET "x" = $2, "y" = $3, "z" = $4, "radius" = $5 WHERE "landmarkID" = $1"#,
                    &[&id, &f(1), &f(2), &f(3), &f(4)],
                )
                .await?;
            }
            Ok(Value::None)
        }
        "GetCelestialStatistic" => celestial_statistic(server, args.first().and_then(Value::as_int).unwrap_or(0)).await,
        // Systems where an owner has stations (infosvc "Settled systems").
        "GetStationSolarSystemsByOwner" => Ok(db::rowset(
            &server.pool,
            r#"SELECT DISTINCT "solarSystemID" FROM static.stations WHERE "corporationID" = $1
               UNION SELECT DISTINCT s."solarSystemID" FROM game.offices o JOIN static.stations s USING ("stationID")
               WHERE o."corporationID" = $1"#,
            &[&(args.first().and_then(Value::as_int).unwrap_or(0) as i32)],
        )
        .await?),
        _ => {
            tracing::warn!(method, args = ?args, "UNIMPLEMENTED config call answered None");
            Ok(Value::None)
        }
    }
}

/// service.ROLE_GML: may edit map landmarks.
const ROLE_GML: i64 = 8;

const OWNER_HEADER: [&str; 3] = ["ownerID", "ownerName", "typeID"];
const LOCATION_HEADER: [&str; 2] = ["locationID", "locationName"];

/// Static owners (factions, NPC corps), players (typeID 1, the Character type,
/// so EveOwners.IsCharacter holds), and owner 1 "EVE System", which celestial
/// slims carry and no table lists.
async fn owners(server: &Server, ids: &[i64]) -> CallResult {
    let ids32: Vec<i32> = ids.iter().map(|&i| i as i32).collect();
    let c = server.pool.get().await?;
    let mut lines = rows::lines(
        &c.query(
            r#"SELECT "ownerID", "ownerName", "typeID" FROM static.owners WHERE "ownerID" = ANY($1)
               UNION ALL
               SELECT "characterID", "characterName", 1 FROM game.characters WHERE "characterID" = ANY($1)
               UNION ALL
               SELECT "corporationID", "corporationName", $2 FROM game.corporations WHERE "corporationID" = ANY($1)"#,
            &[&ids32, &server.world.type_id("Corporation")],
        )
        .await?,
    )?;
    if ids.contains(&1) {
        let corp_type = server
            .world
            .types
            .iter()
            .find(|(_, t)| t.name == "Corporation")
            .map(|(id, _)| *id)
            .unwrap_or(1);
        lines.push(vec![
            Value::Int(1),
            Value::str("EVE System"),
            Value::Int(corp_type),
        ]);
    }
    Ok(rows::header_lines(&OWNER_HEADER, lines))
}

/// The bulk table (map + stations), then celestials, then items (named by
/// itemName or their type, e.g. a ship in a hangar).
async fn locations(server: &Server, ids: &[i64]) -> CallResult {
    let ids32: Vec<i32> = ids
        .iter()
        .filter(|&&i| i <= i32::MAX as i64)
        .map(|&i| i as i32)
        .collect();
    let c = server.pool.get().await?;
    let rs = c
        .query(
            r#"SELECT "locationID"::bigint, "locationName" FROM static.locations WHERE "locationID" = ANY($1)
               UNION ALL
               SELECT "itemID"::bigint, "itemName" FROM static.celestials
                 WHERE "itemID" = ANY($1) AND "itemID" NOT IN (SELECT "locationID" FROM static.locations)
               UNION ALL
               SELECT i."itemID", coalesce(i."itemName", t."typeName") FROM game.items i
                 JOIN static.types t USING ("typeID") WHERE i."itemID" = ANY($2)"#,
            &[&ids32, &ids],
        )
        .await?;
    let lines = rs
        .iter()
        .map(|r| vec![Value::from(r.get::<_, i64>(0)), db::text_value(r.get(1))])
        .collect();
    Ok(rows::header_lines(&LOCATION_HEADER, lines))
}

async fn by_ids(server: &Server, table: &str, key: &str, ids: &[i64]) -> CallResult {
    let ids32: Vec<i32> = ids.iter().map(|&i| i as i32).collect();
    let c = server.pool.get().await?;
    let stmt = c
        .prepare(&format!(
            r#"SELECT * FROM static.{table} WHERE "{key}" = ANY($1)"#
        ))
        .await?;
    let rs = c.query(&stmt, &[&ids32]).await?;
    let header: Vec<&str> = stmt.columns().iter().map(|c| c.name()).collect();
    Ok(rows::header_lines(&header, rows::lines(&rs)?))
}

/// infosvc reads `self.units.has_key(unitID)` and `self.units[unitID].displayName`:
/// a util.IndexRowset {items: {unitID: line}, header, idName}.
async fn units(server: &Server) -> CallResult {
    let c = server.pool.get().await?;
    let stmt = c
        .prepare(r#"SELECT * FROM static.units ORDER BY "unitID""#)
        .await?;
    let rs = c.query(&stmt, &[]).await?;
    let header: Vec<Value> = stmt
        .columns()
        .iter()
        .map(|c| Value::str(c.name()))
        .collect();
    let items = rs
        .iter()
        .map(|r| Ok((Value::Int(r.get(0)), Value::List(db::row_values(r)?))))
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(Value::instance(
        "util.IndexRowset",
        Value::dict([
            (Value::str("items"), Value::Dict(items)),
            (Value::str("header"), Value::List(header)),
            (Value::str("idName"), Value::str("unitID")),
        ]),
    ))
}

/// static.map_objects columns, the shipped map cache's row header.
const MAP_COLS: &str = r#""groupID", "typeID", "itemID", "itemName", "locationID", "orbitID", "connector", "x", "y", "z",
    "xMin", "xMax", "yMin", "yMax", "zMin", "zMax", "luminosity", "factionID", "security""#;

/// Map rows of `groups` anywhere under `id` (the universe for
/// const.locationUniverse: regions have no parent).
async fn map_below(server: &Server, id: i64, groups: &[i32]) -> CallResult {
    let root = if id == server.world.constant("locationUniverse") as i64 { None } else { Some(id as i32) };
    Ok(db::rowset(
        &server.pool,
        &format!(
            r#"WITH RECURSIVE sub AS (
                   SELECT * FROM static.map_objects WHERE "locationID" IS NOT DISTINCT FROM $1
                   UNION ALL
                   SELECT m.* FROM static.map_objects m JOIN sub ON m."locationID" = sub."itemID" WHERE sub."groupID" < 5)
               SELECT {MAP_COLS} FROM sub WHERE "groupID" = ANY($2) ORDER BY "itemID""#
        ),
        &[&root, &groups],
    )
    .await?)
}

/// A solar system's celestials (sun, planets, moons, belts, gates, stations)
/// as map rows.
async fn system_items(server: &Server, system: i64) -> anyhow::Result<Value> {
    db::rowset(
        &server.pool,
        r#"SELECT "groupID", "typeID", "itemID", "itemName", "solarSystemID" AS "locationID", "orbitID", 0 AS "connector",
                  "x", "y", "z", NULL::float8 AS "xMin", NULL::float8 AS "xMax", NULL::float8 AS "yMin", NULL::float8 AS "yMax",
                  NULL::float8 AS "zMin", NULL::float8 AS "zMax", NULL::float8 AS "luminosity", NULL::integer AS "factionID",
                  NULL::float8 AS "security", "radius"
           FROM static.celestials WHERE "solarSystemID" = $1 ORDER BY "itemID""#,
        &[&(system as i32)],
    )
    .await
}

async fn connections(server: &Server, id: i64) -> CallResult {
    let root = if id == server.world.constant("locationUniverse") as i64 { None } else { Some(id as i32) };
    let c = server.pool.get().await?;
    let rs = c
        .query(
            r#"WITH sys AS (
                   SELECT s."itemID" AS sol, s."locationID" AS con, k."locationID" AS reg
                   FROM static.map_objects s JOIN static.map_objects k ON k."itemID" = s."locationID" WHERE s."groupID" = 5),
               gates AS (
                   SELECT c."solarSystemID" AS src, dc."solarSystemID" AS dst, min(c."itemID") AS gate, min(sg."destinationID") AS dest
                   FROM static.stargates sg JOIN static.celestials c ON c."itemID" = sg."stargateID"
                   JOIN static.celestials dc ON dc."itemID" = sg."destinationID" GROUP BY 1, 2)
               SELECT CASE WHEN f.reg <> t.reg THEN 2 WHEN f.con <> t.con THEN 1 ELSE 0 END,
                      f.reg, f.con, f.sol, g.gate, g.dest, t.sol, t.con, t.reg
               FROM static.map_jumps j
               JOIN sys f ON f.sol = j."fromSolarSystemID" JOIN sys t ON t.sol = j."toSolarSystemID"
               LEFT JOIN gates g ON g.src = f.sol AND g.dst = t.sol
               WHERE $1::integer IS NULL OR f.sol = $1 OR f.con = $1 OR f.reg = $1"#,
            &[&root],
        )
        .await?;
    Ok(Value::List(
        rs.iter()
            .map(|r| {
                Value::Tuple(
                    (0..9)
                        .map(|i| match r.try_get::<_, Option<i32>>(i) {
                            Ok(Some(v)) => Value::Int(v),
                            _ => Value::None,
                        })
                        .collect(),
                )
            })
            .collect(),
    ))
}

/// TEMPLATE DEFAULT physical facts for a sun (infosvc lists every column),
/// derived from the system's shipped luminosity and the sun's radius.
async fn celestial_statistic(server: &Server, item_id: i64) -> CallResult {
    Ok(db::rowset(
        &server.pool,
        r#"SELECT CASE WHEN coalesce(m."luminosity", 1) > 1.5 THEN 'B0' WHEN coalesce(m."luminosity", 1) > 0.8 THEN 'G2'
                       ELSE 'K5' END AS "spectralClass",
                  round((5800 * power(coalesce(m."luminosity", 1), 0.25))::numeric, 0)::float8 AS "temperature",
                  coalesce(m."luminosity", 1) AS "luminosity",
                  c."radius" AS "radius",
                  round((4.6e9 / greatest(coalesce(m."luminosity", 1), 0.1))::numeric, -6)::float8 AS "age"
           FROM static.celestials c JOIN static.map_objects m ON m."itemID" = c."solarSystemID"
           WHERE c."itemID" = $1 AND c."groupID" = (SELECT "groupID" FROM static.groups WHERE "groupName" = 'Sun')"#,
        &[&(item_id as i32)],
    )
    .await?)
}
