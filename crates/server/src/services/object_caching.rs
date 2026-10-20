//! objectCaching.GetCachableObject(shared, objectID, objectVersion, nodeID)
//! (net/objectCaching.py:266). Returns an objectCaching.CachedObject.

use anyhow::anyhow;
use marshal::Value;
use proto::gpcs::Call;

use super::CallResult;
use crate::{statics, Server};

pub fn get_cachable_object(server: &Server, call: &Call) -> CallResult {
    let object_id = call
        .args()
        .get(1)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("GetCachableObject without an objectID: {:?}", call.args()))?;
    let table = server
        .statics
        .get(object_id)
        .ok_or_else(|| anyhow!("no cached object {object_id}"))?;
    tracing::info!(
        object_id,
        rows = table.rows,
        bytes = table.pickle.len(),
        "serving cached object"
    );
    Ok(statics::cached_object(table, server.config.node_id))
}
