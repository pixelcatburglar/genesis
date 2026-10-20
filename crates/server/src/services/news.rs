//! `newsmgr`: the station forums (ui/station/news). Channels list threads,
//! threads list responses; pages are `(first, count)` with first 1-based.
//! Field names are the entry classes' (channelentry, threadentry,
//! responseentry): sc_title, sc_modified, sc_replies, sc_authorID,
//! sc_created, sc_lastauthorID, reads, authorID, created, text.

use marshal::Value;
use proto::gpcs::Call;

use super::{Args, CallResult, Session};
use crate::{db, Server};

pub async fn dispatch(
    server: &Server,
    session: &mut Session,
    method: &str,
    call: &Call,
) -> CallResult {
    let a = Args::of(call);
    let me = session.char_id().unwrap_or(0);
    let page = |i: usize| -> (i64, i64) {
        let first = a.int(i).unwrap_or(1).max(1);
        (first - 1, a.int(i + 1).unwrap_or(20).clamp(1, 200))
    };
    match method {
        "GetChannels" => Ok(db::rowset(
            &server.pool,
            r#"SELECT c."channelID", c."title" AS "sc_title", c."description",
                      coalesce(max(t."created"), now()) AS "sc_modified", count(t."threadID")::int AS "sc_replies"
               FROM game.news_channels c LEFT JOIN game.news_threads t USING ("channelID")
               GROUP BY c."channelID" ORDER BY c."channelID""#,
            &[],
        )
        .await?),
        "GetThreads" => {
            let (offset, limit) = page(1);
            Ok(db::rowset(
                &server.pool,
                r#"SELECT t."threadID", t."channelID", t."title" AS "sc_title", t."description",
                          t."authorID" AS "sc_authorID", t."created" AS "sc_created",
                          coalesce(max(r."created"), t."created") AS "sc_modified",
                          coalesce((SELECT r2."authorID" FROM game.news_responses r2 WHERE r2."threadID" = t."threadID"
                                    ORDER BY r2."responseID" DESC LIMIT 1), t."authorID") AS "sc_lastauthorID",
                          count(r."responseID")::int AS "sc_replies", t."reads"
                   FROM game.news_threads t LEFT JOIN game.news_responses r USING ("threadID")
                   WHERE t."channelID" = $1 GROUP BY t."threadID"
                   ORDER BY "sc_modified" DESC OFFSET $2 LIMIT $3"#,
                &[&a.i32(0).unwrap_or(0), &offset, &limit],
            )
            .await?)
        }
        "GetResponses" => {
            let thread = a.i32(0).unwrap_or(0);
            let (offset, limit) = page(1);
            db::exec(&server.pool, r#"UPDATE game.news_threads SET "reads" = "reads" + 1 WHERE "threadID" = $1"#, &[&thread]).await?;
            Ok(db::rowset(
                &server.pool,
                r#"SELECT "responseID", "threadID", "authorID", "created", "text" FROM game.news_responses
                   WHERE "threadID" = $1 ORDER BY "responseID" OFFSET $2 LIMIT $3"#,
                &[&thread, &offset, &limit],
            )
            .await?)
        }
        // NewThread(channelID, title, description)
        "NewThread" => {
            db::exec(
                &server.pool,
                r#"INSERT INTO game.news_threads ("channelID", "authorID", "title", "description") VALUES ($1, $2, $3, $4)"#,
                &[&a.i32(0).unwrap_or(0), &me, &a.text(1).unwrap_or_default(), &a.text(2).unwrap_or_default()],
            )
            .await?;
            Ok(Value::None)
        }
        // NewResponse(threadID, text)
        "NewResponse" => {
            db::exec(
                &server.pool,
                r#"INSERT INTO game.news_responses ("threadID", "authorID", "text") VALUES ($1, $2, $3)"#,
                &[&a.i32(0).unwrap_or(0), &me, &a.text(1).unwrap_or_default()],
            )
            .await?;
            Ok(Value::None)
        }
        m => super::unimplemented("newsmgr", m, call),
    }
}
