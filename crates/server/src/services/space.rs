//! In-space services: the bound `ship` service (board, eject, undock,
//! jettison, scoop, launch), the remote ballpark `beyonce[solarsystemID]`
//! (movement, warp, dock, jump, scans) and `aggressionMgr`.
//!
//! The client never moves its own ship: every order goes to beyonce, and the
//! server answers by pushing DoDestinyUpdate events to everyone in the system
//! (remote/michelle.py RealFlushState), the commander included. Every client
//! then runs destiny on the same events, warps included; the server simulates
//! the same ticks (space::Park::step) so that it agrees with them. Events are
//! queued and sent once per tick (see `flush`). Slim items never change in
//! place (michelle.AddBalls skips known balls), so a ball whose pilot or owner
//! changes is removed and added again.

use anyhow::{anyhow, Context};
use marshal::Value;
use proto::gpcs::Call;

use super::inventory::{self, Item};
use super::{user_error, Args, CallError, CallResult, Session};
use crate::space::ballimg::{Ball, Mode};
use crate::space::{self, Entry, Order, Warp};
use crate::{db, rows, Server};

/// TEMPLATE DEFAULT: how far off the station's surface an undocking ship
/// appears (well inside the 2500 m docking range).
const UNDOCK_CLEARANCE: f64 = 500.0;

/// TEMPLATE DEFAULT: how close a pilot must be to board, scoop or open
/// something (const.maxCargoContainerTransferDistance is the client's range).
fn reach(server: &Server) -> f64 {
    server.world.constant("maxCargoContainerTransferDistance") as f64
}

/// Loads a system's celestials and the player objects floating in it into
/// its park the first time it is needed.
pub async fn ensure_park(server: &Server, system_id: i32) -> anyhow::Result<()> {
    if server.space.with_park(system_id, |p| p.loaded) {
        return Ok(());
    }
    let c = server.pool.get().await?;
    let rows = c
        .query(
            r#"SELECT c."itemID", c."typeID", c."groupID", c."x", c."y", c."z", c."radius", c."itemName",
                      s."corporationID", g."destinationID", d."solarSystemID"
               FROM static.celestials c
               LEFT JOIN static.stations s ON s."stationID" = c."itemID"
               LEFT JOIN static.stargates g ON g."stargateID" = c."itemID"
               LEFT JOIN static.celestials d ON d."itemID" = g."destinationID"
               WHERE c."solarSystemID" = $1"#,
            &[&system_id],
        )
        .await?;
    let groups = (
        server.world.group_id("Station"),
        server.world.group_id("Stargate"),
        server.world.group_id("Sun"),
    );
    let mut entries = Vec::new();
    for r in rows {
        let id: i32 = r.get(0);
        let type_id: i32 = r.get(1);
        let group_id: i32 = r.get(2);
        let category_id = server
            .world
            .type_info(type_id)
            .map(|t| t.category_id)
            .unwrap_or(2);
        let interactive = Some(group_id) == groups.0 || Some(group_id) == groups.1;
        let global = Some(group_id) == groups.2 || interactive;
        let ball = Ball::fixed(
            id as i64,
            r.get(6),
            [r.get(3), r.get(4), r.get(5)],
            interactive,
            global,
        );
        let owner: Option<i32> = r.get(8);
        let jump = match (r.get::<_, Option<i32>>(9), r.get::<_, Option<i32>>(10)) {
            (Some(gate), Some(sys)) => Some((sys, gate as i64)),
            _ => None,
        };
        entries.push(Entry {
            ball,
            type_id,
            group_id,
            category_id,
            owner_id: owner.unwrap_or(1),
            char_id: None,
            corp_id: None,
            jump,
            name: r.get(7),
            client_id: None,
            order: Order::Stop,
            warp: None,
            modules: Vec::new(),
        });
    }
    // Containers, abandoned ships and drones players left here.
    let floating = c
        .query(
            r#"SELECT i."itemID", o."x", o."y", o."z" FROM game.items i JOIN game.space_objects o USING ("itemID")
               WHERE i."locationID" = $1"#,
            &[&(system_id as i64)],
        )
        .await?;
    drop(c);
    for r in floating {
        if let Some(item) = inventory::item(server, r.get(0)).await? {
            entries.push(object_entry(server, &item, [r.get(1), r.get(2), r.get(3)], None).await?);
        }
    }
    // Belt asteroids and pirates.
    entries.extend(super::combat::populate(server, system_id).await?);
    server.space.with_park(system_id, |p| {
        if !p.loaded {
            for e in entries {
                p.entries.insert(e.ball.id, e);
            }
            p.loaded = true;
        }
    });
    Ok(())
}

/// Where a ship warping to `target` comes to rest: just off its surface on
/// the ship's side.
fn warp_arrival(target: &Entry, ship: &Entry) -> [f64; 3] {
    let dir = space::norm(space::sub(ship.ball.pos, target.ball.pos));
    space::add_scaled(
        target.ball.pos,
        dir,
        target.ball.radius as f64 + ship.ball.radius as f64 + space::WARP_ARRIVAL_MARGIN,
    )
}

/// Who flies a ball: (characterID, corporationID, connection).
#[derive(Debug, Clone, Copy)]
pub struct Pilot {
    pub char_id: i32,
    pub corp_id: Option<i32>,
    pub client_id: i64,
}

fn pilot_of(session: &Session) -> Option<Pilot> {
    Some(Pilot {
        char_id: session.char_id()?,
        corp_id: session.attrs.int("corpid").map(|c| c as i32),
        client_id: session.client_id,
    })
}

