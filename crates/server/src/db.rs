//! PostgreSQL pool, schema management, and row -> Value conversion.
//!
//! Two kinds of SQL live under GENESIS_DB_DIR (default ./db):
//!   * static/*.sql - the client-contract tables and world content. Rebuilt from
//!     scratch whenever the files' combined hash changes.
//!   * game/*.sql   - player data migrations, applied once each, in name order.

use std::path::Path;

use anyhow::{Context, Result};
use deadpool_postgres::{Manager, ManagerConfig, Pool, RecyclingMethod};
use marshal::Value;
use tokio_postgres::types::Type;
use tokio_postgres::{NoTls, Row};

pub async fn connect(url: &str) -> Result<Pool> {
    let config: tokio_postgres::Config = url.parse().context("DATABASE_URL")?;
    let manager = Manager::from_config(
        config,
        NoTls,
        ManagerConfig {
            recycling_method: RecyclingMethod::Fast,
        },
    );
    let pool = Pool::builder(manager).max_size(16).build()?;
    // Postgres may still be starting when the container comes up.
    for attempt in 1.. {
        match pool.get().await {
            Ok(_) => break,
            Err(e) if attempt < 30 => {
                tracing::info!("waiting for database ({e})");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            Err(e) => return Err(e).context("database unreachable"),
        }
    }
    Ok(pool)
}

fn sql_files(dir: &Path) -> Result<Vec<(String, String)>> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "sql"))
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|p| {
            let name = p.file_stem().unwrap().to_string_lossy().into_owned();
            Ok((
                name,
                std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?,
            ))
        })
        .collect()
}

pub async fn migrate(pool: &Pool, db_dir: &Path) -> Result<()> {
    let mut client = pool.get().await?;
    client
        .batch_execute(
            "CREATE TABLE IF NOT EXISTS public._game_migrations (name text PRIMARY KEY, applied timestamptz NOT NULL DEFAULT now());
             CREATE TABLE IF NOT EXISTS public._static_version (id int PRIMARY KEY, hash bigint NOT NULL, built timestamptz NOT NULL DEFAULT now());
             CREATE SCHEMA IF NOT EXISTS game;",
        )
        .await?;

    // Static: one hash over every file; rebuild when it moves.
    let statics = sql_files(&db_dir.join("static"))?;
    let mut hasher = crc32fast::Hasher::new();
    for (name, sql) in &statics {
        hasher.update(name.as_bytes());
        hasher.update(sql.as_bytes());
    }
    let hash = hasher.finalize() as i64;
    let current: Option<i64> = client
        .query_opt("SELECT hash FROM public._static_version WHERE id = 1", &[])
        .await?
        .map(|r| r.get(0));
    if current != Some(hash) {
        tracing::info!("building static schema ({} files)", statics.len());
        let started = std::time::Instant::now();
        let tx = client.transaction().await?;
        tx.batch_execute("DROP SCHEMA IF EXISTS static CASCADE")
            .await?;
        for (name, sql) in &statics {
            tx.batch_execute(sql)
                .await
                .with_context(|| format!("static/{name}.sql"))?;
        }
        tx.execute(
            "INSERT INTO public._static_version (id, hash) VALUES (1, $1) ON CONFLICT (id) DO UPDATE SET hash = $1, built = now()",
            &[&hash],
        )
        .await?;
        tx.commit().await?;
        tracing::info!("static schema built in {:.1?}", started.elapsed());
    }

    // Game: incremental.
    for (name, sql) in sql_files(&db_dir.join("game"))? {
        let done = client
            .query_opt(
                "SELECT 1 FROM public._game_migrations WHERE name = $1",
                &[&name],
            )
            .await?;
        if done.is_some() {
            continue;
        }
        let tx = client.transaction().await?;
        tx.batch_execute(&sql)
            .await
            .with_context(|| format!("game/{name}.sql"))?;
        tx.execute(
            "INSERT INTO public._game_migrations (name) VALUES ($1)",
            &[&name],
        )
        .await?;
        tx.commit().await?;
        tracing::info!("applied game migration {name}");
    }
    Ok(())
}

/// Python 2.2 `str` is bytes. Text the client can show is Latin-1; anything
/// outside it goes as `unicode`.
pub fn text_value(s: &str) -> Value {
    if s.chars().all(|c| (c as u32) < 0x100) {
        Value::Str(s.chars().map(|c| c as u8).collect())
    } else {
        Value::Unicode(s.to_owned())
    }
}

