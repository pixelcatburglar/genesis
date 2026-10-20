//! The regional market (ui/station/market/*, marketquote.py): the group tree,
//! quotes, order books, placing/cancelling orders and price history.
//!
//! `marketBroker.GetMarketRegion()` / `market.GetMarketRegion(locationID)`
//! bind `market[regionID]`; quotes and orders come from it. Every station
//! also keeps standing NPC orders (TEMPLATE DEFAULT): it sells every listed
//! type except ore at its base price, and buys ore and minerals at 90% of
//! it. Players' orders live in game.market_orders: a sell order holds its
//! goods, a buy order its money in escrow.
//!
//! A type is an OrderTypeInfo on the wire, pickled by __getstate__: the
//! typeID, or (typeID, subTypeID), or (typeID, productivity, mineral, copy);
//! a typeKey is (typeID, subTypeID).

use std::collections::BTreeMap;

use marshal::Value;
use proto::gpcs::Call;

use super::{inventory, user_error, Args, Bound, CallResult, Session};
use crate::world::Station;
use crate::{db, rows, Server};

const NPC_SELL_STOCK: i64 = 1000;
const NPC_BUY_STOCK: i64 = 100_000;
const NPC_BUY_RATE: f64 = 0.9;

/// typeID (and subTypeID) of an OrderTypeInfo, a typeKey tuple or a plain int.
fn type_key(v: Option<&Value>) -> (i32, Option<i32>) {
    let v = match v {
        Some(Value::Instance { state, .. }) => state.as_ref(),
        Some(v) => v,
        None => return (0, None),
    };
    match v {
        Value::Tuple(t) | Value::List(t) => (
            t.first().and_then(Value::as_int).unwrap_or(0) as i32,
            if t.len() == 2 {
                t.get(1).and_then(Value::as_int).map(|s| s as i32)
            } else {
                None
            },
        ),
        other => (other.as_int().unwrap_or(0) as i32, None),
    }
}

fn key_value(type_id: i32, sub: Option<i32>) -> Value {
    Value::tuple([Value::Int(type_id), Value::from(sub)])
}

/// Where the caller is, for range filters: (station, system, constellation, region).
fn here(server: &Server, session: &Session) -> (Option<i32>, i32, i32, i32) {
    let a = |k: &str| session.attrs.int(k).map(|v| v as i32);
    let station = a("stationid");
    let system = a("solarsystemid2").or(a("solarsystemid")).unwrap_or(0);
    let s = server.world.systems.get(&system);
    (
        station,
        system,
        s.map(|s| s.constellation_id).unwrap_or(0),
        s.map(|s| s.region_id).unwrap_or(0),
    )
}

/// Does `range` around the caller include this station?
fn in_range(
    server: &Server,
    range: i32,
    origin: (Option<i32>, i32, i32, i32),
    st: &Station,
) -> bool {
    let w = &server.world;
    if range == w.constant("rangeStation") {
        origin.0 == Some(st.station_id)
    } else if range == w.constant("rangeSolarSystem") {
        origin.1 == st.solar_system_id
    } else if range == w.constant("rangeConstellation") {
        origin.2 == st.constellation_id
    } else {
        origin.3 == st.region_id
    }
}

#[derive(Debug, Clone)]
struct Order {
    order_id: i32,
    type_id: i32,
    owner: i32,
    char_id: i32,
    bid: bool,
    price: f64,
    vol_entered: i64,
    remaining: i64,
    min_volume: i64,
    range: i32,
    station: i32,
    issued: std::time::SystemTime,
    expires: std::time::SystemTime,
    escrow: f64,
    npc: bool,
}

const ORDER_HEADER: [&str; 15] = [
    "orderID",
    "typeID",
    "ownerID",
    "charID",
    "bid",
    "price",
    "volEntered",
    "volRemaining",
    "volume",
    "minVolume",
    "range",
    "stationID",
    "locationID",
    "issued",
    "expires",
];

impl Order {
    fn row(&self, server: &Server) -> Value {
        let system = server
            .world
            .stations
            .get(&self.station)
            .map(|s| s.solar_system_id)
            .unwrap_or(0);
        rows::row(
            &ORDER_HEADER,
            vec![
                Value::Int(self.order_id),
                Value::Int(self.type_id),
                Value::Int(self.owner),
                Value::Int(self.char_id),
                Value::Int(self.bid as i32),
                Value::Float(self.price),
                Value::from(self.vol_entered),
                Value::from(self.remaining),
                Value::from(self.remaining),
                Value::from(self.min_volume),
                Value::Int(self.range),
                Value::Int(self.station),
                Value::Int(system),
                Value::Long(db::blue_time(self.issued)),
                Value::Long(db::blue_time(self.expires)),
            ],
        )
    }
}

