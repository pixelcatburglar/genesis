//! Industry: the `factory` service, the bound per-station factory manager
//! (moniker.GetFactoryManager) and slot rental on corpStationMgr
//! (ui/station/facilities/facility.py, factories/, researchfacility/,
//! shared/infosvc.py, generic/listwindow.py).
//!
//! A station has factory slots (flagFactorySlotFirst..Last, manufacturing)
//! and research slots (flagResearchFacilitySlotFirst..Last, every activity).
//! A slot is rented for a period, then an item is installed through
//! GetInventory(containerFactory).Add(itemID, 1, slotID, runs, activity): the
//! item moves into the slot (flag = slotID), ingredients are taken, and the
//! ticker completes the runs one by one, then returns the item.

use std::time::{Duration, SystemTime};

use marshal::Value;
use proto::gpcs::Call;

use super::inventory::{self, Item};
use super::{skills, user_error, wallet, Args, Bound, CallResult, Session};
use crate::{db, rows, Server};

/// TEMPLATE DEFAULT rent per slot per rental period (const.rentalPeriodOffice days).
const SLOT_RENT: f64 = 10_000.0;
/// TEMPLATE DEFAULT seconds for a duplication or reverse-engineering attempt.
const ATTEMPT_SECONDS: i32 = 3_600;
/// TEMPLATE DEFAULT runs on a fresh blueprint copy.
const COPY_RUNS: i32 = 10;

fn slot_range(server: &Server, research: bool) -> (i32, i32) {
    let w = &server.world;
    if research {
        (w.flag("ResearchFacilitySlotFirst"), w.flag("ResearchFacilitySlotLast"))
    } else {
        (w.flag("FactorySlotFirst"), w.flag("FactorySlotLast"))
    }
}

fn is_slot(server: &Server, flag: i32) -> bool {
    let (f0, _) = slot_range(server, false);
    let (_, r1) = slot_range(server, true);
    (f0..=r1).contains(&flag)
}

fn is_research_slot(server: &Server, slot: i32) -> bool {
    let (r0, r1) = slot_range(server, true);
    (r0..=r1).contains(&slot)
}

#[derive(Debug, Clone)]
struct Blueprint {
    type_id: i32,
    product: i32,
    manufacturing_time: i32,
    wastage: f64,
    mineral_time: i32,
    copy_time: i32,
    productivity_time: i32,
    copy: bool,
    mineral_level: i32,
    productivity_level: i32,
    runs_left: i32,
}

async fn blueprint(server: &Server, item_id: i64) -> anyhow::Result<Option<Blueprint>> {
    let c = server.pool.get().await?;
    Ok(c.query_opt(
        r#"SELECT it."typeID", b."productTypeID", coalesce(i."manufacturingTime", 3600), coalesce(i."wastageFactor", 0.1),
                  coalesce(i."researchMineralTime", 3600), coalesce(i."researchCopyTime", 1800),
                  coalesce(i."researchProductivityTime", 3600), coalesce(bp."copy", 0), coalesce(bp."mineralLevel", 0),
                  coalesce(bp."productivityLevel", 0), coalesce(bp."licensedProductionRunsRemaining", -1)
           FROM game.items it JOIN static.bptypes b ON b."blueprintTypeID" = it."typeID"
           LEFT JOIN static.blueprint_info i ON i."blueprintTypeID" = b."blueprintTypeID"
           LEFT JOIN game.blueprints bp ON bp."itemID" = it."itemID"
           WHERE it."itemID" = $1"#,
        &[&item_id],
    )
    .await?
    .map(|r| Blueprint {
        type_id: r.get(0),
        product: r.get(1),
        manufacturing_time: r.get(2),
        wastage: r.get(3),
        mineral_time: r.get(4),
        copy_time: r.get(5),
        productivity_time: r.get(6),
        copy: r.get::<_, i32>(7) != 0,
        mineral_level: r.get(8),
        productivity_level: r.get(9),
        runs_left: r.get(10),
    }))
}

/// What one portion of a type is made of: (materialTypeID, quantity).
pub async fn materials(server: &Server, type_id: i32) -> anyhow::Result<Vec<(i32, i32)>> {
    let c = server.pool.get().await?;
    Ok(c.query(
        r#"SELECT "materialTypeID", "quantity" FROM static.type_materials WHERE "typeID" = $1 ORDER BY 1"#,
        &[&type_id],
    )
    .await?
    .iter()
    .map(|r| (r.get(0), r.get(1)))
    .collect())
}