/// One column of a row as the client's Python type.
pub fn column_value(row: &Row, i: usize) -> Result<Value> {
    let ty = row.columns()[i].type_();
    Ok(match *ty {
        Type::INT2 => row
            .get::<_, Option<i16>>(i)
            .map(|v| Value::Int(v as i32))
            .unwrap_or(Value::None),
        Type::INT4 => row
            .get::<_, Option<i32>>(i)
            .map(Value::Int)
            .unwrap_or(Value::None),
        Type::INT8 => row
            .get::<_, Option<i64>>(i)
            .map(Value::from)
            .unwrap_or(Value::None),
        Type::FLOAT4 => row
            .get::<_, Option<f32>>(i)
            .map(|v| Value::Float(v as f64))
            .unwrap_or(Value::None),
        Type::FLOAT8 => row
            .get::<_, Option<f64>>(i)
            .map(Value::Float)
            .unwrap_or(Value::None),
        Type::BOOL => row
            .get::<_, Option<bool>>(i)
            .map(Value::from)
            .unwrap_or(Value::None),
        Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME => row
            .get::<_, Option<&str>>(i)
            .map(text_value)
            .unwrap_or(Value::None),
        // Dates travel as blue time (util.FmtDate takes blue.os.GetTime() units).
        Type::TIMESTAMPTZ | Type::TIMESTAMP => row
            .get::<_, Option<std::time::SystemTime>>(i)
            .map(|t| Value::Long(blue_time(t)))
            .unwrap_or(Value::None),
        ref other => anyhow::bail!(
            "column {} has unsupported type {other}",
            row.columns()[i].name()
        ),
    })
}

/// Windows FILETIME ticks (100 ns since 1601) of a wall-clock time.
pub fn blue_time(t: std::time::SystemTime) -> i64 {
    const EPOCH_DIFF: i64 = 116_444_736_000_000_000;
    match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => EPOCH_DIFF + (d.as_nanos() / 100) as i64,
        Err(e) => EPOCH_DIFF - (e.duration().as_nanos() / 100) as i64,
    }
}

/// The inverse of `blue_time`, for dates the client sends.
pub fn system_time(blue: i64) -> std::time::SystemTime {
    const EPOCH_DIFF: i64 = 116_444_736_000_000_000;
    let ticks = blue - EPOCH_DIFF;
    if ticks >= 0 {
        std::time::UNIX_EPOCH + std::time::Duration::from_nanos(ticks as u64 * 100)
    } else {
        std::time::UNIX_EPOCH - std::time::Duration::from_nanos((-ticks) as u64 * 100)
    }
}

pub fn row_values(row: &Row) -> Result<Vec<Value>> {
    (0..row.len()).map(|i| column_value(row, i)).collect()
}

pub type Params<'a> = &'a [&'a (dyn tokio_postgres::types::ToSql + Sync)];

/// A query as util.Rowset, the header taken from the statement's columns (so
/// an empty result still names its columns). Column names are the attribute
/// names the client reads, so queries alias with quoted camelCase.
pub async fn rowset(pool: &Pool, sql: &str, params: Params<'_>) -> Result<Value> {
    let c = pool.get().await?;
    let stmt = c.prepare(sql).await?;
    let rs = c.query(&stmt, params).await?;
    let header: Vec<&str> = stmt.columns().iter().map(|c| c.name()).collect();
    Ok(crate::rows::rowset(
        &header,
        rs.iter().map(row_values).collect::<Result<_>>()?,
    ))
}

/// A query's rows as a list of util.Row (for code that indexes or appends).
pub async fn row_list(pool: &Pool, sql: &str, params: Params<'_>) -> Result<Vec<Value>> {
    let c = pool.get().await?;
    let stmt = c.prepare(sql).await?;
    let rs = c.query(&stmt, params).await?;
    let header: Vec<&str> = stmt.columns().iter().map(|c| c.name()).collect();
    rs.iter()
        .map(|r| Ok(crate::rows::row(&header, row_values(r)?)))
        .collect()
}

/// The first row as util.Row, or None.
pub async fn row_opt(pool: &Pool, sql: &str, params: Params<'_>) -> Result<Value> {
    Ok(row_list(pool, sql, params)
        .await?
        .into_iter()
        .next()
        .unwrap_or(Value::None))
}

/// Runs a statement; returns rows affected.
pub async fn exec(pool: &Pool, sql: &str, params: Params<'_>) -> Result<u64> {
    Ok(pool.get().await?.execute(sql, params).await?)
}

/// A single scalar (first column of the first row), if any.
pub async fn scalar<T: for<'r> tokio_postgres::types::FromSql<'r>>(
    pool: &Pool,
    sql: &str,
    params: Params<'_>,
) -> Result<Option<T>> {
    Ok(pool
        .get()
        .await?
        .query_opt(sql, params)
        .await?
        .map(|r| r.get(0)))
}
