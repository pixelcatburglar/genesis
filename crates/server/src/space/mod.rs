//! Solar-system ballparks (the server half of remote/michelle.py's Park).
//!
//! The client runs its own destiny ballpark and only learns state through
//! DoDestinyUpdate notifications: a list of (stamp, (funcName, args)) events
//! that michelle applies as SetState / AddBalls / RemoveBall(s) or as
//! `_parent_<funcName>(*args)` on the native ballpark. Stamps count
//! simulation ticks (const.simulationTimeStep ms each); the client's park
//! takes SetState's stamp as its current time and advances one per tick.

pub mod ballimg;

use std::collections::HashMap;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Mutex;

use marshal::Value;

use ballimg::{Ball, Mode};

/// Destiny's tick length in ms (const.simulationTimeStep, gen_seed.py).
pub const TICK_MS: i64 = 1000;
const DT: f64 = TICK_MS as f64 / 1000.0;
/// Ticks are counted from 2000-01-01 so stamps stay well inside an int32.
const EPOCH_UNIX_MS: i64 = 946_684_800_000;
pub const AU: f64 = 149_597_870_700.0;
/// Land this far outside the target's surface: inside the 2500 m dock/jump
/// range (const.maxDockingDistance) and well inside minWarpDistance.
pub const WARP_ARRIVAL_MARGIN: f64 = 1500.0;
/// Ballpark.friction and Ballpark.warpSpeed (AU/s) as destiny's Ballpark
/// constructor sets them (destiny.dll 0x100109f0); michelle changes neither.
pub const FRICTION: f64 = 1.0e6;
pub const WARP_SPEED_AU: f64 = 2.0;
/// When a warp engages, destiny moves the destination this far back towards
/// the ship (0x1000c140): a warp drops out short of the WarpTo point.
pub const WARP_DROP_SHORT: f64 = 20000.0;
/// Destiny's WarpTo only warps to points at least this far away (it compares
/// the squared distance with 1e10, 0x10013590); closer ones become GotoPoint.
pub const MIN_NATIVE_WARP: f64 = 100_000.0;
/// A warping ship engages once its heading is this aligned with the warp
/// point (1 - cos, 0x10013de1 case 3).
const ALIGN_TOLERANCE: f64 = 0.01;
/// GotoDirection aims at a point this far along the direction (0x1000f9b0).
const DIRECTION_REACH: f64 = 1.0e17;

/// The last tick the server simulated. Clients' parks count ticks on their
/// own clocks from SetState's stamp, so the server only has to keep this
/// advancing once per tick.
static TICK: AtomicI32 = AtomicI32::new(0);

/// Starts the tick count at wall-clock ticks since 2000 (any start works;
/// this one keeps stamps readable).
pub fn start_clock() {
    let unix_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    TICK.store(((unix_ms - EPOCH_UNIX_MS) / TICK_MS) as i32, Ordering::SeqCst);
}

/// The tick the next simulation step runs at: what every event created now
/// is stamped with (michelle applies an event just before it evolves the
/// tick the event is stamped with).
pub fn stamp_now() -> i32 {
    TICK.load(Ordering::SeqCst) + 1
}

/// Marks tick `stamp_now()` as simulated.
pub fn advance_clock() {
    TICK.fetch_add(1, Ordering::SeqCst);
}

/// Rewrites a (stamp, (funcName, args)) event's stamp.
pub fn restamp(event: &Value, stamp: i32) -> Value {
    match event {
        Value::Tuple(items) if items.len() == 2 => {
            Value::tuple([Value::Int(stamp), items[1].clone()])
        }
        other => other.clone(),
    }
}