impl Blueprint {
    /// Mineral research cuts the waste: wastageFactor / (1 + mineralLevel).
    fn waste(&self) -> f64 {
        self.wastage / (1.0 + self.mineral_level as f64)
    }

    async fn per_run(&self, server: &Server) -> anyhow::Result<Vec<(i32, i64)>> {
        Ok(materials(server, self.product)
            .await?
            .into_iter()
            .map(|(m, q)| (m, (q as f64 * (1.0 + self.waste())).ceil() as i64))
            .collect())
    }

    /// Seconds per manufacturing run: productivity research and Industry
    /// each take 5% / 4% off, down to half the base time.
    fn build_seconds(&self, industry: i32) -> i32 {
        let f = (1.0 - 0.05 * self.productivity_level as f64).max(0.5) * (1.0 - 0.04 * industry as f64);
        ((self.manufacturing_time as f64 * f) as i32).max(1)
    }
}

fn type_name(server: &Server, type_id: i32) -> String {
    server.world.type_info(type_id).map(|t| t.name.clone()).unwrap_or_default()
}

/// [KeyVal(mineral=name, amount=n)] for the slot display.
fn ingredient_list(server: &Server, list: &[(i32, i64)], times: i64) -> Value {
    Value::List(
        list.iter()
            .map(|(m, q)| rows::keyval(vec![("mineral", Value::from(type_name(server, *m))), ("amount", Value::from(q * times))]))
            .collect(),
    )
}

struct Rental {
    renter: i32,
    corp: bool,
}

async fn rental(server: &Server, station: i32, slot: i32) -> anyhow::Result<Option<Rental>> {
    let c = server.pool.get().await?;
    Ok(c.query_opt(
        r#"SELECT "renterID", "forCorporateUse" FROM game.factory_slots
           WHERE "stationID" = $1 AND "slotID" = $2 AND "rentedUntil" > now()"#,
        &[&station, &slot],
    )
    .await?
    .map(|r| Rental { renter: r.get(0), corp: r.get(1) }))
}

#[derive(Debug, Clone)]
struct Job {
    id: i32,
    station: i32,
    slot: i32,
    installer: i32,
    owner: i32,
    item_id: i64,
    activity: i32,
    runs: i32,
    runs_done: i32,
    run_seconds: i32,
    output: i64,
    started: SystemTime,
}

fn job_of(r: &tokio_postgres::Row) -> Job {
    Job {
        id: r.get("jobID"),
        station: r.get("stationID"),
        slot: r.get("slotID"),
        installer: r.get("installerID"),
        owner: r.get("ownerID"),
        item_id: r.get("itemID"),
        activity: r.get("activity"),
        runs: r.get("runs"),
        runs_done: r.get("runsDone"),
        run_seconds: r.get("runSeconds"),
        output: r.get::<_, Option<i64>>("outputID").unwrap_or(r.get::<_, i32>("stationID") as i64),
        started: r.get("started"),
    }
}

