//! Ship dogma in space: capacitor and shields, targeting, module activation
//! (weapons, mining lasers, shield boosters), damage, death, belt asteroids
//! and belt pirates. Runs once per destiny tick from space::simulate.
//!
//! The client shows what the server reports (environment/godma.py):
//! * OnGodmaShipEffect(itemID, effectName, time, start, active,
//!   environment=(itemID, charID, shipID, targetID), duration, repeat, error)
//!   drives the module buttons;
//! * OnSpecialFX(shipID, moduleID, targetID, area, guid, isOffensive, start,
//!   active, duration, repeat) inside DoDestinyUpdate draws turret fire and
//!   beams (parklife/sfxMgr.py; guid names an environment/effects class);
//! * OnDamageStateChange(itemID, ((shield, tau), armor, hull)) inside
//!   DoDestinyUpdate updates the damage bars (michelle.GetDamageState);
//! * OnTarget('add'|'lost'|'otheradd'|'otherlost'|'clear', id, reason)
//!   drives the target manager (parklife/targetMgr.py);
//! * OnModuleAttributeChange(ownerID, itemID, attributeName, time, new, old)
//!   updates charged attributes (capacitor 'charge', 'shieldCharge') and
//!   damage ('armorDamage', 'damage').

use std::collections::HashMap;
use std::sync::Mutex;

use marshal::Value;
use proto::gpcs::Call;

use super::inventory;
use super::{user_error, Args, CallResult, Session};
use crate::space::{self, Entry, Order};
use crate::{db, Server};

/// Dogma figures of one ball, loaded from its type (and item overrides).
#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub cap_max: f64,
    pub cap_recharge_ms: f64,
    pub shield_max: f64,
    pub shield_recharge_ms: f64,
    pub armor_max: f64,
    pub hull_max: f64,
    pub max_targets: usize,
    pub target_range: f64,
    pub scan_speed_ms: f64,
}

#[derive(Debug, Clone)]
struct Active {
    effect: String,
    target: Option<i64>,
    /// Cycles left (None: until stopped).
    repeat: Option<i32>,
    next_ms: i64,
    duration_ms: f64,
    stop: bool,
}

#[derive(Debug, Clone)]
pub struct ShipState {
    pub system: i32,
    /// Who owns the ball (a pirate's corporation, a drone's pilot).
    pub owner: i32,
    pub pilot: Option<i32>,
    pub stats: Stats,
    pub capacitor: f64,
    pub shield: f64,
    pub armor_damage: f64,
    pub hull_damage: f64,
    targets: Vec<i64>,
    locking: Vec<(i64, i64)>,
    active: HashMap<i64, Active>,
    last_ms: i64,
    /// Belt pirates: what to shoot and when.
    npc: Option<Npc>,
}

#[derive(Debug, Clone)]
struct Npc {
    weapon: Weapon,
    attack_range: f64,
    bounty: f64,
    loot_tier: i32,
    target: Option<i64>,
    next_shot_ms: i64,
    home: [f64; 3],
}

#[derive(Debug, Clone, Default)]
struct Weapon {
    range: f64,
    falloff: f64,
    cycle_ms: f64,
    damage: f64,
}

/// An asteroid's ore left (units).
#[derive(Debug, Clone)]
struct Rock {
    remaining: i64,
}

#[derive(Default)]
pub struct Combat {
    ships: Mutex<HashMap<i64, ShipState>>,
    rocks: Mutex<HashMap<i64, Rock>>,
    /// Pirates waiting to respawn: (system, belt, slot, at ms).
    respawns: Mutex<Vec<(i32, i64, i64, i64)>>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn blue_now() -> i64 {
    crate::statics::blue_time_now()
}

/// Type attributes by name.
async fn type_attrs(server: &Server, type_id: i32) -> anyhow::Result<HashMap<String, f64>> {
    let c = server.pool.get().await?;
    Ok(c.query(
        r#"SELECT a."attributeName", ta."value" FROM static.dgmtypeattribs ta JOIN static.dgmattribs a USING ("attributeID")
           WHERE ta."typeID" = $1"#,
        &[&type_id],
    )
    .await?
    .iter()
    .map(|r| (r.get(0), r.get(1)))
    .collect())
}

async fn item_attrs(server: &Server, item_id: i64) -> anyhow::Result<HashMap<String, f64>> {
    let c = server.pool.get().await?;
    Ok(c.query(
        r#"SELECT a."attributeName", ia."value" FROM game.item_attributes ia JOIN static.dgmattribs a USING ("attributeID")
           WHERE ia."itemID" = $1"#,
        &[&item_id],
    )
    .await?
    .iter()
    .map(|r| (r.get(0), r.get(1)))
    .collect())
}

fn stats_of(a: &HashMap<String, f64>) -> Stats {
    let g = |n: &str, d: f64| a.get(n).copied().unwrap_or(d);
    Stats {
        cap_max: g("capacitorCapacity", 100.0),
        cap_recharge_ms: g("rechargeRate", 100000.0).max(1000.0),
        shield_max: g("shieldCapacity", 100.0),
        shield_recharge_ms: g("shieldRechargeRate", 600000.0).max(1000.0),
        armor_max: g("armorHP", 100.0),
        hull_max: g("hp", 100.0).max(1.0),
        max_targets: g("maxLockedTargets", 2.0) as usize,
        target_range: g("maxTargetRange", 20000.0),
        scan_speed_ms: g("scanSpeed", 2000.0),
    }
}

/// The state of a ball, loaded on first use.
async fn ensure_ship(server: &Server, system: i32, ship: i64) -> anyhow::Result<()> {
    if server.combat.ships.lock().unwrap().contains_key(&ship) {
        return Ok(());
    }
    let Some(entry) = server
        .space
        .with_park(system, |p| p.entries.get(&ship).cloned())
    else {
        return Ok(());
    };
    let attrs = type_attrs(server, entry.type_id).await?;
    let saved = item_attrs(server, ship).await?;
    let stats = stats_of(&attrs);
    let state = ShipState {
        system,
        owner: entry.owner_id,
        pilot: entry.char_id,
        capacitor: stats.cap_max,
        shield: saved
            .get("shieldCharge")
            .copied()
            .unwrap_or(stats.shield_max),
        armor_damage: saved.get("armorDamage").copied().unwrap_or(0.0),
        hull_damage: saved.get("damage").copied().unwrap_or(0.0),
        stats,
        targets: Vec::new(),
        locking: Vec::new(),
        active: HashMap::new(),
        last_ms: now_ms(),
        npc: None,
    };
    server
        .combat
        .ships
        .lock()
        .unwrap()
        .entry(ship)
        .or_insert(state);
    Ok(())
}

/// Forgets a ball (docked, jumped, destroyed); its damage is kept on the item.
pub async fn forget(server: &Server, ship: i64) {
    let state = server.combat.ships.lock().unwrap().remove(&ship);
    let Some(s) = state else { return };
    if s.npc.is_some() {
        return;
    }
    for (name, value) in [
        ("armorDamage", s.armor_damage),
        ("damage", s.hull_damage),
        ("shieldCharge", s.shield),
    ] {
        if let Some(attr) = server.world.attribute(name) {
            db::exec(
                &server.pool,
                r#"INSERT INTO game.item_attributes VALUES ($1, $2, $3) ON CONFLICT ("itemID", "attributeID") DO UPDATE SET "value" = $3"#,
                &[&ship, &attr, &value],
            )
            .await
            .ok();
        }
    }
    // Others targeting it lose it.
    let mut lost = Vec::new();
    for (id, other) in server.combat.ships.lock().unwrap().iter_mut() {
        if other.targets.contains(&ship) || other.locking.iter().any(|(t, _)| *t == ship) {
            other.targets.retain(|&t| t != ship);
            other.locking.retain(|(t, _)| *t != ship);
            other.active.retain(|_, a| a.target != Some(ship));
            if let Some(p) = other.pilot {
                lost.push((p, *id));
            }
        }
    }
    for (pilot, _) in lost {
        super::notify_chars(
            server,
            &[pilot],
            "OnTarget",
            vec![Value::str("lost"), Value::from(ship), Value::str("Gone")],
        );
    }
}

fn damage_state(s: &ShipState) -> Value {
    let frac = |v: f64, max: f64| {
        if max > 0.0 {
            (v / max).clamp(0.0, 1.0)
        } else {
            1.0
        }
    };
    Value::tuple([
        Value::tuple([
            Value::Float(frac(s.shield, s.stats.shield_max)),
            Value::Float(s.stats.shield_recharge_ms / 5.0),
        ]),
        Value::Float(1.0 - frac(s.armor_damage, s.stats.armor_max)),
        Value::Float(1.0 - frac(s.hull_damage, s.stats.hull_max)),
    ])
}

fn attribute_event(owner: i32, item: i64, name: &str, new: f64, old: f64) -> Vec<Value> {
    vec![
        Value::Int(owner),
        Value::from(item),
        Value::str(name),
        Value::Long(blue_now()),
        Value::Float(new),
        Value::Float(old),
    ]
}

/// The capacitor/shield recharge curve godma also runs (GetChargeValue):
/// sqrt(E/Ec) approaches 1 with tau = rechargeRate/5.
fn recharge(value: f64, max: f64, recharge_ms: f64, dt_ms: f64) -> f64 {
    if max <= 0.0 {
        return 0.0;
    }
    let tau = recharge_ms / 5.0;
    let sq = (value.max(0.0) / max).sqrt();
    let next = 1.0 + (sq - 1.0) * (-dt_ms / tau).exp();
    (next * next * max).min(max)
}

/// The special effect class a module's activation draws.
fn fx_guid(server: &Server, type_id: i32, effect: &str) -> &'static str {
    let group = server.world.type_info(type_id).map(|t| t.group_id);
    let w = &server.world;
    match effect {
        "mining" => "effects.Mining",
        "shieldBoosting" => "effects.ShieldBoosting",
        _ if group == w.group_id("EnergyWeapon") => "effects.Laser",
        _ if group == w.group_id("ProjectileWeapon") => "effects.ProjectileFired",
        _ => "effects.HybridFired",
    }
}