async fn base_price(server: &Server, type_id: i32) -> anyhow::Result<Option<f64>> {
    db::scalar(
        &server.pool,
        r#"SELECT "basePrice" FROM static.type_prices WHERE "typeID" = $1"#,
        &[&type_id],
    )
    .await
}

fn is_ore_or_mineral(server: &Server, type_id: i32) -> bool {
    let g = server.world.type_info(type_id).map(|t| t.group_id);
    g.is_some() && (g == server.world.group_id("Asteroid") || g == server.world.group_id("Mineral"))
}

/// The standing NPC orders of a station for a type.
fn npc_orders(server: &Server, st: &Station, type_id: i32, price: Option<f64>) -> Vec<Order> {
    let Some(price) = price else {
        return Vec::new();
    };
    let now = std::time::SystemTime::now();
    let year = std::time::Duration::from_secs(365 * 86400);
    let mut out = Vec::new();
    let resource = is_ore_or_mineral(server, type_id);
    let ore =
        server.world.type_info(type_id).map(|t| t.group_id) == server.world.group_id("Asteroid");
    let base = Order {
        order_id: 0,
        type_id,
        owner: st.corporation_id,
        char_id: st.corporation_id,
        bid: false,
        price,
        vol_entered: NPC_SELL_STOCK,
        remaining: NPC_SELL_STOCK,
        min_volume: 1,
        range: server.world.constant("rangeStation"),
        station: st.station_id,
        issued: now,
        expires: now + year,
        escrow: 0.0,
        npc: true,
    };
    if !ore {
        out.push(base.clone());
    }
    if resource {
        out.push(Order {
            bid: true,
            price: (price * NPC_BUY_RATE).floor().max(1.0),
            vol_entered: NPC_BUY_STOCK,
            remaining: NPC_BUY_STOCK,
            ..base
        });
    }
    out
}

fn order_from(r: &tokio_postgres::Row) -> Order {
    Order {
        order_id: r.get("orderID"),
        type_id: r.get("typeID"),
        owner: r.get("ownerID"),
        char_id: r.get("charID"),
        bid: r.get::<_, i32>("bid") != 0,
        price: r.get("price"),
        vol_entered: r.get::<_, i32>("volEntered") as i64,
        remaining: r.get::<_, i32>("volRemaining") as i64,
        min_volume: r.get::<_, i32>("minVolume") as i64,
        range: r.get("range"),
        station: r.get("stationID"),
        issued: r.get("issued"),
        expires: r.get("expires"),
        escrow: r.get("escrow"),
        npc: false,
    }
}

/// Every live order of a type in a region (players' and the stations').
async fn orders_of(server: &Server, region: i32, type_id: i32) -> anyhow::Result<Vec<Order>> {
    let c = server.pool.get().await?;
    let rs = c
        .query(
            r#"SELECT * FROM game.market_orders WHERE "typeID" = $1 AND "regionID" = $2 AND "volRemaining" > 0 AND "expires" > now()"#,
            &[&type_id, &region],
        )
        .await?;
    drop(c);
    let mut out: Vec<Order> = rs.iter().map(order_from).collect();
    let price = base_price(server, type_id).await?;
    for st in server
        .world
        .stations
        .values()
        .filter(|s| s.region_id == region)
    {
        out.extend(npc_orders(server, st, type_id, price));
    }
    Ok(out)
}

/// The orders a caller in `origin` sees with `range`.
async fn visible(
    server: &Server,
    origin: (Option<i32>, i32, i32, i32),
    range: i32,
    type_id: i32,
) -> anyhow::Result<Vec<Order>> {
    Ok(orders_of(server, origin.3, type_id)
        .await?
        .into_iter()
        .filter(|o| {
            server
                .world
                .stations
                .get(&o.station)
                .is_some_and(|st| in_range(server, range, origin, st))
        })
        .collect())
}

const QUOTE_HEADER: [&str; 18] = [
    "typeKey",
    "locationID",
    "bid",
    "ask",
    "delta",
    "last",
    "high",
    "low",
    "volume",
    "open",
    "close",
    "time",
    "openTime",
    "closeTime",
    "bidVolume",
    "bidLocationID",
    "askVolume",
    "askLocationID",
];

