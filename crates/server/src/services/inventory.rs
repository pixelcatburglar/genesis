//! Inventory: the `invbroker` bound object and the inventories it hands out
//! (environment/invCache.py, util/moniker.py GetInventoryMgr, ui/deco/
//! container.py, item.py).
//!
//! `invbroker` binds on (stationID, groupStation) or (solarsystemID,
//! groupSolarSystem). GetInventory(containerID, ownerID) and
//! GetInventoryFromId(itemID, passive) each return a NEW bound object whose
//! List() returns util.Row items with the client's hardcoded inventory header.
//!
//! Every change is announced as OnItemChange(item, {ix: oldValue}) to the
//! owner: invCache rebuilds the old row from the change dict to find which
//! cached containers to update, and drops items whose quantity is 0.

use anyhow::anyhow;
use marshal::Value;
use proto::gpcs::Call;

use super::{user_error, Args, Bound, CallError, CallResult, Session};
use crate::{db, rows, Server};

/// godma.py:339 / cfg.py ItemsRecordset2.
pub const INV_HEADER: [&str; 11] = [
    "itemID",
    "typeID",
    "ownerID",
    "locationID",
    "flag",
    "contraband",
    "singleton",
    "quantity",
    "groupID",
    "categoryID",
    "customInfo",
];

#[derive(Debug, Clone)]
pub struct Item {
    pub item_id: i64,
    pub type_id: i32,
    pub owner_id: i32,
    pub location_id: i64,
    pub flag: i32,
    pub contraband: i32,
    pub singleton: i32,
    pub quantity: i32,
    pub custom_info: Option<String>,
}

impl Item {
    fn from_row(r: &tokio_postgres::Row) -> Item {
        Item {
            item_id: r.get("itemID"),
            type_id: r.get("typeID"),
            owner_id: r.get("ownerID"),
            location_id: r.get("locationID"),
            flag: r.get("flag"),
            contraband: r.get("contraband"),
            singleton: r.get("singleton"),
            quantity: r.get("quantity"),
            custom_info: r.get("customInfo"),
        }
    }

    pub fn row(&self, server: &Server) -> Value {
        rows::row(&INV_HEADER, self.line(server))
    }

    /// The row's values in INV_HEADER order.
    pub fn line(&self, server: &Server) -> Vec<Value> {
        let t = server.world.type_info(self.type_id);
        vec![
            Value::from(self.item_id),
            Value::Int(self.type_id),
            Value::Int(self.owner_id),
            Value::from(self.location_id),
            Value::Int(self.flag),
            Value::Int(self.contraband),
            Value::Int(self.singleton),
            Value::Int(self.quantity),
            t.map(|t| Value::Int(t.group_id)).unwrap_or(Value::None),
            t.map(|t| Value::Int(t.category_id)).unwrap_or(Value::None),
            self.custom_info
                .as_deref()
                .map(Value::str)
                .unwrap_or(Value::None),
        ]
    }

    pub fn volume(&self, server: &Server) -> f64 {
        self.quantity as f64
            * server
                .world
                .type_info(self.type_id)
                .map(|t| t.volume)
                .unwrap_or(0.0)
    }

    pub fn category(&self, server: &Server) -> i32 {
        server
            .world
            .type_info(self.type_id)
            .map(|t| t.category_id)
            .unwrap_or(0)
    }
}