async fn jobs_where(server: &Server, filter: &str, params: db::Params<'_>) -> anyhow::Result<Vec<Job>> {
    let c = server.pool.get().await?;
    Ok(c.query(&format!(r#"SELECT * FROM game.factory_jobs WHERE {filter}"#), params).await?.iter().map(job_of).collect())
}

async fn slot_job(server: &Server, station: i32, slot: i32) -> anyhow::Result<Option<Job>> {
    Ok(jobs_where(server, r#""stationID" = $1 AND "slotID" = $2"#, &[&station, &slot]).await?.into_iter().next())
}

/// Can the caller work this slot: rented to them, or to their corporation
/// and they manage its factories.
async fn can_use(server: &Server, session: &Session, r: &Option<Rental>) -> anyhow::Result<bool> {
    let me = session.char_id().unwrap_or(0);
    let corp = session.attrs.int("corpid").unwrap_or(0) as i32;
    Ok(match r {
        Some(r) if !r.corp => r.renter == me,
        Some(r) => r.renter == corp && super::corp::has_role(server, me, corp, &["FactoryManager"]).await?,
        None => false,
    })
}

/// The slot as facility.py reads it: slotID, canUse, forCorporateUse,
/// rented, publiclyAvailable, accessAllowed, installedItem (inventory row).
async fn slot_info(server: &Server, session: &Session, station: i32, slot: i32) -> anyhow::Result<Value> {
    let r = rental(server, station, slot).await?;
    let usable = can_use(server, session, &r).await?;
    let job = slot_job(server, station, slot).await?;
    let access = usable || job.is_none();
    let installed = match &job {
        Some(j) if access => inventory::item(server, j.item_id).await?.map(|i| i.row(server)).unwrap_or(Value::None),
        _ => Value::None,
    };
    Ok(rows::keyval(vec![
        ("slotID", Value::Int(slot)),
        ("canUse", Value::from(usable)),
        ("forCorporateUse", Value::from(r.as_ref().is_some_and(|r| r.corp))),
        ("rented", Value::from(r.is_some())),
        ("publiclyAvailable", Value::Int(1)),
        ("accessAllowed", Value::from(access)),
        ("installedItem", installed),
    ]))
}

fn notify_slot(server: &Server, chars: &[i32], station: i32, slot: i32, hint: &str) {
    super::notify_chars(server, chars, "OnSlotStateChanged", vec![Value::Int(station), Value::Int(slot), Value::str(hint), Value::None]);
}

/// factory.GetBlueprintAttributes / GetBlueprintInformation /
/// GetMineralCompositionOfItemType.
pub async fn service(server: &Server, _session: &mut Session, method: &str, call: &Call) -> CallResult {
    let a = Args::of(call);
    match method {
        // A dict: infosvc tests bpi.has_key(...) for each line it shows.
        "GetBlueprintAttributes" => {
            let Some(bp) = blueprint(server, a.int(0).unwrap_or(0)).await? else { return Ok(Value::Dict(vec![])) };
            Ok(Value::Dict(
                [
                    ("manufacturingTime", Value::Int(bp.manufacturing_time)),
                    ("mineralLevel", Value::Int(bp.mineral_level)),
                    ("wastageFactor", Value::Float(bp.waste())),
                    ("researchMineralTime", Value::Int(bp.mineral_time)),
                    ("researchCopyTime", Value::Int(bp.copy_time)),
                    ("productTypeID", Value::Int(bp.product)),
                    ("copy", Value::from(bp.copy)),
                    ("researchProductivityTime", Value::Int(bp.productivity_time)),
                    ("productivityLevel", Value::Int(bp.productivity_level)),
                ]
                .into_iter()
                .map(|(k, v)| (Value::str(k), v))
                .collect(),
            ))
        }
        "GetBlueprintInformation" => {
            let Some(bp) = blueprint(server, a.int(0).unwrap_or(0)).await? else { return Ok(Value::None) };
            Ok(rows::keyval(vec![
                ("blueprintTypeID", Value::Int(bp.type_id)),
                ("productTypeID", Value::Int(bp.product)),
                ("copy", Value::from(bp.copy)),
                ("mineralLevel", Value::Int(bp.mineral_level)),
                ("productivityLevel", Value::Int(bp.productivity_level)),
                ("licensedProductionRunsRemaining", Value::Int(bp.runs_left)),
                ("wastageFactor", Value::Float(bp.waste())),
                ("manufacturingTime", Value::Int(bp.manufacturing_time)),
            ]))
        }
        // [Row(typeID, quantity)] per portion of the type.
        "GetMineralCompositionOfItemType" => Ok(rows::rowset(
            &["typeID", "quantity"],
            materials(server, a.i32(0).unwrap_or(0)).await?.into_iter().map(|(t, q)| vec![Value::Int(t), Value::Int(q)]).collect(),
        )),
        m => super::unimplemented("factory", m, call),
    }
}

/// The bound factory manager of a station.
pub async fn bound(server: &Server, session: &mut Session, bound: &Bound, call: &Call) -> CallResult {
    let a = Args::of(call);
    let station = bound.params.as_int().unwrap_or(0) as i32;
    match call.method() {
        "RegisterForSlotStateChanges" | "UnregisterForSlotStateChanges" => Ok(Value::None),
        "GetFactorySlots" | "GetResearchFacilitySlots" => {
            let (first, last) = slot_range(server, call.method() == "GetResearchFacilitySlots");
            let mut out = Vec::new();
            for slot in first..=last {
                out.push(slot_info(server, session, station, slot).await?);
            }
            Ok(Value::List(out))
        }
        "GetFactorySlot" => Ok(slot_info(server, session, station, a.i32(0).unwrap_or(0)).await?),
        // GetFactoryStatus(installedItemID) -> dict
        "GetFactoryStatus" => {
            let item_id = a.int(0).unwrap_or(0);
            let Some(j) = jobs_where(server, r#""itemID" = $1"#, &[&item_id]).await?.into_iter().next() else {
                return Ok(Value::None);
            };
            status(server, &j).await
        }
        // EjectItemInSlot(slotID, message, ...): stops the job and returns
        // the item; ingredients of runs not started come back too.
        "EjectItemInSlot" => {
            let me = session.char_id().unwrap_or(0);
            let corp = session.attrs.int("corpid").unwrap_or(0) as i32;
            let Some(j) = slot_job(server, station, a.i32(0).unwrap_or(0)).await? else { return Ok(Value::None) };
            let manager = super::corp::has_role(server, me, corp, &["FactoryManager"]).await?
                && server.world.stations.get(&station).is_some_and(|s| s.corporation_id == corp);
            if j.owner != me && j.owner != corp && !manager {
                return Err(user_error("CrpAccessDenied", vec![]));
            }
            let left = (j.runs - runs_finished(&j)).max(0) as i64;
            if j.activity == server.world.constant("activityManufacturing") && left > 0 {
                if let Some(bp) = blueprint(server, j.item_id).await? {
                    for (m, q) in bp.per_run(server).await? {
                        inventory::give(server, m, j.owner, j.output, server.world.flag("Hangar"), (q * left) as i32).await?;
                    }
                }
            }
            finish(server, &j, false).await?;
            if let Some(msg) = a.text(1).filter(|_| j.owner != me) {
                super::notify_chars(server, &[j.installer], "OnRemoteMessage", vec![Value::str("CustomInfo"), Value::dict([(Value::str("info"), Value::from(msg))])]);
            }
            Ok(Value::None)
        }
        m => super::unimplemented("factory[station]", m, call),
    }
}

fn runs_finished(j: &Job) -> i32 {
    let elapsed = SystemTime::now().duration_since(j.started).unwrap_or_default().as_secs() as i64;
    ((elapsed / j.run_seconds.max(1) as i64) as i32).min(j.runs)
}

/// The installed job's state (facility.py DisplayFactory).
async fn status(server: &Server, j: &Job) -> CallResult {
    let w = &server.world;
    let elapsed = SystemTime::now().duration_since(j.started).unwrap_or_default().as_secs() as i64;
    let total = j.runs as i64 * j.run_seconds as i64;
    let done = runs_finished(j);
    let item = inventory::item(server, j.item_id).await?;
    let bp = blueprint(server, j.item_id).await?;
    let manufacturing = j.activity == w.constant("activityManufacturing");
    let per_run = match &bp {
        Some(bp) if manufacturing => bp.per_run(server).await?,
        _ => Vec::new(),
    };
    let what = match &bp {
        Some(bp) if manufacturing => type_name(server, bp.product),
        _ => item.as_ref().map(|i| type_name(server, i.type_id)).unwrap_or_default(),
    };
    let batch_left = if done >= j.runs { 0 } else { j.run_seconds as i64 - (elapsed - done as i64 * j.run_seconds as i64) };
    Ok(Value::Dict(
        [
            ("activity", Value::Int(j.activity)),
            ("manufacturing", Value::from(what)),
            ("secondsTotal", Value::from(total)),
            ("secondsRemaining", Value::from((total - elapsed).max(0))),
            ("iterationsCompleted", Value::Int(done)),
            ("iterationsRequested", Value::Int(j.runs)),
            ("batchSecondsTotal", Value::Int(j.run_seconds)),
            ("batchSecondsRemaining", Value::from(batch_left.max(0))),
            ("ingridientsTotal", ingredient_list(server, &per_run, j.runs as i64)),
            ("ingridientsRequired", Value::List(vec![])),
            ("ingridientsPerBatch", ingredient_list(server, &per_run, 1)),
            ("batchProductionTime", Value::Int(j.run_seconds)),
            ("wastageFactor", Value::Float(bp.as_ref().map(|b| b.waste()).unwrap_or(0.0))),
        ]
        .into_iter()
        .map(|(k, v)| (Value::str(k), v))
        .collect(),
    ))
}

/// The hangar an item at this station sits in: the station itself, or a
/// corporation office there.
async fn hangar_of(server: &Server, station: i32, item: &Item) -> anyhow::Result<Option<i64>> {
    if item.location_id == station as i64 {
        return Ok(Some(station as i64));
    }
    Ok(db::scalar::<i64>(
        &server.pool,
        r#"SELECT "officeID" FROM game.offices WHERE "officeID" = $1 AND "stationID" = $2"#,
        &[&item.location_id, &station],
    )
    .await?)
}

/// Units of a type an owner keeps in a hangar.
async fn held(server: &Server, owner: i32, hangar: i64, type_id: i32) -> anyhow::Result<Vec<Item>> {
    inventory::items_where(
        server,
        r#""ownerID" = $1 AND "locationID" = $2 AND "typeID" = $3 AND "flag" = $4"#,
        &[&owner, &hangar, &type_id, &server.world.flag("Hangar")],
    )
    .await
}

/// GetInventory(containerFactory).Add(itemID, 1, slotID, runs, activity).
pub async fn install(server: &Server, session: &mut Session, call: &Call) -> CallResult {
    let a = Args::of(call);
    let w = &server.world;
    let me = session.char_id().unwrap_or(0);
    let item_id = a.int(0).unwrap_or(0);
    let slot = a.get(2, "flag").and_then(Value::as_int).unwrap_or(0) as i32;
    let runs = a.get(3, "qty").and_then(Value::as_int).unwrap_or(1).max(1) as i32;
    let activity = a.get(4, "activity").and_then(Value::as_int).unwrap_or(w.constant("activityManufacturing") as i64) as i32;
    let Some(station) = session.attrs.int("stationid").map(|s| s as i32) else { return Ok(Value::None) };
    let info = |s: &str| user_error("CustomInfo", vec![("info", Value::from(s.to_owned()))]);
    if !is_slot(server, slot) {
        return Err(info("That is not a factory slot."));
    }
    let r = rental(server, station, slot).await?;
    if !can_use(server, session, &r).await? {
        return Err(info("You must rent this slot before installing anything in it."));
    }
    if slot_job(server, station, slot).await?.is_some() {
        return Err(info("Something is already installed in this slot."));
    }
    let research = is_research_slot(server, slot);
    // base_factories / base_researchfacility DoSkillChecks: the skill must
    // be known (injected), at any level.
    let skill = if research { "Science" } else { "Industry" };
    let known: Option<i64> = db::scalar(
        &server.pool,
        r#"SELECT "itemID" FROM game.items WHERE "locationID" = $1 AND "typeID" = $2 AND "flag" = $3 LIMIT 1"#,
        &[&(me as i64), &w.type_id(skill), &w.flag("Skill")],
    )
    .await?;
    if known.is_none() {
        return Err(info(&format!("You need the {skill} skill to use this facility.")));
    }
    let Some(it) = inventory::item(server, item_id).await? else { return Ok(Value::None) };
    let corp = session.attrs.int("corpid").unwrap_or(0) as i32;
    if it.owner_id != me && !(it.owner_id == corp && r.as_ref().is_some_and(|r| r.corp)) {
        return Err(user_error("PermissionDenied", vec![]));
    }
    let Some(hangar) = hangar_of(server, station, &it).await? else {
        return Err(info("The item must be in a hangar at this station."));
    };
    let act = |n: &str| w.constant(n);
    let bp = blueprint(server, item_id).await?;
    let mut runs = runs;
    let run_seconds = if activity == act("activityManufacturing") {
        let Some(bp) = &bp else { return Err(info("Only blueprints can be used to manufacture.")) };
        let product_cat = w.type_info(bp.product).map(|t| t.category_id).unwrap_or(0);
        let lab_only = [w.category_id("Skill"), w.category_id("Implant")].contains(&product_cat);
        if lab_only != research {
            return Err(info(if research { "Use a factory to manufacture this." } else { "Use a research facility to manufacture this." }));
        }
        if bp.copy && bp.runs_left >= 0 {
            if bp.runs_left == 0 {
                return Err(info("This blueprint copy has no production runs left."));
            }
            runs = runs.min(bp.runs_left);
        }
        // Every run's ingredients are taken now (IngredientsMissingForBlueprint).
        let need: Vec<(i32, i64)> = bp.per_run(server).await?.into_iter().map(|(m, q)| (m, q * runs as i64)).collect();
        let mut missing = Vec::new();
        for (m, q) in &need {
            let have: i64 = held(server, it.owner_id, hangar, *m).await?.iter().map(|i| i.quantity as i64).sum();
            if have < *q {
                missing.push(format!("{} x {}", q - have, type_name(server, *m)));
            }
        }
        if !missing.is_empty() {
            return Err(user_error("IngredientsMissingForBlueprint", vec![("minerals", Value::from(missing.join("<br>")))]));
        }
        for (m, mut q) in need {
            for stack in held(server, it.owner_id, hangar, m).await? {
                if q <= 0 {
                    break;
                }
                if stack.quantity as i64 <= q {
                    q -= stack.quantity as i64;
                    inventory::delete_item(server, &stack).await?;
                } else {
                    inventory::set_quantity(server, &stack, stack.quantity - q as i32).await?;
                    q = 0;
                }
            }
        }
        bp.build_seconds(skills::skill_level(server, me, w.type_id("Industry")).await?)
    } else if [act("activityCopying"), act("activityResearchingMineralProductivity"), act("activityResearchingTimeProductivity")].contains(&activity) {
        if !research {
            return Err(info("Research can not be done in a factory."));
        }
        let Some(bp) = &bp else { return Err(info("Only blueprints can be researched or copied.")) };
        if bp.copy {
            return Err(info("Activity unavailable for copied blueprint."));
        }
        match activity {
            a if a == act("activityCopying") => bp.copy_time,
            a if a == act("activityResearchingMineralProductivity") => bp.mineral_time * (1 + bp.mineral_level),
            _ => bp.productivity_time * (1 + bp.productivity_level),
        }
    } else if [act("activityDuplicating"), act("activityReverseEngineering")].contains(&activity) {
        if !research {
            return Err(info("Research can not be done in a factory."));
        }
        let cat = it.category(server);
        let ok = ["Ship", "Module", "Charge", "Implant", "Drone"].iter().any(|c| w.category_id(c) == cat);
        if !ok || Some(it.item_id) == session.attrs.int("shipid") {
            return Err(user_error("NoItemsAvailableForActivity", vec![("activityName", Value::str("this activity"))]));
        }
        runs = 1;
        ATTEMPT_SECONDS
    } else {
        return Err(user_error("UnknownActivity", vec![]));
    };
    // One unit goes into the slot.
    let unit = inventory::split_one(server, &it).await?;
    inventory::move_item(server, &unit, station as i64, slot).await?;
    db::exec(
        &server.pool,
        r#"INSERT INTO game.factory_jobs ("stationID", "slotID", "installerID", "ownerID", "itemID", "activity", "runs", "runSeconds", "outputID")
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)"#,
        &[&station, &slot, &me, &unit.owner_id, &unit.item_id, &activity, &runs, &run_seconds, &hangar],
    )
    .await?;
    notify_slot(server, &[me], station, slot, "installed");
    Ok(Value::None)
}

/// Results of `n` finished runs.
async fn deliver(server: &Server, j: &Job, n: i32) -> anyhow::Result<()> {
    let w = &server.world;
    let act = |s: &str| w.constant(s);
    let hangar = w.flag("Hangar");
    let Some(it) = inventory::item(server, j.item_id).await? else { return Ok(()) };
    let bp = blueprint(server, j.item_id).await?;
    let mut created = Vec::new();
    match j.activity {
        a if a == act("activityManufacturing") => {
            let Some(bp) = bp else { return Ok(()) };
            let t = w.type_info(bp.product);
            let portion: i32 = db::scalar(&server.pool, r#"SELECT "portionSize" FROM static.types WHERE "typeID" = $1"#, &[&bp.product])
                .await?
                .unwrap_or(1)
                .max(1);
            if t.is_some_and(|t| t.category_id == w.category_id("Ship")) {
                for _ in 0..n * portion {
                    created.push(inventory::create_item(server, bp.product, j.owner, j.output, hangar, 1, true).await?);
                }
            } else {
                inventory::give(server, bp.product, j.owner, j.output, hangar, n * portion).await?;
            }
            if bp.copy && bp.runs_left > 0 {
                db::exec(
                    &server.pool,
                    r#"UPDATE game.blueprints SET "licensedProductionRunsRemaining" = greatest(0, "licensedProductionRunsRemaining" - $2) WHERE "itemID" = $1"#,
                    &[&j.item_id, &n],
                )
                .await?;
            }
        }
        a if a == act("activityCopying") => {
            let Some(bp) = bp else { return Ok(()) };
            for _ in 0..n {
                let copy = inventory::create_item(server, it.type_id, j.owner, j.output, hangar, 1, true).await?;
                db::exec(
                    &server.pool,
                    r#"INSERT INTO game.blueprints VALUES ($1, 1, $2, $3, $4)"#,
                    &[&copy.item_id, &bp.mineral_level, &bp.productivity_level, &COPY_RUNS],
                )
                .await?;
                created.push(copy);
            }
        }
        a if a == act("activityResearchingMineralProductivity") || a == act("activityResearchingTimeProductivity") => {
            let col = if a == act("activityResearchingMineralProductivity") { "mineralLevel" } else { "productivityLevel" };
            db::exec(
                &server.pool,
                &format!(
                    r#"INSERT INTO game.blueprints ("itemID", "{col}") VALUES ($1, $2)
                       ON CONFLICT ("itemID") DO UPDATE SET "{col}" = game.blueprints."{col}" + $2"#
                ),
                &[&j.item_id, &n],
            )
            .await?;
        }
        // Chance = base chance x skill level (facility.py AskAcceptProductionJobRisk).
        a if a == act("activityDuplicating") => {
            let base: f64 = db::scalar(&server.pool, r#"SELECT "chanceOfDuplicating" FROM static.types WHERE "typeID" = $1"#, &[&it.type_id])
                .await?
                .unwrap_or(0.0);
            let chance = base * skills::skill_level(server, j.installer, w.type_id("Duplicating")).await? as f64;
            if super::combat::roll(j.id as i64 * 7919 + j.item_id) < chance {
                created.push(inventory::create_item(server, it.type_id, j.owner, j.output, hangar, 1, it.singleton != 0).await?);
            }
        }
        a if a == act("activityReverseEngineering") => {
            let found: Option<(i32, f64)> = {
                let c = server.pool.get().await?;
                c.query_opt(
                    r#"SELECT "blueprintTypeID", "chanceOfReverseEngineering" FROM static.bptypes WHERE "productTypeID" = $1 LIMIT 1"#,
                    &[&it.type_id],
                )
                .await?
                .map(|r| (r.get(0), r.get(1)))
            };
            if let Some((bp_type, base)) = found {
                let chance = base * skills::skill_level(server, j.installer, w.type_id("ReverseEngineering")).await? as f64;
                if super::combat::roll(j.id as i64 * 104_729 + j.item_id) < chance {
                    created.push(inventory::create_item(server, bp_type, j.owner, j.output, hangar, 1, true).await?);
                }
            }
        }
        _ => {}
    }
    for c in &created {
        inventory::notify_new(server, c);
    }
    Ok(())
}

/// Ends a job: the item goes back to its hangar (a reverse-engineered item is
/// used up) and the slot is free again.
async fn finish(server: &Server, j: &Job, completed: bool) -> anyhow::Result<()> {
    db::exec(&server.pool, r#"DELETE FROM game.factory_jobs WHERE "jobID" = $1"#, &[&j.id]).await?;
    if let Some(it) = inventory::item(server, j.item_id).await? {
        if completed && j.activity == server.world.constant("activityReverseEngineering") {
            inventory::delete_item(server, &it).await?;
        } else if it.category(server) == server.world.category_id("Blueprint") {
            // Blueprints never stack: each keeps its own research levels.
            inventory::move_item(server, &it, j.output, server.world.flag("Hangar")).await?;
        } else {
            inventory::stack_into(server, &it, j.output, server.world.flag("Hangar")).await?;
        }
    }
    notify_slot(server, &[j.installer], j.station, j.slot, if completed { "completed" } else { "ejected" });
    Ok(())
}

/// Advances every job (called from the skill ticker).
pub async fn tick(server: &Server) -> anyhow::Result<()> {
    for j in jobs_where(server, "true", &[]).await? {
        let done = runs_finished(&j);
        if done > j.runs_done {
            deliver(server, &j, done - j.runs_done).await?;
            db::exec(&server.pool, r#"UPDATE game.factory_jobs SET "runsDone" = $2 WHERE "jobID" = $1"#, &[&j.id, &done]).await?;
            if done < j.runs {
                notify_slot(server, &[j.installer], j.station, j.slot, "batch");
            }
        }
        if done >= j.runs {
            finish(server, &j, true).await?;
        }
    }
    Ok(())
}

/// Hide items installed in slots from the hangar list.
pub fn hangar_filter(server: &Server) -> String {
    let (first, _) = slot_range(server, false);
    let (_, last) = slot_range(server, true);
    format!(r#""flag" NOT BETWEEN {first} AND {last}"#)
}

/// corpStationMgr slot rental:
/// AreAny{Factory,ResearchFacility}SlotsAvailableForRenting(forCorpUse),
/// GetQuoteForRentingA{Factory,ResearchFacility}Slot(forCorpUse),
/// Rent{Factory,ResearchFacility}Slot(cost, forCorpUse) -> slotID,
/// CancelRentOfFactorySlot(slotID).
pub async fn rental_call(server: &Server, session: &mut Session, method: &str, call: &Call) -> CallResult {
    let a = Args::of(call);
    let me = session.char_id().unwrap_or(0);
    let corp = session.attrs.int("corpid").unwrap_or(0) as i32;
    let Some(station) = session.attrs.int("stationid").map(|s| s as i32) else { return Ok(Value::None) };
    let research = method.contains("ResearchFacility");
    let (first, last) = slot_range(server, research);
    let free = || async move {
        let c = server.pool.get().await?;
        let taken: Vec<i32> = c
            .query(
                r#"SELECT "slotID" FROM game.factory_slots WHERE "stationID" = $1 AND "rentedUntil" > now()
                   UNION SELECT "slotID" FROM game.factory_jobs WHERE "stationID" = $1"#,
                &[&station],
            )
            .await?
            .iter()
            .map(|r| r.get(0))
            .collect();
        anyhow::Ok((first..=last).find(|s| !taken.contains(s)))
    };
    let for_corp = |i: usize| a.get(i, "forCorpUse").and_then(Value::as_int).unwrap_or(0) != 0;
    match method {
        m if m.starts_with("AreAny") => {
            let corp_ok = !for_corp(0) || (super::corp::is_player_corp(corp) && super::corp::has_role(server, me, corp, &["FactoryManager"]).await?);
            Ok(Value::from(corp_ok && free().await?.is_some()))
        }
        m if m.starts_with("GetQuoteForRenting") => Ok(Value::Float(SLOT_RENT)),
        m if m.starts_with("Rent") => {
            let corp_use = for_corp(1);
            if corp_use && !super::corp::has_role(server, me, corp, &["FactoryManager"]).await? {
                return Err(user_error("CrpAccessDenied", vec![]));
            }
            let Some(slot) = free().await? else { return Ok(Value::None) };
            let renter = if corp_use { corp } else { me };
            let landlord = server.world.stations.get(&station).map(|s| s.corporation_id).unwrap_or(0);
            wallet::transfer(server, renter, landlord, SLOT_RENT, "refFactorySlotRentalFee", Some(station as i64), "").await?;
            let days = Duration::from_secs(86_400 * server.world.constant("rentalPeriodOffice").max(1) as u64);
            db::exec(
                &server.pool,
                r#"INSERT INTO game.factory_slots VALUES ($1, $2, $3, $4, $5)
                   ON CONFLICT ("stationID", "slotID") DO UPDATE SET "renterID" = $3, "forCorporateUse" = $4, "rentedUntil" = $5"#,
                &[&station, &slot, &renter, &corp_use, &(SystemTime::now() + days)],
            )
            .await?;
            super::notify_chars(server, &[me], "OnFactorySlotRentalChanged", vec![Value::Int(slot)]);
            Ok(Value::Int(slot))
        }
        "CancelRentOfFactorySlot" => {
            let slot = a.i32(0).unwrap_or(0);
            let r = rental(server, station, slot).await?;
            if !can_use(server, session, &r).await? {
                return Err(user_error("CrpAccessDenied", vec![]));
            }
            if slot_job(server, station, slot).await?.is_some() {
                return Err(user_error("CustomInfo", vec![("info", Value::str("Uninstall the item in this slot first."))]));
            }
            db::exec(&server.pool, r#"DELETE FROM game.factory_slots WHERE "stationID" = $1 AND "slotID" = $2"#, &[&station, &slot]).await?;
            super::notify_chars(server, &[me], "OnFactorySlotRentalChanged", vec![Value::Int(slot)]);
            Ok(Value::None)
        }
        m => super::unimplemented("corpStationMgr", m, call),
    }
}