/// dogmaIM location methods for targets and modules.
pub async fn dogma_call(server: &Server, session: &mut Session, call: &Call) -> CallResult {
    let a = Args::of(call);
    let Some(system) = session.attrs.int("solarsystemid").map(|s| s as i32) else {
        return match call.method() {
            "GetTargets" | "GetTargeters" => Ok(Value::List(vec![])),
            _ => Err(user_error(
                "CustomInfo",
                vec![("info", Value::str("Only in space."))],
            )),
        };
    };
    let ship = session.attrs.int("shipid").unwrap_or(0);
    ensure_ship(server, system, ship).await?;
    match call.method() {
        "GetTargets" => {
            let t = server
                .combat
                .ships
                .lock()
                .unwrap()
                .get(&ship)
                .map(|s| s.targets.clone())
                .unwrap_or_default();
            Ok(Value::List(t.into_iter().map(Value::from).collect()))
        }
        "GetTargeters" => {
            let t: Vec<Value> = server
                .combat
                .ships
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, s)| s.targets.contains(&ship))
                .map(|(id, _)| Value::from(*id))
                .collect();
            Ok(Value::List(t))
        }
        // AddTarget(targetID): locks after the ship's scan time.
        "AddTarget" => {
            let target = a.int(0).unwrap_or(0);
            let dist = server.space.with_park(system, |p| {
                match (p.entries.get(&ship), p.entries.get(&target)) {
                    (Some(s), Some(t)) => {
                        Some(space::len(space::sub(s.ball.pos, t.ball.pos)) - t.ball.radius as f64)
                    }
                    _ => None,
                }
            });
            let Some(dist) = dist else {
                return Err(user_error("TargetingAttemptCancelled", vec![]));
            };
            let mut ships = server.combat.ships.lock().unwrap();
            let s = ships
                .get_mut(&ship)
                .ok_or_else(|| anyhow::anyhow!("no ship state"))?;
            if s.targets.contains(&target) || s.locking.iter().any(|(t, _)| *t == target) {
                return Ok(Value::None);
            }
            if s.targets.len() + s.locking.len() >= s.stats.max_targets.max(1) {
                return Err(user_error("TargetTooManyLocked", vec![]));
            }
            if dist > s.stats.target_range {
                return Err(user_error(
                    "TargetTooFar",
                    vec![("range", Value::Float(s.stats.target_range))],
                ));
            }
            s.locking
                .push((target, now_ms() + s.stats.scan_speed_ms as i64));
            Ok(Value::None)
        }
        "RemoveTarget" => {
            let target = a.int(0).unwrap_or(0);
            unlock(server, ship, target, "Removed");
            Ok(Value::None)
        }
        "ClearTargets" => {
            let targets = server
                .combat
                .ships
                .lock()
                .unwrap()
                .get(&ship)
                .map(|s| s.targets.clone())
                .unwrap_or_default();
            for t in targets {
                unlock(server, ship, t, "Cleared");
            }
            if let Some(me) = session.char_id() {
                super::notify_chars(server, &[me], "OnTarget", vec![Value::str("clear")]);
            }
            Ok(Value::None)
        }
        // Activate(itemID, effectName, targetID, repeat)
        "Activate" => {
            let module = a.int(0).unwrap_or(0);
            let effect = a.text(1).unwrap_or_default();
            let target = a.int(2);
            let repeat = a.int(3).unwrap_or(0) as i32;
            activate(
                server, session, system, ship, module, &effect, target, repeat,
            )
            .await
        }
        // Deactivate(itemID, effectName): stops at the end of the cycle.
        "Deactivate" => {
            let module = a.int(0).unwrap_or(0);
            if let Some(act) = server
                .combat
                .ships
                .lock()
                .unwrap()
                .get_mut(&ship)
                .and_then(|s| s.active.get_mut(&module))
            {
                act.stop = true;
            }
            Ok(Value::None)
        }
        m => super::unimplemented("dogmaIM", m, call),
    }
}

fn unlock(server: &Server, ship: i64, target: i64, reason: &str) {
    let pilot = {
        let mut ships = server.combat.ships.lock().unwrap();
        let Some(s) = ships.get_mut(&ship) else {
            return;
        };
        s.targets.retain(|&t| t != target);
        s.locking.retain(|(t, _)| *t != target);
        for act in s.active.values_mut() {
            if act.target == Some(target) {
                act.stop = true;
            }
        }
        s.pilot
    };
    if let Some(p) = pilot {
        super::notify_chars(
            server,
            &[p],
            "OnTarget",
            vec![Value::str("lost"), Value::from(target), Value::str(reason)],
        );
    }
    let target_pilot = server
        .combat
        .ships
        .lock()
        .unwrap()
        .get(&target)
        .and_then(|s| s.pilot);
    if let Some(tp) = target_pilot {
        super::notify_chars(
            server,
            &[tp],
            "OnTarget",
            vec![
                Value::str("otherlost"),
                Value::from(ship),
                Value::str(reason),
            ],
        );
    }
}

/// A fitted module's figures and its loaded charge.
struct ModuleInfo {
    type_id: i32,
    attrs: HashMap<String, f64>,
    charge: Option<inventory::Item>,
}

