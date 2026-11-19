//! A view field: stored bytes that the host shows as another value (a proto message as JSON, or as "N bytes").
//!
//! The Rust side reads the bytes and asks the host once for each page or record. It never decodes them.

use serde_json::{json, Value};

use crate::auth::AuthConfig;
use crate::bridge::CallContext;
use crate::list::ListError;
use crate::site::Column;

/// The host function that converts the stored bytes of a view field.
pub const HOST_FUNCTION: &str = "admin.view";

/// Replaces the stored bytes (base64 or null) of each view column in `rows` with the host's view of them.
pub(crate) async fn convert_rows(viewer: Option<&AuthConfig>, model: &str, columns: &[Column], rows: &mut [&mut Vec<Value>]) -> Result<(), ListError> {
    for (at, c) in columns.iter().enumerate().filter(|(_, c)| c.view.is_some()) {
        if rows.is_empty() {
            break;
        }
        let stored: Vec<Value> = rows.iter().map(|r| r[at].clone()).collect();
        let shown = ask(viewer, model, c, stored).await?;
        for (row, v) in rows.iter_mut().zip(shown) {
            row[at] = v;
        }
    }
    Ok(())
}

async fn ask(viewer: Option<&AuthConfig>, model: &str, c: &Column, stored: Vec<Value>) -> Result<Vec<Value>, ListError> {
    if stored.iter().all(Value::is_null) {
        return Ok(stored);
    }
    let auth = viewer.ok_or_else(|| ListError::Engine("a view field needs the host bridge: serve the admin with auth".into()))?;
    let (bridge, ctx) = (&auth.bridge, CallContext { request_id: auth.security.call_id(), scope: json!({}), tx: None });
    let n = stored.len();
    let answer = bridge.call(HOST_FUNCTION, json!({"model": model, "field": c.field, "values": stored}), ctx).await.map_err(|e| ListError::Engine(format!("{}: {}", e.kind(), e.message())))?;
    match answer {
        Value::Array(v) if v.len() == n => Ok(v),
        _ => Err(ListError::Engine(format!("{HOST_FUNCTION} returns a list with one value for each of {n} values"))),
    }
}
