//! Account lookup and (optional) auto-registration on first login.

use anyhow::Result;
use deadpool_postgres::Pool;

/// Returns (userID, role) or None when the credentials are rejected.
pub async fn login(
    pool: &Pool,
    user: &str,
    password: &str,
    auto_create: bool,
) -> Result<Option<(i64, i64)>> {
    if user.is_empty() {
        return Ok(None);
    }
    let client = pool.get().await?;
    let row = client
        .query_opt(r#"SELECT "userID", "password", "role" FROM game.accounts WHERE lower("userName") = lower($1)"#, &[&user])
        .await?;
    let (id, role) = match row {
        Some(r) => {
            let stored: String = r.get(1);
            if stored != password {
                return Ok(None);
            }
            (r.get::<_, i32>(0), r.get::<_, i64>(2))
        }
        None if auto_create => {
            let r = client
                .query_one(
                    r#"INSERT INTO game.accounts ("userName", "password") VALUES ($1, $2) RETURNING "userID", "role""#,
                    &[&user, &password],
                )
                .await?;
            tracing::info!(user, "created account");
            (r.get::<_, i32>(0), r.get::<_, i64>(1))
        }
        None => return Ok(None),
    };
    client
        .execute(
            r#"UPDATE game.accounts SET "lastLogin" = now() WHERE "userID" = $1"#,
            &[&id],
        )
        .await?;
    Ok(Some((id as i64, role)))
}