async fn module_info(
    server: &Server,
    ship: i64,
    module: i64,
) -> anyhow::Result<Option<ModuleInfo>> {
    // A drone is its own weapon.
    if module == ship {
        let Some(item) = inventory::item(server, ship).await? else {
            return Ok(None);
        };
        return Ok(Some(ModuleInfo {
            type_id: item.type_id,
            attrs: type_attrs(server, item.type_id).await?,
            charge: None,
        }));
    }
    let Some(item) = inventory::item(server, module)
        .await?
        .filter(|i| i.location_id == ship)
    else {
        return Ok(None);
    };
    let attrs = type_attrs(server, item.type_id).await?;
    let charge = inventory::items_where(
        server,
        r#""locationID" = $1 AND "flag" = $2 AND "singleton" = 0 AND "itemID" <> $3"#,
        &[&ship, &item.flag, &module],
    )
    .await?
    .into_iter()
    .next();
    Ok(Some(ModuleInfo {
        type_id: item.type_id,
        attrs,
        charge,
    }))
}

async fn has_effect(server: &Server, type_id: i32, effect: &str) -> anyhow::Result<bool> {
    Ok(db::scalar::<i32>(
        &server.pool,
        r#"SELECT 1 FROM static.dgmtypeeffects te JOIN static.dgmeffects e USING ("effectID")
           WHERE te."typeID" = $1 AND e."effectName" = $2"#,
        &[&type_id, &effect],
    )
    .await?
    .is_some())
}

#[allow(clippy::too_many_arguments)]
async fn activate(
    server: &Server,
    session: &mut Session,
    system: i32,
    ship: i64,
    module: i64,
    effect: &str,
    target: Option<i64>,
    repeat: i32,
) -> CallResult {
    let Some(info) = module_info(server, ship, module).await? else {
        return Err(user_error(
            "CustomInfo",
            vec![(
                "info",
                Value::str("That module is not fitted to your ship."),
            )],
        ));
    };
    if !has_effect(server, info.type_id, effect).await? {
        return Err(user_error(
            "CustomInfo",
            vec![("info", Value::from(format!("That module cannot {effect}.")))],
        ));
    }
    let a = |n: &str| info.attrs.get(n).copied().unwrap_or(0.0);
    let targeted = matches!(effect, "targetAttack" | "mining");
    if targeted {
        let Some(t) = target else {
            return Err(user_error(
                "CustomInfo",
                vec![("info", Value::str("Select a target first."))],
            ));
        };
        let locked = server
            .combat
            .ships
            .lock()
            .unwrap()
            .get(&ship)
            .is_some_and(|s| s.targets.contains(&t));
        if !locked {
            return Err(user_error(
                "CustomInfo",
                vec![("info", Value::str("You must lock that target first."))],
            ));
        }
        let dist = server.space.with_park(system, |p| {
            match (p.entries.get(&ship), p.entries.get(&t)) {
                (Some(s), Some(o)) => {
                    space::len(space::sub(s.ball.pos, o.ball.pos))
                        - o.ball.radius as f64
                        - s.ball.radius as f64
                }
                _ => f64::MAX,
            }
        });
        if dist > a("maxRange") + a("falloff") {
            return Err(user_error(
                "CustomInfo",
                vec![("info", Value::str("The target is out of range."))],
            ));
        }
        if effect == "mining" && !server.combat.rocks.lock().unwrap().contains_key(&t) {
            return Err(user_error(
                "CustomInfo",
                vec![("info", Value::str("You can only mine asteroids."))],
            ));
        }
    }
    if a("chargeGroup1") > 0.0 && info.charge.is_none() {
        return Err(user_error(
            "CustomInfo",
            vec![("info", Value::str("Load ammunition first."))],
        ));
    }
    let duration = if effect == "targetAttack" {
        a("speed")
    } else {
        a("duration")
    }
    .max(1000.0);
    let need = a("capacitorNeed");
    {
        let mut ships = server.combat.ships.lock().unwrap();
        let s = ships
            .get_mut(&ship)
            .ok_or_else(|| anyhow::anyhow!("no ship state"))?;
        if s.capacitor < need {
            return Err(user_error(
                "CustomInfo",
                vec![("info", Value::str("Not enough capacitor charge."))],
            ));
        }
        s.active.insert(
            module,
            Active {
                effect: effect.to_owned(),
                target,
                repeat: if repeat > 0 { Some(repeat) } else { None },
                next_ms: now_ms(),
                duration_ms: duration,
                stop: false,
            },
        );
    }
    let me = session.char_id().unwrap_or(0);
    super::notify_chars(
        server,
        &[me],
        "OnGodmaShipEffect",
        effect_args(module, effect, me, ship, target, true, duration, repeat),
    );
    let stamp = space::stamp_now();
    super::space::broadcast(
        server,
        system,
        vec![space::event(
            stamp,
            "OnSpecialFX",
            fx_args(
                server,
                ship,
                module,
                info.type_id,
                target,
                effect,
                true,
                duration,
                repeat,
            ),
        )],
    );
    Ok(Value::None)
}

fn effect_args(
    module: i64,
    effect: &str,
    char_id: i32,
    ship: i64,
    target: Option<i64>,
    on: bool,
    duration: f64,
    repeat: i32,
) -> Vec<Value> {
    vec![
        Value::from(module),
        Value::str(effect),
        Value::Long(blue_now()),
        Value::Int(on as i32),
        Value::Int(on as i32),
        Value::tuple([
            Value::from(module),
            Value::Int(char_id),
            Value::from(ship),
            Value::from(target),
        ]),
        Value::Float(duration),
        Value::Int(repeat),
        Value::None,
    ]
}

#[allow(clippy::too_many_arguments)]
fn fx_args(
    server: &Server,
    ship: i64,
    module: i64,
    type_id: i32,
    target: Option<i64>,
    effect: &str,
    on: bool,
    duration: f64,
    repeat: i32,
) -> Vec<Value> {
    vec![
        Value::from(ship),
        Value::from(module),
        Value::from(target),
        Value::None,
        Value::str(fx_guid(server, type_id, effect)),
        Value::Int((effect == "targetAttack") as i32),
        Value::Int(on as i32),
        Value::Int(on as i32),
        Value::Float(duration),
        Value::Int(repeat),
    ]
}

/// Damage dealt by a module cycle (charge damages times the multiplier, or
/// the module's own damage for lasers and drones).
fn cycle_damage(info: &ModuleInfo, charge_attrs: &HashMap<String, f64>) -> f64 {
    let sum = |a: &HashMap<String, f64>| {
        [
            "emDamage",
            "thermalDamage",
            "kineticDamage",
            "explosiveDamage",
        ]
        .iter()
        .map(|k| a.get(*k).copied().unwrap_or(0.0))
        .sum::<f64>()
    };
    let base = if info.charge.is_some() {
        sum(charge_attrs)
    } else {
        sum(&info.attrs)
    };
    base * info.attrs.get("damageMultiplier").copied().unwrap_or(1.0)
}

/// Chance to hit at a distance: certain inside optimal range, then halving
/// per falloff (a template default of the turret formula's range term).
fn hit_chance(dist: f64, range: f64, falloff: f64) -> f64 {
    if dist <= range {
        return 1.0;
    }
    if falloff <= 0.0 {
        return 0.0;
    }
    0.5f64.powf(((dist - range) / falloff).powi(2))
}

pub fn roll(seed: i64) -> f64 {
    let mut x = seed as u64 ^ 0x9E37_79B9_7F4A_7C15;
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
    x ^= x >> 33;
    (x % 10_000) as f64 / 10_000.0
}

