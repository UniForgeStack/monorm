//! The key of a record in a URL: `/admin/<resource>/<key>`.
//!
//! A composite key is the percent-encoded text of every key field in key order, joined with `,`.
//! A `,` inside a value is `%2C`, so one split is exact. The same rule runs for the list, the
//! detail route and `admin.url`, and only this file knows it.

use monorm_core::ir::ColType;
use serde_json::{json, Value};

/// The path of the SPA under which the pages live. The API prefix is a separate setting.
pub const UI_BASE: &str = "/admin";

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn unescape(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The key segment of the values of the key fields (as the JSON of a row cell), in key order.
pub fn encode(values: &[Value]) -> String {
    values.iter().map(|v| escape(&text(v))).collect::<Vec<_>>().join(",")
}

/// The key values (the JSON that `json::bind` takes) from a key segment, for the key types of a resource.
pub fn decode(types: &[ColType], raw: &str) -> Result<Vec<Value>, String> {
    let bad = || format!("the key {raw:?} does not fit this resource: it has {} key field(s)", types.len());
    let parts: Vec<&str> = raw.split(',').collect();
    if parts.len() != types.len() {
        return Err(bad());
    }
    let mut out = Vec::with_capacity(types.len());
    for (part, ty) in parts.iter().zip(types) {
        let t = unescape(part).ok_or_else(bad)?;
        out.push(match ty {
            ColType::Int => json!(t.parse::<i64>().map_err(|_| bad())?),
            ColType::Float => json!(t.parse::<f64>().map_err(|_| bad())?),
            ColType::Bool => json!(t.parse::<bool>().map_err(|_| bad())?),
            _ => Value::String(t),
        });
    }
    Ok(out)
}

/// The page of a record: `/admin/<resource>/<key>`.
pub fn url(resource: &str, key: &str) -> String {
    format!("{UI_BASE}/{resource}/{key}")
}