/// A ball for an item (ship, capsule, container, drone), from its type's
/// dogma attributes.
pub async fn object_entry(
    server: &Server,
    item: &Item,
    pos: [f64; 3],
    pilot: Option<Pilot>,
) -> anyhow::Result<Entry> {
    let c = server.pool.get().await?;
    let attrs: std::collections::HashMap<String, f64> = c
        .query(
            r#"SELECT a."attributeName", ta."value" FROM static.dgmtypeattribs ta
               JOIN static.dgmattribs a USING ("attributeID") WHERE ta."typeID" = $1"#,
            &[&item.type_id],
        )
        .await?
        .iter()
        .map(|r| (r.get(0), r.get(1)))
        .collect();
    let a = |n: &str, d: f64| attrs.get(n).copied().unwrap_or(d);
    let t = server.world.type_info(item.type_id);
    let category_id = t.map(|t| t.category_id).unwrap_or(0);
    let movable = category_id == server.world.category_id("Ship")
        || category_id == server.world.category_id("Drone");
    let name: Option<String> = c
        .query_opt(
            r#"SELECT "itemName" FROM game.items WHERE "itemID" = $1"#,
            &[&item.item_id],
        )
        .await?
        .and_then(|r| r.get(0));
    let w = &server.world;
    let modules: Vec<(i64, i32)> = c
        .query(
            r#"SELECT "itemID", "typeID" FROM game.items WHERE "locationID" = $1 AND "flag" BETWEEN $2 AND $3 AND "singleton" = 1
               ORDER BY "flag""#,
            &[&item.item_id, &w.flag("SlotFirst"), &w.flag("SlotLast")],
        )
        .await?
        .iter()
        .map(|r| (r.get(0), r.get(1)))
        .collect();
    Ok(Entry {
        ball: Ball {
            id: item.item_id,
            mass: a("mass", 1.0e6) as f32,
            radius: a("radius", if movable { 40.0 } else { 100.0 }) as f32,
            max_velocity: if movable {
                a("maxVelocity", 200.0) as f32
            } else {
                0.0
            },
            is_free: movable,
            is_global: false,
            is_massive: true,
            is_interactive: true,
            pos,
            vel: [0.0; 3],
            agility: a("agility", 1.0) as f32,
            // speedFraction gates every sublight order (maxVelocity * fraction).
            speed_fraction: if movable { 1.0 } else { 0.0 },
            mode: if movable { Mode::Stop } else { Mode::Rigid },
            goto: pos,
            follow_id: 0,
            follow_range: 0.0,
            owner_id: item.owner_id as i64,
            effect_stamp: -1,
        },
        type_id: item.type_id,
        group_id: t.map(|t| t.group_id).unwrap_or(0),
        category_id,
        owner_id: item.owner_id,
        char_id: pilot.map(|p| p.char_id),
        corp_id: pilot.and_then(|p| p.corp_id),
        jump: None,
        name: name.unwrap_or_else(|| t.map(|t| t.name.clone()).unwrap_or_default()),
        client_id: pilot.map(|p| p.client_id),
        order: Order::Stop,
        warp: None,
        modules,
    })
}

/// Sends destiny events to every client in a system.
pub fn broadcast(server: &Server, system_id: i32, events: Vec<Value>) {
    broadcast_except(server, system_id, None, events);
}

fn broadcast_except(server: &Server, system_id: i32, except: Option<i64>, events: Vec<Value>) {
    if events.is_empty() {
        return;
    }
    server.space.queue(system_id, except, events);
}

/// Sends every client in space the tick's queued events as ONE packet
/// stamped `stamp`. michelle applies one DoDestinyUpdate per tick, rewinding
/// (FindState) to the last state it stored when the stamp is behind; that
/// state is the tick after the previous packet, so a second packet with the
/// same stamp cannot be placed, and the client throws its park away and asks
/// for a new SetState (UpdateStateRequest).
fn flush(server: &Server, stamp: i32) {
    let queued = server.space.take_outbox();
    let mut systems: Vec<i32> = queued.iter().map(|(s, _, _)| *s).collect();
    systems.sort_unstable();
    systems.dedup();
    for system in systems {
        for (id, c) in server
            .registry
            .select(None, |c| c.solar_system_id == Some(system as i64))
        {
            let events: Vec<Value> = queued
                .iter()
                .filter(|(s, except, _)| *s == system && *except != Some(id))
                .map(|(_, _, e)| space::restamp(e, stamp))
                .collect();
            if !events.is_empty() {
                c.send(crate::conn::event(
                    server.config.node_id,
                    id,
                    "DoDestinyUpdate",
                    vec![Value::List(events)],
                ));
            }
        }
    }
}

fn sol_item(server: &Server, system_id: i32) -> Value {
    let name = server
        .world
        .systems
        .get(&system_id)
        .map(|s| s.name.clone())
        .unwrap_or_default();
    Entry {
        ball: Ball::fixed(system_id as i64, 0.0, [0.0; 3], false, true),
        type_id: 5,
        group_id: 5,
        category_id: 2,
        owner_id: 1,
        char_id: None,
        corp_id: None,
        jump: None,
        name,
        client_id: None,
        order: Order::Stop,
        warp: None,
        modules: Vec::new(),
    }
    .slim()
}

/// Adds a ball to a park and tells the system (not `except`, who learns it
/// another way).
fn add_ball(server: &Server, system_id: i32, entry: Entry, except: Option<i64>) {
    let add = space::add_balls(&[&entry]);
    server
        .space
        .with_park(system_id, |p| p.entries.insert(entry.ball.id, entry));
    broadcast_except(server, system_id, except, vec![add]);
}

fn remove_ball(server: &Server, system_id: i32, id: i64) -> Option<Entry> {
    let e = server.space.with_park(system_id, |p| p.entries.remove(&id));
    broadcast(
        server,
        system_id,
        vec![space::event(
            space::stamp_now(),
            "RemoveBall",
            vec![Value::from(id)],
        )],
    );
    e
}

/// Records where a floating item is.
async fn place_item(server: &Server, item_id: i64, pos: [f64; 3]) -> anyhow::Result<()> {
    db::exec(
        &server.pool,
        r#"INSERT INTO game.space_objects ("itemID", "x", "y", "z") VALUES ($1, $2, $3, $4)
           ON CONFLICT ("itemID") DO UPDATE SET "x" = $2, "y" = $3, "z" = $4"#,
        &[&item_id, &pos[0], &pos[1], &pos[2]],
    )
    .await?;
    Ok(())
}

async fn unplace_item(server: &Server, item_id: i64) -> anyhow::Result<()> {
    db::exec(
        &server.pool,
        r#"DELETE FROM game.space_objects WHERE "itemID" = $1"#,
        &[&item_id],
    )
    .await?;
    Ok(())
}

/// Puts the session's ship into a system at `pos` and tells the others.
async fn enter_space(
    server: &Server,
    session: &Session,
    system_id: i32,
    pos: [f64; 3],
) -> anyhow::Result<()> {
    ensure_park(server, system_id).await?;
    let ship_id = session.attrs.int("shipid").context("no ship")?;
    let ship = inventory::item(server, ship_id)
        .await?
        .ok_or_else(|| anyhow!("ship {ship_id} missing"))?;
    let entry = object_entry(server, &ship, pos, pilot_of(session)).await?;
    // Others see the ship arrive; the arriving client gets it in SetState.
    add_ball(server, system_id, entry, Some(session.client_id));
    let c = server.pool.get().await?;
    c.execute(
        r#"UPDATE game.items SET "locationID" = $2, "flag" = 0 WHERE "itemID" = $1"#,
        &[&ship_id, &(system_id as i64)],
    )
    .await?;
    c.execute(
        r#"UPDATE game.characters SET "stationID" = NULL, "solarSystemID" = $2 WHERE "characterID" = $1"#,
        &[&session.char_id().unwrap_or(0), &system_id],
    )
    .await?;
    Ok(())
}

/// Removes the session's ship from a park and tells the rest of the system.
async fn leave_space(server: &Server, session: &Session, system_id: i32) {
    let Some(ship_id) = session.attrs.int("shipid") else {
        return;
    };
    server
        .space
        .with_park(system_id, |p| p.entries.remove(&ship_id));
    super::combat::forget(server, ship_id).await;
    let ev = space::event(space::stamp_now(), "RemoveBall", vec![Value::from(ship_id)]);
    broadcast_except(server, system_id, Some(session.client_id), vec![ev]);
}