/// Applies damage to a ball: shields, then armor, then hull. Returns true when
/// it is destroyed.
fn apply_damage(
    server: &Server,
    system: i32,
    target: i64,
    amount: f64,
    events: &mut Vec<Value>,
    notes: &mut Vec<(i32, Vec<Value>)>,
) -> bool {
    let mut ships = server.combat.ships.lock().unwrap();
    let Some(s) = ships.get_mut(&target) else {
        return false;
    };
    let mut left = amount;
    let (old_shield, old_armor, old_hull) = (s.shield, s.armor_damage, s.hull_damage);
    let from_shield = left.min(s.shield);
    s.shield -= from_shield;
    left -= from_shield;
    let from_armor = left.min(s.stats.armor_max - s.armor_damage);
    s.armor_damage += from_armor;
    left -= from_armor;
    s.hull_damage += left;
    let stamp = space::stamp_now();
    events.push(space::event(
        stamp,
        "OnDamageStateChange",
        vec![Value::from(target), damage_state(s)],
    ));
    if let Some(p) = s.pilot {
        let mut changes = Vec::new();
        if s.shield != old_shield {
            changes.push(("shieldCharge", s.shield, old_shield));
        }
        if s.armor_damage != old_armor {
            changes.push(("armorDamage", s.armor_damage, old_armor));
        }
        if s.hull_damage != old_hull {
            changes.push(("damage", s.hull_damage, old_hull));
        }
        for (n, new, old) in changes {
            notes.push((p, attribute_event(p, target, n, new, old)));
        }
    }
    let _ = system;
    s.hull_damage >= s.stats.hull_max
}

/// One second of combat in every loaded system.
pub async fn tick(server: &std::sync::Arc<Server>) {
    let now = now_ms();
    let ids: Vec<(i64, i32)> = server
        .combat
        .ships
        .lock()
        .unwrap()
        .iter()
        .map(|(id, s)| (*id, s.system))
        .collect();
    let mut deaths: Vec<(i32, i64, Option<i64>)> = Vec::new();
    for (ship, system) in ids {
        let mut events = Vec::new();
        let mut notes: Vec<(i32, Vec<Value>)> = Vec::new();
        let mut cycles: Vec<(i64, Active)> = Vec::new();
        let mut finished: Vec<(i64, Active)> = Vec::new();
        let mut locked_now = Vec::new();
        let pilot;
        {
            let mut ships = server.combat.ships.lock().unwrap();
            let Some(s) = ships.get_mut(&ship) else {
                continue;
            };
            pilot = s.pilot;
            let dt = (now - s.last_ms) as f64;
            s.last_ms = now;
            s.capacitor = recharge(s.capacitor, s.stats.cap_max, s.stats.cap_recharge_ms, dt);
            s.shield = recharge(s.shield, s.stats.shield_max, s.stats.shield_recharge_ms, dt);
            let (done, pending): (Vec<_>, Vec<_>) =
                s.locking.iter().partition(|(_, at)| *at <= now);
            s.locking = pending;
            for (t, _) in done {
                if s.targets.len() < s.stats.max_targets.max(1) {
                    s.targets.push(t);
                    locked_now.push(t);
                }
            }
            for (module, act) in s.active.iter_mut() {
                if act.next_ms <= now {
                    if act.stop || act.repeat == Some(0) {
                        finished.push((*module, act.clone()));
                    } else {
                        cycles.push((*module, act.clone()));
                        act.next_ms = now + act.duration_ms as i64;
                        if let Some(r) = act.repeat.as_mut() {
                            *r -= 1;
                        }
                    }
                }
            }
            for (m, _) in &finished {
                s.active.remove(m);
            }
        }
        for t in locked_now {
            if let Some(p) = pilot {
                super::notify_chars(
                    server,
                    &[p],
                    "OnTarget",
                    vec![Value::str("add"), Value::from(t)],
                );
            }
            let tp = server
                .combat
                .ships
                .lock()
                .unwrap()
                .get(&t)
                .and_then(|s| s.pilot);
            if let Some(tp) = tp {
                super::notify_chars(
                    server,
                    &[tp],
                    "OnTarget",
                    vec![Value::str("otheradd"), Value::from(ship)],
                );
            }
        }
        for (module, act) in finished {
            if let Some(p) = pilot {
                notes.push((
                    p,
                    effect_args(
                        module,
                        &act.effect,
                        p,
                        ship,
                        act.target,
                        false,
                        act.duration_ms,
                        0,
                    ),
                ));
            }
            let type_id = inventory::item(server, module)
                .await
                .ok()
                .flatten()
                .map(|i| i.type_id)
                .unwrap_or(0);
            events.push(space::event(
                space::stamp_now(),
                "OnSpecialFX",
                fx_args(
                    server,
                    ship,
                    module,
                    type_id,
                    act.target,
                    &act.effect,
                    false,
                    act.duration_ms,
                    0,
                ),
            ));
        }
        for (module, act) in cycles {
            match run_cycle(
                server,
                system,
                ship,
                pilot,
                module,
                &act,
                &mut events,
                &mut notes,
            )
            .await
            {
                Ok(Some(dead)) => deaths.push((system, dead, Some(ship))),
                Ok(None) => {}
                Err(e) => tracing::warn!("module cycle failed: {e:#}"),
            }
        }
        flush(server, system, events, notes);
    }
    // Pirates pick targets and shoot.
    for (system, dead, killer) in npc_tick(server, now).await {
        deaths.push((system, dead, killer));
    }
    for (system, dead, killer) in deaths {
        if let Err(e) = kill(server, system, dead, killer).await {
            tracing::warn!("destroying {dead}: {e:#}");
        }
    }
    respawn_pirates(server, now).await;
}

fn flush(server: &Server, system: i32, events: Vec<Value>, notes: Vec<(i32, Vec<Value>)>) {
    if !events.is_empty() {
        super::space::broadcast(server, system, events);
    }
    for (p, args) in notes {
        let name = if args.len() == 9 {
            "OnGodmaShipEffect"
        } else {
            "OnModuleAttributeChange"
        };
        super::notify_chars(server, &[p], name, args);
    }
}

