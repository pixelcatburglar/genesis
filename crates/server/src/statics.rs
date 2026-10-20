//! The login-time static data (`initvals`) and the cached objects that carry it.
//!
//! AuthenticationRsp.payload = (initvals, serviceInfo), where initvals maps
//! each key (static.initvals) to a `util.CachedObject` stub. The client then
//! calls objectCaching.GetCachableObject per stub and receives an
//! `objectCaching.CachedObject` holding the marshalled table
//! (util/cachedObject.py, net/objectCaching.py).

use std::collections::HashMap;

use anyhow::{Context, Result};
use deadpool_postgres::Pool;
use marshal::Value;

use crate::db;

pub struct CachedTable {
    pub key: String,
    /// The table value, marshalled once (CachedObject.pickle).
    pub pickle: Vec<u8>,
    /// (blue time, checksum). objectCaching refetches only when the checksum
    /// differs AND the time is newer (__NewerVersion / __OlderVersion).
    pub version: (i64, i32),
    pub rows: usize,
}

pub struct Statics {
    pub tables: Vec<CachedTable>,
    by_key: HashMap<String, usize>,
}

/// Windows FILETIME: 100 ns ticks since 1601, the unit of blue.os.GetTime().
pub fn blue_time_now() -> i64 {
    const EPOCH_DIFF: i64 = 116_444_736_000_000_000;
    let since_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    EPOCH_DIFF + (since_unix.as_nanos() / 100) as i64
}

impl Statics {
    pub async fn load(pool: &Pool) -> Result<Statics> {
        let client = pool.get().await?;
        let specs = client
            .query(
                r#"SELECT "key", "tbl", "shape", "orderBy" FROM static.initvals ORDER BY "key""#,
                &[],
            )
            .await?;
        let now = blue_time_now();
        let mut tables = Vec::new();
        for spec in specs {
            let (key, tbl, shape, order): (String, String, String, String) =
                (spec.get(0), spec.get(1), spec.get(2), spec.get(3));
            let stmt = client
                .prepare(&format!("SELECT * FROM static.{tbl} ORDER BY {order}"))
                .await?;
            let rows = client
                .query(&stmt, &[])
                .await
                .with_context(|| format!("loading static.{tbl}"))?;
            // Header from the statement, so empty tables still have one.
            let header = Value::List(
                stmt.columns()
                    .iter()
                    .map(|c| Value::str(c.name()))
                    .collect(),
            );
            let lines = rows
                .iter()
                .map(|r| db::row_values(r).map(Value::List))
                .collect::<Result<Vec<_>>>()?;
            let n = lines.len();
            let value = match shape.as_str() {
                // cfg reads GetCachedObject()[0] / [1].
                "bulk" => Value::tuple([header, Value::List(lines)]),
                // Iterated as row objects. util.Rowset has no __getstate__, so
                // its __dict__ is the state; RowClass defaults at class level.
                _ => Value::instance(
                    "util.Rowset",
                    Value::dict([
                        (Value::str("header"), header),
                        (Value::str("lines"), Value::List(lines)),
                    ]),
                ),
            };
            let pickle = marshal::encode(&value);
            let crc = crc32fast::hash(&pickle) as i32;
            tables.push(CachedTable {
                key,
                pickle,
                version: (now, crc),
                rows: n,
            });
        }
        let by_key = tables
            .iter()
            .enumerate()
            .map(|(i, t)| (t.key.clone(), i))
            .collect();
        Ok(Statics { tables, by_key })
    }

    pub fn get(&self, key: &str) -> Option<&CachedTable> {
        self.by_key.get(key).map(|&i| &self.tables[i])
    }

    /// The initvals dict for AuthenticationRsp.
    pub fn initvals(&self, node_id: i64) -> Value {
        Value::Dict(
            self.tables
                .iter()
                .map(|t| {
                    (
                        Value::str(&t.key),
                        cached_object_stub(&t.key, t.version, node_id),
                    )
                })
                .collect(),
        )
    }
}

fn version_value(v: (i64, i32)) -> Value {
    Value::tuple([Value::Long(v.0), Value::Int(v.1)])
}

/// util.CachedObject.__getstate__: (__objectID__, __nodeID__, __objectVersion__,
/// __shared__); the trailing __shared__ is dropped when true, and __setstate__
/// restores it as 1.
pub fn cached_object_stub(object_id: &str, version: (i64, i32), node_id: i64) -> Value {
    Value::instance(
        "util.CachedObject",
        Value::tuple([
            Value::str(object_id),
            Value::from(node_id),
            version_value(version),
        ]),
    )
}

/// objectCaching.CachedObject.__getstate__:
/// (version, object, nodeID, shared, pickle, compressed, objectID).
/// The object travels as its pickle; GetObject() unmarshals it on demand.
pub fn cached_object(table: &CachedTable, node_id: i64) -> Value {
    Value::instance(
        "objectCaching.CachedObject",
        Value::tuple([
            version_value(table.version),
            Value::None,
            Value::from(node_id),
            Value::Int(1),
            Value::Str(table.pickle.clone()),
            Value::Int(0),
            Value::str(&table.key),
        ]),
    )
}