/// marketQuote.GetNullQuote's row with the best bid/ask filled in.
async fn quote(
    server: &Server,
    origin: (Option<i32>, i32, i32, i32),
    range: i32,
    type_id: i32,
    sub: Option<i32>,
) -> anyhow::Result<Value> {
    let orders = visible(server, origin, range, type_id).await?;
    let best_bid = orders
        .iter()
        .filter(|o| o.bid)
        .max_by(|a, b| a.price.total_cmp(&b.price));
    let best_ask = orders
        .iter()
        .filter(|o| !o.bid)
        .min_by(|a, b| a.price.total_cmp(&b.price));
    let c = server.pool.get().await?;
    let last = c
        .query_opt(
            r#"SELECT "closePrice", "highPrice", "lowPrice", "volume" FROM game.market_history WHERE "typeID" = $1 AND "regionID" = $2
               ORDER BY "day" DESC LIMIT 1"#,
            &[&type_id, &origin.3],
        )
        .await?;
    let opt = |v: Option<f64>| v.map(Value::Float).unwrap_or(Value::None);
    let sum = |bid: bool| {
        orders
            .iter()
            .filter(|o| o.bid == bid)
            .map(|o| o.remaining)
            .sum::<i64>()
    };
    Ok(rows::row(
        &QUOTE_HEADER,
        vec![
            key_value(type_id, sub),
            Value::from(origin.0.map(i64::from).unwrap_or(origin.1 as i64)),
            Value::Float(best_bid.map(|o| o.price).unwrap_or(0.0)),
            Value::Float(best_ask.map(|o| o.price).unwrap_or(0.0)),
            Value::Float(0.0),
            opt(last.as_ref().map(|r| r.get(0))),
            opt(last.as_ref().map(|r| r.get(1))),
            opt(last.as_ref().map(|r| r.get(2))),
            Value::from(last.as_ref().map(|r| r.get::<_, i64>(3)).unwrap_or(0)),
            Value::None,
            opt(last.as_ref().map(|r| r.get(0))),
            Value::Long(crate::statics::blue_time_now()),
            Value::None,
            Value::None,
            Value::from(sum(true)),
            Value::from(best_bid.map(|o| o.station)),
            Value::from(sum(false)),
            Value::from(best_ask.map(|o| o.station)),
        ],
    ))
}

/// Types listed in a market group (static types.marketGroupID).
async fn group_types(server: &Server, group: i32) -> anyhow::Result<Vec<i32>> {
    let c = server.pool.get().await?;
    Ok(c.query(
        r#"SELECT "typeID" FROM static.types WHERE "marketGroupID" = $1 ORDER BY "typeID""#,
        &[&group],
    )
    .await?
    .iter()
    .map(|r| r.get(0))
    .collect())
}

/// A group (and its subgroups) summary: how many types have asks and bids.
async fn group_quote(
    server: &Server,
    origin: (Option<i32>, i32, i32, i32),
    range: i32,
    group: i32,
) -> anyhow::Result<Value> {
    let c = server.pool.get().await?;
    let ids: Vec<i32> = c
        .query(
            r#"WITH RECURSIVE g AS (SELECT "marketGroupID" FROM static.market_groups WHERE "marketGroupID" = $1
                 UNION ALL SELECT m."marketGroupID" FROM static.market_groups m JOIN g ON m."parentGroupID" = g."marketGroupID")
               SELECT t."typeID" FROM static.types t JOIN g ON t."marketGroupID" = g."marketGroupID""#,
            &[&group],
        )
        .await?
        .iter()
        .map(|r| r.get(0))
        .collect();
    drop(c);
    let (mut asks, mut bids) = (0, 0);
    for t in ids {
        let orders = visible(server, origin, range, t).await?;
        asks += orders.iter().any(|o| !o.bid) as i32;
        bids += orders.iter().any(|o| o.bid) as i32;
    }
    Ok(rows::keyval(vec![
        ("marketGroupID", Value::Int(group)),
        ("askTypes", Value::Int(asks)),
        ("bidTypes", Value::Int(bids)),
    ]))
}

