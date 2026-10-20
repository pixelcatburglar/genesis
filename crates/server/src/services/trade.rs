//! `trademgr`: direct trades between two pilots in a station
//! (ui/station/pvptrade/pvptradesvc.py, pvptradewnd.py).
//!
//! InitiateTrade(charID) returns a bound trade session; the other pilot gets
//! it in OnTrade('Initiate', initiatorID, tradeSession). Offered items move
//! into a trade container item (List().tradeContainerID); the offer views
//! show the items there owned by each trader. Either side toggles accept;
//! any change clears both accepts; when both accept, items and money swap.
//! OnTrade('StateToggle' | 'MoneyOffer' | 'Cancel' | 'TradeComplete', containerID, ...).

use std::collections::HashMap;
use std::sync::Mutex;

use marshal::Value;
use proto::gpcs::Call;

use super::inventory;
use super::{user_error, wallet, Args, Bound, CallResult, Session};
use crate::{registry::Push, rows, Server};

#[derive(Debug, Clone)]
pub struct Trade {
    traders: [i32; 2],
    station: i32,
    money: [f64; 2],
    accepted: [bool; 2],
}

#[derive(Default)]
pub struct Trades {
    open: Mutex<HashMap<i64, Trade>>,
}

impl Trades {
    fn get(&self, id: i64) -> Option<Trade> {
        self.open.lock().unwrap().get(&id).cloned()
    }

    fn update(&self, id: i64, f: impl FnOnce(&mut Trade)) -> Option<Trade> {
        let mut open = self.open.lock().unwrap();
        let t = open.get_mut(&id)?;
        f(t);
        Some(t.clone())
    }
}

fn tell(server: &Server, t: &Trade, what: &str, mut args: Vec<Value>) {
    args.insert(0, Value::str(what));
    super::notify_chars(server, &t.traders, "OnTrade", args);
}

fn side(t: &Trade, me: i32) -> Option<usize> {
    t.traders.iter().position(|&c| c == me)
}

fn flags(t: &Trade) -> Value {
    Value::List(t.accepted.iter().map(|&a| Value::from(a)).collect())
}

pub async fn dispatch(server: &Server, session: &mut Session, method: &str, call: &Call) -> CallResult {
    let a = Args::of(call);
    let me = session.char_id().unwrap_or(0);
    match method {
        "InitiateTrade" => {
            let other = a.i32(0).unwrap_or(0);
            let Some(station) = session.attrs.int("stationid").map(|s| s as i32) else {
                return Err(user_error("CustomInfo", vec![("info", Value::str("You must be docked to trade."))]));
            };
            let there = server.registry.select(None, |c| c.char_id == Some(other));
            let Some((_, peer)) = there.into_iter().find(|(_, c)| c.location_id == Some(station as i64)) else {
                return Err(user_error("CustomInfo", vec![("info", Value::str("That pilot is not in this station."))]));
            };
            if other == me {
                return Ok(Value::None);
            }
            let w = &server.world;
            let container =
                inventory::create_item(server, w.type_id("PlasticWrap"), w.constant("ownerSystem"), station as i64, w.flag("None"), 1, true)
                    .await?;
            server.trades.open.lock().unwrap().insert(
                container.item_id,
                Trade { traders: [me, other], station, money: [0.0; 2], accepted: [false; 2] },
            );
            peer.tx
                .send(Push::BindEvent {
                    service: "trademgr".into(),
                    params: Value::from(container.item_id),
                    event: "OnTrade".into(),
                    args: vec![Value::str("Initiate"), Value::Int(me)],
                })
                .ok();
            Ok(session.bind_value(server, "trademgr", Value::from(container.item_id)))
        }
        m => super::unimplemented("trademgr", m, call),
    }
}