/// A pilot logging off in space: the ship leaves the park; its position is
/// kept for the next login.
pub async fn on_disconnect(server: &Server, session: &Session) {
    let (Some(system), Some(ship), Some(me)) = (
        session.attrs.int("solarsystemid"),
        session.attrs.int("shipid"),
        session.char_id(),
    ) else {
        return;
    };
    let pos = server
        .space
        .with_park(system as i32, |p| p.entries.get(&ship).map(|e| e.ball.pos));
    leave_space(server, session, system as i32).await;
    if let Some(p) = pos {
        db::exec(
            &server.pool,
            r#"UPDATE game.characters SET "spaceX" = $2, "spaceY" = $3, "spaceZ" = $4 WHERE "characterID" = $1"#,
            &[&me, &p[0], &p[1], &p[2]],
        )
        .await
        .ok();
    }
}

fn surface_distance(a: &Entry, b: &Entry) -> f64 {
    space::len(space::sub(a.ball.pos, b.ball.pos)) - a.ball.radius as f64 - b.ball.radius as f64
}

/// (my ship, the other ball) in the current park, both cloned.
fn me_and(server: &Server, session: &Session, other: i64) -> Option<(i32, Entry, Entry)> {
    let system = session.attrs.int("solarsystemid")? as i32;
    let ship = session.attrs.int("shipid")?;
    server
        .space
        .with_park(system, |p| {
            Some((
                p.entries.get(&ship)?.clone(),
                p.entries.get(&other)?.clone(),
            ))
        })
        .map(|(a, b)| (system, a, b))
}

fn too_far() -> CallError {
    user_error(
        "CustomInfo",
        vec![("info", Value::str("You are too far away."))],
    )
}

/// Makes `new_ship` (a ball in this system) the session's ship; the old ship
/// stays behind unpiloted, a capsule is destroyed.
async fn switch_ship(
    server: &Server,
    session: &mut Session,
    system: i32,
    new_ship: i64,
) -> anyhow::Result<()> {
    let me = session.char_id().unwrap_or(0);
    let old = session.attrs.int("shipid");
    let pilot = pilot_of(session);
    let mut events = Vec::new();
    let stamp = space::stamp_now();
    let capsule_type = server.world.type_id("Capsule");
    let old_entry = server.space.with_park(system, |p| {
        let o = old.and_then(|id| p.entries.get_mut(&id));
        let old_entry = o.map(|o| {
            o.char_id = None;
            o.corp_id = None;
            o.client_id = None;
            o.order = Order::Stop;
            o.clone()
        });
        if let Some(n) = p.entries.get_mut(&new_ship) {
            n.char_id = pilot.map(|p| p.char_id);
            n.corp_id = pilot.and_then(|p| p.corp_id);
            n.client_id = pilot.map(|p| p.client_id);
            events.push(space::event(
                stamp,
                "RemoveBall",
                vec![Value::from(new_ship)],
            ));
            events.push(space::add_balls(&[&n.clone()]));
        }
        old_entry
    });
    match old_entry {
        Some(o) if o.type_id == capsule_type => {
            server
                .space
                .with_park(system, |p| p.entries.remove(&o.ball.id));
            events.push(space::event(
                stamp,
                "RemoveBall",
                vec![Value::from(o.ball.id)],
            ));
            if let Some(item) = inventory::item(server, o.ball.id).await? {
                inventory::delete_item(server, &item).await?;
            }
        }
        Some(o) => {
            events.push(space::event(
                stamp,
                "RemoveBall",
                vec![Value::from(o.ball.id)],
            ));
            events.push(space::add_balls(&[&o]));
            place_item(server, o.ball.id, o.ball.pos).await?;
        }
        None => {}
    }
    unplace_item(server, new_ship).await?;
    // gameui moves the ego at once to a ball the park has, or else when the
    // ball's AddBalls arrives (wannaBeEgo).
    broadcast(server, system, events);
    db::exec(
        &server.pool,
        r#"UPDATE game.characters SET "shipID" = $2 WHERE "characterID" = $1"#,
        &[&me, &new_ship],
    )
    .await?;
    session.change(server, &[("shipid", Value::from(new_ship))]);
    Ok(())
}

/// A fresh capsule for the pilot, next to `pos`.
async fn spawn_capsule(
    server: &Server,
    session: &Session,
    system: i32,
    pos: [f64; 3],
) -> anyhow::Result<i64> {
    let me = session.char_id().unwrap_or(0);
    let capsule = inventory::create_item(
        server,
        server.world.type_id("Capsule"),
        me,
        system as i64,
        0,
        1,
        true,
    )
    .await?;
    let at = space::add_scaled(pos, [0.0, 1.0, 0.0], 100.0);
    let entry = object_entry(server, &capsule, at, None).await?;
    add_ball(server, system, entry, None);
    Ok(capsule.item_id)
}

/// The cargo of a ship (or anything) spilled into a new container at `pos`.
async fn spill(
    server: &Server,
    system: i32,
    owner: i32,
    items: Vec<Item>,
    pos: [f64; 3],
) -> anyhow::Result<Option<i64>> {
    if items.is_empty() {
        return Ok(None);
    }
    let can = inventory::create_item(
        server,
        server.world.type_id("CargoContainer"),
        owner,
        system as i64,
        0,
        1,
        true,
    )
    .await?;
    for it in items {
        inventory::move_item(server, &it, can.item_id, server.world.flag("None")).await?;
    }
    place_item(server, can.item_id, pos).await?;
    let entry = object_entry(server, &can, pos, None).await?;
    add_ball(server, system, entry, None);
    Ok(Some(can.item_id))
}

pub async fn spill_items(
    server: &Server,
    system: i32,
    owner: i32,
    items: Vec<Item>,
    pos: [f64; 3],
) -> anyhow::Result<Option<i64>> {
    spill(server, system, owner, items, pos).await
}

/// A pilot's ship destroyed by someone else: its connection runs destroy_ship.
pub async fn destroy_remote(
    server: &Server,
    system: i32,
    ship: i64,
    char_id: i32,
) -> anyhow::Result<()> {
    let clients = server.registry.select(None, |c| c.char_id == Some(char_id));
    match clients.first() {
        Some((_, c)) => {
            c.tx.send(crate::registry::Push::Destroyed { system, ship })
                .ok();
        }
        None => {
            remove_ball(server, system, ship);
        }
    }
    Ok(())
}

