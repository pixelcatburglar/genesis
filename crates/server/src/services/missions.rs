//! `missionMgr`: courier contracts between players (ui/station/missions/
//! base_missions.py, missionentry.py, ui/shared/neocom/journal.py).
//!
//! The issuer picks items in a station hangar; CreateCourierMission (on the
//! bound ('courier', stationID) creator, moniker.GetCourierMissionCreator)
//! packs them into a plastic wrap ("crate", rec.craterID) and escrows the
//! reward. Accepting pays the accept fee to the issuer, escrows collateral and
//! hands the crate to the acceptor; delivering it in the drop station's
//! hangar pays reward + collateral and gives the crate back to the issuer.
//!
//! Mission rows: state is 'i' (issued) or 'a' (accepted) (SetScrollHeaders).

use marshal::Value;
use proto::gpcs::Call;

use super::inventory::{self, Item};
use super::{user_error, wallet, Args, Bound, CallResult, Session};
use crate::{db, rows, Server};

/// TEMPLATE DEFAULT fees (shown by the create form).
const BASIC_FEE: f64 = 1_000.0;
const TRACKING_FEE: f64 = 500.0;

const HEADER: [&str; 14] = [
    "missionID",
    "state",
    "issuerID",
    "acceptorID",
    "pickupStationID",
    "pickupSolarSystemID",
    "dropStationID",
    "dropSolarSystemID",
    "craterID",
    "volume",
    "acceptFee",
    "totalReward",
    "collateral",
    "expires",
];

#[derive(Debug, Clone)]
struct Courier {
    id: i32,
    issuer: i32,
    acceptor: Option<i32>,
    start: i32,
    dest: i32,
    package: Option<i64>,
    reward: f64,
    collateral: f64,
    accept_fee: f64,
    status: String,
}

const COLS: &str = r#""missionID", "issuerID", "acceptorID", "startStationID", "destinationID", "packageID", "volume",
    "reward", "collateral", "acceptFee", "status", "expires""#;

fn courier_of(r: &tokio_postgres::Row) -> Courier {
    Courier {
        id: r.get("missionID"),
        issuer: r.get("issuerID"),
        acceptor: r.get("acceptorID"),
        start: r.get("startStationID"),
        dest: r.get("destinationID"),
        package: r.get("packageID"),
        reward: r.get("reward"),
        collateral: r.get("collateral"),
        accept_fee: r.get("acceptFee"),
        status: r.get("status"),
    }
}

fn system_of(server: &Server, station: i32) -> Value {
    server
        .world
        .stations
        .get(&station)
        .map(|s| Value::Int(s.solar_system_id))
        .unwrap_or(Value::None)
}

fn line(server: &Server, r: &tokio_postgres::Row) -> Vec<Value> {
    let m = courier_of(r);
    let expires: std::time::SystemTime = r.get("expires");
    vec![
        Value::Int(m.id),
        Value::str(if m.status == "accepted" { "a" } else { "i" }),
        Value::Int(m.issuer),
        m.acceptor.map(Value::Int).unwrap_or(Value::None),
        Value::Int(m.start),
        system_of(server, m.start),
        Value::Int(m.dest),
        system_of(server, m.dest),
        m.package.map(Value::from).unwrap_or(Value::None),
        Value::Float(r.get("volume")),
        Value::Float(m.accept_fee),
        Value::Float(m.reward),
        Value::Float(m.collateral),
        Value::Long(db::blue_time(expires)),
    ]
}

