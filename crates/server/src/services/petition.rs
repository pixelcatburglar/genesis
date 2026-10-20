//! `petitioner` (ui/shared/neocom/petition.py): players file petitions in a
//! category; petitionees (session role & ROLE_PETITIONEE) work them from
//! queues, claim, chat, escalate and close them.
//!
//! Petition rows are read for petitionID, petitionerID, categoryID, subject,
//! petition, properties (a dict: `.keys()` and `[k]`), claimed, closed,
//! deleted, updated, escalatesTo, createDate, touchDate, age (days),
//! isOnline. Events: OnNewPetition(petitionID, charID, subject, categoryID)
//! and OnAssignPetition(petitionID, assigneeID) to petitionees,
//! OnClosePetition() and OnPetitionMessage(petitionID, charID, text,
//! messageID) to the other party.

use marshal::Value;
use proto::gpcs::Call;

use super::{user_error, Args, CallResult, Session};
use crate::{db, rows, Server};

pub const ROLE_PETITIONEE: i64 = 256;

const PETITION_HEADER: [&str; 15] = [
    "petitionID",
    "petitionerID",
    "categoryID",
    "subject",
    "petition",
    "properties",
    "claimed",
    "closed",
    "deleted",
    "updated",
    "escalatesTo",
    "createDate",
    "touchDate",
    "age",
    "isOnline",
];

async fn petitions(
    server: &Server,
    filter: &str,
    params: db::Params<'_>,
) -> anyhow::Result<Vec<Value>> {
    let c = server.pool.get().await?;
    let rs = c
        .query(
            &format!(
                r#"SELECT p."petitionID", p."petitionerID", p."categoryID", p."subject", p."petition",
                          p."properties"::text, (p."claimedBy" IS NOT NULL)::int, p."closed", p."deleted", p."updated",
                          q."escalatesTo", p."createDate", p."touchDate",
                          (extract(epoch FROM now() - p."createDate") / 86400.0)::float8
                   FROM game.petitions p LEFT JOIN static.petition_queues q USING ("queueID")
                   WHERE {filter} ORDER BY p."petitionID""#
            ),
            params,
        )
        .await?;
    rs.iter()
        .map(|r| {
            let mut line = Vec::with_capacity(PETITION_HEADER.len());
            for i in 0..5 {
                line.push(db::column_value(r, i)?);
            }
            let props: Option<String> = r.get(5);
            line.push(properties_dict(props.as_deref()));
            for i in 6..13 {
                line.push(db::column_value(r, i)?);
            }
            line.push(Value::Float(r.get(13)));
            line.push(Value::Int(super::is_online(server, r.get(1)) as i32));
            Ok(rows::row(&PETITION_HEADER, line))
        })
        .collect()
}

fn properties_dict(json: Option<&str>) -> Value {
    let Some(map) = json
        .and_then(|j| serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(j).ok())
    else {
        return Value::None;
    };
    Value::Dict(
        map.into_iter()
            .map(|(k, v)| {
                (
                    db::text_value(&k),
                    db::text_value(
                        v.as_str()
                            .map(str::to_owned)
                            .unwrap_or(v.to_string())
                            .as_str(),
                    ),
                )
            })
            .collect(),
    )
}

/// The CreatePetition properties dict as JSON text.
fn properties_json(v: Option<&Value>) -> Option<String> {
    let Value::Dict(pairs) = v? else { return None };
    let map: serde_json::Map<String, serde_json::Value> = pairs
        .iter()
        .filter_map(|(k, v)| {
            let val = v
                .as_text()
                .or_else(|| v.as_float().map(|f| f.to_string()))?;
            Some((k.as_text()?, serde_json::Value::String(val)))
        })
        .collect();
    Some(serde_json::Value::Object(map).to_string())
}