/// market.GetMarketGroups: {parentGroupID: [group rows]} with every group's
/// own key present (base_market indexes marketgroups[groupID] for children)
/// and `types` listing the group's typeIDs.
async fn market_groups(server: &Server) -> anyhow::Result<Value> {
    let c = server.pool.get().await?;
    let rs = c
        .query(
            r#"SELECT g."marketGroupID", g."parentGroupID", g."marketGroupName", g."description", g."graphicID", g."hasTypes",
                      coalesce(array_agg(t."typeID" ORDER BY t."typeID") FILTER (WHERE t."typeID" IS NOT NULL), '{}')
               FROM static.market_groups g LEFT JOIN static.types t USING ("marketGroupID")
               GROUP BY g."marketGroupID" ORDER BY g."marketGroupName""#,
            &[],
        )
        .await?;
    let header = [
        "marketGroupID",
        "parentGroupID",
        "marketGroupName",
        "description",
        "graphicID",
        "hasTypes",
        "types",
    ];
    let mut children: BTreeMap<Option<i32>, Vec<Value>> = BTreeMap::new();
    for r in &rs {
        let id: i32 = r.get(0);
        children.entry(Some(id)).or_default();
        let types: Vec<i32> = r.get(6);
        let line = vec![
            Value::Int(id),
            Value::from(r.get::<_, Option<i32>>(1)),
            db::text_value(r.get(2)),
            db::text_value(r.get(3)),
            Value::from(r.get::<_, Option<i32>>(4)),
            Value::Int(r.get(5)),
            Value::List(types.into_iter().map(Value::Int).collect()),
        ];
        children
            .entry(r.get::<_, Option<i32>>(1))
            .or_default()
            .push(rows::row(&header, line));
    }
    Ok(Value::Dict(
        children
            .into_iter()
            .map(|(k, v)| (Value::from(k), Value::List(v)))
            .collect(),
    ))
}

fn duration_days(server: &Server, timeframe: i32) -> u64 {
    let w = &server.world;
    match timeframe {
        t if t == w.constant("timeframeDay") => 1,
        t if t == w.constant("timeframeDay5") => 5,
        t if t == w.constant("timeframeMonth") => 30,
        _ => 365,
    }
}

async fn record_trade(
    server: &Server,
    type_id: i32,
    region: i32,
    price: f64,
    qty: i64,
) -> anyhow::Result<()> {
    db::exec(
        &server.pool,
        r#"INSERT INTO game.market_history VALUES ($1, $2, current_date, $3, $3, $3, $4)
           ON CONFLICT ("typeID", "regionID", "day") DO UPDATE SET
               "lowPrice" = least(game.market_history."lowPrice", $3), "highPrice" = greatest(game.market_history."highPrice", $3),
               "closePrice" = $3, "volume" = game.market_history."volume" + $4"#,
        &[&type_id, &region, &price, &qty],
    )
    .await?;
    Ok(())
}

async fn order_changed(server: &Server, order_id: i32) -> anyhow::Result<()> {
    let c = server.pool.get().await?;
    if let Some(r) = c
        .query_opt(
            r#"SELECT * FROM game.market_orders WHERE "orderID" = $1"#,
            &[&order_id],
        )
        .await?
    {
        let o = order_from(&r);
        super::notify_chars(
            server,
            &[o.char_id],
            "OnOwnOrderChanged",
            vec![o.row(server), Value::str("Modify")],
        );
    }
    Ok(())
}

/// Delivers goods into an owner's hangar at a station.
async fn deliver(
    server: &Server,
    owner: i32,
    station: i32,
    type_id: i32,
    qty: i64,
) -> anyhow::Result<()> {
    if owner < 100_000_000 || qty <= 0 {
        return Ok(());
    }
    let w = &server.world;
    let ship = w.type_info(type_id).map(|t| t.category_id) == Some(w.category_id("Ship"));
    let item = inventory::create_item(
        server,
        type_id,
        owner,
        station as i64,
        w.flag("Hangar"),
        qty as i32,
        false,
    )
    .await?;
    inventory::notify_new(server, &item);
    if !ship {
        inventory::stack_into(server, &item, station as i64, w.flag("Hangar")).await?;
    }
    Ok(())
}

/// Takes `qty` from an order (a player's order shrinks; NPC stock is endless).
async fn take(server: &Server, o: &Order, qty: i64) -> anyhow::Result<()> {
    if o.npc {
        return Ok(());
    }
    db::exec(
        &server.pool,
        r#"UPDATE game.market_orders SET "volRemaining" = "volRemaining" - $2,
               "escrow" = greatest(0, "escrow" - CASE WHEN "bid" = 1 THEN "price" * $2 ELSE 0 END)
           WHERE "orderID" = $1"#,
        &[&o.order_id, &(qty as i32)],
    )
    .await?;
    order_changed(server, o.order_id).await
}

