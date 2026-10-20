//! Brokers: the bound `broker[stationID]` (moniker.GetBrokerManager,
//! ui/station/brokers/base_brokers.py).
//!
//! TEMPLATE DEFAULT: a station's brokers are its NPC agents. A pilot signs
//! up with one broker per station (signing with another voids the old
//! rules) and gives it rules: allow or disallow a category, group or type.
//! The broker then puts every packaged stack in the pilot's hangar there
//! that the most specific matching rule allows up for sale at base price
//! (market::broker_sell), checked by the ticker.

use marshal::Value;
use proto::gpcs::Call;

use super::{inventory, market, Args, Bound, CallResult, Session};
use crate::{db, rows, Server};

const RULE_HEADER: [&str; 6] = ["brokerID", "clientID", "categoryID", "groupID", "typeID", "allowed"];

async fn is_broker_at(server: &Server, broker: i32, station: i32) -> anyhow::Result<bool> {
    Ok(db::scalar::<i32>(
        &server.pool,
        r#"SELECT "agentID" FROM static.agents WHERE "agentID" = $1 AND "stationID" = $2"#,
        &[&broker, &station],
    )
    .await?
    .is_some())
}

pub async fn bound(server: &Server, session: &mut Session, bound: &Bound, call: &Call) -> CallResult {
    let a = Args::of(call);
    let me = session.char_id().unwrap_or(0);
    let station = bound.params.as_int().unwrap_or(0) as i32;
    match call.method() {
        // [Row(brokerid, stationID)]
        "GetBrokersAtStation" => Ok(db::rowset(
            &server.pool,
            r#"SELECT "agentID" AS "brokerid", "stationID" FROM static.agents WHERE "stationID" = $1 ORDER BY 1"#,
            &[&station],
        )
        .await?),
        "GetBrokersForClient" => Ok(db::rowset(
            &server.pool,
            r#"SELECT "brokerID" AS "brokerid", "stationID" FROM game.broker_clients WHERE "stationID" = $1 AND "clientID" = $2"#,
            &[&station, &me],
        )
        .await?),
        "IsCharWithBroker" => {
            let broker = a.i32(0).unwrap_or(0);
            let with: Option<i32> = db::scalar(
                &server.pool,
                r#"SELECT "brokerID" FROM game.broker_clients WHERE "stationID" = $1 AND "clientID" = $2"#,
                &[&station, &me],
            )
            .await?;
            Ok(Value::Int((with == Some(broker)) as i32))
        }
        "Signup" => {
            let broker = a.i32(0).unwrap_or(0);
            if !is_broker_at(server, broker, station).await? {
                return Err(super::user_error("CustomInfo", vec![("info", Value::str("That broker does not work here."))]));
            }
            resign(server, station, me).await?;
            db::exec(&server.pool, r#"INSERT INTO game.broker_clients VALUES ($1, $2, $3)"#, &[&station, &me, &broker]).await?;
            Ok(Value::None)
        }
        "Resign" => {
            resign(server, station, me).await?;
            Ok(Value::None)
        }
        "GetRulesForClient" => {
            let broker = a.i32(0).unwrap_or(0);
            let c = server.pool.get().await?;
            let rs = c
                .query(
                    r#"SELECT "brokerID", "clientID", "categoryID", "groupID", "typeID", "allowed" FROM game.broker_rules
                       WHERE "brokerID" = $1 AND "clientID" = $2 ORDER BY 3, 4, 5"#,
                    &[&broker, &me],
                )
                .await?;
            Ok(rows::rowset_from(&RULE_HEADER, &rs)?)
        }
        // AddRuleForClient(brokerID, categoryID, groupID, typeID, allowed)
        "AddRuleForClient" => {
            let broker = a.i32(0).unwrap_or(0);
            let (cat, grp, typ) = (a.i32(1).unwrap_or(0), a.i32(2).unwrap_or(0), a.i32(3).unwrap_or(0));
            let allowed = a.i32(4).unwrap_or(0).clamp(0, 1);
            db::exec(
                &server.pool,
                r#"INSERT INTO game.broker_rules VALUES ($1, $2, $3, $4, $5, $6)
                   ON CONFLICT ("brokerID", "clientID", "categoryID", "groupID", "typeID") DO UPDATE SET "allowed" = $6"#,
                &[&broker, &me, &cat, &grp, &typ, &allowed],
            )
            .await?;
            Ok(Value::None)
        }
        "RemoveRuleForClient" => {
            let broker = a.i32(0).unwrap_or(0);
            let (cat, grp, typ) = (a.i32(1).unwrap_or(0), a.i32(2).unwrap_or(0), a.i32(3).unwrap_or(0));
            db::exec(
                &server.pool,
                r#"DELETE FROM game.broker_rules WHERE "brokerID" = $1 AND "clientID" = $2 AND "categoryID" = $3 AND "groupID" = $4 AND "typeID" = $5"#,
                &[&broker, &me, &cat, &grp, &typ],
            )
            .await?;
            Ok(Value::None)
        }
        m => super::unimplemented("broker[station]", m, call),
    }
}

async fn resign(server: &Server, station: i32, client: i32) -> anyhow::Result<()> {
    let old: Option<i32> = db::scalar(
        &server.pool,
        r#"DELETE FROM game.broker_clients WHERE "stationID" = $1 AND "clientID" = $2 RETURNING "brokerID""#,
        &[&station, &client],
    )
    .await?;
    if let Some(b) = old {
        db::exec(&server.pool, r#"DELETE FROM game.broker_rules WHERE "brokerID" = $1 AND "clientID" = $2"#, &[&b, &client]).await?;
    }
    Ok(())
}

/// Lists what each broker's rules allow (called from the skill ticker).
pub async fn tick(server: &Server) -> anyhow::Result<()> {
    let c = server.pool.get().await?;
    let clients: Vec<(i32, i32, i32)> = c
        .query(r#"SELECT "stationID", "clientID", "brokerID" FROM game.broker_clients"#, &[])
        .await?
        .iter()
        .map(|r| (r.get(0), r.get(1), r.get(2)))
        .collect();
    drop(c);
    let w = &server.world;
    for (station, client, broker) in clients {
        let c = server.pool.get().await?;
        let rules: Vec<(i32, i32, i32, bool)> = c
            .query(
                r#"SELECT "categoryID", "groupID", "typeID", "allowed" <> 0 FROM game.broker_rules WHERE "brokerID" = $1 AND "clientID" = $2"#,
                &[&broker, &client],
            )
            .await?
            .iter()
            .map(|r| (r.get(0), r.get(1), r.get(2), r.get(3)))
            .collect();
        drop(c);
        if !rules.iter().any(|r| r.3) {
            continue;
        }
        let ship: Option<i64> = db::scalar::<Option<i64>>(
            &server.pool,
            r#"SELECT "shipID" FROM game.characters WHERE "characterID" = $1"#,
            &[&client],
        )
        .await?
        .flatten();
        for it in inventory::items_where(
            server,
            r#""ownerID" = $1 AND "locationID" = $2 AND "flag" = $3 AND "singleton" = 0"#,
            &[&client, &(station as i64), &w.flag("Hangar")],
        )
        .await?
        {
            if Some(it.item_id) == ship {
                continue;
            }
            let Some(t) = w.type_info(it.type_id) else { continue };
            let by_type = rules.iter().find(|r| r.2 == it.type_id);
            let by_group = rules.iter().find(|r| r.2 == 0 && r.1 == t.group_id);
            let by_cat = rules.iter().find(|r| r.2 == 0 && r.1 == 0 && r.0 == t.category_id);
            if by_type.or(by_group).or(by_cat).is_some_and(|r| r.3) {
                if let Err(e) = market::broker_sell(server, client, &it).await {
                    tracing::debug!(client, item = it.item_id, "broker sale skipped: {e:?}");
                }
            }
        }
    }
    Ok(())
}