/// ship[(locationID, groupID)] methods.
pub async fn ship(server: &Server, session: &mut Session, call: &Call) -> CallResult {
    let a = Args::of(call);
    let me = session.char_id().unwrap_or(0);
    let w = &server.world;
    match call.method() {
        // Board(shipID, asPassenger=0). Docked: any assembled ship of mine in
        // this hangar. In space: an empty ship within reach.
        "Board" => {
            let ship_id = a.int(0).ok_or_else(|| anyhow!("Board without ship"))?;
            let Some(target) = inventory::item(server, ship_id).await? else {
                return Ok(Value::None);
            };
            if target.category(server) != w.category_id("Ship") || target.singleton == 0 {
                return Err(user_error(
                    "CustomInfo",
                    vec![("info", Value::str("Assemble the ship first."))],
                ));
            }
            if let Some(station) = session.attrs.int("stationid") {
                if target.location_id != station || target.owner_id != me {
                    return Err(user_error(
                        "CustomInfo",
                        vec![("info", Value::str("That ship is not in your hangar."))],
                    ));
                }
                db::exec(
                    &server.pool,
                    r#"UPDATE game.characters SET "shipID" = $2 WHERE "characterID" = $1"#,
                    &[&me, &ship_id],
                )
                .await?;
                session.change(server, &[("shipid", Value::from(ship_id))]);
                return Ok(Value::None);
            }
            let (system, mine, other) = me_and(server, session, ship_id).ok_or_else(too_far)?;
            if other.char_id.is_some() {
                return Err(user_error(
                    "CustomInfo",
                    vec![("info", Value::str("That ship already has a pilot."))],
                ));
            }
            if target.owner_id != me
                && target.owner_id != session.attrs.int("corpid").unwrap_or(0) as i32
            {
                return Err(user_error(
                    "CustomInfo",
                    vec![("info", Value::str("That ship is not yours."))],
                ));
            }
            if surface_distance(&mine, &other) > reach(server) {
                return Err(too_far());
            }
            switch_ship(server, session, system, ship_id).await?;
            Ok(Value::None)
        }
        // Eject(): out of the ship. In space a capsule takes over; docked the
        // pilot simply leaves it in the hangar.
        "Eject" | "Evacuate" => {
            if session.attrs.int("stationid").is_some() {
                db::exec(
                    &server.pool,
                    r#"UPDATE game.characters SET "shipID" = NULL WHERE "characterID" = $1"#,
                    &[&me],
                )
                .await?;
                session.change(server, &[("shipid", Value::None)]);
                return Ok(Value::None);
            }
            let system = session.attrs.int("solarsystemid").context("not in space")? as i32;
            let ship = session.attrs.int("shipid").context("no ship")?;
            let (pos, is_capsule) = server
                .space
                .with_park(system, |p| {
                    p.entries
                        .get(&ship)
                        .map(|e| (e.ball.pos, e.type_id == w.type_id("Capsule")))
                })
                .context("ship not in park")?;
            if is_capsule {
                return Ok(Value::None);
            }
            let capsule = spawn_capsule(server, session, system, pos).await?;
            switch_ship(server, session, system, capsule).await?;
            Ok(Value::None)
        }
        // Jettison(itemIDs=None): cargo (all of it by default) into a new container.
        "Jettison" => {
            let system = session.attrs.int("solarsystemid").context("not in space")? as i32;
            let ship = session.attrs.int("shipid").context("no ship")?;
            let ids = a.ints(0);
            let cargo = inventory::items_where(
                server,
                r#""locationID" = $1 AND "flag" = $2"#,
                &[&ship, &w.flag("Cargo")],
            )
            .await?;
            let items: Vec<Item> = cargo
                .into_iter()
                .filter(|i| ids.is_empty() || ids.contains(&i.item_id))
                .collect();
            let pos = server
                .space
                .with_park(system, |p| p.entries.get(&ship).map(|e| e.ball.pos))
                .context("ship not in park")?;
            spill(
                server,
                system,
                me,
                items,
                space::add_scaled(pos, [0.0, 0.0, 1.0], 500.0),
            )
            .await?;
            Ok(Value::None)
        }
        // Scoop(objectID): a container into the cargo hold, contents and all.
        "Scoop" | "ScoopDrone" => {
            let obj = a.int(0).unwrap_or(0);
            let (system, mine, other) = me_and(server, session, obj).ok_or_else(too_far)?;
            if surface_distance(&mine, &other) > reach(server) {
                return Err(too_far());
            }
            let Some(item) = inventory::item(server, obj).await? else {
                return Ok(Value::None);
            };
            let flag = if call.method() == "ScoopDrone" {
                w.flag("DroneBay")
            } else {
                w.flag("Cargo")
            };
            let ship_item = inventory::item(server, mine.ball.id)
                .await?
                .context("ship missing")?;
            let capacity = if flag == w.flag("DroneBay") {
                inventory::type_attribute(server, ship_item.type_id, "droneCapacity")
                    .await?
                    .unwrap_or(0.0)
            } else {
                w.type_info(ship_item.type_id)
                    .map(|t| t.capacity)
                    .unwrap_or(0.0)
            };
            let used: f64 = inventory::items_where(
                server,
                r#""locationID" = $1 AND "flag" = $2"#,
                &[&mine.ball.id, &flag],
            )
            .await?
            .iter()
            .map(|i| i.volume(server))
            .sum();
            if used + item.volume(server) > capacity {
                return Err(user_error(
                    if flag == w.flag("DroneBay") {
                        "NotEnoughDroneBaySpace"
                    } else {
                        "NotEnoughCargoSpace"
                    },
                    vec![],
                ));
            }
            remove_ball(server, system, obj);
            unplace_item(server, obj).await?;
            let item = if item.owner_id != me {
                inventory::set_owner(server, &item, me).await?
            } else {
                item
            };
            inventory::move_item(server, &item, mine.ball.id, flag).await?;
            Ok(Value::None)
        }
        // Drop(itemID): launch a drone from the drone bay.
        "Drop" => {
            let system = session.attrs.int("solarsystemid").context("not in space")? as i32;
            let ship = session.attrs.int("shipid").context("no ship")?;
            let Some(drone) = inventory::item(server, a.int(0).unwrap_or(0))
                .await?
                .filter(|i| i.location_id == ship)
            else {
                return Ok(Value::None);
            };
            let drone = inventory::split_one(server, &drone).await?;
            let pos = server
                .space
                .with_park(system, |p| p.entries.get(&ship).map(|e| e.ball.pos))
                .context("ship not in park")?;
            let at = space::add_scaled(pos, [1.0, 0.0, 0.0], 200.0);
            inventory::move_item(server, &drone, system as i64, w.flag("None")).await?;
            place_item(server, drone.item_id, at).await?;
            let mut entry = object_entry(server, &drone, at, None).await?;
            entry.order = Order::Orbit {
                target: ship,
                range: 500.0,
            };
            add_ball(server, system, entry, None);
            broadcast(
                server,
                system,
                vec![space::event(
                    space::stamp_now(),
                    "Orbit",
                    vec![
                        Value::from(drone.item_id),
                        Value::from(ship),
                        Value::Float(500.0),
                        Value::Int(-1),
                    ],
                )],
            );
            Ok(Value::None)
        }
        // AssembleShip(itemID): a packaged hull becomes a ship.
        "AssembleShip" => {
            let Some(it) = inventory::item(server, a.int(0).unwrap_or(0))
                .await?
                .filter(|i| i.owner_id == me)
            else {
                return Ok(Value::None);
            };
            let it = inventory::split_one(server, &it).await?;
            db::exec(
                &server.pool,
                r#"UPDATE game.items SET "singleton" = 1 WHERE "itemID" = $1"#,
                &[&it.item_id],
            )
            .await?;
            let mut new = it.clone();
            new.singleton = 1;
            inventory::notify_change(server, &new, vec![("ixSingleton", Value::Int(0))]);
            Ok(Value::None)
        }
        // SelfDestruct(shipID): the ship blows up, its cargo floats free and
        // the pilot escapes in a capsule; a capsule's pilot wakes at home.
        "SelfDestruct" => {
            let system = session.attrs.int("solarsystemid").context("not in space")? as i32;
            let ship = session.attrs.int("shipid").context("no ship")?;
            destroy_ship(server, session, system, ship).await?;
            Ok(Value::None)
        }
        // Leaves the station: the session drops stationid and gains
        // solarsystemid; gameui then adds the ballpark and binds beyonce.
        "Undock" => {
            let station_id = session
                .attrs
                .int("stationid")
                .ok_or_else(|| anyhow!("not docked"))? as i32;
            if session.attrs.int("shipid").is_none() {
                return Err(user_error("NeedShipToUndock", vec![]));
            }
            let st = w
                .stations
                .get(&station_id)
                .context("unknown station")?
                .clone();
            // Undock point: just off the station's hull (its ball radius
            // is measured from the model), on the side away from the sun.
            ensure_park(server, st.solar_system_id).await?;
            let radius = server
                .space
                .with_park(st.solar_system_id, |p| {
                    p.entries.get(&(station_id as i64)).map(|e| e.ball.radius as f64)
                })
                .unwrap_or(0.0);
            let pos = space::add_scaled(st.pos, space::norm(st.pos), radius + UNDOCK_CLEARANCE);
            enter_space(server, session, st.solar_system_id, pos).await?;
            session.change(
                server,
                &[
                    ("stationid", Value::None),
                    ("solarsystemid", Value::Int(st.solar_system_id)),
                    ("solarsystemid2", Value::Int(st.solar_system_id)),
                    ("locationid", Value::Int(st.solar_system_id)),
                ],
            );
            if let Some(me) = session.char_id() {
                super::character::record_arrival(server, me, st.solar_system_id, false).await?;
            }
            tracing::info!(char = ?session.char_id(), station_id, "undocked");
            Ok(Value::None)
        }
        m => super::unimplemented("ship", m, call),
    }
}

