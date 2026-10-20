//! Bookmarks and the people & places address book (ui/shared/neocom/
//! bookmarks.py, addressbook.py) and bookmark vouchers (`voucher` service,
//! ui/deco/item.py).
//!
//! A bookmark row names what was bookmarked by itemID/typeID: the map screens
//! filter on the type's category (celestial, station, ship), the address book
//! on groupCharacter. A spot in space is bookmarked as the solar system with
//! x/y/z (bookmarks.py BookmarkLocationPopup takes the ship's ball position
//! when the location is the current system or ship). memo is
//! 'label\tnote' (bookmarks.Zip).

use marshal::Value;
use proto::gpcs::Call;

use super::{Args, Bound, CallResult, Session};
use crate::{db, Server};

const COLUMNS: &str = r#"b."bookmarkID", b."ownerID", b."itemID", b."typeID", b."flag", b."memo", b."created",
    b."x", b."y", b."z", b."locationID""#;

/// The rows of one owner's bookmarks. `online` is read by the address book
/// (addressbook.py GetPersonalAddressBook compares it to its filter).
async fn bookmarks(
    server: &Server,
    owner: i32,
    extra: &str,
    params: db::Params<'_>,
) -> anyhow::Result<Vec<Vec<Value>>> {
    let c = server.pool.get().await?;
    let sql = format!(
        r#"SELECT {COLUMNS} FROM game.bookmarks b WHERE b."ownerID" = $1 {extra} ORDER BY b."bookmarkID""#
    );
    let mut all: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = vec![&owner];
    all.extend_from_slice(params);
    let rs = c.query(&sql, &all).await?;
    let online: std::collections::HashSet<i32> = server
        .registry
        .select(None, |c| c.char_id.is_some())
        .into_iter()
        .filter_map(|(_, c)| c.char_id)
        .collect();
    rs.iter()
        .map(|r| {
            let mut line = db::row_values(r)?;
            let item: Option<i64> = r.get(2);
            line.push(Value::Int(
                item.map(|i| online.contains(&(i as i32))).unwrap_or(false) as i32,
            ));
            Ok(line)
        })
        .collect()
}

const HEADER: [&str; 12] = [
    "bookmarkID",
    "ownerID",
    "itemID",
    "typeID",
    "flag",
    "memo",
    "created",
    "x",
    "y",
    "z",
    "locationID",
    "online",
];