/// CharPlaceOrder(typeInfo, quantity, price, range, duration, bid, itemID).
#[allow(clippy::too_many_arguments)]
async fn place_order(
    server: &Server,
    me: i32,
    origin: (Option<i32>, i32, i32, i32),
    owner: i32,
    type_id: i32,
    sub: Option<i32>,
    qty: i64,
    price: f64,
    range: i32,
    timeframe: i32,
    bid: bool,
    item_id: Option<i64>,
    at_station: Option<i32>,
) -> CallResult {
    let w = &server.world;
    let station_id = at_station.or(origin.0).ok_or_else(|| {
        user_error(
            "CustomInfo",
            vec![("info", Value::str("You must be docked to trade."))],
        )
    })?;
    let st = w
        .stations
        .get(&station_id)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("unknown station {station_id}"))?;
    if qty <= 0 || !(price > 0.0) {
        return Err(user_error(
            "CustomInfo",
            vec![("info", Value::str("Enter a quantity and a price."))],
        ));
    }
    let station_origin = (
        Some(st.station_id),
        st.solar_system_id,
        st.constellation_id,
        st.region_id,
    );
    let mut left = qty;
    if bid {
        // Buy from the cheapest sellers the order's range reaches.
        let mut sellers: Vec<Order> = visible(server, station_origin, range, type_id)
            .await?
            .into_iter()
            .filter(|o| !o.bid && o.price <= price)
            .collect();
        sellers.sort_by(|a, b| a.price.total_cmp(&b.price));
        for o in sellers {
            if left == 0 {
                break;
            }
            let q = left.min(o.remaining);
            super::wallet::transfer(
                server,
                owner,
                o.owner,
                o.price * q as f64,
                "refMarketTransaction",
                Some(type_id as i64),
                "",
            )
            .await?;
            take(server, &o, q).await?;
            deliver(server, owner, o.station, type_id, q).await?;
            record_trade(server, type_id, st.region_id, o.price, q).await?;
            left -= q;
        }
        if left > 0 {
            let escrow = price * left as f64;
            super::wallet::transfer(
                server,
                owner,
                w.constant("ownerSystem"),
                escrow,
                "refMarketTransaction",
                Some(type_id as i64),
                "Escrow",
            )
            .await?;
            insert_order(
                server, type_id, sub, owner, me, true, price, qty, left, range, &st, timeframe,
                escrow,
            )
            .await?;
        }
        return Ok(Value::None);
    }
    // Selling: the goods come out of the hangar first.
    let Some(item) = inventory::item(server, item_id.unwrap_or(0)).await? else {
        return Err(user_error(
            "CustomInfo",
            vec![("info", Value::str("That item is gone."))],
        ));
    };
    if item.owner_id != owner || item.location_id != station_id as i64 || item.type_id != type_id {
        return Err(user_error(
            "CustomInfo",
            vec![("info", Value::str("That item is not in your hangar here."))],
        ));
    }
    let qty = qty.min(item.quantity as i64);
    left = qty;
    if qty == item.quantity as i64 {
        inventory::delete_item(server, &item).await?;
    } else {
        inventory::set_quantity(server, &item, item.quantity - qty as i32).await?;
    }
    // Buy orders whose range covers this station, best price first.
    let mut buyers: Vec<Order> = orders_of(server, st.region_id, type_id)
        .await?
        .into_iter()
        .filter(|o| o.bid && o.price >= price)
        .filter(|o| {
            w.stations.get(&o.station).is_some_and(|bs| {
                let o_origin = (
                    Some(bs.station_id),
                    bs.solar_system_id,
                    bs.constellation_id,
                    bs.region_id,
                );
                in_range(server, o.range, o_origin, &st)
            })
        })
        .collect();
    buyers.sort_by(|a, b| b.price.total_cmp(&a.price));
    for o in buyers {
        if left == 0 {
            break;
        }
        let q = left.min(o.remaining);
        let payer = if o.npc {
            o.owner
        } else {
            w.constant("ownerSystem")
        };
        super::wallet::transfer(
            server,
            payer,
            owner,
            o.price * q as f64,
            "refMarketTransaction",
            Some(type_id as i64),
            "",
        )
        .await?;
        take(server, &o, q).await?;
        deliver(server, o.owner, station_id, type_id, q).await?;
        record_trade(server, type_id, st.region_id, o.price, q).await?;
        left -= q;
    }
    if left > 0 {
        insert_order(
            server, type_id, sub, owner, me, false, price, qty, left, range, &st, timeframe, 0.0,
        )
        .await?;
    }
    Ok(Value::None)
}