pub async fn items_where(
    server: &Server,
    filter: &str,
    params: &[&(dyn tokio_postgres::types::ToSql + Sync)],
) -> anyhow::Result<Vec<Item>> {
    let c = server.pool.get().await?;
    let rs = c
        .query(
            &format!(r#"SELECT * FROM game.items WHERE {filter} ORDER BY "itemID""#),
            params,
        )
        .await?;
    Ok(rs.iter().map(Item::from_row).collect())
}

pub async fn item(server: &Server, item_id: i64) -> anyhow::Result<Option<Item>> {
    Ok(items_where(server, r#""itemID" = $1"#, &[&item_id])
        .await?
        .into_iter()
        .next())
}

/// The character as an inventory item: itemID = characterID, the Character
/// type, located where the character is (godma CharGetInfo's invItem).
pub async fn character_item(server: &Server, char_id: i32) -> anyhow::Result<Option<Item>> {
    let c = server.pool.get().await?;
    let r = c
        .query_opt(
            r#"SELECT coalesce("stationID", "solarSystemID") FROM game.characters WHERE "characterID" = $1"#,
            &[&char_id],
        )
        .await?;
    Ok(r.map(|r| Item {
        item_id: char_id as i64,
        type_id: 1,
        owner_id: char_id,
        location_id: r.get::<_, i32>(0) as i64,
        flag: server.world.flag("Pilot"),
        contraband: 0,
        singleton: 1,
        quantity: 1,
        custom_info: None,
    }))
}

/// Inserts an item and returns it.
pub async fn create_item(
    server: &Server,
    type_id: i32,
    owner_id: i32,
    location_id: i64,
    flag: i32,
    quantity: i32,
    singleton: bool,
) -> anyhow::Result<Item> {
    let c = server.pool.get().await?;
    let r = c
        .query_one(
            r#"INSERT INTO game.items ("typeID", "ownerID", "locationID", "flag", "quantity", "singleton")
               VALUES ($1, $2, $3, $4, $5, $6) RETURNING *"#,
            &[&type_id, &owner_id, &location_id, &flag, &quantity, &(singleton as i32)],
        )
        .await?;
    Ok(Item::from_row(&r))
}

/// invCache.OnItemChange(item, change): `change` maps const.ix* column indices
/// to the OLD values. Sent to every connected client of the owner (and of the
/// previous owner, when it changed).
pub fn notify_change(server: &Server, item: &Item, change: Vec<(&str, Value)>) {
    let old_owner = change
        .iter()
        .find(|(ix, _)| *ix == "ixOwnerID")
        .and_then(|(_, v)| v.as_int())
        .map(|o| o as i32);
    let change = Value::Dict(
        change
            .into_iter()
            .map(|(ix, old)| (Value::Int(server.world.constant(ix)), old))
            .collect(),
    );
    let row = item.row(server);
    let mut owners = vec![item.owner_id];
    owners.extend(old_owner.filter(|&o| o != item.owner_id));
    super::notify_chars(server, &owners, "OnItemChange", vec![row, change]);
}

/// A new item: it "moved" from nowhere.
pub fn notify_new(server: &Server, item: &Item) {
    notify_change(
        server,
        item,
        vec![
            ("ixLocationID", Value::Int(0)),
            ("ixQuantity", Value::Int(0)),
        ],
    );
}

/// Moves an item (all of it) to a location/flag.
pub async fn move_item(
    server: &Server,
    item: &Item,
    location: i64,
    flag: i32,
) -> anyhow::Result<Item> {
    db::exec(
        &server.pool,
        r#"UPDATE game.items SET "locationID" = $2, "flag" = $3 WHERE "itemID" = $1"#,
        &[&item.item_id, &location, &flag],
    )
    .await?;
    let mut new = item.clone();
    new.location_id = location;
    new.flag = flag;
    notify_change(
        server,
        &new,
        vec![
            ("ixLocationID", Value::from(item.location_id)),
            ("ixFlag", Value::Int(item.flag)),
        ],
    );
    Ok(new)
}

pub async fn set_quantity(server: &Server, item: &Item, quantity: i32) -> anyhow::Result<Item> {
    db::exec(
        &server.pool,
        r#"UPDATE game.items SET "quantity" = $2 WHERE "itemID" = $1"#,
        &[&item.item_id, &quantity],
    )
    .await?;
    let mut new = item.clone();
    new.quantity = quantity;
    notify_change(
        server,
        &new,
        vec![("ixQuantity", Value::Int(item.quantity))],
    );
    Ok(new)
}

pub async fn set_owner(server: &Server, item: &Item, owner: i32) -> anyhow::Result<Item> {
    db::exec(
        &server.pool,
        r#"UPDATE game.items SET "ownerID" = $2 WHERE "itemID" = $1"#,
        &[&item.item_id, &owner],
    )
    .await?;
    let mut new = item.clone();
    new.owner_id = owner;
    notify_change(server, &new, vec![("ixOwnerID", Value::Int(item.owner_id))]);
    Ok(new)
}

/// Removes an item: the client sees it leave for the junkyard with nothing left.
pub async fn delete_item(server: &Server, item: &Item) -> anyhow::Result<()> {
    db::exec(
        &server.pool,
        r#"DELETE FROM game.items WHERE "itemID" = $1"#,
        &[&item.item_id],
    )
    .await?;
    let mut gone = item.clone();
    gone.location_id = server.world.constant("locationJunkyard") as i64;
    gone.quantity = 0;
    notify_change(
        server,
        &gone,
        vec![
            ("ixLocationID", Value::from(item.location_id)),
            ("ixQuantity", Value::Int(item.quantity)),
        ],
    );
    Ok(())
}

/// Splits `qty` off a stack into a new item at the same place.
pub async fn split(server: &Server, item: &Item, qty: i32) -> anyhow::Result<Item> {
    if qty >= item.quantity {
        return Ok(item.clone());
    }
    set_quantity(server, item, item.quantity - qty).await?;
    let part = create_item(
        server,
        item.type_id,
        item.owner_id,
        item.location_id,
        item.flag,
        qty,
        false,
    )
    .await?;
    notify_new(server, &part);
    Ok(part)
}

pub async fn split_one(server: &Server, item: &Item) -> anyhow::Result<Item> {
    split(server, item, 1).await
}

/// Gives `qty` units of a type: onto a same-typed stack there when one
/// exists, else as a new stack. Either way the owner is told.
pub async fn give(server: &Server, type_id: i32, owner: i32, location: i64, flag: i32, qty: i32) -> anyhow::Result<Item> {
    let existing = items_where(
        server,
        r#""locationID" = $1 AND "flag" = $2 AND "typeID" = $3 AND "ownerID" = $4 AND "singleton" = 0"#,
        &[&location, &flag, &type_id, &owner],
    )
    .await?;
    if let Some(dst) = existing.into_iter().next() {
        return set_quantity(server, &dst, dst.quantity + qty).await;
    }
    let it = create_item(server, type_id, owner, location, flag, qty, false).await?;
    notify_new(server, &it);
    Ok(it)
}

/// Puts a stack onto a same-typed stack at the destination when one exists.
pub async fn stack_into(
    server: &Server,
    item: &Item,
    location: i64,
    flag: i32,
) -> anyhow::Result<Item> {
    if item.singleton == 0 {
        let existing = items_where(
            server,
            r#""locationID" = $1 AND "flag" = $2 AND "typeID" = $3 AND "ownerID" = $4 AND "singleton" = 0 AND "itemID" <> $5"#,
            &[&location, &flag, &item.type_id, &item.owner_id, &item.item_id],
        )
        .await?;
        if let Some(dst) = existing.into_iter().next() {
            let merged = set_quantity(server, &dst, dst.quantity + item.quantity).await?;
            delete_item(server, item).await?;
            return Ok(merged);
        }
    }
    move_item(server, item, location, flag).await
}

/// What an inventory bound object is a view of.
#[derive(Debug, Clone)]
enum Container {
    /// GetInventory(const.container*, owner): a station-wide container.
    Special { container: i64, owner: Option<i64> },
    /// GetInventoryFromId(itemID): the contents of an item (ship, character...).
    Item(i64),
}

fn container_of(bound: &Bound) -> Option<Container> {
    let p = bound.params.as_seq()?;
    match p.first()?.as_str()? {
        "special" => Some(Container::Special {
            container: p.get(1)?.as_int()?,
            owner: p.get(2).and_then(Value::as_int),
        }),
        "item" => Some(Container::Item(p.get(1)?.as_int()?)),
        _ => None,
    }
}

/// Can the caller reach this item: in my hangar or my corp's office here, in
/// my ship, on my character, or inside a container in one of those.
async fn reachable(server: &Server, session: &Session, item: &Item) -> anyhow::Result<bool> {
    let me = session.char_id().unwrap_or(0) as i64;
    let corp = session.attrs.int("corpid").unwrap_or(0);
    let ship = session.attrs.int("shipid");
    let here = [session.attrs.int("stationid"), ship, Some(me)];
    let mine = item.owner_id as i64 == me || item.owner_id as i64 == corp;
    let mut loc = item.location_id;
    for _ in 0..4 {
        if here.contains(&Some(loc)) {
            return Ok(mine || ship == Some(loc));
        }
        match self::item(server, loc).await? {
            Some(parent) => loc = parent.location_id,
            None => return Ok(false),
        }
    }
    Ok(false)
}

/// invbroker[(locationID, groupID)] methods.
pub async fn broker(
    server: &Server,
    session: &mut Session,
    _bound: &Bound,
    call: &Call,
) -> CallResult {
    let a = Args::of(call);
    let me = session.char_id().unwrap_or(0);
    let w = &server.world;
    match call.method() {
        "GetInventory" => {
            let container = a.int(0).ok_or_else(|| anyhow!("GetInventory without id"))?;
            let owner = a.int(1);
            let params = Value::tuple([
                Value::str("special"),
                Value::from(container),
                owner.map(Value::from).unwrap_or(Value::None),
            ]);
            Ok(session.bind_value(server, "inventory", params))
        }
        "GetInventoryFromId" => {
            let item_id = a
                .int(0)
                .ok_or_else(|| anyhow!("GetInventoryFromId without id"))?;
            Ok(session.bind_value(
                server,
                "inventory",
                Value::tuple([Value::str("item"), Value::from(item_id)]),
            ))
        }
        // AssembleCargoContainer(itemID, ...) / AssembleConstructionPlatform(itemID, ...):
        // a packaged container becomes a singleton that can hold things.
        "AssembleCargoContainer" | "AssembleConstructionPlatform" => {
            let Some(it) = item(server, a.int(0).unwrap_or(0))
                .await?
                .filter(|i| i.owner_id == me)
            else {
                return Ok(Value::None);
            };
            let it = split_one(server, &it).await?;
            db::exec(
                &server.pool,
                r#"UPDATE game.items SET "singleton" = 1 WHERE "itemID" = $1"#,
                &[&it.item_id],
            )
            .await?;
            let mut new = it.clone();
            new.singleton = 1;
            notify_change(server, &new, vec![("ixSingleton", Value::Int(0))]);
            Ok(Value::None)
        }
        // AtmWithdraw(amount): cash leaves the wallet as a stack of Credits in the hangar.
        "AtmWithdraw" => {
            let amount = a.int(0).unwrap_or(0).clamp(0, i32::MAX as i64) as i32;
            let Some(station) = session.attrs.int("stationid") else {
                return Ok(Value::None);
            };
            if amount == 0 {
                return Ok(Value::None);
            }
            super::wallet::transfer(
                server,
                me,
                w.constant("ownerSystem"),
                amount as f64,
                "refATMWithdraw",
                Some(station),
                "",
            )
            .await?;
            let credits = create_item(
                server,
                w.type_id("Credits"),
                me,
                station,
                w.flag("Hangar"),
                amount,
                false,
            )
            .await?;
            notify_new(server, &credits);
            stack_into(server, &credits, station, w.flag("Hangar")).await?;
            Ok(Value::None)
        }
        // AtmDeposit(itemID, quantity): Credits back into the wallet.
        "AtmDeposit" => {
            let Some(it) = item(server, a.int(0).unwrap_or(0))
                .await?
                .filter(|i| i.owner_id == me && i.type_id == w.type_id("Credits"))
            else {
                return Ok(Value::None);
            };
            let qty = (a.int(1).unwrap_or(0) as i32).clamp(0, it.quantity);
            if qty == 0 {
                return Ok(Value::None);
            }
            if qty == it.quantity {
                delete_item(server, &it).await?;
            } else {
                set_quantity(server, &it, it.quantity - qty).await?;
            }
            super::wallet::transfer(
                server,
                w.constant("ownerSystem"),
                me,
                qty as f64,
                "refATMDeposit",
                session.attrs.int("stationid"),
                "",
            )
            .await?;
            Ok(Value::None)
        }
        m => super::unimplemented("invbroker", m, call),
    }
}

/// Items in a container, as the client's List() expects them.
async fn list(
    server: &Server,
    session: &Session,
    container: &Container,
    flag: Option<i64>,
) -> anyhow::Result<Vec<Item>> {
    let char_id = session.char_id().unwrap_or(0);
    let w = &server.world;
    let mut items = match container {
        Container::Special { container, owner } => {
            let owner = owner.unwrap_or(char_id as i64) as i32;
            match *container as i32 {
                c if c == w.constant("containerHangar") => {
                    let station = session.attrs.int("stationid").unwrap_or(0);
                    // Items installed in factory slots are not in the hangar.
                    items_where(
                        server,
                        &format!(r#""locationID" = $1 AND "ownerID" = $2 AND {}"#, super::factory::hangar_filter(server)),
                        &[&station, &owner],
                    )
                    .await?
                }
                c if c == w.constant("containerWallet") => {
                    let abstract_loc = w.constant("locationAbstract") as i64;
                    items_where(
                        server,
                        r#""locationID" = $1 AND "ownerID" = $2"#,
                        &[&abstract_loc, &owner],
                    )
                    .await?
                }
                // assetsinfo.py / base_missions: everything I own, anywhere.
                c if c == w.constant("containerGlobal") => {
                    items_where(server, r#""ownerID" = $1"#, &[&owner]).await?
                }
                _ => Vec::new(),
            }
        }
        Container::Item(id) => items_where(server, r#""locationID" = $1"#, &[id]).await?,
    };
    if let Some(f) = flag {
        items.retain(|i| i.flag as i64 == f);
    }
    Ok(items)
}

/// A dogma attribute of a type, if it has one.
pub async fn type_attribute(
    server: &Server,
    type_id: i32,
    name: &str,
) -> anyhow::Result<Option<f64>> {
    let Some(attr) = server.world.attribute(name) else {
        return Ok(None);
    };
    db::scalar(
        &server.pool,
        r#"SELECT "value" FROM static.dgmtypeattribs WHERE "typeID" = $1 AND "attributeID" = $2"#,
        &[&type_id, &attr],
    )
    .await
}

/// The room left in a container's `flag` compartment.
async fn capacity_left(server: &Server, container: &Item, flag: i32) -> anyhow::Result<f64> {
    let w = &server.world;
    let capacity = if flag == w.flag("DroneBay") {
        type_attribute(server, container.type_id, "droneCapacity")
            .await?
            .unwrap_or(0.0)
    } else {
        w.type_info(container.type_id)
            .map(|t| t.capacity)
            .unwrap_or(0.0)
    };
    let used: f64 = items_where(
        server,
        r#""locationID" = $1 AND "flag" = $2"#,
        &[&container.item_id, &flag],
    )
    .await?
    .iter()
    .map(|i| i.volume(server))
    .sum();
    Ok(capacity - used)
}

async fn check_password(
    server: &Server,
    item_id: i64,
    which: i32,
    given: Option<&Value>,
) -> Result<(), CallError> {
    let pw: Option<String> = db::scalar(
        &server.pool,
        r#"SELECT "password" FROM game.container_passwords WHERE "itemID" = $1 AND "which" = $2"#,
        &[&item_id, &which],
    )
    .await?;
    match pw {
        Some(pw) if given.and_then(Value::as_text).as_deref() != Some(pw.as_str()) => {
            Err(user_error("PermissionDenied", vec![]))
        }
        _ => Ok(()),
    }
}

fn out_of_reach(what: &str) -> CallError {
    user_error(
        "CustomInfo",
        vec![("info", Value::from(format!("{what} is out of reach.")))],
    )
}

/// Puts (part of) an item into a container (inventory.Add).
async fn add(
    server: &Server,
    session: &mut Session,
    container: &Container,
    item_id: i64,
    qty: Option<i32>,
    flag: Option<i32>,
) -> CallResult {
    let w = &server.world;
    let me = session.char_id().unwrap_or(0);
    let Some(it) = item(server, item_id).await? else {
        return Ok(Value::None);
    };
    if !reachable(server, session, &it).await? {
        return Err(out_of_reach("That item"));
    }
    let qty = qty.unwrap_or(it.quantity).clamp(1, it.quantity.max(1));
    match container {
        Container::Special { container, owner } => {
            if *container as i32 != w.constant("containerHangar") {
                return Ok(Value::None);
            }
            let Some(station) = session.attrs.int("stationid") else {
                return Ok(Value::None);
            };
            if Some(it.item_id) == session.attrs.int("shipid") {
                return Ok(Value::None);
            }
            let part = split(server, &it, qty).await?;
            let part = match owner {
                Some(o) if *o as i32 != part.owner_id => {
                    set_owner(server, &part, *o as i32).await?
                }
                _ => part,
            };
            stack_into(server, &part, station, w.flag("Hangar")).await?;
        }
        Container::Item(target_id) if *target_id == me as i64 => {
            // A skill book dropped on the character is learnt (level 0).
            if it.category(server) != w.category_id("Skill") {
                return Err(user_error(
                    "CustomInfo",
                    vec![("info", Value::str("Only skills can be injected."))],
                ));
            }
            let known = items_where(
                server,
                r#""locationID" = $1 AND "typeID" = $2"#,
                &[&(me as i64), &it.type_id],
            )
            .await?;
            if !known.is_empty() {
                return Err(user_error(
                    "CustomInfo",
                    vec![("info", Value::str("You already know that skill."))],
                ));
            }
            let book = split_one(server, &it).await?;
            for name in ["skillLevel", "skillPoints"] {
                if let Some(attr) = w.attribute(name) {
                    db::exec(
                        &server.pool,
                        r#"INSERT INTO game.item_attributes VALUES ($1, $2, 0) ON CONFLICT ("itemID", "attributeID") DO UPDATE SET "value" = 0"#,
                        &[&book.item_id, &attr],
                    )
                    .await?;
                }
            }
            db::exec(
                &server.pool,
                r#"UPDATE game.items SET "singleton" = 1 WHERE "itemID" = $1"#,
                &[&book.item_id],
            )
            .await?;
            move_item(server, &book, me as i64, w.flag("Skill")).await?;
        }
        Container::Item(target_id) => {
            let Some(target) = item(server, *target_id).await? else {
                return Ok(Value::None);
            };
            if !reachable(server, session, &target).await? {
                return Err(out_of_reach("That container"));
            }
            if it.item_id == target.item_id {
                return Ok(Value::None);
            }
            let is_ship = target.category(server) == w.category_id("Ship");
            let flag = flag.filter(|&f| f != 0).unwrap_or(if is_ship {
                w.flag("Cargo")
            } else {
                w.flag("None")
            });
            if (w.flag("SlotFirst")..=w.flag("SlotLast")).contains(&flag) {
                // Fitting: one assembled module per slot (godma takes it from there).
                if it.category(server) != w.category_id("Module") {
                    return Err(user_error(
                        "CustomInfo",
                        vec![("info", Value::str("Only modules fit in slots."))],
                    ));
                }
                if !items_where(
                    server,
                    r#""locationID" = $1 AND "flag" = $2"#,
                    &[&target.item_id, &flag],
                )
                .await?
                .is_empty()
                {
                    return Err(user_error(
                        "CustomInfo",
                        vec![("info", Value::str("That slot is occupied."))],
                    ));
                }
                let module = split_one(server, &it).await?;
                db::exec(
                    &server.pool,
                    r#"UPDATE game.items SET "singleton" = 1 WHERE "itemID" = $1"#,
                    &[&module.item_id],
                )
                .await?;
                let mut module = module;
                module.singleton = 1;
                move_item(server, &module, target.item_id, flag).await?;
                return Ok(Value::None);
            }
            if it.category(server) == w.category_id("Ship") && it.singleton != 0 {
                return Err(user_error(
                    "CustomInfo",
                    vec![(
                        "info",
                        Value::str("Assembled ships cannot go in a cargo hold."),
                    )],
                ));
            }
            let unit = w.type_info(it.type_id).map(|t| t.volume).unwrap_or(0.0);
            let left = capacity_left(server, &target, flag).await?;
            if unit * qty as f64 > left + 1e-6 {
                let key = if flag == w.flag("DroneBay") {
                    "NotEnoughDroneBaySpace"
                } else {
                    "NotEnoughCargoSpace"
                };
                return Err(user_error(
                    key,
                    vec![
                        ("available", Value::Float(left.max(0.0))),
                        ("volume", Value::Float(unit * qty as f64)),
                    ],
                ));
            }
            let part = split(server, &it, qty).await?;
            stack_into(server, &part, target.item_id, flag).await?;
        }
    }
    Ok(Value::None)
}

/// inventory[...] methods.
pub async fn inventory(
    server: &Server,
    session: &mut Session,
    bound: &Bound,
    call: &Call,
) -> CallResult {
    let container =
        container_of(bound).ok_or_else(|| anyhow!("bad inventory params {:?}", bound.params))?;
    let a = Args::of(call);
    let w = &server.world;
    let me = session.char_id().unwrap_or(0);
    match call.method() {
        // List(flag=None, detailed=0, securityCode=None)
        "List" => {
            if let Container::Item(id) = container {
                check_password(server, id, 1, a.kw.get("securityCode")).await?;
            }
            let items = list(server, session, &container, a.int(0)).await?;
            Ok(Value::List(items.iter().map(|i| i.row(server)).collect()))
        }
        "ListCargo" => {
            let items = list(server, session, &container, Some(w.flag("Cargo") as i64)).await?;
            Ok(Value::List(items.iter().map(|i| i.row(server)).collect()))
        }
        // sessions.py: the items held by one item (who is aboard a ship).
        "SelectItems" => {
            let of = a.int(0).unwrap_or(0);
            Ok(Value::List(
                items_where(server, r#""locationID" = $1"#, &[&of])
                    .await?
                    .iter()
                    .map(|i| i.row(server))
                    .collect(),
            ))
        }
        // invCacheContainer.GetItemID/GetTypeID: the container item itself.
        "GetItem" => match container {
            Container::Item(id) => {
                if Some(id as i32) == session.char_id() {
                    return Ok(character_item(server, id as i32)
                        .await?
                        .map(|i| i.row(server))
                        .unwrap_or(Value::None));
                }
                Ok(item(server, id)
                    .await?
                    .map(|i| i.row(server))
                    .unwrap_or(Value::None))
            }
            Container::Special { .. } => Ok(Value::None),
        },
        // util.Row(['capacity', 'used']) (invCacheContainer.GetCapacity).
        "GetCapacity" => {
            let flag = a.int(0).filter(|&f| f != 0);
            let items = list(server, session, &container, flag).await?;
            let used: f64 = items.iter().map(|i| i.volume(server)).sum();
            let capacity = match container {
                Container::Item(id) => match item(server, id).await? {
                    Some(it) => {
                        capacity_left(
                            server,
                            &it,
                            flag.map(|f| f as i32).unwrap_or(w.flag("Cargo")),
                        )
                        .await?
                            + used
                    }
                    None => 0.0,
                },
                Container::Special { .. } => 1.0e12,
            };
            Ok(rows::row(
                &["capacity", "used"],
                vec![Value::Float(capacity), Value::Float(used)],
            ))
        }
        // Add(itemID, quantity=None, flag=None)
        "Add" if matches!(container, Container::Special { container: c, .. } if c as i32 == server.world.constant("containerFactory")) => {
            super::factory::install(server, session, call).await
        }
        "Add" => {
            let qty = a
                .get(1, "quantity")
                .and_then(Value::as_int)
                .map(|q| q as i32);
            let flag = a.get(2, "flag").and_then(Value::as_int).map(|f| f as i32);
            add(
                server,
                session,
                &container,
                a.int(0).unwrap_or(0),
                qty,
                flag,
            )
            .await
        }
        // MergeItems(srcItemID, dstItemID, quantity)
        "MergeItems" => {
            let (Some(src), Some(dst)) = (
                item(server, a.int(0).unwrap_or(0)).await?,
                item(server, a.int(1).unwrap_or(0)).await?,
            ) else {
                return Ok(Value::None);
            };
            if src.type_id != dst.type_id
                || src.singleton != 0
                || dst.singleton != 0
                || src.item_id == dst.item_id
            {
                return Ok(Value::None);
            }
            if !reachable(server, session, &src).await? || !reachable(server, session, &dst).await?
            {
                return Err(out_of_reach("That item"));
            }
            let qty = (a.int(2).unwrap_or(src.quantity as i64) as i32).clamp(1, src.quantity);
            set_quantity(server, &dst, dst.quantity + qty).await?;
            if qty == src.quantity {
                delete_item(server, &src).await?;
            } else {
                set_quantity(server, &src, src.quantity - qty).await?;
            }
            Ok(Value::None)
        }
        // ReplaceCharges(flag, itemID, quantity): load ammo into the module in `flag`.
        "ReplaceCharges" => {
            let Container::Item(ship) = container else {
                return Ok(Value::None);
            };
            let flag = a.i32(0).unwrap_or(0);
            let Some(charge) = item(server, a.int(1).unwrap_or(0)).await? else {
                return Ok(Value::None);
            };
            if !reachable(server, session, &charge).await? {
                return Err(out_of_reach("That charge"));
            }
            let qty = (a.int(2).unwrap_or(charge.quantity as i64) as i32).clamp(1, charge.quantity);
            // Whatever is loaded goes back to the cargo hold first.
            for loaded in items_where(
                server,
                r#""locationID" = $1 AND "flag" = $2 AND "singleton" = 0"#,
                &[&ship, &flag],
            )
            .await?
            {
                stack_into(server, &loaded, ship, w.flag("Cargo")).await?;
            }
            let part = split(server, &charge, qty).await?;
            move_item(server, &part, ship, flag).await?;
            Ok(Value::Int(1))
        }
        // A mission package: its contents spill out where it is, the wrap goes.
        "BreakPlasticWrap" => {
            let Container::Item(id) = container else {
                return Ok(Value::None);
            };
            let Some(wrap) = item(server, id).await?.filter(|i| i.owner_id == me) else {
                return Ok(Value::None);
            };
            if super::missions::is_courier_package(server, id).await? {
                return Err(user_error(
                    "CustomInfo",
                    vec![(
                        "info",
                        Value::str("This package belongs to a courier mission."),
                    )],
                ));
            }
            for inner in items_where(server, r#""locationID" = $1"#, &[&id]).await? {
                move_item(server, &inner, wrap.location_id, wrap.flag).await?;
            }
            delete_item(server, &wrap).await?;
            Ok(Value::None)
        }
        // Items are never put in a trash here, so there is nothing to take out.
        "TakeOutTrash" => Ok(Value::None),
        "SetPassword" | "SetPassword2" => {
            let Container::Item(id) = container else {
                return Ok(Value::None);
            };
            if item(server, id).await?.map(|i| i.owner_id) != Some(me) {
                return Err(user_error("PermissionDenied", vec![]));
            }
            let which = if call.method() == "SetPassword" { 1 } else { 2 };
            let pw = a.text(0).unwrap_or_default();
            if pw.is_empty() {
                db::exec(
                    &server.pool,
                    r#"DELETE FROM game.container_passwords WHERE "itemID" = $1 AND "which" = $2"#,
                    &[&id, &which],
                )
                .await?;
            } else {
                db::exec(
                    &server.pool,
                    r#"INSERT INTO game.container_passwords VALUES ($1, $2, $3) ON CONFLICT ("itemID", "which") DO UPDATE SET "password" = $3"#,
                    &[&id, &which, &pw],
                )
                .await?;
            }
            Ok(Value::None)
        }
        // Construction platforms (navigation.py BuildConstructionPlatform): the
        // build password guards it; no blueprints for platforms exist yet.
        "Build" => {
            if let Container::Item(id) = container {
                check_password(server, id, 2, a.kw.get("securityCode")).await?;
            }
            Err(user_error(
                "CustomInfo",
                vec![(
                    "info",
                    Value::str("There is nothing this platform can build."),
                )],
            ))
        }
        // hangar.py: owners with hangars at this station.
        "GetHangarOwnerIDs" => {
            let station = session.attrs.int("stationid").unwrap_or(0);
            let c = server.pool.get().await?;
            let owners: Vec<Value> = c
                .query(
                    r#"SELECT DISTINCT "ownerID" FROM game.items WHERE "locationID" = $1"#,
                    &[&station],
                )
                .await?
                .iter()
                .map(|r| Value::Int(r.get(0)))
                .collect();
            Ok(Value::List(owners))
        }
        m => super::unimplemented("inventory", m, call),
    }
}
