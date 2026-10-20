//! Builders for the client's row containers (script/sys/rowset.py).
//!
//! None of these classes define __getstate__, so their `__dict__` is the wire
//! state: util.Rowset {header, lines}, util.Row {header, line},
//! util.KeyVal {field: value, ...}.

use anyhow::Result;
use marshal::Value;
use tokio_postgres::Row as PgRow;

use crate::db;

fn header_value(header: &[&str]) -> Value {
    Value::List(header.iter().map(|h| Value::str(h)).collect())
}

/// util.Rowset: iterable, indexable, sliceable; rows read by attribute.
pub fn rowset(header: &[&str], lines: Vec<Vec<Value>>) -> Value {
    Value::instance(
        "util.Rowset",
        Value::dict([
            (Value::str("header"), header_value(header)),
            (
                Value::str("lines"),
                Value::List(lines.into_iter().map(Value::List).collect()),
            ),
        ]),
    )
}

/// util.Row: one line read by attribute or by name/index subscript.
pub fn row(header: &[&str], line: Vec<Value>) -> Value {
    Value::instance(
        "util.Row",
        Value::dict([
            (Value::str("header"), header_value(header)),
            (Value::str("line"), Value::List(line)),
        ]),
    )
}

/// util.KeyVal: an attribute bag.
pub fn keyval(fields: Vec<(&str, Value)>) -> Value {
    Value::instance(
        "util.KeyVal",
        Value::Dict(
            fields
                .into_iter()
                .map(|(k, v)| (Value::str(k), v))
                .collect(),
        ),
    )
}

/// `(header, lines)`: the shape the Recordset fetchers (GetMulti*Ex) and the
/// bulk tables use.
pub fn header_lines(header: &[&str], lines: Vec<Vec<Value>>) -> Value {
    Value::tuple([
        header_value(header),
        Value::List(lines.into_iter().map(Value::List).collect()),
    ])
}

pub fn lines(rows: &[PgRow]) -> Result<Vec<Vec<Value>>> {
    rows.iter().map(db::row_values).collect()
}

/// A query result as util.Rowset, with an explicit header so empty results
/// still carry their columns.
pub fn rowset_from(header: &[&str], rows: &[PgRow]) -> Result<Value> {
    Ok(rowset(header, lines(rows)?))
}