async fn log(
    server: &Server,
    petition: i32,
    char_id: Option<i32>,
    user: &str,
    text: &str,
) -> anyhow::Result<()> {
    db::exec(
        &server.pool,
        r#"INSERT INTO game.petition_log ("petitionID", "characterID", "userName", "eventText") VALUES ($1, $2, $3, $4)"#,
        &[&petition, &char_id, &user, &text],
    )
    .await?;
    Ok(())
}

/// Connected petitionees.
fn petitionees(server: &Server) -> Vec<i32> {
    server
        .registry
        .select(None, |c| {
            c.char_id.is_some() && c.role & ROLE_PETITIONEE != 0
        })
        .into_iter()
        .filter_map(|(_, c)| c.char_id)
        .collect()
}

async fn petition_parties(
    server: &Server,
    id: i32,
) -> anyhow::Result<Option<(i32, Option<i32>, i32)>> {
    let c = server.pool.get().await?;
    Ok(c.query_opt(r#"SELECT "petitionerID", "claimedBy", "closed" FROM game.petitions WHERE "petitionID" = $1"#, &[&id])
        .await?
        .map(|r| (r.get(0), r.get(1), r.get(2))))
}

pub async fn dispatch(
    server: &Server,
    session: &mut Session,
    method: &str,
    call: &Call,
) -> CallResult {
    let a = Args::of(call);
    let me = session.char_id().unwrap_or(0);
    let petitionee = session.role & ROLE_PETITIONEE != 0;
    let user = session.user_name.clone();
    match method {
        "GetCategories" => Ok(db::rowset(
            &server.pool,
            r#"SELECT "categoryID", "displayName", "description" FROM static.petition_categories ORDER BY "categoryID""#,
            &[],
        )
        .await?),
        "GetCategoryDetails" => Ok(db::rowset(
            &server.pool,
            r#"SELECT "categoryID", "displayName", "description" FROM static.petition_categories WHERE "categoryID" = $1"#,
            &[&a.i32(0).unwrap_or(0)],
        )
        .await?),
        "GetCategoryName" => {
            let name: Option<String> = db::scalar(
                &server.pool,
                r#"SELECT "displayName" FROM static.petition_categories WHERE "categoryID" = $1"#,
                &[&a.i32(0).unwrap_or(0)],
            )
            .await?;
            Ok(db::text_value(&name.unwrap_or_default()))
        }
        // The extra fields the create form asks for: rows read for `k`.
        "GetCategoryProperties" => Ok(db::rowset(
            &server.pool,
            r#"SELECT "propertyName" AS "k" FROM static.petition_properties WHERE "categoryID" = $1 ORDER BY "propertyName""#,
            &[&a.i32(0).unwrap_or(0)],
        )
        .await?),
        "GetQueues" => Ok(db::rowset(
            &server.pool,
            r#"SELECT "queueID", "queueName", "escalatesTo" FROM static.petition_queues ORDER BY "queueID""#,
            &[],
        )
        .await?),
        // CreatePetition(subject, petition, categoryID, properties)
        "CreatePetition" => {
            let subject = a.text(0).unwrap_or_default();
            let text = a.text(1).unwrap_or_default();
            let category = a.i32(2).unwrap_or(1);
            let props = properties_json(a.pos.get(3));
            let c = server.pool.get().await?;
            let id: i32 = c
                .query_one(
                    r#"INSERT INTO game.petitions ("petitionerID", "categoryID", "subject", "petition", "properties", "queueID")
                       VALUES ($1, $2, $3, $4, $5::text::jsonb,
                               coalesce((SELECT "queueID" FROM static.petition_categories WHERE "categoryID" = $2), 1))
                       RETURNING "petitionID""#,
                    &[&me, &category, &subject, &text, &props],
                )
                .await?
                .get(0);
            log(server, id, Some(me), &user, "Petition created").await?;
            super::notify_chars(
                server,
                &petitionees(server),
                "OnNewPetition",
                vec![Value::Int(id), Value::Int(me), db::text_value(&subject), Value::Int(category)],
            );
            Ok(Value::Int(id))
        }
        "GetMyPetitions" => Ok(Value::List(
            petitions(server, r#"p."petitionerID" = $1 AND p."deleted" = 0"#, &[&me]).await?,
        )),
        "GetPetition" => Ok(petitions(server, r#"p."petitionID" = $1"#, &[&a.i32(0).unwrap_or(0)])
            .await?
            .into_iter()
            .next()
            .unwrap_or(Value::None)),
        "GetClaimedPetitions" => Ok(Value::List(
            petitions(server, r#"p."claimedBy" = $1 AND p."closed" = 0"#, &[&me]).await?,
        )),
        "GetPetitionQueue" => Ok(Value::List(
            petitions(
                server,
                r#"p."queueID" = $1 AND p."claimedBy" IS NULL AND p."closed" = 0 AND p."deleted" = 0"#,
                &[&a.i32(0).unwrap_or(0)],
            )
            .await?,
        )),
        "ClaimPetition" | "UnClaimPetition" | "ClosePetition" | "EscalatePetition" if !petitionee => {
            Err(user_error("PermissionDenied", vec![]))
        }
        // ClaimPetition(id) > 0 on success.
        "ClaimPetition" => {
            let id = a.i32(0).unwrap_or(0);
            let n = db::exec(
                &server.pool,
                r#"UPDATE game.petitions SET "claimedBy" = $2, "touchDate" = now()
                   WHERE "petitionID" = $1 AND "claimedBy" IS NULL AND "closed" = 0"#,
                &[&id, &me],
            )
            .await?;
            if n > 0 {
                log(server, id, Some(me), &user, "Petition claimed").await?;
                super::notify_chars(server, &petitionees(server), "OnAssignPetition", vec![Value::Int(id), Value::Int(me)]);
            }
            Ok(Value::Int(n as i32))
        }
        "UnClaimPetition" => {
            let id = a.i32(0).unwrap_or(0);
            db::exec(&server.pool, r#"UPDATE game.petitions SET "claimedBy" = NULL, "touchDate" = now() WHERE "petitionID" = $1"#, &[&id])
                .await?;
            log(server, id, Some(me), &user, "Petition unclaimed").await?;
            super::notify_chars(server, &petitionees(server), "OnAssignPetition", vec![Value::Int(id), Value::None]);
            Ok(Value::None)
        }
        // EscalatePetition(id, escalatesTo): to the next queue, unclaimed.
        "EscalatePetition" => {
            let id = a.i32(0).unwrap_or(0);
            let to = a.i32(1);
            db::exec(
                &server.pool,
                r#"UPDATE game.petitions SET "queueID" = coalesce($2, "queueID"), "claimedBy" = NULL, "touchDate" = now()
                   WHERE "petitionID" = $1"#,
                &[&id, &to],
            )
            .await?;
            log(server, id, Some(me), &user, "Petition escalated").await?;
            Ok(Value::None)
        }
        "ClosePetition" => {
            let id = a.i32(0).unwrap_or(0);
            db::exec(&server.pool, r#"UPDATE game.petitions SET "closed" = 1, "touchDate" = now() WHERE "petitionID" = $1"#, &[&id])
                .await?;
            log(server, id, Some(me), &user, "Petition closed").await?;
            if let Some((petitioner, _, _)) = petition_parties(server, id).await? {
                super::notify_chars(server, &[petitioner], "OnClosePetition", vec![Value::Int(id)]);
            }
            Ok(Value::None)
        }
        // Cancel: before anyone claimed it. Delete: a closed petition.
        "CancelPetition" | "DeletePetition" => {
            let id = a.i32(0).unwrap_or(0);
            db::exec(
                &server.pool,
                r#"UPDATE game.petitions SET "deleted" = 1, "closed" = 1, "touchDate" = now()
                   WHERE "petitionID" = $1 AND "petitionerID" = $2"#,
                &[&id, &me],
            )
            .await?;
            log(server, id, Some(me), &user, if method == "CancelPetition" { "Petition cancelled" } else { "Petition deleted" })
                .await?;
            Ok(Value::None)
        }
        "GetPetitionLog" => Ok(db::rowset(
            &server.pool,
            r#"SELECT "logID", "characterID", "userName", "eventText", "logDateTime" FROM game.petition_log
               WHERE "petitionID" = $1 ORDER BY "logID""#,
            &[&a.i32(0).unwrap_or(0)],
        )
        .await?),
        "GetPetitionMessages" => Ok(db::rowset(
            &server.pool,
            r#"SELECT "messageID", "petitionID", "senderID", "text", "comment", "sentDate" FROM game.petition_messages
               WHERE "petitionID" = $1 AND ("comment" = 0 OR $2) ORDER BY "messageID""#,
            &[&a.i32(0).unwrap_or(0), &petitionee],
        )
        .await?),
        // Messages to me in my own petitions that I have not read.
        "GetUnreadMessages" => Ok(db::rowset(
            &server.pool,
            r#"SELECT m."messageID", m."petitionID", m."senderID", m."text", m."sentDate" FROM game.petition_messages m
               JOIN game.petitions p USING ("petitionID")
               WHERE p."petitionerID" = $1 AND m."senderID" <> $1 AND m."comment" = 0 AND NOT ($1 = ANY(m."readBy"))
               ORDER BY m."messageID""#,
            &[&me],
        )
        .await?),
        "MarkAsRead" => {
            db::exec(
                &server.pool,
                r#"UPDATE game.petition_messages SET "readBy" = array_append("readBy", $2::integer)
                   WHERE "messageID" = $1 AND NOT ($2 = ANY("readBy"))"#,
                &[&a.i32(0).unwrap_or(0), &me],
            )
            .await?;
            if session.attrs.int("charid").is_some() {
                db::exec(
                    &server.pool,
                    r#"UPDATE game.petitions SET "updated" = 0 WHERE "petitionID" =
                       (SELECT "petitionID" FROM game.petition_messages WHERE "messageID" = $1) AND "claimedBy" = $2"#,
                    &[&a.i32(0).unwrap_or(0), &me],
                )
                .await?;
            }
            Ok(Value::None)
        }
        // PetitionerChat(id, text) / PetitioneeChat(id, text, comment=0)
        "PetitionerChat" | "PetitioneeChat" => {
            let id = a.i32(0).unwrap_or(0);
            let text = a.text(1).unwrap_or_default();
            let comment = (method == "PetitioneeChat" && a.int(2).unwrap_or(0) != 0) as i32;
            let Some((petitioner, claimed, closed)) = petition_parties(server, id).await? else {
                return Ok(Value::None);
            };
            if closed != 0 {
                return Err(user_error("PetitionClosed", vec![]));
            }
            let c = server.pool.get().await?;
            let msg: i32 = c
                .query_one(
                    r#"INSERT INTO game.petition_messages ("petitionID", "senderID", "text", "comment", "readBy")
                       VALUES ($1, $2, $3, $4, ARRAY[$2::integer]) RETURNING "messageID""#,
                    &[&id, &me, &text, &comment],
                )
                .await?
                .get(0);
            // "Updated"/"Waiting" on the petitionee's list follows who spoke last.
            let updated = (me == petitioner) as i32;
            c.execute(r#"UPDATE game.petitions SET "touchDate" = now(), "updated" = $2 WHERE "petitionID" = $1"#, &[&id, &updated])
                .await?;
            if comment == 0 {
                let other = if me == petitioner { claimed.into_iter().collect::<Vec<_>>() } else { vec![petitioner] };
                super::notify_chars(
                    server,
                    &other,
                    "OnPetitionMessage",
                    vec![Value::Int(id), Value::Int(me), db::text_value(&text), Value::Int(msg)],
                );
            }
            Ok(Value::None)
        }
        m => super::unimplemented("petitioner", m, call),
    }
}
