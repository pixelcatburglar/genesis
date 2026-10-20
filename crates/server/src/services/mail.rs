//! `pager`: EVE mail (ui/shared/neocom/inbox.py, comtool/newmessage.py) and
//! its owner search, which every "find a character/corporation" box uses
//! (uix.GetCharacterByName, infosvc Search, the browser's evemail: links).

use marshal::Value;
use proto::gpcs::Call;

use super::{Args, CallResult, Session};
use crate::{db, Server};

/// Characters a page to `receiver` reaches: the character itself, or every
/// member of a corporation.
async fn recipients(server: &Server, receiver: i32) -> anyhow::Result<Vec<i32>> {
    let c = server.pool.get().await?;
    let members: Vec<i32> = c
        .query(
            r#"SELECT "characterID" FROM game.characters WHERE "corporationID" = $1 AND "deleted" IS NULL"#,
            &[&receiver],
        )
        .await?
        .iter()
        .map(|r| r.get(0))
        .collect();
    Ok(if members.is_empty() {
        vec![receiver]
    } else {
        members
    })
}

/// Every owner whose name matches. flag 1 asks for an exact name; otherwise
/// the text is a name prefix (uix.SearchCharacter: "have %s at the beginning
/// of their name"). groupIDFilter narrows by the owner type's group.
pub async fn find_owner(
    server: &Server,
    name: &str,
    exact: bool,
    group: Option<i32>,
) -> anyhow::Result<Value> {
    let pattern = if exact {
        name.to_lowercase()
    } else {
        format!("{}%", super::like_escape(&name.to_lowercase()))
    };
    let op = if exact { "=" } else { "LIKE" };
    db::rowset(
        &server.pool,
        &format!(
            r#"SELECT o."ownerID", o."ownerName", o."typeID", t."groupID" FROM (
                   SELECT "ownerID", "ownerName", "typeID" FROM static.owners
                   UNION ALL SELECT "characterID", "characterName", 1 FROM game.characters WHERE "deleted" IS NULL
                   UNION ALL SELECT "corporationID", "corporationName", {corp} FROM game.corporations WHERE "deleted" = 0
               ) o JOIN static.types t USING ("typeID")
               WHERE lower(o."ownerName") {op} $1 AND ($2::integer IS NULL OR t."groupID" = $2)
               ORDER BY o."ownerName" LIMIT 100"#,
            corp = server.world.type_id("Corporation"),
        ),
        &[&pattern, &group],
    )
    .await
}

pub async fn dispatch(
    server: &Server,
    session: &mut Session,
    method: &str,
    call: &Call,
) -> CallResult {
    let a = Args::of(call);
    let me = session.char_id().unwrap_or(0);
    match method {
        // inbox.LoadInbox reads pageID, charownerID (the sender) and subject.
        "GetPages" => Ok(db::rowset(
            &server.pool,
            r#"SELECT "pageID", "charownerID", "subject", "created" FROM game.pages
               WHERE "receiverID" = $1 ORDER BY "pageID""#,
            &[&me],
        )
        .await?),
        // inbox.GetPageMessage: created and text.
        "GetPageMessage" => Ok(db::row_opt(
            &server.pool,
            r#"SELECT "pageID", "charownerID", "subject", "text", "created" FROM game.pages
               WHERE "pageID" = $1 AND "receiverID" = $2"#,
            &[&a.i32(0).unwrap_or(0), &me],
        )
        .await?),
        // Page(receivers, subject, text, attachments): the subject already
        // carries 'subject:::attachments' (newmessage.py Send).
        "Page" => {
            let subject = a.text(1).unwrap_or_default();
            let text = a.text(2).unwrap_or_default();
            let c = server.pool.get().await?;
            for receiver in a.ints(0) {
                for to in recipients(server, receiver as i32).await? {
                    let id: i32 = c
                        .query_one(
                            r#"INSERT INTO game.pages ("receiverID", "charownerID", "subject", "text")
                               VALUES ($1, $2, $3, $4) RETURNING "pageID""#,
                            &[&to, &me, &subject, &text],
                        )
                        .await?
                        .get(0);
                    // inbox.OnPage(messageid, senderid, subject)
                    super::notify_chars(
                        server,
                        &[to],
                        "OnPage",
                        vec![Value::Int(id), Value::Int(me), db::text_value(&subject)],
                    );
                }
            }
            Ok(Value::None)
        }
        "DeletePages" => {
            let ids: Vec<i32> = a.ints(0).into_iter().map(|i| i as i32).collect();
            db::exec(
                &server.pool,
                r#"DELETE FROM game.pages WHERE "receiverID" = $1 AND "pageID" = ANY($2)"#,
                &[&me, &ids],
            )
            .await?;
            Ok(Value::None)
        }
        // FindOwner(name, flag=0, groupIDFilter=None)
        "FindOwner" => {
            let name = a.text(0).unwrap_or_default();
            let exact = a.get(1, "flag").and_then(Value::as_int) == Some(1);
            let group = a
                .get(2, "groupIDFilter")
                .and_then(Value::as_int)
                .map(|g| g as i32);
            Ok(find_owner(server, &name, exact, group).await?)
        }
        m => super::unimplemented("pager", m, call),
    }
}