async fn bookmark_row(server: &Server, owner: i32, bookmark_id: i32) -> anyhow::Result<Value> {
    let lines = bookmarks(server, owner, r#"AND b."bookmarkID" = $2"#, &[&bookmark_id]).await?;
    Ok(lines
        .into_iter()
        .next()
        .map(|l| crate::rows::row(&HEADER, l))
        .unwrap_or(Value::None))
}

/// What a location ID is: (typeID, x, y, z, solar system), from the map,
/// stations, celestials or the live ballpark.
async fn locate(
    server: &Server,
    item_id: i64,
) -> anyhow::Result<Option<(i32, Option<[f64; 3]>, Option<i64>)>> {
    let c = server.pool.get().await?;
    // Regions, constellations, systems: bookmarks.GetSolConReg reads the
    // map item itself.
    if let Some(r) = c
        .query_opt(
            r#"SELECT "typeID" FROM static.map_objects WHERE "itemID" = $1"#,
            &[&(item_id as i32)],
        )
        .await?
    {
        return Ok(Some((r.get(0), None, Some(item_id))));
    }
    if let Some(st) = server.world.stations.get(&(item_id as i32)) {
        return Ok(Some((
            st.type_id,
            Some(st.pos),
            Some(st.solar_system_id as i64),
        )));
    }
    if let Some(r) = c
        .query_opt(
            r#"SELECT "typeID", "x", "y", "z", "solarSystemID" FROM static.celestials WHERE "itemID" = $1"#,
            &[&(item_id as i32)],
        )
        .await?
    {
        let pos = [r.get::<_, f64>(1), r.get(2), r.get(3)];
        return Ok(Some((r.get(0), Some(pos), r.get::<_, Option<i32>>(4).map(i64::from))));
    }
    if let Some(it) = super::inventory::item(server, item_id).await? {
        return Ok(Some((it.type_id, None, Some(it.location_id))));
    }
    Ok(None)
}

/// The ship's position in the current ballpark.
fn ship_position(server: &Server, session: &Session) -> Option<[f64; 3]> {
    let ss = session.attrs.int("solarsystemid")? as i32;
    let ship = session.attrs.int("shipid")?;
    server
        .space
        .with_park(ss, |p| p.entries.get(&ship).map(|e| e.ball.pos))
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
        "GetBookmarks" => Ok(crate::rows::rowset(
            &HEADER,
            bookmarks(server, me, "", &[]).await?,
        )),
        // BookmarkLocation(itemID, ownerID, memo)
        "BookmarkLocation" => {
            let item_id = a.int(0).unwrap_or(0);
            let memo = a.text(2).unwrap_or_default();
            let ss = session.attrs.int("solarsystemid");
            let here = Some(item_id) == ss || Some(item_id) == session.attrs.int("shipid");
            let (item, type_id, pos, location) = if here {
                let ss = ss.unwrap_or(0);
                (
                    ss,
                    server.world.type_id("SolarSystem"),
                    ship_position(server, session),
                    Some(ss),
                )
            } else {
                match locate(server, item_id).await? {
                    Some((t, pos, loc)) => (item_id, t, pos, loc),
                    None => return Err(super::user_error("BookmarkLocationUnknown", vec![])),
                }
            };
            let c = server.pool.get().await?;
            let id: i32 = c
                .query_one(
                    r#"INSERT INTO game.bookmarks ("ownerID", "itemID", "typeID", "memo", "x", "y", "z", "locationID")
                       VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING "bookmarkID""#,
                    &[&me, &item, &type_id, &memo, &pos.map(|p| p[0]), &pos.map(|p| p[1]), &pos.map(|p| p[2]), &location],
                )
                .await?
                .get(0);
            Ok(bookmark_row(server, me, id).await?)
        }
        "BookmarkCharacter" => {
            let char_id = a.int(0).unwrap_or(0);
            let exists: Option<i32> = db::scalar(
                &server.pool,
                r#"SELECT "bookmarkID" FROM game.bookmarks WHERE "ownerID" = $1 AND "itemID" = $2 AND "typeID" = $3"#,
                &[&me, &char_id, &server.world.type_id("Character")],
            )
            .await?;
            if exists.is_none() {
                db::exec(
                    &server.pool,
                    r#"INSERT INTO game.bookmarks ("ownerID", "itemID", "typeID") VALUES ($1, $2, $3)"#,
                    &[&me, &char_id, &server.world.type_id("Character")],
                )
                .await?;
            }
            Ok(Value::None)
        }
        "DeleteBookmark" => {
            let id = a.i32(0).unwrap_or(0);
            db::exec(
                &server.pool,
                r#"DELETE FROM game.bookmarks WHERE "bookmarkID" = $1 AND "ownerID" = $2"#,
                &[&id, &me],
            )
            .await?;
            Ok(Value::None)
        }
        // UpdateBookmark(bookmarkID, ownerID, memo)
        "UpdateBookmark" => {
            let id = a.i32(0).unwrap_or(0);
            let memo = a.text(2).unwrap_or_default();
            db::exec(
                &server.pool,
                r#"UPDATE game.bookmarks SET "memo" = $3 WHERE "bookmarkID" = $1 AND "ownerID" = $2"#,
                &[&id, &me, &memo],
            )
            .await?;
            Ok(Value::None)
        }
        // CreateBookmarkVoucher(bookmarkID, memo): a typeBookmark item in the
        // hangar (or the ship's cargo in space) that AddBookmarkFromVoucher
        // turns back into a bookmark.
        "CreateBookmarkVoucher" => {
            let id = a.i32(0).unwrap_or(0);
            let c = server.pool.get().await?;
            let Some(bm) = c
                .query_opt(
                    r#"SELECT "itemID", "typeID", "memo", "x", "y", "z", "locationID" FROM game.bookmarks
                       WHERE "bookmarkID" = $1 AND "ownerID" = $2"#,
                    &[&id, &me],
                )
                .await?
            else {
                return Ok(Value::None);
            };
            let (location, flag) = match session.attrs.int("stationid") {
                Some(st) => (st, server.world.flag("Hangar")),
                None => (
                    session.attrs.int("shipid").unwrap_or(0),
                    server.world.flag("Cargo"),
                ),
            };
            let item = super::inventory::create_item(
                server,
                server.world.type_id("Bookmark"),
                me,
                location,
                flag,
                1,
                true,
            )
            .await?;
            let memo: String = bm.get(2);
            c.execute(
                r#"INSERT INTO game.vouchers ("itemID", "description", "typeID", "bmItemID", "bmTypeID", "bmX", "bmY", "bmZ", "bmLocationID")
                   VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)"#,
                &[
                    &item.item_id,
                    &memo,
                    &server.world.type_id("Bookmark"),
                    &bm.get::<_, Option<i64>>(0),
                    &bm.get::<_, i32>(1),
                    &bm.get::<_, Option<f64>>(3),
                    &bm.get::<_, Option<f64>>(4),
                    &bm.get::<_, Option<f64>>(5),
                    &bm.get::<_, Option<i64>>(6),
                ],
            )
            .await?;
            super::inventory::notify_new(server, &item);
            Ok(item.row(server))
        }
        // AddBookmarkFromVoucher(itemID): the voucher stays (shift-drag copies).
        "AddBookmarkFromVoucher" => {
            let item_id = a.int(0).unwrap_or(0);
            let c = server.pool.get().await?;
            let Some(v) = c
                .query_opt(
                    r#"SELECT "description", "bmItemID", "bmTypeID", "bmX", "bmY", "bmZ", "bmLocationID"
                       FROM game.vouchers WHERE "itemID" = $1"#,
                    &[&item_id],
                )
                .await?
            else {
                return Ok(Value::None);
            };
            let id: i32 = c
                .query_one(
                    r#"INSERT INTO game.bookmarks ("ownerID", "itemID", "typeID", "memo", "x", "y", "z", "locationID")
                       VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING "bookmarkID""#,
                    &[
                        &me,
                        &v.get::<_, Option<i64>>(1),
                        &v.get::<_, Option<i32>>(2).unwrap_or(server.world.type_id("SolarSystem")),
                        &v.get::<_, String>(0),
                        &v.get::<_, Option<f64>>(3),
                        &v.get::<_, Option<f64>>(4),
                        &v.get::<_, Option<f64>>(5),
                        &v.get::<_, Option<i64>>(6),
                    ],
                )
                .await?
                .get(0);
            // bookmarks.py appends 'online' to the header itself.
            let row = bookmark_row(server, me, id).await?;
            Ok(strip_online(row))
        }
        m => super::unimplemented("bookmark", m, call),
    }
}