#[allow(clippy::too_many_arguments)]
async fn insert_order(
    server: &Server,
    type_id: i32,
    sub: Option<i32>,
    owner: i32,
    char_id: i32,
    bid: bool,
    price: f64,
    entered: i64,
    remaining: i64,
    range: i32,
    st: &Station,
    timeframe: i32,
    escrow: f64,
) -> anyhow::Result<()> {
    let expires = std::time::SystemTime::now()
        + std::time::Duration::from_secs(duration_days(server, timeframe) * 86400);
    let c = server.pool.get().await?;
    let id: i32 = c
        .query_one(
            r#"INSERT INTO game.market_orders ("typeID", "subTypeID", "ownerID", "charID", "bid", "price", "volEntered", "volRemaining",
                   "range", "stationID", "solarSystemID", "constellationID", "regionID", "expires", "escrow")
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15) RETURNING "orderID""#,
            &[
                &type_id,
                &sub,
                &owner,
                &char_id,
                &(bid as i32),
                &price,
                &(entered as i32),
                &(remaining as i32),
                &range,
                &st.station_id,
                &st.solar_system_id,
                &st.constellation_id,
                &st.region_id,
                &expires,
                &escrow,
            ],
        )
        .await?
        .get(0);
    drop(c);
    if let Some(r) = server
        .pool
        .get()
        .await?
        .query_opt(
            r#"SELECT * FROM game.market_orders WHERE "orderID" = $1"#,
            &[&id],
        )
        .await?
    {
        let o = order_from(&r);
        super::notify_chars(
            server,
            &[char_id],
            "OnOwnOrderChanged",
            vec![o.row(server), Value::str("Add")],
        );
    }
    Ok(())
}

/// marketBroker / market service calls.
pub async fn service(
    server: &Server,
    session: &mut Session,
    service: &str,
    method: &str,
    call: &Call,
) -> CallResult {
    let a = Args::of(call);
    match (service, method) {
        ("market", "GetMarketGroups") => Ok(market_groups(server).await?),
        // GetMarketRegion([locationID]): the bound market of that region.
        ("marketBroker", "GetMarketRegion") | ("market", "GetMarketRegion") => {
            let region = match a.int(0) {
                Some(loc) => server
                    .world
                    .stations
                    .get(&(loc as i32))
                    .map(|s| s.region_id)
                    .or_else(|| server.world.systems.get(&(loc as i32)).map(|s| s.region_id))
                    .unwrap_or(loc as i32),
                None => here(server, session).3,
            };
            Ok(session.bind_value(server, "market", Value::Int(region)))
        }
        // GetPriceHistory(range, typeInfo, timeframe, frequency): daily rows
        // with time, lowPrice, highPrice, closePrice, volume.
        ("market", "GetPriceHistory") => {
            let (type_id, _) = type_key(a.pos.get(1));
            let days = duration_days(server, a.i32(2).unwrap_or(0)) as i32;
            Ok(db::rowset(
                &server.pool,
                r#"SELECT "day"::timestamptz AS "time", "lowPrice", "highPrice", "closePrice", "volume" FROM game.market_history
                   WHERE "typeID" = $1 AND "regionID" = $2 AND "day" > current_date - $3 ORDER BY "day""#,
                &[&type_id, &here(server, session).3, &days],
            )
            .await?)
        }
        _ => super::unimplemented(service, method, call),
    }
}

