//! Money: balances, transfers with journal entries, and the `account`
//! service (ui/shared/neocom/wallet.py, accountsvc.py, corporation/
//! accounts.py).
//!
//! Characters and player corporations hold cash; NPC owners (< 100,000,000)
//! are bottomless sources and sinks. Every change is journalled per owner and
//! announced as OnAccountChange('cash', ownerID, balance) to that owner's
//! connected characters (for a corporation, its members).

use marshal::Value;
use proto::gpcs::Call;

use super::{user_error, Args, CallError, CallResult, Session};
use crate::{db, Server};

pub const ACCOUNT_CASH: &str = "cash";

fn is_player_corp(owner: i32) -> bool {
    (140_000_000..150_000_000).contains(&owner)
}

fn is_player(owner: i32) -> bool {
    owner >= 100_000_000
}

pub async fn balance(server: &Server, owner: i32) -> anyhow::Result<f64> {
    let sql = if is_player_corp(owner) {
        r#"SELECT "balance" FROM game.corporations WHERE "corporationID" = $1"#
    } else {
        r#"SELECT "balance" FROM game.characters WHERE "characterID" = $1"#
    };
    Ok(db::scalar::<f64>(&server.pool, sql, &[&owner])
        .await?
        .unwrap_or(0.0))
}

/// Characters to tell about an owner's balance.
async fn listeners(server: &Server, owner: i32) -> anyhow::Result<Vec<i32>> {
    if is_player_corp(owner) {
        let c = server.pool.get().await?;
        Ok(c.query(
            r#"SELECT "characterID" FROM game.characters WHERE "corporationID" = $1"#,
            &[&owner],
        )
        .await?
        .iter()
        .map(|r| r.get(0))
        .collect())
    } else {
        Ok(vec![owner])
    }
}

