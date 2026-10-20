//! Standings: `standingmgr` (a character's private standings and notes, read
//! by show-info and the threat assessment) and `standing` (what NPC
//! corporations and factions think of me, for the character sheet).

use marshal::Value;
use proto::gpcs::Call;

use super::{Args, CallResult, Session};
use crate::{db, rows, Server};

const PRIVATE_HEADER: [&str; 4] = [
    "charCharStanding",
    "corpCorpStanding",
    "charSecurity",
    "notes",
];

/// infosvc/threat.py: my standing towards `other`, my corp's towards theirs,
/// and their security status.
async fn private_standing(
    server: &Server,
    me: i32,
    my_corp: i32,
    other: i32,
) -> anyhow::Result<Value> {
    let c = server.pool.get().await?;
    let r = c
        .query_one(
            r#"SELECT coalesce((SELECT "standing" FROM game.standings WHERE "fromID" = $1 AND "toID" = $3), 0.0),
                      coalesce((SELECT s."standing" FROM game.standings s JOIN game.characters ch
                                  ON s."toID" = ch."corporationID" WHERE s."fromID" = $2 AND ch."characterID" = $3), 0.0),
                      coalesce((SELECT "securityStatus" FROM game.characters WHERE "characterID" = $3), 0.0),
                      coalesce((SELECT "notes" FROM game.standings WHERE "fromID" = $1 AND "toID" = $3), '')"#,
            &[&me, &my_corp, &other],
        )
        .await?;
    Ok(rows::row(
        &PRIVATE_HEADER,
        vec![
            Value::Float(r.get(0)),
            Value::Float(r.get(1)),
            Value::Float(r.get(2)),
            db::text_value(r.get(3)),
        ],
    ))
}

pub async fn set_standing(
    server: &Server,
    from: i32,
    to: i32,
    standing: Option<f64>,
    notes: Option<&str>,
) -> anyhow::Result<()> {
    db::exec(
        &server.pool,
        r#"INSERT INTO game.standings ("fromID", "toID", "standing", "notes") VALUES ($1, $2, coalesce($3::float8, 0.0), coalesce($4::text, ''))
           ON CONFLICT ("fromID", "toID") DO UPDATE SET
               "standing" = coalesce($3::float8, game.standings."standing"), "notes" = coalesce($4::text, game.standings."notes")"#,
        &[&from, &to, &standing, &notes],
    )
    .await?;
    Ok(())
}

pub async fn standingmgr(
    server: &Server,
    session: &mut Session,
    method: &str,
    call: &Call,
) -> CallResult {
    let a = Args::of(call);
    let me = session.char_id().unwrap_or(0);
    let corp = session.attrs.int("corpid").unwrap_or(0) as i32;
    match method {
        "GetPrivateStanding" => {
            Ok(private_standing(server, me, corp, a.i32(0).unwrap_or(0)).await?)
        }
        // threat.py zips the reply with the requested IDs.
        "GetPrivateStandingsEx" => {
            let mut out = Vec::new();
            for id in a.ints(0) {
                out.push(private_standing(server, me, corp, id as i32).await?);
            }
            Ok(Value::List(out))
        }
        // The (misspelt) call behind the character sheet's security status.
        "GetSecutiryStatus" => {
            let s: Option<f64> = db::scalar(
                &server.pool,
                r#"SELECT "securityStatus" FROM game.characters WHERE "characterID" = $1"#,
                &[&me],
            )
            .await?;
            Ok(Value::Float(s.unwrap_or(0.0)))
        }
        // SetPrivateStanding(toID, standing -10..10, notes)
        "SetPrivateStanding" => {
            let standing = a.float(1).unwrap_or(0.0).clamp(-10.0, 10.0);
            let notes = a.text(2);
            set_standing(
                server,
                me,
                a.i32(0).unwrap_or(0),
                Some(standing),
                notes.as_deref(),
            )
            .await?;
            Ok(Value::None)
        }
        "SetPrivateStandingNotes" => {
            let notes = a.text(1).unwrap_or_default();
            set_standing(server, me, a.i32(0).unwrap_or(0), None, Some(&notes)).await?;
            Ok(Value::None)
        }
        m => super::unimplemented("standingmgr", m, call),
    }
}

/// standing.GetStandingsTowardsOwner(): rows fromID/toID/standing for every
/// owner with a standing towards me (charactersheet.py filters by group).
pub async fn standing(
    server: &Server,
    session: &mut Session,
    method: &str,
    call: &Call,
) -> CallResult {
    let me = session.char_id().unwrap_or(0);
    match method {
        "GetStandingsTowardsOwner" => Ok(db::rowset(
            &server.pool,
            r#"SELECT "fromID", "toID", "standing" FROM game.standings WHERE "toID" = $1 ORDER BY "fromID""#,
            &[&me],
        )
        .await?),
        m => super::unimplemented("standing", m, call),
    }
}