/// One cycle of an active module. Returns a destroyed target's ID.
#[allow(clippy::too_many_arguments)]
async fn run_cycle(
    server: &Server,
    system: i32,
    ship: i64,
    pilot: Option<i32>,
    module: i64,
    act: &Active,
    events: &mut Vec<Value>,
    notes: &mut Vec<(i32, Vec<Value>)>,
) -> anyhow::Result<Option<i64>> {
    let Some(info) = module_info(server, ship, module).await? else {
        stop_module(server, ship, module);
        return Ok(None);
    };
    let a = |n: &str| info.attrs.get(n).copied().unwrap_or(0.0);
    // Capacitor.
    let need = a("capacitorNeed");
    let cap = {
        let mut ships = server.combat.ships.lock().unwrap();
        let Some(s) = ships.get_mut(&ship) else {
            return Ok(None);
        };
        if s.capacitor < need {
            s.active.get_mut(&module).map(|x| x.stop = true);
            return Ok(None);
        }
        let old = s.capacitor;
        s.capacitor -= need;
        (old, s.capacitor)
    };
    if let Some(p) = pilot {
        if need > 0.0 {
            notes.push((p, attribute_event(p, ship, "charge", cap.1, cap.0)));
        }
    }
    // Ammunition.
    let mut charge_attrs = HashMap::new();
    if let Some(ch) = &info.charge {
        charge_attrs = type_attrs(server, ch.type_id).await?;
        if ch.quantity <= 1 {
            inventory::delete_item(server, ch).await?;
            stop_module(server, ship, module);
        } else {
            inventory::set_quantity(server, ch, ch.quantity - 1).await?;
        }
    } else if a("chargeGroup1") > 0.0 {
        stop_module(server, ship, module);
        return Ok(None);
    }
    match act.effect.as_str() {
        "shieldBoosting" => {
            let mut ships = server.combat.ships.lock().unwrap();
            if let Some(s) = ships.get_mut(&ship) {
                let old = s.shield;
                s.shield = (s.shield + a("shieldBonus")).min(s.stats.shield_max);
                events.push(space::event(
                    space::stamp_now(),
                    "OnDamageStateChange",
                    vec![Value::from(ship), damage_state(s)],
                ));
                if let Some(p) = pilot {
                    notes.push((p, attribute_event(p, ship, "shieldCharge", s.shield, old)));
                }
            }
            Ok(None)
        }
        "mining" => {
            let Some(rock) = act.target else {
                return Ok(None);
            };
            // Mining drones fill their owner's ship.
            let hold = if module == ship {
                let pilot_ship = pilot.and_then(|p| {
                    server.space.with_park(system, |park| {
                        park.entries
                            .values()
                            .find(|e| e.char_id == Some(p))
                            .map(|e| e.ball.id)
                    })
                });
                match pilot_ship {
                    Some(s) => s,
                    None => return Ok(None),
                }
            } else {
                ship
            };
            mine(server, system, hold, pilot, rock, a("miningAmount")).await?;
            Ok(None)
        }
        _ => {
            let Some(target) = act.target else {
                return Ok(None);
            };
            let dist = server.space.with_park(system, |p| {
                match (p.entries.get(&ship), p.entries.get(&target)) {
                    (Some(s), Some(t)) => Some(
                        space::len(space::sub(s.ball.pos, t.ball.pos))
                            - t.ball.radius as f64
                            - s.ball.radius as f64,
                    ),
                    _ => None,
                }
            });
            let Some(dist) = dist else {
                stop_module(server, ship, module);
                return Ok(None);
            };
            ensure_ship(server, system, target).await?;
            let chance = hit_chance(dist, a("maxRange"), a("falloff"));
            let hit = roll(now_ms() ^ module) < chance;
            let dmg = if hit {
                cycle_damage(&info, &charge_attrs) * (0.5 + roll(module ^ target ^ now_ms()))
            } else {
                0.0
            };
            let target_pilot = server
                .combat
                .ships
                .lock()
                .unwrap()
                .get(&target)
                .and_then(|s| s.pilot);
            let msg = |key: &str| {
                vec![
                    Value::str(key),
                    Value::dict([
                        (Value::str("source"), Value::from(ship)),
                        (Value::str("target"), Value::from(target)),
                        (Value::str("damage"), Value::Int(dmg.round() as i32)),
                    ]),
                ]
            };
            let key = if hit { "AttackHit" } else { "AttackMiss" };
            for p in pilot.into_iter().chain(target_pilot) {
                super::notify_chars(server, &[p], "OnDamageMessage", msg(key));
            }
            if dmg > 0.0 && apply_damage(server, system, target, dmg, events, notes) {
                return Ok(Some(target));
            }
            Ok(None)
        }
    }
}

fn stop_module(server: &Server, ship: i64, module: i64) {
    if let Some(a) = server
        .combat
        .ships
        .lock()
        .unwrap()
        .get_mut(&ship)
        .and_then(|s| s.active.get_mut(&module))
    {
        a.stop = true;
    }
}

/// Moves ore from an asteroid into the miner's cargo hold.
async fn mine(
    server: &Server,
    system: i32,
    ship: i64,
    pilot: Option<i32>,
    rock: i64,
    amount_m3: f64,
) -> anyhow::Result<()> {
    let Some(rock_entry) = server
        .space
        .with_park(system, |p| p.entries.get(&rock).cloned())
    else {
        return Ok(());
    };
    let w = &server.world;
    let unit = w
        .type_info(rock_entry.type_id)
        .map(|t| t.volume)
        .unwrap_or(0.1)
        .max(0.01);
    let Some(ship_item) = inventory::item(server, ship).await? else {
        return Ok(());
    };
    let capacity = w
        .type_info(ship_item.type_id)
        .map(|t| t.capacity)
        .unwrap_or(0.0);
    let used: f64 = inventory::items_where(
        server,
        r#""locationID" = $1 AND "flag" = $2"#,
        &[&ship, &w.flag("Cargo")],
    )
    .await?
    .iter()
    .map(|i| i.volume(server))
    .sum();
    let room = ((capacity - used) / unit).floor() as i64;
    let want = (amount_m3 / unit).floor() as i64;
    let (units, depleted) = {
        let mut rocks = server.combat.rocks.lock().unwrap();
        let Some(r) = rocks.get_mut(&rock) else {
            return Ok(());
        };
        let units = want.min(room).min(r.remaining).max(0);
        r.remaining -= units;
        (units, r.remaining <= 0)
    };
    if let Some(p) = pilot {
        if units == 0 && room <= 0 {
            super::notify_chars(
                server,
                &[p],
                "OnDamageMessage",
                vec![Value::str("MiningCargoFull"), Value::Dict(vec![])],
            );
        }
    }
    if units > 0 {
        let owner = pilot.unwrap_or(rock_entry.owner_id);
        let ore = inventory::create_item(
            server,
            rock_entry.type_id,
            owner,
            ship,
            w.flag("Cargo"),
            units as i32,
            false,
        )
        .await?;
        inventory::notify_new(server, &ore);
        inventory::stack_into(server, &ore, ship, w.flag("Cargo")).await?;
        db::exec(
            &server.pool,
            r#"INSERT INTO game.asteroid_depletion ("asteroidID", "mined") VALUES ($1, $2)
               ON CONFLICT ("asteroidID") DO UPDATE SET "mined" = game.asteroid_depletion."mined" + $2"#,
            &[&rock, &units],
        )
        .await?;
    }
    if depleted {
        server.combat.rocks.lock().unwrap().remove(&rock);
        server.space.with_park(system, |p| p.entries.remove(&rock));
        super::space::broadcast(
            server,
            system,
            vec![space::event(
                space::stamp_now(),
                "RemoveBall",
                vec![Value::from(rock)],
            )],
        );
        forget(server, rock).await;
    }
    Ok(())
}

