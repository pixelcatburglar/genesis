//! The client's session (script/sys/sessions.py base.Session) as the server
//! tracks it, and the SessionChangeNotification that keeps the two in step.
//!
//! The client applies a change dict VERBATIM (ApplyRemoteAttributeChanges): no
//! dependent attributes are derived, so every attribute that changes must be
//! listed - e.g. locationid alongside stationid. machoNet also closes the
//! connection on a charid change whose old value is not None.

use marshal::Value;
use proto::{Address, Command, Packet};

/// base.Session.__persistvars__ plus the non-persisted locationid.
pub const ATTRIBUTES: &[&str] = &[
    "userid",
    "regionid",
    "constellationid",
    "role",
    "charid",
    "corpid",
    "corprole",
    "gangid",
    "gangrole",
    "shipid",
    "stationid",
    "locrole",
    "solarsystemid",
    "solarsystemid2",
    "flag",
    "address",
    "callback",
    "locationid",
];

#[derive(Debug, Default, Clone)]
pub struct Attributes {
    values: Vec<(&'static str, Value)>,
}

impl Attributes {
    pub fn get(&self, name: &str) -> &Value {
        self.values
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v)
            .unwrap_or(&Value::None)
    }

    pub fn int(&self, name: &str) -> Option<i64> {
        self.get(name).as_int()
    }

    /// Applies changes and returns the (old, new) pairs that actually differ.
    pub fn apply(
        &mut self,
        changes: &[(&'static str, Value)],
    ) -> Vec<(&'static str, Value, Value)> {
        let mut diff = Vec::new();
        for (name, new) in changes {
            debug_assert!(
                ATTRIBUTES.contains(name),
                "unknown session attribute {name}"
            );
            let old = self.get(name).clone();
            if &old != new {
                diff.push((*name, old, new.clone()));
                self.values.retain(|(k, _)| k != name);
                self.values.push((name, new.clone()));
            }
        }
        diff
    }
}

/// SessionChangeNotification(change=(0, {attr: (old, new)})), channel
/// 'sessionchange' (its __machodesc__ default, which the client routes on).
pub fn change_notification(
    node_id: i64,
    client_id: i64,
    diff: &[(&'static str, Value, Value)],
) -> Option<Packet> {
    if diff.is_empty() {
        return None;
    }
    let change = Value::Dict(
        diff.iter()
            .map(|(k, old, new)| (Value::str(k), Value::tuple([old.clone(), new.clone()])))
            .collect(),
    );
    let mut p = Packet::new(
        Command::SessionChangeNotification,
        Address::node(node_id),
        Address::client(client_id),
        vec![Value::tuple([Value::Int(0), change])],
    );
    p.oob_set("channel", Value::str("sessionchange"));
    Some(p)
}