/// A celestial or ship in a park, with what its slim item needs.
#[derive(Debug, Clone)]
pub struct Entry {
    pub ball: Ball,
    pub type_id: i32,
    pub group_id: i32,
    pub category_id: i32,
    pub owner_id: i32,
    pub char_id: Option<i32>,
    pub corp_id: Option<i32>,
    /// Stargates: (destination system, destination gate).
    pub jump: Option<(i32, i64)>,
    pub name: String,
    /// Connection steering this ship.
    pub client_id: Option<i64>,
    /// Movement order, kept so the server can integrate the ship's position.
    pub order: Order,
    /// Warp in progress (streamed by the server; the client does not integrate warp).
    pub warp: Option<Warp>,
    /// Fitted modules (moduleID, typeID): ship.py FitHardpoints mounts a
    /// turret pair per weapon or mining laser.
    pub modules: Vec<(i64, i32)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Order {
    Stop,
    Direction([f64; 3]),
    Point([f64; 3]),
    Follow { target: i64, range: f64 },
    Orbit { target: i64, range: f64 },
}

/// A warp in progress, simulated exactly as destiny does it on every client
/// (the server never streams positions; it only has to agree with them).
#[derive(Debug, Clone)]
pub struct Warp {
    /// The point sent in WarpTo.
    pub point: [f64; 3],
    /// Where the ship is put when it drops out of warp.
    pub dest: [f64; 3],
    /// The ball warped to (0 for a bookmark): the warp indicator's target.
    pub target: i64,
    /// Set once the ship is aligned and the warp drive engages.
    pub engaged: Option<Engaged>,
}

#[derive(Debug, Clone)]
pub struct Engaged {
    /// effectStamp: the tick the drive engaged.
    pub start: i32,
    /// The destination after the drop-short (gotoX/Y/Z while warping).
    pub goto: [f64; 3],
    /// followRange: distance from the engage point to `goto`.
    pub distance: f64,
}

/// What a simulation step did to a warp.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WarpStep {
    Engaged,
    /// Dropped out: destiny switched the ship to GotoDirection at full speed.
    Exited,
}

#[derive(Debug, Default)]
pub struct Park {
    pub system_id: i32,
    pub entries: HashMap<i64, Entry>,
    pub loaded: bool,
    /// The last tick simulated in this park.
    pub tick: i32,
}

/// Destiny events waiting for the next tick: (system, connection that must
/// not get it, event).
type Outbox = Vec<(i32, Option<i64>, Value)>;

#[derive(Default)]
pub struct Space {
    parks: Mutex<HashMap<i32, Park>>,
    outbox: Mutex<Outbox>,
}

impl Space {
    pub fn with_park<R>(&self, system_id: i32, f: impl FnOnce(&mut Park) -> R) -> R {
        let mut parks = self.parks.lock().unwrap();
        let park = parks.entry(system_id).or_insert_with(|| Park {
            system_id,
            tick: stamp_now() - 1,
            ..Park::default()
        });
        f(park)
    }

    pub fn systems(&self) -> Vec<i32> {
        self.parks.lock().unwrap().keys().copied().collect()
    }

    /// Queues events for everyone in a system. michelle applies one
    /// DoDestinyUpdate per tick and cannot rewind past the last one it
    /// applied, so all of a tick's events must travel in one packet.
    pub fn queue(&self, system_id: i32, except: Option<i64>, events: Vec<Value>) {
        let mut out = self.outbox.lock().unwrap();
        out.extend(events.into_iter().map(|e| (system_id, except, e)));
    }

    pub fn take_outbox(&self) -> Outbox {
        std::mem::take(&mut *self.outbox.lock().unwrap())
    }
}

pub fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub fn len(a: [f64; 3]) -> f64 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}

pub fn norm(a: [f64; 3]) -> [f64; 3] {
    let l = len(a);
    if l == 0.0 {
        [1.0, 0.0, 0.0]
    } else {
        [a[0] / l, a[1] / l, a[2] / l]
    }
}

pub fn add_scaled(a: [f64; 3], d: [f64; 3], s: f64) -> [f64; 3] {
    [a[0] + d[0] * s, a[1] + d[1] * s, a[2] + d[2] * s]
}