/// Destroys a ball: a player's ship (the pilot ejects), a pirate (bounty and
/// loot), or anything else.
async fn kill(
    server: &std::sync::Arc<Server>,
    system: i32,
    dead: i64,
    killer: Option<i64>,
) -> anyhow::Result<()> {
    let state = server.combat.ships.lock().unwrap().get(&dead).cloned();
    let Some(state) = state else { return Ok(()) };
    let killer_pilot = killer.and_then(|k| {
        server
            .combat
            .ships
            .lock()
            .unwrap()
            .get(&k)
            .and_then(|s| s.pilot)
    });
    let pos = server
        .space
        .with_park(system, |p| p.entries.get(&dead).map(|e| e.ball.pos))
        .unwrap_or([0.0; 3]);
    let stamp = space::stamp_now();
    super::space::broadcast(
        server,
        system,
        vec![space::event(
            stamp,
            "OnSpecialFX",
            vec![
                Value::from(dead),
                Value::None,
                Value::None,
                Value::None,
                Value::str("effects.Explosion"),
                Value::Int(0),
                Value::Int(1),
                Value::Int(0),
                Value::Float(-1.0),
                Value::None,
            ],
        )],
    );
    if let Some(npc) = state.npc.clone() {
        // Bounty, loot, and a new pirate later.
        server.combat.ships.lock().unwrap().remove(&dead);
        server.space.with_park(system, |p| p.entries.remove(&dead));
        super::space::broadcast(
            server,
            system,
            vec![space::event(stamp, "RemoveBall", vec![Value::from(dead)])],
        );
        if let Some(p) = killer_pilot {
            super::wallet::transfer(
                server,
                server.world.constant("ownerCONCORD"),
                p,
                npc.bounty,
                "refBountyPrize",
                Some(dead),
                "",
            )
            .await
            .ok();
            let loot = loot_for(server, npc.loot_tier, dead);
            let mut items = Vec::new();
            for (type_id, qty) in loot {
                items.push(
                    inventory::create_item(server, type_id, p, system as i64, 0, qty, false)
                        .await?,
                );
            }
            super::space::spill_items(server, system, p, items, pos).await?;
        }
        let (belt, slot) = pirate_slot(dead);
        server.combat.respawns.lock().unwrap().push((
            system,
            belt,
            slot,
            now_ms() + 15 * 60 * 1000,
        ));
        return Ok(());
    }
    forget(server, dead).await;
    match state.pilot {
        Some(char_id) => super::space::destroy_remote(server, system, dead, char_id).await?,
        None => {
            server.space.with_park(system, |p| p.entries.remove(&dead));
            super::space::broadcast(
                server,
                system,
                vec![space::event(stamp, "RemoveBall", vec![Value::from(dead)])],
            );
            if let Some(item) = inventory::item(server, dead).await? {
                inventory::delete_item(server, &item).await?;
            }
        }
    }
    Ok(())
}

/// TEMPLATE DEFAULT loot: ammunition and minerals, more for tougher pirates.
fn loot_for(server: &Server, tier: i32, seed: i64) -> Vec<(i32, i32)> {
    let w = &server.world;
    let pick =
        |names: &[&str], n: i64| names[(n.unsigned_abs() % names.len() as u64) as usize].to_owned();
    let ammo = pick(
        &["AntimatterChargeS", "IronChargeS", "PhasedPlasmaS", "EMPS"],
        seed,
    );
    let mineral = pick(
        &[
            "Ferrite",
            "Silicate",
            "CrystalAlloy",
            "IsotopeGel",
            "Nocturne",
        ],
        seed / 7,
    );
    vec![
        (w.type_id(&ammo), 50 * tier),
        (w.type_id(&mineral), 200 * tier),
    ]
}

// ---------------------------------------------------------------- the belts

fn asteroid_id(belt: i64, i: i64) -> i64 {
    1_900_000_000 + (belt - 40_000_000) * 16 + i
}

fn pirate_id(belt: i64, slot: i64) -> i64 {
    2_000_000_000 + (belt - 40_000_000) * 4 + slot
}

fn pirate_slot(id: i64) -> (i64, i64) {
    let k = id - 2_000_000_000;
    (k / 4 + 40_000_000, k % 4)
}

fn hash(parts: &[i64]) -> u64 {
    let mut x = 0xcbf2_9ce4_8422_2325u64;
    for p in parts {
        for b in p.to_le_bytes() {
            x ^= b as u64;
            x = x.wrapping_mul(0x100_0000_01b3);
        }
    }
    x
}

/// Asteroids and pirates for a system's belts, created when its park loads.
/// Deterministic from the belt ID; what was mined is remembered.
pub async fn populate(server: &Server, system: i32) -> anyhow::Result<Vec<Entry>> {
    let w = &server.world;
    let Some(belt_group) = w.group_id("AsteroidBelt") else {
        return Ok(Vec::new());
    };
    let c = server.pool.get().await?;
    let belts = c
        .query(
            r#"SELECT "itemID", "x", "y", "z" FROM static.celestials WHERE "solarSystemID" = $1 AND "groupID" = $2"#,
            &[&system, &belt_group],
        )
        .await?;
    let ores = c
        .query(
            r#"SELECT t."typeID", coalesce(ta."value", 1000.0) FROM static.types t JOIN static.groups g USING ("groupID")
               LEFT JOIN static.dgmtypeattribs ta ON ta."typeID" = t."typeID"
                    AND ta."attributeID" = (SELECT "attributeID" FROM static.dgmattribs WHERE "attributeName" = 'radius')
               WHERE g."groupName" = 'Asteroid' ORDER BY t."typeID""#,
            &[],
        )
        .await?;
    let pirates = c
        .query(
            r#"SELECT t."typeID" FROM static.types t JOIN static.groups g USING ("groupID")
               WHERE g."groupName" = 'Pirate Drone' ORDER BY t."typeID""#,
            &[],
        )
        .await?;
    let depleted: HashMap<i64, i64> = c
        .query(r#"SELECT "asteroidID", "mined" FROM game.asteroid_depletion WHERE "asteroidID" BETWEEN $1 AND $2"#, &[
            &asteroid_id(40_000_000, 0),
            &asteroid_id(40_200_000, 0),
        ])
        .await?
        .iter()
        .map(|r| (r.get(0), r.get(1)))
        .collect();
    drop(c);
    if ores.is_empty() {
        return Ok(Vec::new());
    }
    let security = w.systems.get(&system).map(|s| s.security).unwrap_or(1.0);
    let mut out = Vec::new();
    for b in belts {
        let belt: i64 = b.get::<_, i32>(0) as i64;
        let centre = [b.get::<_, f64>(1), b.get::<_, f64>(2), b.get::<_, f64>(3)];
        for i in 0..8 {
            let id = asteroid_id(belt, i);
            let h = hash(&[belt, i]);
            // Lower security, richer ores further down the list.
            let span = ((1.0 - security.clamp(-1.0, 1.0)) * ores.len() as f64 / 2.0)
                .ceil()
                .max(2.0) as u64;
            let ore = &ores[((h % span) as usize).min(ores.len() - 1)];
            let total = 4000 + (h >> 8) as i64 % 12000;
            let remaining = total - depleted.get(&id).copied().unwrap_or(0);
            if remaining <= 0 {
                continue;
            }
            let angle = (h >> 20) as f64 / (1u64 << 44) as f64 * std::f64::consts::TAU;
            let dist = 8000.0 + ((h >> 40) % 25000) as f64;
            let pos = [
                centre[0] + angle.cos() * dist,
                centre[1] + ((h >> 30) % 4000) as f64 - 2000.0,
                centre[2] + angle.sin() * dist,
            ];
            let type_id: i32 = ore.get(0);
            let radius: f64 = ore.get(1);
            let mut e = Entry {
                ball: crate::space::ballimg::Ball::fixed(id, radius, pos, true, false),
                type_id,
                group_id: w.type_info(type_id).map(|t| t.group_id).unwrap_or(0),
                category_id: w.type_info(type_id).map(|t| t.category_id).unwrap_or(0),
                owner_id: 1,
                char_id: None,
                corp_id: None,
                jump: None,
                name: w
                    .type_info(type_id)
                    .map(|t| t.name.clone())
                    .unwrap_or_default(),
                client_id: None,
                order: Order::Stop,
                warp: None,
                modules: Vec::new(),
            };
            e.ball.is_global = false;
            server
                .combat
                .rocks
                .lock()
                .unwrap()
                .insert(id, Rock { remaining });
            out.push(e);
        }
        // Pirates in the lower-security belts.
        if security < 0.8 && !pirates.is_empty() {
            let n = ((0.8 - security) * 4.0).ceil().clamp(1.0, 3.0) as i64;
            for slot in 0..n {
                out.push(
                    pirate_entry(
                        server,
                        system,
                        belt,
                        centre,
                        slot,
                        pirates.iter().map(|r| r.get(0)).collect(),
                    )
                    .await?,
                );
            }
        }
    }
    Ok(out)
}