async fn listing(server: &Server, filter: &str, params: db::Params<'_>) -> CallResult {
    let c = server.pool.get().await?;
    let rs = c
        .query(
            &format!(r#"SELECT {COLS} FROM game.courier_missions WHERE "expires" > now() AND ({filter}) ORDER BY "missionID""#),
            params,
        )
        .await?;
    Ok(rows::rowset(
        &HEADER,
        rs.iter().map(|r| line(server, r)).collect(),
    ))
}

async fn courier(server: &Server, id: i32) -> anyhow::Result<Option<Courier>> {
    let c = server.pool.get().await?;
    Ok(c.query_opt(
        &format!(r#"SELECT {COLS} FROM game.courier_missions WHERE "missionID" = $1"#),
        &[&id],
    )
    .await?
    .map(|r| courier_of(&r)))
}

async fn set_status(
    server: &Server,
    id: i32,
    status: &str,
    acceptor: Option<i32>,
) -> anyhow::Result<()> {
    db::exec(
        &server.pool,
        r#"UPDATE game.courier_missions SET "status" = $2, "acceptorID" = $3 WHERE "missionID" = $1"#,
        &[&id, &status, &acceptor],
    )
    .await?;
    Ok(())
}

/// Is this item the crate of a running courier contract? (BreakPlasticWrap
/// must not open it.)
pub async fn is_courier_package(server: &Server, item_id: i64) -> anyhow::Result<bool> {
    Ok(db::scalar::<i32>(
        &server.pool,
        r#"SELECT "missionID" FROM game.courier_missions WHERE "packageID" = $1 AND "status" IN ('open', 'accepted') LIMIT 1"#,
        &[&item_id],
    )
    .await?
    .is_some())
}

/// The escrow holder: the corporation owning the pickup station.
fn escrow(server: &Server, station: i32) -> i32 {
    server
        .world
        .stations
        .get(&station)
        .map(|s| s.corporation_id)
        .unwrap_or(0)
}

/// Items the character can pack at a station: its hangar there, minus the
/// ship it is flying.
async fn packable(server: &Server, session: &Session, station: i32) -> anyhow::Result<Vec<Item>> {
    let me = session.char_id().unwrap_or(0);
    let ship = session.attrs.int("shipid").unwrap_or(0);
    let hangar = server.world.flag("Hangar");
    Ok(inventory::items_where(
        server,
        r#""ownerID" = $1 AND "locationID" = $2 AND "flag" = $3 AND "itemID" <> $4"#,
        &[&me, &(station as i64), &hangar, &ship],
    )
    .await?)
}

/// Gives the crate to `owner`, back in a station hangar if it is elsewhere.
async fn hand_over(
    server: &Server,
    package: Option<i64>,
    owner: i32,
    station: i32,
) -> anyhow::Result<()> {
    let Some(id) = package else { return Ok(()) };
    let Some(mut crate_) = inventory::item(server, id).await? else {
        return Ok(());
    };
    if !server
        .world
        .stations
        .contains_key(&(crate_.location_id as i32))
    {
        crate_ = inventory::move_item(server, &crate_, station as i64, server.world.flag("Hangar"))
            .await?;
    }
    inventory::set_owner(server, &crate_, owner).await?;
    Ok(())
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
        "GetAvailableCourierMissions" => {
            listing(
                server,
                r#""status" = 'open' OR ("status" = 'accepted' AND "acceptorID" = $1)"#,
                &[&me],
            )
            .await
        }
        "GetMyCourierMissions" => {
            listing(
                server,
                r#""status" IN ('open', 'accepted') AND ("issuerID" = $1 OR "acceptorID" = $1)"#,
                &[&me],
            )
            .await
        }
        "AcceptCourierMission" => {
            let Some(m) = courier(server, a.i32(0).unwrap_or(0))
                .await?
                .filter(|m| m.status == "open")
            else {
                return Err(user_error(
                    "CustomInfo",
                    vec![(
                        "info",
                        Value::str("That courier mission is no longer available."),
                    )],
                ));
            };
            if m.issuer == me {
                return Err(user_error(
                    "CustomInfo",
                    vec![(
                        "info",
                        Value::str("You cannot accept your own courier mission."),
                    )],
                ));
            }
            if session.attrs.int("stationid") != Some(m.start as i64) {
                return Err(user_error(
                    "CustomInfo",
                    vec![(
                        "info",
                        Value::str(
                            "You must be docked at the pickup station to accept this mission.",
                        ),
                    )],
                ));
            }
            if wallet::balance(server, me).await? < m.accept_fee + m.collateral {
                return Err(user_error(
                    "NotEnoughMoney",
                    vec![
                        ("balance", Value::Float(wallet::balance(server, me).await?)),
                        ("amount", Value::Float(m.accept_fee + m.collateral)),
                    ],
                ));
            }
            wallet::transfer(
                server,
                me,
                m.issuer,
                m.accept_fee,
                "refMissionCost",
                Some(m.id as i64),
                "",
            )
            .await?;
            wallet::transfer(
                server,
                me,
                escrow(server, m.start),
                m.collateral,
                "refCourierMissionEscrow",
                Some(m.id as i64),
                "",
            )
            .await?;
            set_status(server, m.id, "accepted", Some(me)).await?;
            hand_over(server, m.package, me, m.start).await?;
            Ok(Value::None)
        }
        "DeliverCourierPackage" => {
            let Some(m) = courier(server, a.i32(0).unwrap_or(0))
                .await?
                .filter(|m| m.status == "accepted" && m.acceptor == Some(me))
            else {
                return Ok(Value::None);
            };
            let crate_ = match m.package {
                Some(id) => inventory::item(server, id).await?,
                None => None,
            };
            let delivered = crate_.as_ref().is_some_and(|c| {
                c.location_id == m.dest as i64
                    && c.owner_id == me
                    && c.flag == server.world.flag("Hangar")
            });
            if !delivered {
                return Err(user_error(
                    "CustomInfo",
                    vec![(
                        "info",
                        Value::str(
                            "The package must be in your hangar at the destination station.",
                        ),
                    )],
                ));
            }
            set_status(server, m.id, "delivered", Some(me)).await?;
            hand_over(server, m.package, m.issuer, m.dest).await?;
            wallet::transfer(
                server,
                escrow(server, m.start),
                me,
                m.reward,
                "refMissionReward",
                Some(m.id as i64),
                "",
            )
            .await?;
            wallet::transfer(
                server,
                escrow(server, m.start),
                me,
                m.collateral,
                "refCourierMissionEscrow",
                Some(m.id as i64),
                "",
            )
            .await?;
            super::notify_chars(
                server,
                &[m.issuer],
                "OnCourierMissionChange",
                vec![Value::str("delivered"), Value::Int(m.id)],
            );
            Ok(Value::None)
        }
        // The issuer closes an untaken mission (unpacking the crate), or the
        // acceptor gives up (forfeiting the collateral to the issuer).
        "CancelMission" => {
            let Some(m) = courier(server, a.i32(0).unwrap_or(0)).await? else {
                return Ok(Value::None);
            };
            if m.status == "open" && m.issuer == me {
                set_status(server, m.id, "cancelled", None).await?;
                if let Some(wrap) = match m.package {
                    Some(id) => inventory::item(server, id).await?,
                    None => None,
                } {
                    for inner in
                        inventory::items_where(server, r#""locationID" = $1"#, &[&wrap.item_id])
                            .await?
                    {
                        inventory::move_item(server, &inner, wrap.location_id, wrap.flag).await?;
                    }
                    inventory::delete_item(server, &wrap).await?;
                }
                wallet::transfer(
                    server,
                    escrow(server, m.start),
                    me,
                    m.reward,
                    "refCourierMissionEscrow",
                    Some(m.id as i64),
                    "",
                )
                .await?;
            } else if m.status == "accepted" && m.acceptor == Some(me) {
                set_status(server, m.id, "cancelled", Some(me)).await?;
                hand_over(server, m.package, m.issuer, m.start).await?;
                wallet::transfer(
                    server,
                    escrow(server, m.start),
                    m.issuer,
                    m.reward + m.collateral,
                    "refCourierMissionEscrow",
                    Some(m.id as i64),
                    "",
                )
                .await?;
                super::notify_chars(
                    server,
                    &[m.issuer],
                    "OnCourierMissionChange",
                    vec![Value::str("failed"), Value::Int(m.id)],
                );
            }
            Ok(Value::None)
        }
        m => super::unimplemented("missionMgr", m, call),
    }
}

/// The bound ('courier', stationID) mission creator.
pub async fn bound(
    server: &Server,
    session: &mut Session,
    bound: &Bound,
    call: &Call,
) -> CallResult {
    let a = Args::of(call);
    let me = session.char_id().unwrap_or(0);
    let station = bound
        .params
        .as_seq()
        .and_then(|p| p.get(1))
        .and_then(Value::as_int)
        .unwrap_or(0) as i32;
    match call.method() {
        // KeyVal(itemList=KeyVal(header, items={itemID: line}), basicFee, trackingFee)
        "GetCourierOptions" => {
            let items = packable(server, session, station).await?;
            let lines = items
                .iter()
                .map(|it| (Value::from(it.item_id), Value::List(it.line(server))))
                .collect();
            Ok(rows::keyval(vec![
                (
                    "itemList",
                    rows::keyval(vec![
                        (
                            "header",
                            Value::List(
                                inventory::INV_HEADER
                                    .iter()
                                    .map(|h| Value::str(h))
                                    .collect(),
                            ),
                        ),
                        ("items", Value::Dict(lines)),
                    ]),
                ),
                ("basicFee", Value::Float(BASIC_FEE)),
                ("trackingFee", Value::Float(TRACKING_FEE)),
            ]))
        }
        // CreateCourierMission([(itemID, None)], dropStationID, None, collateral,
        //     tracking=, reward=, acceptFee=) -> (missionID, [(rec, exc, (reason,))])
        "CreateCourierMission" => {
            let wanted: Vec<i64> = a
                .pos
                .first()
                .and_then(Value::as_seq)
                .map(|s| {
                    s.iter()
                        .filter_map(|e| e.as_seq().and_then(|t| t.first()).and_then(Value::as_int))
                        .collect()
                })
                .unwrap_or_default();
            let dest = a.i32(1).unwrap_or(station);
            let collateral = a
                .get(3, "collateral")
                .and_then(Value::as_float)
                .unwrap_or(0.0)
                .max(0.0);
            let tracking = a.get(4, "tracking").and_then(Value::as_int).unwrap_or(0) != 0;
            let reward = a
                .get(5, "reward")
                .and_then(Value::as_float)
                .unwrap_or(0.0)
                .max(0.0);
            let accept_fee = a
                .get(6, "acceptFee")
                .and_then(Value::as_float)
                .unwrap_or(0.0)
                .max(0.0);
            if !server.world.stations.contains_key(&dest) || dest == station {
                return Err(user_error(
                    "CustomInfo",
                    vec![(
                        "info",
                        Value::str("Pick a different station to deliver to."),
                    )],
                ));
            }
            let ok = packable(server, session, station).await?;
            let mut take = Vec::new();
            let mut excluded = Vec::new();
            for id in wanted {
                match ok.iter().find(|i| i.item_id == id) {
                    Some(it) => take.push(it.clone()),
                    None => {
                        if let Some(it) = inventory::item(server, id).await? {
                            excluded.push(Value::tuple([
                                it.row(server),
                                Value::None,
                                Value::tuple([Value::str(
                                    "Not in your hangar at the pickup station",
                                )]),
                            ]));
                        }
                    }
                }
            }
            if take.is_empty() {
                return Ok(Value::tuple([Value::None, Value::List(excluded)]));
            }
            let cost = reward + BASIC_FEE + if tracking { TRACKING_FEE } else { 0.0 };
            wallet::transfer(
                server,
                me,
                escrow(server, station),
                cost,
                "refCourierMissionEscrow",
                None,
                "",
            )
            .await?;
            let hangar = server.world.flag("Hangar");
            let wrap = inventory::create_item(
                server,
                server.world.type_id("PlasticWrap"),
                me,
                station as i64,
                hangar,
                1,
                true,
            )
            .await?;
            inventory::notify_new(server, &wrap);
            let mut volume = 0.0;
            for it in &take {
                volume += it.volume(server);
                inventory::move_item(server, it, wrap.item_id, 0).await?;
            }
            let id: i32 = db::scalar(
                &server.pool,
                r#"INSERT INTO game.courier_missions ("issuerID", "startStationID", "destinationID", "packageID", "volume", "reward",
                       "collateral", "acceptFee", "expires")
                   VALUES ($1, $2, $3, $4, $5, $6, $7, $8, now() + interval '14 days') RETURNING "missionID""#,
                &[&me, &station, &dest, &wrap.item_id, &volume, &reward, &collateral, &accept_fee],
            )
            .await?
            .unwrap_or(0);
            Ok(Value::tuple([Value::Int(id), Value::List(excluded)]))
        }
        m => super::unimplemented("missionMgr[courier]", m, call),
    }
}