/// market[regionID] methods.
pub async fn bound(
    server: &Server,
    session: &mut Session,
    _bound: &Bound,
    call: &Call,
) -> CallResult {
    let a = Args::of(call);
    let origin = here(server, session);
    let me = session.char_id().unwrap_or(0);
    let w = &server.world;
    // GetQuote/GetOrders take (typeKey, range) from marketQuote and
    // (locationID, typeKey) from its *Internal variants.
    let key_and_range = || -> (i32, Option<i32>, i32) {
        if a.pos.get(1).is_some_and(|v| v.as_seq().is_some()) {
            let (t, s) = type_key(a.pos.get(1));
            (t, s, w.constant("rangeStation"))
        } else {
            let (t, s) = type_key(a.pos.first());
            (t, s, a.i32(1).unwrap_or(w.constant("rangeStation")))
        }
    };
    match call.method() {
        "GetQuote" => {
            let (t, s, range) = key_and_range();
            Ok(quote(server, origin, range, t, s).await?)
        }
        // GetOrders(typeKey, range): bids and asks, best first.
        "GetOrders" => {
            let (t, _, range) = key_and_range();
            let orders = visible(server, origin, range, t).await?;
            let mut bids: Vec<&Order> = orders.iter().filter(|o| o.bid).collect();
            let mut asks: Vec<&Order> = orders.iter().filter(|o| !o.bid).collect();
            bids.sort_by(|a, b| b.price.total_cmp(&a.price));
            asks.sort_by(|a, b| a.price.total_cmp(&b.price));
            Ok(rows::keyval(vec![
                (
                    "bids",
                    Value::List(bids.iter().map(|o| o.row(server)).collect()),
                ),
                (
                    "asks",
                    Value::List(asks.iter().map(|o| o.row(server)).collect()),
                ),
            ]))
        }
        "GetGroupQuote" => Ok(group_quote(
            server,
            origin,
            a.i32(1).unwrap_or(w.constant("rangeStation")),
            a.i32(0).unwrap_or(0),
        )
        .await?),
        // {typeKey: quote} for every type of a group.
        "GetGroupQuotes" => {
            let range = a.i32(1).unwrap_or(w.constant("rangeStation"));
            let mut out = Vec::new();
            for t in group_types(server, a.i32(0).unwrap_or(0)).await? {
                out.push((
                    key_value(t, None),
                    quote(server, origin, range, t, None).await?,
                ));
            }
            Ok(Value::Dict(out))
        }
        // {marketGroupID: group summary} for the root groups.
        "GetGroupsQuotes" => {
            let range = a.i32(0).unwrap_or(w.constant("rangeStation"));
            let c = server.pool.get().await?;
            let groups: Vec<i32> = c
                .query(
                    r#"SELECT "marketGroupID" FROM static.market_groups ORDER BY 1"#,
                    &[],
                )
                .await?
                .iter()
                .map(|r| r.get(0))
                .collect();
            drop(c);
            let mut out = Vec::new();
            for g in groups {
                out.push((Value::Int(g), group_quote(server, origin, range, g).await?));
            }
            Ok(Value::Dict(out))
        }
        // GetOrderQuotesHood(typeID, locationID, buysell[, depth]): per
        // station of the area, the best price and volume on one side; the
        // caller's own station is always present.
        "GetOrderQuotesHood" => {
            let type_id = type_key(a.pos.first()).0;
            let area = a.i32(1).unwrap_or(origin.1);
            let want_bids = a.int(2).unwrap_or(0) != 0;
            let orders = orders_of(server, origin.3, type_id).await?;
            let mut best: BTreeMap<i32, (f64, i64)> = BTreeMap::new();
            if let Some(st) = origin.0 {
                best.insert(st, (0.0, 0));
            }
            for o in orders.iter().filter(|o| o.bid == want_bids) {
                let Some(st) = w.stations.get(&o.station) else {
                    continue;
                };
                if ![
                    st.station_id,
                    st.solar_system_id,
                    st.constellation_id,
                    st.region_id,
                ]
                .contains(&area)
                {
                    continue;
                }
                let e = best.entry(o.station).or_insert((0.0, 0));
                let better = e.1 == 0
                    || if want_bids {
                        o.price > e.0
                    } else {
                        o.price < e.0
                    };
                if better {
                    e.0 = o.price;
                }
                e.1 += o.remaining;
            }
            Ok(Value::Dict(
                best.into_iter()
                    .map(|(st, (price, vol))| {
                        let system = w.stations.get(&st).map(|s| s.solar_system_id).unwrap_or(0);
                        (
                            Value::Int(st),
                            rows::keyval(vec![
                                ("bestPrice", Value::Float(price)),
                                ("volume", Value::from(vol)),
                                ("locationID", Value::Int(system)),
                                ("stationID", Value::Int(st)),
                            ]),
                        )
                    })
                    .collect(),
            ))
        }
        "CharPlaceOrder" | "CorpPlaceOrder" => {
            let owner = if call.method() == "CorpPlaceOrder" {
                let corp = session.attrs.int("corpid").unwrap_or(0) as i32;
                if !super::corp::is_accountant(server, session, corp).await? {
                    return Err(user_error("CrpAccessDenied", vec![]));
                }
                corp
            } else {
                me
            };
            let (t, s) = type_key(a.pos.first());
            let item = a.int(6).filter(|&i| i > 0);
            place_order(
                server,
                session.char_id().unwrap_or(0),
                here(server, session),
                owner,
                t,
                s,
                a.int(1).unwrap_or(0),
                a.float(2).unwrap_or(0.0),
                a.i32(3).unwrap_or(w.constant("rangeStation")),
                a.i32(4).unwrap_or(0),
                a.int(5).unwrap_or(1) != 0,
                item,
                None,
            )
            .await
        }
        // CharPlaceRemoteBid(locationID, typeInfo, quantity, price, duration)
        "CharPlaceRemoteBid" => {
            let (t, s) = type_key(a.pos.get(1));
            place_order(
                server,
                session.char_id().unwrap_or(0),
                here(server, session),
                me,
                t,
                s,
                a.int(2).unwrap_or(0),
                a.float(3).unwrap_or(0.0),
                w.constant("rangeStation"),
                a.i32(4).unwrap_or(0),
                true,
                None,
                a.i32(0),
            )
            .await
        }
        // My orders (mymarketorders.py reads typeInfo, bid, price, volEntered,
        // volRemaining, stationID, expires, orderID).
        "CharGetOrders" => {
            let c = server.pool.get().await?;
            let rs = c
                .query(
                    r#"SELECT * FROM game.market_orders WHERE "charID" = $1 AND "volRemaining" > 0 AND "expires" > now() ORDER BY "orderID""#,
                    &[&me],
                )
                .await?;
            Ok(Value::List(
                rs.iter()
                    .map(|r| {
                        // A KeyVal, not a Row: the client assigns data.typeInfo.
                        let o = order_from(r);
                        let sub: Option<i32> = r.get("subTypeID");
                        let mut fields = vec![(Value::str("typeInfo"), key_value(o.type_id, sub))];
                        if let Value::Instance { state, .. } = o.row(server) {
                            if let (Some(Value::List(h)), Some(Value::List(l))) =
                                (state.get("header"), state.get("line"))
                            {
                                fields.extend(h.iter().cloned().zip(l.iter().cloned()));
                            }
                        }
                        Value::instance("util.KeyVal", Value::Dict(fields))
                    })
                    .collect(),
            ))
        }
        // CharCancelOrder(orderID): goods or escrow go back.
        "CharCancelOrder" => {
            let id = a.i32(0).unwrap_or(0);
            let c = server.pool.get().await?;
            let Some(r) = c
                .query_opt(r#"DELETE FROM game.market_orders WHERE "orderID" = $1 AND "charID" = $2 RETURNING *"#, &[&id, &me])
                .await?
            else {
                return Ok(Value::None);
            };
            drop(c);
            let o = order_from(&r);
            if o.bid {
                super::wallet::transfer(
                    server,
                    w.constant("ownerSystem"),
                    o.owner,
                    o.escrow,
                    "refMarketTransaction",
                    Some(o.type_id as i64),
                    "Escrow refund",
                )
                .await?;
            } else {
                deliver(server, o.owner, o.station, o.type_id, o.remaining).await?;
            }
            super::notify_chars(
                server,
                &[me],
                "OnOwnOrderChanged",
                vec![o.row(server), Value::str("Expiry")],
            );
            Ok(Value::None)
        }
        m => super::unimplemented("market", m, call),
    }
}

/// A broker's sale on its client's behalf: the item's stack, at the type's
/// base price, from the station it is in.
pub async fn broker_sell(server: &Server, owner: i32, item: &inventory::Item) -> CallResult {
    let Some(price) = base_price(server, item.type_id).await? else { return Ok(Value::None) };
    let station = item.location_id as i32;
    let Some(st) = server.world.stations.get(&station) else { return Ok(Value::None) };
    let origin = (Some(station), st.solar_system_id, st.constellation_id, st.region_id);
    place_order(
        server,
        owner,
        origin,
        owner,
        item.type_id,
        None,
        item.quantity as i64,
        price,
        server.world.constant("rangeStation"),
        server.world.constant("timeframeMonth"),
        false,
        Some(item.item_id),
        Some(station),
    )
    .await
}