async fn pirate_entry(
    server: &Server,
    system: i32,
    belt: i64,
    centre: [f64; 3],
    slot: i64,
    types: Vec<i32>,
) -> anyhow::Result<Entry> {
    let w = &server.world;
    let id = pirate_id(belt, slot);
    let h = hash(&[belt, slot, 7]);
    let security = w.systems.get(&system).map(|s| s.security).unwrap_or(1.0);
    let tier = (((0.8 - security) * 3.0) as usize).min(types.len() - 1);
    let type_id = types[(h as usize % (tier + 1)).min(types.len() - 1)];
    let attrs = type_attrs(server, type_id).await?;
    let g = |n: &str, d: f64| attrs.get(n).copied().unwrap_or(d);
    let pos = [
        centre[0] + (h % 20000) as f64 - 10000.0,
        centre[1],
        centre[2] + ((h >> 16) % 20000) as f64 - 10000.0,
    ];
    let owner = 1_000_127; // Guristas
    let ball = crate::space::ballimg::Ball {
        id,
        mass: g("mass", 1.0e6) as f32,
        radius: g("radius", 40.0) as f32,
        max_velocity: g("maxVelocity", 250.0) as f32,
        is_free: true,
        is_global: false,
        is_massive: true,
        is_interactive: true,
        pos,
        vel: [0.0; 3],
        agility: g("agility", 2.0) as f32,
        speed_fraction: 1.0,
        mode: crate::space::ballimg::Mode::Stop,
        goto: pos,
        follow_id: 0,
        follow_range: 0.0,
        owner_id: owner as i64,
        effect_stamp: -1,
    };
    let stats = stats_of(&attrs);
    let state = ShipState {
        system,
        owner,
        pilot: None,
        capacitor: stats.cap_max,
        shield: stats.shield_max,
        armor_damage: 0.0,
        hull_damage: 0.0,
        stats,
        targets: Vec::new(),
        locking: Vec::new(),
        active: HashMap::new(),
        last_ms: now_ms(),
        npc: Some(Npc {
            weapon: Weapon {
                range: g("maxRange", 4000.0),
                falloff: g("falloff", 2000.0),
                cycle_ms: g("speed", 4000.0),
                damage: (g("kineticDamage", 3.0) + g("thermalDamage", 2.0))
                    * g("damageMultiplier", 1.0),
            },
            attack_range: g("entityAttackRange", 20000.0),
            bounty: g("entityKillBounty", 1000.0),
            loot_tier: g("entityLootValue", 1.0) as i32,
            target: None,
            next_shot_ms: 0,
            home: pos,
        }),
    };
    server.combat.ships.lock().unwrap().insert(id, state);
    Ok(Entry {
        ball,
        type_id,
        group_id: w.type_info(type_id).map(|t| t.group_id).unwrap_or(0),
        category_id: w.type_info(type_id).map(|t| t.category_id).unwrap_or(0),
        owner_id: owner,
        char_id: None,
        corp_id: None,
        jump: None,
        name: w
            .type_info(type_id)
            .map(|t| t.name.clone())
            .unwrap_or_default(),
        client_id: None,
        order: Order::Stop,
        warp: None,
        modules: Vec::new(),
    })
}