/// Destiny's warp curve (destiny.dll 0x1000ff30) for a warp engaged
/// `distance` short of its destination, `t` seconds after engaging:
/// (remaining distance, speed, closing in). The ship accelerates
/// exponentially from 3 m/s at 3 s^-1 to the peak speed
/// min(1.5 * distance, warpSpeed AU/s), cruises, then closes in with the
/// remaining distance decaying at 1 s^-1 (speed = remaining distance). It
/// drops out on the first closing-in tick slower than its maxVelocity.
pub fn warp_curve(distance: f64, t: f64) -> (f64, f64, bool) {
    let peak = (1.5 * distance).min(WARP_SPEED_AU * AU);
    let third = peak / 3.0;
    let t1 = third.ln() / 3.0;
    let t2 = (third.ln() - 2.0) / 3.0 + distance / peak;
    if t < t1 {
        let e = (3.0 * t).exp();
        (distance - e, 3.0 * e, false)
    } else if t < t2 {
        (distance - third - peak * (t - t1), peak, false)
    } else {
        let r = third * (t2 - t).exp();
        (r, r, true)
    }
}

/// The WarpTo point that makes destiny's drop-short land a ship warping from
/// `from` at `dest`.
pub fn warp_point(from: [f64; 3], dest: [f64; 3]) -> [f64; 3] {
    add_scaled(dest, norm(sub(from, dest)), -WARP_DROP_SHORT)
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Destiny's thrust towards `goal` (0x1000bd80): maxVelocity * speedFraction
/// * friction / (agility * mass), scaled by (d / r0)^4 when braking within
/// r0 = one tick of travel plus the ball's radius.
fn thrust(b: &Ball, goal: [f64; 3], brake: bool) -> [f64; 3] {
    let m = (b.agility as f64 * b.mass as f64).max(1e-9);
    let speed = b.max_velocity as f64 * b.speed_fraction as f64;
    let mut a = speed * FRICTION / m;
    let d = sub(goal, b.pos);
    let d2 = dot(d, d);
    let r0 = speed * DT + b.radius as f64;
    if brake && d2 < r0 * r0 {
        let f = d2 / (r0 * r0);
        a *= f * f;
    }
    let n = if d2 > 0.0 { norm(d) } else { [0.0; 3] };
    [n[0] * a, n[1] * a, n[2] * a]
}

impl Entry {
    /// foo.SlimItem: its __dict__ (remote/slimItem.py). Stargates list their
    /// destinations as rows of (locationID, toCelestialID).
    pub fn slim(&self) -> Value {
        let jumps = match self.jump {
            Some((sys, gate)) => vec![crate::rows::row(
                &["locationID", "toCelestialID"],
                vec![Value::Int(sys), Value::from(gate)],
            )],
            None => vec![],
        };
        Value::instance(
            "foo.SlimItem",
            Value::dict([
                (Value::str("itemID"), Value::from(self.ball.id)),
                (Value::str("ballID"), Value::from(self.ball.id)),
                (
                    Value::str("charID"),
                    Value::from(self.char_id.map(i64::from)),
                ),
                (Value::str("ownerID"), Value::Int(self.owner_id)),
                (Value::str("groupID"), Value::Int(self.group_id)),
                (Value::str("categoryID"), Value::Int(self.category_id)),
                (Value::str("typeID"), Value::Int(self.type_id)),
                (
                    Value::str("modules"),
                    Value::List(
                        self.modules
                            .iter()
                            .map(|(m, t)| Value::tuple([Value::from(*m), Value::Int(*t)]))
                            .collect(),
                    ),
                ),
                (Value::str("jumps"), Value::List(jumps)),
                (Value::str("securityStatus"), Value::None),
                (
                    Value::str("corpID"),
                    Value::from(self.corp_id.map(i64::from)),
                ),
                (Value::str("name"), crate::db::text_value(&self.name)),
            ]),
        )
    }

    pub fn image(&self) -> Value {
        Value::BlueObject(ballimg::image(
            &self.ball,
            crate::statics::blue_time_now() as u64,
        ))
    }

    /// Brings the ball's destiny mode fields in line with its order, so that
    /// SetState and AddBalls hand clients a ball already doing it.
    fn sync_mode(&mut self, others: &HashMap<i64, ([f64; 3], f32)>) {
        let b = &mut self.ball;
        if !b.is_free {
            return;
        }
        b.effect_stamp = -1;
        b.follow_id = 0;
        b.follow_range = 0.0;
        if let Some(w) = &self.warp {
            b.mode = Mode::Warp;
            match &w.engaged {
                Some(g) => {
                    b.goto = g.goto;
                    b.follow_range = g.distance;
                    b.effect_stamp = g.start;
                }
                None => b.goto = w.point,
            }
            return;
        }
        match &self.order {
            Order::Stop => b.mode = Mode::Stop,
            Order::Direction(d) => {
                b.mode = Mode::Goto;
                b.goto = add_scaled(b.pos, norm(*d), DIRECTION_REACH);
            }
            Order::Point(p) => {
                b.mode = Mode::Goto;
                b.goto = *p;
            }
            Order::Follow { target, range } | Order::Orbit { target, range } => {
                b.mode = if matches!(self.order, Order::Orbit { .. }) {
                    Mode::Orbit
                } else {
                    Mode::Follow
                };
                b.follow_id = *target;
                b.follow_range = *range;
                if let Some((p, _)) = others.get(target) {
                    b.goto = *p;
                }
            }
        }
    }

    /// One destiny tick (destiny.dll 0x10013c50) for a free ball. The mode
    /// gives a thrust and v' = (v + dt * a) * m / (m + friction * dt) with
    /// m = agility * mass, then x' = x + dt * v'. A warping ball aligns
    /// (GotoPoint towards the warp point) until its heading is within
    /// ALIGN_TOLERANCE, then follows the warp curve from this tick on.
    pub fn step(&mut self, tick: i32, others: &HashMap<i64, ([f64; 3], f32)>) -> Option<WarpStep> {
        if !self.ball.is_free {
            return None;
        }
        let mut happened = None;
        if let Some(w) = self.warp.as_mut() {
            let b = &self.ball;
            if w.engaged.is_none() {
                let to = norm(sub(w.point, b.pos));
                let heading = if dot(b.vel, b.vel) > 0.0 { norm(b.vel) } else { [0.0; 3] };
                if (1.0 - dot(to, heading)).abs() < ALIGN_TOLERANCE {
                    let goto = add_scaled(w.point, norm(sub(b.pos, w.point)), WARP_DROP_SHORT);
                    w.engaged = Some(Engaged {
                        start: tick,
                        goto,
                        distance: len(sub(goto, b.pos)),
                    });
                    happened = Some(WarpStep::Engaged);
                }
            }
            if let Some(g) = w.engaged.clone() {
                let (remaining, speed, closing) = warp_curve(g.distance, (tick - g.start) as f64 * DT);
                let b = &mut self.ball;
                let n = norm(sub(b.pos, g.goto));
                b.pos = add_scaled(g.goto, n, remaining);
                let v = if closing && speed < b.max_velocity as f64 {
                    // Destiny drops out into GotoDirection along the warp.
                    self.order = Order::Direction([-n[0], -n[1], -n[2]]);
                    self.warp = None;
                    happened = Some(WarpStep::Exited);
                    b.max_velocity as f64
                } else {
                    speed
                };
                b.vel = [-n[0] * v, -n[1] * v, -n[2] * v];
                return happened;
            }
        }
        let b = &self.ball;
        let force = match (&self.warp, &self.order) {
            (Some(w), _) => thrust(b, w.point, true),
            (None, Order::Stop) => [0.0; 3],
            (None, Order::Direction(d)) => thrust(b, add_scaled(b.pos, norm(*d), DIRECTION_REACH), false),
            (None, Order::Point(p)) => thrust(b, *p, true),
            // FOLLOW aims at the target's surface plus the range; ORBIT is
            // approximated the same way (only clients draw the circle).
            (None, Order::Follow { target, range }) | (None, Order::Orbit { target, range }) => {
                match others.get(target) {
                    Some((t, r)) => {
                        let away = sub(b.pos, *t);
                        let dir = if dot(away, away) > 0.0 { norm(away) } else { [1.0, 0.0, 0.0] };
                        thrust(b, add_scaled(*t, dir, *r as f64 + b.radius as f64 + range), true)
                    }
                    None => [0.0; 3],
                }
            }
        };
        let b = &mut self.ball;
        let m = (b.agility as f64 * b.mass as f64).max(1e-9);
        let keep = m / (m + FRICTION * DT);
        for i in 0..3 {
            b.vel[i] = (b.vel[i] + DT * force[i]) * keep;
            b.pos[i] += DT * b.vel[i];
        }
        if self.order == Order::Stop && self.warp.is_none() && dot(b.vel, b.vel) < 0.1 {
            b.vel = [0.0; 3];
        }
        happened
    }
}

impl Park {
    /// Every ball's (position, radius).
    pub fn positions(&self) -> HashMap<i64, ([f64; 3], f32)> {
        self.entries
            .iter()
            .map(|(id, e)| (*id, (e.ball.pos, e.ball.radius)))
            .collect()
    }

    /// Simulates the next tick; returns the warps that engaged or ended,
    /// with the warp as it was before the step.
    pub fn step(&mut self) -> Vec<(i64, WarpStep, Warp)> {
        self.tick += 1;
        let tick = self.tick;
        let positions = self.positions();
        let mut out = Vec::new();
        for e in self.entries.values_mut() {
            let before = e.warp.clone();
            if let (Some(s), Some(w)) = (e.step(tick, &positions), before) {
                out.push((e.ball.id, s, w));
            }
        }
        out
    }

    /// A ball with its destiny mode fields brought up to date.
    pub fn synced(&self, id: i64) -> Option<Entry> {
        let mut e = self.entries.get(&id)?.clone();
        e.sync_mode(&self.positions());
        Some(e)
    }

    /// The SetState event for a client whose ego ball is `ego`. `damage`
    /// holds the known damage states (michelle.GetDamageState).
    pub fn set_state(&self, ego: i64, sol_item: Value, damage_known: Vec<(Value, Value)>) -> Value {
        let mut ids: Vec<&i64> = self.entries.keys().collect();
        ids.sort();
        let synced: Vec<Entry> = ids.iter().filter_map(|id| self.synced(**id)).collect();
        let balls = synced.iter().map(|e| e.image()).collect();
        let slims = synced.iter().map(|e| e.slim()).collect();
        // michelle.GetDamageState: (shield as (fraction, tau), armor, hull).
        let mut damage = damage_known;
        for id in &ids {
            let e = &self.entries[id];
            let known = damage.iter().any(|(k, _)| k.as_int() == Some(**id));
            if !known && (e.category_id == 6 || e.client_id.is_some()) {
                damage.push((
                    Value::from(**id),
                    Value::tuple([
                        Value::tuple([Value::Float(1.0), Value::Float(100000.0)]),
                        Value::Float(1.0),
                        Value::Float(1.0),
                    ]),
                ));
            }
        }
        // The client's park starts at this tick and evolves it next, as the
        // server does.
        let stamp = self.tick + 1;
        event(
            stamp,
            "SetState",
            vec![
                Value::List(balls),
                Value::List(slims),
                Value::Int(stamp),
                Value::from(ego),
                sol_item,
                Value::Dict(damage),
            ],
        )
    }
}

/// One (stamp, (funcName, args)) destiny event.
pub fn event(stamp: i32, func: &str, args: Vec<Value>) -> Value {
    Value::tuple([
        Value::Int(stamp),
        Value::tuple([Value::str(func), Value::Tuple(args)]),
    ])
}

/// AddBalls chunk: ((ball, slimItem) list, damage dict).
pub fn add_balls(entries: &[&Entry]) -> Value {
    let state = entries
        .iter()
        .map(|e| {
            let mut e = (*e).clone();
            e.sync_mode(&HashMap::new());
            Value::tuple([e.image(), e.slim()])
        })
        .collect();
    event(
        stamp_now(),
        "AddBalls",
        vec![Value::tuple([Value::List(state), Value::Dict(vec![])])],
    )
}

#[allow(dead_code)]
pub fn mode_name(m: Mode) -> &'static str {
    match m {
        Mode::Goto => "GOTO",
        Mode::Follow => "FOLLOW",
        Mode::Stop => "STOP",
        Mode::Warp => "WARP",
        Mode::Orbit => "ORBIT",
        _ => "OTHER",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ship(pos: [f64; 3]) -> Entry {
        Entry {
            ball: Ball {
                id: 1,
                mass: 1.0e6,
                radius: 40.0,
                max_velocity: 300.0,
                is_free: true,
                is_global: false,
                is_massive: true,
                is_interactive: true,
                pos,
                vel: [0.0; 3],
                agility: 1.0,
                speed_fraction: 1.0,
                mode: Mode::Stop,
                goto: pos,
                follow_id: 0,
                follow_range: 0.0,
                owner_id: 1,
                effect_stamp: -1,
            },
            type_id: 0,
            group_id: 0,
            category_id: 6,
            owner_id: 1,
            char_id: None,
            corp_id: None,
            jump: None,
            name: String::new(),
            client_id: None,
            order: Order::Stop,
            warp: None,
            modules: Vec::new(),
        }
    }

    #[test]
    fn warp_curve_is_continuous() {
        for d in [2.0e5, 3.0e7, 5.0 * AU] {
            let peak = (1.5 * d).min(WARP_SPEED_AU * AU);
            let t1 = (peak / 3.0).ln() / 3.0;
            let t2 = ((peak / 3.0).ln() - 2.0) / 3.0 + d / peak;
            for t in [t1, t2] {
                let (a, _, _) = warp_curve(d, t - 1e-9);
                let (b, _, _) = warp_curve(d, t + 1e-9);
                assert!((a - b).abs() < 1e-3 * d.max(1.0), "jump at {t} for {d}: {a} vs {b}");
            }
            assert!((warp_curve(d, 0.0).0 - (d - 1.0)).abs() < 1e-6);
        }
    }

    /// A ship at rest aligns at once, engages, and drops out (flying on
    /// towards the destination) where warp_point aimed it.
    #[test]
    fn warp_drops_out_at_destination() {
        let mut park = Park::default();
        let from = [3.0 * AU, 0.0, 0.0];
        let dest = [0.0, 0.0, 1.0e7];
        let mut s = ship(from);
        s.warp = Some(Warp {
            point: warp_point(from, dest),
            dest,
            target: 0,
            engaged: None,
        });
        park.entries.insert(1, s);
        let mut engaged = None;
        for _ in 0..10_000 {
            for (_, step, _) in park.step() {
                match step {
                    WarpStep::Engaged => engaged = Some(park.tick),
                    WarpStep::Exited => {
                        assert!(engaged.is_some_and(|t| t <= 3), "slow to align: {engaged:?}");
                        let e = &park.entries[&1];
                        let off = len(sub(e.ball.pos, dest));
                        assert!(off < 300.0, "dropped out {off} m from the destination");
                        let Order::Direction(d) = e.order else {
                            panic!("not flying on: {:?}", e.order)
                        };
                        assert!(dot(d, norm(sub(dest, from))) > 0.999);
                        assert!((len(e.ball.vel) - 300.0).abs() < 1e-6);
                        return;
                    }
                }
            }
        }
        panic!("warp never ended");
    }
}