fn strip_online(row: Value) -> Value {
    if let Value::Instance { guid, state } = row {
        if let Value::Dict(mut pairs) = *state {
            for (k, v) in pairs.iter_mut() {
                if matches!(k.as_str(), Some("header") | Some("line")) {
                    if let Value::List(items) = v {
                        items.pop();
                    }
                }
            }
            return Value::Instance {
                guid,
                state: Box::new(Value::Dict(pairs)),
            };
        }
        return Value::Instance { guid, state };
    }
    row
}

/// voucher.GetObject(itemID) binds the voucher; its methods describe it.
pub async fn get_object(server: &Server, session: &mut Session, call: &Call) -> CallResult {
    let item_id = Args::of(call).int(0).unwrap_or(0);
    Ok(session.bind_value(server, "voucher", Value::from(item_id)))
}

/// voucher[itemID]: GetDescription() is the bookmark memo; GetTypeInfo() is
/// (typeID, subTypeID), compared with OrderTypeInfo.GetTypeKey by the market.
pub async fn voucher(
    server: &Server,
    _session: &mut Session,
    bound: &Bound,
    call: &Call,
) -> CallResult {
    let item_id = bound.params.as_int().unwrap_or(0);
    let c = server.pool.get().await?;
    let r = c
        .query_opt(
            r#"SELECT "description", "typeID", "subTypeID" FROM game.vouchers WHERE "itemID" = $1"#,
            &[&item_id],
        )
        .await?;
    match call.method() {
        "GetDescription" => Ok(r
            .map(|r| db::text_value(r.get(0)))
            .unwrap_or(Value::str(""))),
        "GetTypeInfo" => Ok(match r {
            Some(r) => Value::tuple([
                Value::from(r.get::<_, Option<i32>>(1)),
                Value::from(r.get::<_, Option<i32>>(2)),
            ]),
            None => Value::tuple([Value::None, Value::None]),
        }),
        m => super::unimplemented("voucher", m, call),
    }
}