async fn respawn_pirates(server: &Server, now: i64) {
    let due: Vec<(i32, i64, i64, i64)> = {
        let mut r = server.combat.respawns.lock().unwrap();
        let (due, wait): (Vec<_>, Vec<_>) = r.iter().partition(|(_, _, _, at)| *at <= now);
        *r = wait;
        due
    };
    for (system, belt, slot, _) in due {
        let Ok(c) = server.pool.get().await else {
            continue;
        };
        let Ok(Some(b)) = c
            .query_opt(
                r#"SELECT "x", "y", "z" FROM static.celestials WHERE "itemID" = $1"#,
                &[&(belt as i32)],
            )
            .await
        else {
            continue;
        };
        let Ok(types) = c
            .query(r#"SELECT t."typeID" FROM static.types t JOIN static.groups g USING ("groupID") WHERE g."groupName" = 'Pirate Drone' ORDER BY 1"#, &[])
            .await
        else {
            continue;
        };
        drop(c);
        let centre = [b.get(0), b.get(1), b.get(2)];
        if let Ok(e) = pirate_entry(
            server,
            system,
            belt,
            centre,
            slot,
            types.iter().map(|r| r.get(0)).collect(),
        )
        .await
        {
            let add = space::add_balls(&[&e]);
            server
                .space
                .with_park(system, |p| p.entries.insert(e.ball.id, e));
            super::space::broadcast(server, system, vec![add]);
        }
    }
}

/// Pirates: pick the nearest pilot in range, approach to optimal, shoot.
async fn npc_tick(server: &Server, now: i64) -> Vec<(i32, i64, Option<i64>)> {
    let npcs: Vec<(i64, i32, Npc)> = server
        .combat
        .ships
        .lock()
        .unwrap()
        .iter()
        .filter_map(|(id, s)| s.npc.clone().map(|n| (*id, s.system, n)))
        .collect();
    let mut deaths = Vec::new();
    let capsule = server.world.type_id("Capsule");
    for (id, system, npc) in npcs {
        let (me, players): (Option<[f64; 3]>, Vec<(i64, [f64; 3])>) =
            server.space.with_park(system, |p| {
                (
                    p.entries.get(&id).map(|e| e.ball.pos),
                    // Pirates leave capsules alone.
                    p.entries
                        .values()
                        .filter(|e| e.char_id.is_some() && e.warp.is_none() && e.type_id != capsule)
                        .map(|e| (e.ball.id, e.ball.pos))
                        .collect(),
                )
            });
        let Some(me) = me else { continue };
        let current = npc
            .target
            .and_then(|t| players.iter().find(|(id, _)| *id == t).copied());
        let target = match current {
            Some(t) if space::len(space::sub(t.1, me)) <= npc.attack_range * 1.5 => Some(t),
            _ => players
                .iter()
                .filter(|(_, pos)| space::len(space::sub(*pos, me)) <= npc.attack_range)
                .min_by(|a, b| {
                    space::len(space::sub(a.1, me)).total_cmp(&space::len(space::sub(b.1, me)))
                })
                .copied(),
        };
        let mut events = Vec::new();
        let mut notes = Vec::new();
        match target {
            None => {
                if let Some(old) = npc.target {
                    set_npc(server, id, |n| n.target = None);
                    unlock_npc(server, id, old);
                    let home = npc.home;
                    server.space.with_park(system, |p| {
                        if let Some(e) = p.entries.get_mut(&id) {
                            e.order = Order::Point(home);
                        }
                    });
                    events.push(space::event(
                        space::stamp_now(),
                        "GotoPoint",
                        vec![
                            Value::from(id),
                            Value::Float(home[0]),
                            Value::Float(home[1]),
                            Value::Float(home[2]),
                        ],
                    ));
                }
            }
            Some((tid, tpos)) => {
                if npc.target != Some(tid) {
                    if let Some(old) = npc.target {
                        unlock_npc(server, id, old);
                    }
                    set_npc(server, id, |n| {
                        n.target = Some(tid);
                        n.next_shot_ms = now + 3000;
                    });
                    if let Some(s) = server.combat.ships.lock().unwrap().get_mut(&id) {
                        s.targets = vec![tid];
                    }
                    ensure_ship(server, system, tid).await.ok();
                    let tp = server
                        .combat
                        .ships
                        .lock()
                        .unwrap()
                        .get(&tid)
                        .and_then(|s| s.pilot);
                    if let Some(tp) = tp {
                        super::notify_chars(
                            server,
                            &[tp],
                            "OnTarget",
                            vec![Value::str("otheradd"), Value::from(id)],
                        );
                    }
                    let range = npc.weapon.range * 0.8;
                    server.space.with_park(system, |p| {
                        if let Some(e) = p.entries.get_mut(&id) {
                            e.order = Order::Orbit { target: tid, range };
                        }
                    });
                    events.push(space::event(
                        space::stamp_now(),
                        "Orbit",
                        vec![
                            Value::from(id),
                            Value::from(tid),
                            Value::Float(range),
                            Value::Int(-1),
                        ],
                    ));
                } else if npc.next_shot_ms <= now {
                    set_npc(server, id, |n| {
                        n.next_shot_ms = now + n.weapon.cycle_ms as i64
                    });
                    let dist = space::len(space::sub(tpos, me));
                    let hit =
                        roll(now ^ id) < hit_chance(dist, npc.weapon.range, npc.weapon.falloff);
                    let dmg = if hit {
                        npc.weapon.damage * (0.5 + roll(id ^ tid ^ now))
                    } else {
                        0.0
                    };
                    events.push(space::event(
                        space::stamp_now(),
                        "OnSpecialFX",
                        vec![
                            Value::from(id),
                            Value::from(id),
                            Value::from(tid),
                            Value::None,
                            Value::str("effects.HybridFired"),
                            Value::Int(1),
                            Value::Int(1),
                            Value::Int(0),
                            Value::Float(npc.weapon.cycle_ms),
                            Value::Int(1),
                        ],
                    ));
                    let tp = server
                        .combat
                        .ships
                        .lock()
                        .unwrap()
                        .get(&tid)
                        .and_then(|s| s.pilot);
                    if let Some(tp) = tp {
                        super::notify_chars(
                            server,
                            &[tp],
                            "OnDamageMessage",
                            vec![
                                Value::str(if hit { "AttackHit" } else { "AttackMiss" }),
                                Value::dict([
                                    (Value::str("source"), Value::from(id)),
                                    (Value::str("target"), Value::from(tid)),
                                    (Value::str("damage"), Value::Int(dmg.round() as i32)),
                                ]),
                            ],
                        );
                    }
                    if dmg > 0.0 && apply_damage(server, system, tid, dmg, &mut events, &mut notes)
                    {
                        deaths.push((system, tid, Some(id)));
                    }
                }
            }
        }
        flush(server, system, events, notes);
    }
    deaths
}

fn set_npc(server: &Server, id: i64, f: impl FnOnce(&mut Npc)) {
    if let Some(n) = server
        .combat
        .ships
        .lock()
        .unwrap()
        .get_mut(&id)
        .and_then(|s| s.npc.as_mut())
    {
        f(n);
    }
}

fn unlock_npc(server: &Server, npc: i64, target: i64) {
    if let Some(s) = server.combat.ships.lock().unwrap().get_mut(&npc) {
        s.targets.clear();
    }
    let tp = server
        .combat
        .ships
        .lock()
        .unwrap()
        .get(&target)
        .and_then(|s| s.pilot);
    if let Some(tp) = tp {
        super::notify_chars(
            server,
            &[tp],
            "OnTarget",
            vec![
                Value::str("otherlost"),
                Value::from(npc),
                Value::str("Lost"),
            ],
        );
    }
}

/// The damage dict a SetState carries for every ball with state.
pub fn damage_states(server: &Server, ids: &[i64]) -> Vec<(Value, Value)> {
    let ships = server.combat.ships.lock().unwrap();
    ids.iter()
        .filter_map(|id| ships.get(id).map(|s| (Value::from(*id), damage_state(s))))
        .collect()
}

/// Online module effects a fitted ship reports (godma activeEffects).
pub async fn online_effect_id(server: &Server) -> anyhow::Result<Option<i32>> {
    db::scalar(
        &server.pool,
        r#"SELECT "effectID" FROM static.dgmeffects WHERE "effectName" = 'online'"#,
        &[],
    )
    .await
}

/// entity[solarsystemID]: drone commands (navigation.py/menusvc.py).
pub async fn entity(server: &Server, session: &mut Session, call: &Call) -> CallResult {
    let a = Args::of(call);
    let Some(system) = session.attrs.int("solarsystemid").map(|s| s as i32) else {
        return Ok(Value::None);
    };
    let ship = session.attrs.int("shipid").unwrap_or(0);
    let me = session.char_id().unwrap_or(0);
    let drone = a.int(0).unwrap_or(0);
    let owned = server.space.with_park(system, |p| {
        p.entries.get(&drone).is_some_and(|e| e.owner_id == me)
    });
    if !owned {
        return Ok(Value::None);
    }
    ensure_ship(server, system, drone).await?;
    if let Some(s) = server.combat.ships.lock().unwrap().get_mut(&drone) {
        s.pilot = Some(me);
        s.owner = me;
    }
    match call.method() {
        // CmdAttack(droneID, targetID) / CmdMine(droneID, asteroidID): the
        // drone orbits the target and cycles its weapon or mining effect.
        "CmdAttack" | "CmdMine" | "CmdMineRepeatedly" => {
            let target = a.int(1).unwrap_or(0);
            let effect = if call.method() == "CmdAttack" {
                "targetAttack"
            } else {
                "mining"
            };
            let attrs = type_attrs(
                server,
                server
                    .space
                    .with_park(system, |p| p.entries.get(&drone).map(|e| e.type_id))
                    .unwrap_or(0),
            )
            .await?;
            let duration = attrs
                .get(if effect == "mining" {
                    "duration"
                } else {
                    "speed"
                })
                .copied()
                .unwrap_or(4000.0);
            let range = attrs.get("maxRange").copied().unwrap_or(1000.0) * 0.8;
            ensure_ship(server, system, target).await?;
            if let Some(s) = server.combat.ships.lock().unwrap().get_mut(&drone) {
                s.targets = vec![target];
                s.active.insert(
                    drone,
                    Active {
                        effect: effect.to_owned(),
                        target: Some(target),
                        repeat: if call.method() == "CmdMine" {
                            Some(1)
                        } else {
                            None
                        },
                        next_ms: now_ms() + 2000,
                        duration_ms: duration,
                        stop: false,
                    },
                );
            }
            server.space.with_park(system, |p| {
                if let Some(e) = p.entries.get_mut(&drone) {
                    e.order = Order::Orbit { target, range };
                }
            });
            super::space::broadcast(
                server,
                system,
                vec![space::event(
                    space::stamp_now(),
                    "Orbit",
                    vec![
                        Value::from(drone),
                        Value::from(target),
                        Value::Float(range),
                        Value::Int(-1),
                    ],
                )],
            );
            Ok(Value::None)
        }
        // CmdReturnHome(droneID): back to orbit the ship.
        "CmdReturnHome" => {
            if let Some(s) = server.combat.ships.lock().unwrap().get_mut(&drone) {
                s.active.clear();
                s.targets.clear();
            }
            server.space.with_park(system, |p| {
                if let Some(e) = p.entries.get_mut(&drone) {
                    e.order = Order::Orbit {
                        target: ship,
                        range: 500.0,
                    };
                }
            });
            super::space::broadcast(
                server,
                system,
                vec![space::event(
                    space::stamp_now(),
                    "Orbit",
                    vec![
                        Value::from(drone),
                        Value::from(ship),
                        Value::Float(500.0),
                        Value::Int(-1),
                    ],
                )],
            );
            Ok(Value::None)
        }
        m => super::unimplemented("entity", m, call),
    }
}