/// The bound trade session.
pub async fn bound(server: &Server, session: &mut Session, bound: &Bound, call: &Call) -> CallResult {
    let a = Args::of(call);
    let me = session.char_id().unwrap_or(0);
    let id = bound.params.as_int().unwrap_or(0);
    let Some(t) = server.trades.get(id) else {
        return Err(user_error("CustomInfo", vec![("info", Value::str("This trade is over."))]));
    };
    let Some(mine) = side(&t, me) else { return Err(user_error("PermissionDenied", vec![])) };
    match call.method() {
        "List" => {
            let items = inventory::items_where(server, r#""locationID" = $1"#, &[&id]).await?;
            Ok(rows::keyval(vec![
                ("tradeContainerID", Value::from(id)),
                ("traders", Value::List(t.traders.iter().map(|&c| Value::Int(c)).collect())),
                ("items", Value::List(items.iter().map(|i| i.row(server)).collect())),
                ("money", Value::List(t.money.iter().map(|&m| Value::Float(m)).collect())),
                ("state", flags(&t)),
            ]))
        }
        // Add(itemID, quantity): from my hangar here into the offer.
        "Add" => {
            let Some(it) = inventory::item(server, a.int(0).unwrap_or(0)).await? else { return Ok(Value::None) };
            if it.owner_id != me || it.location_id != t.station as i64 || Some(it.item_id) == session.attrs.int("shipid") {
                return Err(user_error("CustomInfo", vec![("info", Value::str("Only items in your hangar here can be traded."))]));
            }
            let qty = a.get(1, "quantity").and_then(Value::as_int).map(|q| q as i32).unwrap_or(it.quantity);
            let part = inventory::split(server, &it, qty.clamp(1, it.quantity)).await?;
            let moved = inventory::move_item(server, &part, id, server.world.flag("None")).await?;
            show_to_peer(server, &t, mine, &moved, part.location_id, part.flag);
            reset(server, id);
            Ok(Value::None)
        }
        "OfferMoney" => {
            let amount = a.float(0).unwrap_or(0.0).max(0.0);
            if wallet::balance(server, me).await? < amount {
                return Err(user_error("NotEnoughMoney", vec![("balance", Value::Float(wallet::balance(server, me).await?)), ("amount", Value::Float(amount))]));
            }
            let t = server.trades.update(id, |t| {
                t.money[mine] = amount;
                t.accepted = [false; 2];
            });
            if let Some(t) = t {
                tell(server, &t, "MoneyOffer", vec![Value::from(id), Value::List(t.money.iter().map(|&m| Value::Float(m)).collect())]);
            }
            Ok(Value::None)
        }
        "ToggleAccept" => {
            let on = a.int(0).unwrap_or(1) != 0;
            let Some(t) = server.trades.update(id, |t| t.accepted[mine] = on) else { return Ok(Value::None) };
            tell(server, &t, "StateToggle", vec![Value::from(id), flags(&t)]);
            if t.accepted == [true, true] {
                complete(server, id, &t).await?;
            }
            Ok(Value::None)
        }
        "Abort" => {
            abort(server, id).await?;
            Ok(Value::None)
        }
        m => super::unimplemented("trademgr[session]", m, call),
    }
}

/// The other trader sees what I put in.
fn show_to_peer(server: &Server, t: &Trade, mine: usize, item: &inventory::Item, old_loc: i64, old_flag: i32) {
    let w = &server.world;
    let change = Value::Dict(vec![
        (Value::Int(w.constant("ixLocationID")), Value::from(old_loc)),
        (Value::Int(w.constant("ixFlag")), Value::Int(old_flag)),
    ]);
    super::notify_chars(server, &[t.traders[1 - mine]], "OnItemChange", vec![item.row(server), change]);
}

fn reset(server: &Server, id: i64) {
    if let Some(t) = server.trades.update(id, |t| t.accepted = [false; 2]) {
        tell(server, &t, "StateToggle", vec![Value::from(id), flags(&t)]);
    }
}

async fn complete(server: &Server, id: i64, t: &Trade) -> anyhow::Result<()> {
    server.trades.open.lock().unwrap().remove(&id);
    let w = &server.world;
    let [a, b] = t.traders;
    // Money first: if either cannot pay, nothing changes hands.
    let pay = async {
        wallet::transfer(server, a, b, t.money[0], "refPlayerTrading", None, "").await?;
        wallet::transfer(server, b, a, t.money[1], "refPlayerTrading", None, "").await
    };
    let paid = pay.await.is_ok();
    for it in inventory::items_where(server, r#""locationID" = $1"#, &[&id]).await? {
        let it = if paid {
            let to = if it.owner_id == a { b } else { a };
            inventory::set_owner(server, &it, to).await?
        } else {
            it
        };
        inventory::stack_into(server, &it, t.station as i64, w.flag("Hangar")).await?;
    }
    if let Some(c) = inventory::item(server, id).await? {
        inventory::delete_item(server, &c).await?;
    }
    tell(server, t, if paid { "TradeComplete" } else { "Cancel" }, vec![Value::from(id)]);
    Ok(())
}

async fn abort(server: &Server, id: i64) -> anyhow::Result<()> {
    let Some(t) = server.trades.open.lock().unwrap().remove(&id) else { return Ok(()) };
    for it in inventory::items_where(server, r#""locationID" = $1"#, &[&id]).await? {
        inventory::stack_into(server, &it, t.station as i64, server.world.flag("Hangar")).await?;
    }
    if let Some(c) = inventory::item(server, id).await? {
        inventory::delete_item(server, &c).await?;
    }
    tell(server, &t, "Cancel", vec![Value::from(id)]);
    Ok(())
}

/// A trader logging off or leaving the station calls the trade off.
pub async fn on_disconnect(server: &Server, char_id: i32) {
    let ids: Vec<i64> = server.trades.open.lock().unwrap().iter().filter(|(_, t)| t.traders.contains(&char_id)).map(|(id, _)| *id).collect();
    for id in ids {
        if let Err(e) = abort(server, id).await {
            tracing::warn!("trade abort: {e:#}");
        }
    }
}