/// Destroys a piloted ship: cargo spills, the pilot ejects (or, in a capsule,
/// wakes up docked in its home station).
pub async fn destroy_ship(
    server: &Server,
    session: &mut Session,
    system: i32,
    ship: i64,
) -> anyhow::Result<()> {
    let me = session.char_id().unwrap_or(0);
    super::combat::forget(server, ship).await;
    let w = &server.world;
    let entry = server
        .space
        .with_park(system, |p| p.entries.get(&ship).cloned())
        .context("ship not in park")?;
    let contents = inventory::items_where(server, r#""locationID" = $1"#, &[&ship]).await?;
    spill(server, system, me, contents, entry.ball.pos).await?;
    let is_capsule = entry.type_id == w.type_id("Capsule");
    db::exec(
        &server.pool,
        r#"INSERT INTO game.system_events ("solarSystemID", "kind") VALUES ($1, $2)"#,
        &[&system, &(if is_capsule { 9 } else { 26 })],
    )
    .await?;
    if !is_capsule {
        let capsule = spawn_capsule(server, session, system, entry.ball.pos).await?;
        switch_ship(server, session, system, capsule).await?;
        remove_ball(server, system, ship);
        if let Some(item) = inventory::item(server, ship).await? {
            inventory::delete_item(server, &item).await?;
        }
        return Ok(());
    }
    // Podded: a new clone in the home station, in a fresh capsule.
    let home = match super::character::home_station(server, me).await {
        Ok(v) => v.as_int().unwrap_or(0) as i32,
        Err(_) => 0,
    };
    let st = w.stations.get(&home).context("no home station")?.clone();
    leave_space(server, session, system).await;
    if let Some(item) = inventory::item(server, ship).await? {
        inventory::delete_item(server, &item).await?;
    }
    let capsule = inventory::create_item(
        server,
        w.type_id("Capsule"),
        me,
        home as i64,
        w.flag("Hangar"),
        1,
        true,
    )
    .await?;
    db::exec(
        &server.pool,
        r#"UPDATE game.characters SET "shipID" = $2, "stationID" = $3, "solarSystemID" = $4, "constellationID" = $5,
               "regionID" = $6 WHERE "characterID" = $1"#,
        &[&me, &capsule.item_id, &home, &st.solar_system_id, &st.constellation_id, &st.region_id],
    )
    .await?;
    session.change(
        server,
        &[
            ("solarsystemid", Value::None),
            ("shipid", Value::from(capsule.item_id)),
            ("stationid", Value::Int(home)),
            ("locationid", Value::Int(home)),
            ("solarsystemid2", Value::Int(st.solar_system_id)),
            ("constellationid", Value::Int(st.constellation_id)),
            ("regionid", Value::Int(st.region_id)),
        ],
    );
    Ok(())
}

fn ship_id(session: &Session) -> anyhow::Result<i64> {
    session.attrs.int("shipid").context("no ship")
}

/// Applies an order to the session's ship and broadcasts the destiny events.
fn order(
    server: &Server,
    session: &Session,
    system_id: i32,
    order: Order,
    events: Vec<(&str, Vec<Value>)>,
) -> anyhow::Result<()> {
    let ship = ship_id(session)?;
    let stamp = space::stamp_now();
    let mut out = Vec::new();
    server.space.with_park(system_id, |p| {
        if let Some(e) = p.entries.get_mut(&ship) {
            // Any order ends a warp (destiny leaves WARP mode on the spot).
            e.order = order;
            e.warp = None;
            // Every movement order reopens the throttle (speedFraction gates
            // all sublight movement; native WarpTo does the same itself).
            if e.ball.speed_fraction == 0.0 {
                e.ball.speed_fraction = 1.0;
                out.push(space::event(
                    stamp,
                    "SetSpeedFraction",
                    vec![Value::from(ship), Value::Float(1.0)],
                ));
            }
        }
    });
    for (f, a) in events {
        out.push(space::event(stamp, f, a));
    }
    broadcast(server, system_id, out);
    Ok(())
}

/// A bookmark's position in this system: its spot, or its item's ball.
async fn bookmark_position(
    server: &Server,
    session: &Session,
    system_id: i32,
    bookmark_id: i32,
) -> anyhow::Result<Option<[f64; 3]>> {
    let c = server.pool.get().await?;
    let Some(r) = c
        .query_opt(
            r#"SELECT "itemID", "x", "y", "z", "locationID" FROM game.bookmarks WHERE "bookmarkID" = $1 AND "ownerID" = $2"#,
            &[&bookmark_id, &session.char_id().unwrap_or(0)],
        )
        .await?
    else {
        return Ok(None);
    };
    if r.get::<_, Option<i64>>(4) != Some(system_id as i64) {
        return Ok(None);
    }
    if let (Some(x), Some(y), Some(z)) = (
        r.get::<_, Option<f64>>(1),
        r.get::<_, Option<f64>>(2),
        r.get::<_, Option<f64>>(3),
    ) {
        return Ok(Some([x, y, z]));
    }
    let item: Option<i64> = r.get(0);
    Ok(item.and_then(|id| {
        server
            .space
            .with_park(system_id, |p| p.entries.get(&id).map(|e| e.ball.pos))
    }))
}

/// Scan result rows (scanner.py: id, typeID, radius, x, y, z, vx, vy, vz).
fn scan(
    server: &Server,
    session: &Session,
    system_id: i32,
    range: f64,
    cone: Option<([f64; 3], f64)>,
) -> Value {
    let ship = session.attrs.int("shipid").unwrap_or(0);
    let lines = server.space.with_park(system_id, |p| {
        let Some(me) = p.entries.get(&ship).map(|e| e.ball.pos) else {
            return Vec::new();
        };
        p.entries
            .values()
            .filter(|e| e.ball.id != ship)
            .filter(|e| {
                let d = space::sub(e.ball.pos, me);
                let dist = space::len(d);
                dist <= range
                    && match cone {
                        Some((dir, angle)) => {
                            let cos =
                                (d[0] * dir[0] + d[1] * dir[1] + d[2] * dir[2]) / dist.max(1.0);
                            cos >= (angle / 2.0).cos()
                        }
                        None => true,
                    }
            })
            .map(|e| {
                vec![
                    Value::from(e.ball.id),
                    Value::Int(e.type_id),
                    Value::Float(e.ball.radius as f64),
                    Value::Float(e.ball.pos[0]),
                    Value::Float(e.ball.pos[1]),
                    Value::Float(e.ball.pos[2]),
                    Value::Float(e.ball.vel[0]),
                    Value::Float(e.ball.vel[1]),
                    Value::Float(e.ball.vel[2]),
                ]
            })
            .collect()
    });
    rows::rowset(
        &["id", "typeID", "radius", "x", "y", "z", "vx", "vy", "vz"],
        lines,
    )
}

/// beyonce[solarsystemID] methods.
pub async fn beyonce(
    server: &Server,
    session: &mut Session,
    system_id: i32,
    call: &Call,
) -> CallResult {
    let a = Args::of(call);
    let f = |i: usize| a.float(i);
    let ship = ship_id(session)?;
    match call.method() {
        "UpdateStateRequest" => {
            send_state(server, session, system_id).await?;
            Ok(Value::None)
        }
        "GotoDirection" => {
            let d = [
                f(0).unwrap_or(1.0),
                f(1).unwrap_or(0.0),
                f(2).unwrap_or(0.0),
            ];
            order(
                server,
                session,
                system_id,
                Order::Direction(d),
                vec![(
                    "GotoDirection",
                    vec![
                        Value::from(ship),
                        Value::Float(d[0]),
                        Value::Float(d[1]),
                        Value::Float(d[2]),
                    ],
                )],
            )?;
            Ok(Value::None)
        }
        // FollowBall / Orbit: the client's Python mirror (sfxMgr BALL_FollowBall
        // / BALL_Orbit) needs all four args although the native format makes
        // range and effectStamp optional.
        "FollowBall" | "Orbit" => {
            let target = a.int(0).ok_or_else(|| anyhow!("no target"))?;
            let range = f(1).unwrap_or(0.0);
            let o = if call.method() == "Orbit" {
                Order::Orbit { target, range }
            } else {
                Order::Follow { target, range }
            };
            order(
                server,
                session,
                system_id,
                o,
                vec![(
                    call.method(),
                    vec![
                        Value::from(ship),
                        Value::from(target),
                        Value::Float(range),
                        Value::Int(-1),
                    ],
                )],
            )?;
            Ok(Value::None)
        }
        "Stop" => {
            order(
                server,
                session,
                system_id,
                Order::Stop,
                vec![("Stop", vec![Value::from(ship)])],
            )?;
            Ok(Value::None)
        }
        "SetSpeedFraction" => {
            let frac = f(0).unwrap_or(1.0).clamp(0.0, 1.0);
            server.space.with_park(system_id, |p| {
                if let Some(e) = p.entries.get_mut(&ship) {
                    e.ball.speed_fraction = frac as f32;
                }
            });
            broadcast(
                server,
                system_id,
                vec![space::event(
                    space::stamp_now(),
                    "SetSpeedFraction",
                    vec![Value::from(ship), Value::Float(frac)],
                )],
            );
            Ok(Value::None)
        }
        "WarpToItem" => {
            let target = a.int(0).ok_or_else(|| anyhow!("no target"))?;
            let to = server.space.with_park(system_id, |p| -> Option<[f64; 3]> {
                Some(warp_arrival(p.entries.get(&target)?, p.entries.get(&ship)?))
            });
            let to = to.ok_or_else(|| anyhow!("no such target {target}"))?;
            warp_to(server, system_id, ship, to, target)?;
            Ok(Value::None)
        }
        // bookmarks.py WarpTo / Approach on a bookmark.
        "WarpToBookmark" | "GotoBookmark" => {
            let Some(to) =
                bookmark_position(server, session, system_id, a.i32(0).unwrap_or(0)).await?
            else {
                return Err(user_error(
                    "CustomInfo",
                    vec![(
                        "info",
                        Value::str("That bookmark is not in this solar system."),
                    )],
                ));
            };
            if call.method() == "WarpToBookmark" {
                warp_to(server, system_id, ship, to, 0)?;
            } else {
                order(
                    server,
                    session,
                    system_id,
                    Order::Point(to),
                    vec![(
                        "GotoPoint",
                        vec![
                            Value::from(ship),
                            Value::Float(to[0]),
                            Value::Float(to[1]),
                            Value::Float(to[2]),
                        ],
                    )],
                )?;
            }
            Ok(Value::None)
        }
        // GangWarp(itemID): every gang member here warps to it with me.
        "GangWarp" | "GangRegroup" => {
            let Some(gang) = session.attrs.int("gangid") else {
                return Ok(Value::None);
            };
            let target = if call.method() == "GangWarp" {
                a.int(0).unwrap_or(ship)
            } else {
                ship
            };
            let members = server.gangs.members(gang);
            let ships: Vec<(i64, [f64; 3])> = server.space.with_park(system_id, |p| {
                let Some(t) = p.entries.get(&target) else {
                    return Vec::new();
                };
                p.entries
                    .values()
                    .filter(|e| {
                        e.char_id.is_some_and(|c| members.contains(&c)) && e.ball.id != target
                    })
                    .map(|e| (e.ball.id, warp_arrival(t, e)))
                    .collect()
            });
            for (s, to) in ships {
                warp_to(server, system_id, s, to, target).ok();
            }
            Ok(Value::None)
        }
        // ConeScan(angle, range, dx, dy, dz) / SphereScan(range)
        "ConeScan" => {
            let dir = space::norm([
                f(2).unwrap_or(0.0),
                f(3).unwrap_or(0.0),
                f(4).unwrap_or(1.0),
            ]);
            Ok(scan(
                server,
                session,
                system_id,
                f(1).unwrap_or(0.0),
                Some((dir, f(0).unwrap_or(1.0))),
            ))
        }
        "SphereScan" => Ok(scan(server, session, system_id, f(0).unwrap_or(0.0), None)),
        // michelle.GetRelativity (a debug comparison of client and server
        // positions): (diff, x, y, z, sTime, tTime, rTime).
        "GetRelativity" => {
            let pos = server
                .space
                .with_park(system_id, |p| p.entries.get(&ship).map(|e| e.ball.pos))
                .unwrap_or([0.0; 3]);
            let mine = [
                f(0).unwrap_or(0.0),
                f(1).unwrap_or(0.0),
                f(2).unwrap_or(0.0),
            ];
            let now = crate::statics::blue_time_now();
            Ok(Value::tuple([
                Value::Float(space::len(space::sub(pos, mine))),
                Value::Float(pos[0]),
                Value::Float(pos[1]),
                Value::Float(pos[2]),
                Value::Int(space::stamp_now()),
                Value::Long(now),
                Value::Long(now),
            ]))
        }
        "Dock" | "TurboDock" => {
            let station = a.i32(0).ok_or_else(|| anyhow!("no station"))?;
            dock(server, session, system_id, station).await
        }
        "StargateJump" => {
            let gate = a.int(0).ok_or_else(|| anyhow!("no gate"))?;
            jump(server, session, system_id, gate).await
        }
        m => super::unimplemented("beyonce", m, call),
    }
}

/// Sends a fresh SetState to this client (bind and UpdateStateRequest). A
/// pilot who logged off in space is put back where it left.
pub async fn send_state(
    server: &Server,
    session: &mut Session,
    system_id: i32,
) -> anyhow::Result<()> {
    ensure_park(server, system_id).await?;
    let ego = ship_id(session)?;
    if !server
        .space
        .with_park(system_id, |p| p.entries.contains_key(&ego))
    {
        let me = session.char_id().unwrap_or(0);
        let c = server.pool.get().await?;
        let saved = c
            .query_opt(r#"SELECT "spaceX", "spaceY", "spaceZ" FROM game.characters WHERE "characterID" = $1"#, &[&me])
            .await?
            .and_then(|r| Some([r.get::<_, Option<f64>>(0)?, r.get::<_, Option<f64>>(1)?, r.get::<_, Option<f64>>(2)?]));
        drop(c);
        let pos = saved.unwrap_or([0.0, 0.0, 1.0e10]);
        if let Some(ship) = inventory::item(server, ego).await? {
            let entry = object_entry(server, &ship, pos, pilot_of(session)).await?;
            add_ball(server, system_id, entry, Some(session.client_id));
        }
    }
    let ids: Vec<i64> = server
        .space
        .with_park(system_id, |p| p.entries.keys().copied().collect());
    let damage = super::combat::damage_states(server, &ids);
    let state = server.space.with_park(system_id, |p| {
        p.set_state(ego, sol_item(server, system_id), damage)
    });
    // The park only exists client-side once Bind returns, so SetState must
    // follow the bind's response.
    session.after.push(crate::conn::event(
        server.config.node_id,
        session.client_id,
        "DoDestinyUpdate",
        vec![Value::List(vec![state])],
    ));
    Ok(())
}

/// Warps `ship` so that it comes to rest at `dest`. Destiny on every client
/// runs the warp itself from the WarpTo event: it aligns, engages, follows
/// the warp curve and drops out WARP_DROP_SHORT before the WarpTo point,
/// carrying on at full speed. The server sends the point that makes the
/// drop-out land on `dest`, simulates the same warp, and stops the ship at
/// `dest` once it has dropped out (see `simulate`).
fn warp_to(
    server: &Server,
    system_id: i32,
    ship: i64,
    dest: [f64; 3],
    target: i64,
) -> Result<(), CallError> {
    let min = server.world.constant("minWarpDistance") as f64;
    let stamp = space::stamp_now();
    let events = server
        .space
        .with_park(system_id, |p| -> anyhow::Result<Option<Vec<Value>>> {
            let e = p
                .entries
                .get_mut(&ship)
                .ok_or_else(|| anyhow!("ship not in park"))?;
            let point = space::warp_point(e.ball.pos, dest);
            if space::len(space::sub(dest, e.ball.pos)) < min
                || space::len(space::sub(point, e.ball.pos)) < space::MIN_NATIVE_WARP
            {
                return Ok(None);
            }
            let mut events = Vec::new();
            // A closed throttle would keep the ship from ever aligning; the
            // native WarpTo, unlike GotoPoint, does not reopen it.
            if e.ball.speed_fraction == 0.0 {
                e.ball.speed_fraction = 1.0;
                events.push(space::event(
                    stamp,
                    "SetSpeedFraction",
                    vec![Value::from(ship), Value::Float(1.0)],
                ));
            }
            e.order = Order::Stop;
            e.warp = Some(Warp {
                point,
                dest,
                target,
                engaged: None,
            });
            events.push(space::event(
                stamp,
                "WarpTo",
                vec![
                    Value::from(ship),
                    Value::Float(point[0]),
                    Value::Float(point[1]),
                    Value::Float(point[2]),
                ],
            ));
            Ok(Some(events))
        })?;
    let Some(events) = events else {
        return Err(user_error(
            "CustomInfo",
            vec![("info", Value::str("That is too close to warp to."))],
        ));
    };
    broadcast(server, system_id, events);
    tracing::info!(ship, target, ?dest, "warp ordered");
    Ok(())
}

/// What a warp's engaging or ending tells the system.
fn warp_events(ship: i64, step: space::WarpStep, warp: &Warp) -> Vec<Value> {
    let stamp = space::stamp_now();
    match step {
        // The warp tunnel and "Warp drive active" (spaceMgr.OnSpecialFX);
        // the effect ends itself once the ship leaves WARP mode
        // (effects.Warping.Wait) and releases itself on the client.
        space::WarpStep::Engaged => vec![space::event(
            stamp,
            "OnSpecialFX",
            vec![
                Value::from(ship),
                Value::None,
                if warp.target != 0 {
                    Value::from(warp.target)
                } else {
                    Value::None
                },
                Value::None,
                Value::str("effects.Warping"),
                Value::Int(0),
                Value::Int(1),
                Value::Int(1),
                Value::Int(-1),
                Value::None,
            ],
        )],
        // Destiny dropped the ship out into GotoDirection at full speed; put
        // it at rest where the warp was meant to end.
        space::WarpStep::Exited => {
            let [x, y, z] = warp.dest.map(Value::Float);
            vec![
                space::event(stamp, "Stop", vec![Value::from(ship)]),
                space::event(stamp, "SetBallPosition", vec![Value::from(ship), x, y, z]),
                space::event(
                    stamp,
                    "SetBallVelocity",
                    vec![
                        Value::from(ship),
                        Value::Float(0.0),
                        Value::Float(0.0),
                        Value::Float(0.0),
                    ],
                ),
            ]
        }
    }
}

async fn dock(
    server: &Server,
    session: &mut Session,
    system_id: i32,
    station_id: i32,
) -> CallResult {
    let ship = ship_id(session)?;
    let st = server
        .world
        .stations
        .get(&station_id)
        .context("unknown station")?
        .clone();
    let max = server.world.constant("maxDockingDistance") as f64;
    let ok = server.space.with_park(system_id, |p| {
        match (p.entries.get(&ship), p.entries.get(&(station_id as i64))) {
            (Some(s), Some(t)) => surface_distance(s, t) <= max,
            _ => false,
        }
    });
    if !ok {
        return Err(user_error(
            "CustomInfo",
            vec![("info", Value::str("You are too far away to dock."))],
        ));
    }
    let pos = |p: [f64; 3]| Value::tuple(p.map(Value::Float));
    session.event(
        server,
        "OnDockingAccepted",
        vec![pos(st.pos), pos(st.pos), Value::Int(station_id)],
    );
    leave_space(server, session, system_id).await;
    let c = server.pool.get().await?;
    c.execute(
        r#"UPDATE game.items SET "locationID" = $2, "flag" = $3 WHERE "itemID" = $1"#,
        &[&ship, &(station_id as i64), &server.world.flag("Hangar")],
    )
    .await?;
    c.execute(
        r#"UPDATE game.characters SET "stationID" = $2, "solarSystemID" = $3 WHERE "characterID" = $1"#,
        &[&session.char_id().unwrap_or(0), &station_id, &st.solar_system_id],
    )
    .await?;
    session.change(
        server,
        &[
            ("solarsystemid", Value::None),
            ("stationid", Value::Int(station_id)),
            ("locationid", Value::Int(station_id)),
        ],
    );
    tracing::info!(char = ?session.char_id(), station_id, "docked");
    Ok(Value::None)
}

async fn jump(server: &Server, session: &mut Session, system_id: i32, gate: i64) -> CallResult {
    let ship = ship_id(session)?;
    let max = server.world.constant("maxStargateJumpingDistance") as f64;
    let (dest_sys, dest_gate, close) = server.space.with_park(system_id, |p| {
        let g = p.entries.get(&gate).cloned();
        let s = p.entries.get(&ship).cloned();
        match (g, s) {
            (Some(g), Some(s)) => match g.jump {
                Some((sys, dg)) => Ok((sys, dg, surface_distance(&s, &g) <= max)),
                None => Err(anyhow!("{gate} is not a stargate")),
            },
            _ => Err(anyhow!("gate or ship missing")),
        }
    })?;
    if !close {
        return Err(user_error(
            "CustomInfo",
            vec![("info", Value::str("You are too far away to jump."))],
        ));
    }
    ensure_park(server, dest_sys).await?;
    let dest_pos = server
        .space
        .with_park(dest_sys, |p| {
            p.entries
                .get(&dest_gate)
                .map(|g| (g.ball.pos, g.ball.radius as f64))
        })
        .context("destination gate missing")?;
    let arrive = space::add_scaled(
        dest_pos.0,
        space::norm(dest_pos.0),
        dest_pos.1 + space::WARP_ARRIVAL_MARGIN,
    );
    leave_space(server, session, system_id).await;
    let sys = server
        .world
        .systems
        .get(&dest_sys)
        .context("unknown destination")?
        .clone();
    // One change from the old system to the new: gameui's 'Jumping' path
    // fades out, drops the old park and binds beyonce for the new system.
    enter_space(server, session, dest_sys, arrive).await?;
    session.change(
        server,
        &[
            ("solarsystemid", Value::Int(dest_sys)),
            ("solarsystemid2", Value::Int(dest_sys)),
            ("constellationid", Value::Int(sys.constellation_id)),
            ("regionid", Value::Int(sys.region_id)),
            ("locationid", Value::Int(dest_sys)),
        ],
    );
    if let Some(me) = session.char_id() {
        super::character::record_arrival(server, me, dest_sys, true).await?;
    }
    tracing::info!(char = ?session.char_id(), from = system_id, to = dest_sys, "jumped");
    Ok(Value::None)
}

/// aggressionMgr[solarsystemID].ConsiderObject(itemID): what attacking it
/// would cost (consider.py reads 'as', 'ac', 'af', 'ds').
pub async fn aggression(
    server: &Server,
    session: &mut Session,
    bound_system: i32,
    call: &Call,
) -> CallResult {
    match call.method() {
        "ConsiderObject" => {
            let target = Args::of(call).int(0).unwrap_or(0);
            let security = server
                .world
                .systems
                .get(&bound_system)
                .map(|s| s.security)
                .unwrap_or(0.0);
            let entry = server
                .space
                .with_park(bound_system, |p| p.entries.get(&target).cloned());
            let mut d = Vec::new();
            if let Some(e) = entry {
                let corp = session.attrs.int("corpid").unwrap_or(0) as i32;
                let players = e.char_id.is_some() && e.corp_id != Some(corp);
                if players && security >= 0.5 {
                    d.push((
                        Value::str("as"),
                        Value::str("CONCORD will respond to unprovoked aggression in this system."),
                    ));
                    d.push((
                        Value::str("ds"),
                        Value::str("Your security status will drop."),
                    ));
                } else if e.owner_id < 100_000_000 && e.owner_id > 1 {
                    d.push((Value::str("ac"), Value::Int(e.owner_id)));
                    if let Some(f) = server
                        .world
                        .systems
                        .get(&bound_system)
                        .and_then(|s| s.faction_id)
                    {
                        d.push((Value::str("af"), Value::Int(f)));
                    }
                }
            }
            Ok(Value::Dict(d))
        }
        m => super::unimplemented("aggressionMgr", m, call),
    }
}

/// Background loop, once per tick: sends the events queued since the last
/// tick (stamped with the tick they precede, as michelle applies them before
/// evolving that tick), then simulates the tick in every park.
pub async fn simulate(server: std::sync::Arc<Server>) {
    space::start_clock();
    let mut interval =
        tokio::time::interval(std::time::Duration::from_millis(space::TICK_MS as u64));
    loop {
        interval.tick().await;
        flush(&server, space::stamp_now());
        for system in server.space.systems() {
            let steps = server.space.with_park(system, |p| p.step());
            for (ship, step, warp) in steps {
                if step == space::WarpStep::Exited {
                    server.space.with_park(system, |p| {
                        if let Some(e) = p.entries.get_mut(&ship) {
                            e.ball.pos = warp.dest;
                            e.ball.vel = [0.0; 3];
                            e.order = Order::Stop;
                        }
                    });
                }
                tracing::debug!(ship, ?step, "warp");
                broadcast(&server, system, warp_events(ship, step, &warp));
            }
        }
        space::advance_clock();
        super::combat::tick(&server).await;
    }
}