/// Moves `amount` from `from` to `to` (either may be an NPC owner), writing a
/// journal line on each player side. Fails with NotEnoughMoney.
pub async fn transfer(
    server: &Server,
    from: i32,
    to: i32,
    amount: f64,
    ref_type: &str,
    arg: Option<i64>,
    reason: &str,
) -> Result<(), CallError> {
    if !(amount > 0.0) || !amount.is_finite() {
        return Ok(());
    }
    let ref_type_id = server.world.constant(ref_type);
    let mut c = server.pool.get().await?;
    let tx = c.transaction().await?;
    let mut changed = Vec::new();
    for (owner, delta) in [(from, -amount), (to, amount)] {
        if !is_player(owner) {
            continue;
        }
        let sql = if is_player_corp(owner) {
            r#"UPDATE game.corporations SET "balance" = "balance" + $2 WHERE "corporationID" = $1 RETURNING "balance""#
        } else {
            r#"UPDATE game.characters SET "balance" = "balance" + $2 WHERE "characterID" = $1 RETURNING "balance""#
        };
        let Some(row) = tx.query_opt(sql, &[&owner, &delta]).await? else {
            continue;
        };
        let new: f64 = row.get(0);
        if new < 0.0 {
            return Err(user_error(
                "NotEnoughMoney",
                vec![
                    ("balance", Value::Float(new - delta)),
                    ("amount", Value::Float(amount)),
                ],
            ));
        }
        tx.execute(
            r#"INSERT INTO game.journal ("ownerID", "refTypeID", "ownerID1", "ownerID2", "argID1", "amount", "balance", "reason")
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8)"#,
            &[&owner, &ref_type_id, &from, &to, &arg, &delta, &new, &reason],
        )
        .await?;
        changed.push((owner, new));
    }
    tx.commit().await?;
    for (owner, new) in changed {
        let chars = listeners(server, owner).await?;
        super::notify_chars(
            server,
            &chars,
            "OnAccountChange",
            vec![
                Value::str(ACCOUNT_CASH),
                Value::Int(owner),
                Value::Float(new),
            ],
        );
    }
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
    let corp = session.attrs.int("corpid").unwrap_or(0) as i32;
    let corp_side = a.int(0).unwrap_or(0) != 0;
    match method {
        // GetCashBalance(isCorpWallet)
        "GetCashBalance" => {
            let owner = if corp_side { corp } else { me };
            if corp_side && !super::corp::is_accountant(server, session, corp).await? {
                return Ok(Value::Float(0.0));
            }
            Ok(Value::Float(balance(server, owner).await?))
        }
        // GiveCash(ownerID, amount, reason)
        "GiveCash" => {
            let to = a.i32(0).unwrap_or(0);
            let amount = a.float(1).unwrap_or(0.0);
            transfer(
                server,
                me,
                to,
                amount,
                "refPlayerDonation",
                None,
                &a.text(2).unwrap_or_default(),
            )
            .await?;
            Ok(Value::None)
        }
        // GiveCashFromCorpAccount(ownerID, amount, reason): an accountant pays
        // from the corporation's account (FmtRef names them in argID1).
        "GiveCashFromCorpAccount" => {
            if !super::corp::is_accountant(server, session, corp).await? {
                return Err(user_error("CrpAccessDenied", vec![]));
            }
            let to = a.i32(0).unwrap_or(0);
            let amount = a.float(1).unwrap_or(0.0);
            transfer(
                server,
                corp,
                to,
                amount,
                "refCorporationPayment",
                Some(me as i64),
                &a.text(2).unwrap_or_default(),
            )
            .await?;
            Ok(Value::None)
        }
        // wallet.py ShowVouchers: rows read for corporationID and shares.
        "GetSharesByShareholder" => {
            let owner = if corp_side { corp } else { me };
            Ok(db::rowset(
                &server.pool,
                r#"SELECT "ownerID", "corporationID", "shares" FROM game.shares WHERE "ownerID" = $1 AND "shares" > 0
                   ORDER BY "corporationID""#,
                &[&owner],
            )
            .await?)
        }
        // MoveCompanyShares(charID, amount): the CEO hands out shares the
        // corporation holds in itself.
        "MoveCompanyShares" => {
            super::corp::move_shares(
                server,
                session,
                a.i32(0).unwrap_or(0),
                a.int(1).unwrap_or(0) as i32,
            )
            .await?;
            Ok(Value::None)
        }
        "GetKeyMap" => Ok(crate::rows::rowset(
            &["accountKey", "accountName"],
            vec![vec![Value::str(ACCOUNT_CASH), Value::str("Cash")]],
        )),
        "GetRefTypes" => {
            let c = server.pool.get().await?;
            let lines = c
                .query(r#"SELECT "constantValue", "constantID" FROM static.constants WHERE "constantID" LIKE 'ref%' ORDER BY 1"#, &[])
                .await?
                .iter()
                .map(|r| vec![Value::Int(r.get(0)), db::text_value(&r.get::<_, String>(1)[3..])])
                .collect();
            Ok(crate::rows::rowset(&["refTypeID", "refTypeName"], lines))
        }
        // GetJournal(accountKey, fromDate, toDate, isCorpWallet)
        "GetJournal" => {
            let corp_side = a.int(3).unwrap_or(0) != 0;
            let owner = if corp_side { corp } else { me };
            if corp_side && !super::corp::is_accountant(server, session, corp).await? {
                return Ok(crate::rows::rowset(&["refID"], vec![]));
            }
            let from = a
                .int(1)
                .map(db::system_time)
                .unwrap_or(std::time::UNIX_EPOCH);
            let to = a.int(2).map(db::system_time).unwrap_or_else(|| {
                std::time::SystemTime::now() + std::time::Duration::from_secs(3600)
            });
            Ok(db::rowset(
                &server.pool,
                r#"SELECT "refID", "refTypeID", "transDate", "ownerID1", "ownerID2", "argID1", "amount", "balance"
                   FROM game.journal WHERE "ownerID" = $1 AND "transDate" BETWEEN $2 AND $3 ORDER BY "refID" DESC"#,
                &[&owner, &from, &to],
            )
            .await?)
        }
        // Balance per account key, as of now.
        "GetStatement" => {
            let owner = if a.int(1).unwrap_or(0) != 0 { corp } else { me };
            Ok(crate::rows::rowset(
                &["accountKey", "balance"],
                vec![vec![
                    Value::str(ACCOUNT_CASH),
                    Value::Float(balance(server, owner).await?),
                ]],
            ))
        }
        m => super::unimplemented("account", m, call),
    }
}
