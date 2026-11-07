//! The key of a record in a URL: `/admin/<resource>/<key>`.
//!
//! A composite key is its percent-encoded fields in key order, joined with `,` (a `,` in a value is `%2C`).
//! The list, the detail route and `admin.url` share this rule, and only this file knows it.

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

/// The detail URL of one record from app code. `key` is a scalar (one key field), a list in key order,
/// or an object of `{field: value}`. A wrong shape or a value of the wrong type is an error.
pub fn url_of(resource: &crate::site::Resource, key: &Value) -> Result<String, String> {
    let fields: Vec<&str> = resource.key_columns.iter().map(|c| c.field.as_str()).collect();
    let wrong = |why: &str| format!("{}: the key takes {} ({why})", resource.name, fields.join(", "));
    let values: Vec<Value> = match key {
        Value::Array(items) => items.clone(),
        Value::Object(map) => {
            if let Some(extra) = map.keys().find(|k| !fields.contains(&k.as_str())) {
                return Err(wrong(&format!("no key field {extra:?}")));
            }
            fields.iter().map(|f| map.get(*f).cloned().ok_or_else(|| wrong(&format!("{f} is missing")))).collect::<Result<_, _>>()?
        }
        scalar => vec![scalar.clone()],
    };
    if values.len() != fields.len() {
        return Err(wrong(&format!("got {} value(s)", values.len())));
    }
    if values.iter().any(|v| !matches!(v, Value::String(_) | Value::Number(_) | Value::Bool(_))) {
        return Err(wrong("a value is a string, a number or a boolean"));
    }
    let segment = encode(&values);
    // A browser reads a whole segment of `.` or `..` (also as `%2E`) as a path step, so the page cannot be reached.
    if matches!(segment.as_str(), "." | "..") {
        return Err(wrong("a key of `.` or `..` has no page URL"));
    }
    let types: Vec<ColType> = resource.key_columns.iter().map(|c| c.ty.ty).collect();
    // The detail route binds the decoded values: a value that it refuses must not get a URL.
    let decoded = decode(&types, &segment).map_err(|m| format!("{}: {m}", resource.name))?;
    for (v, ty) in decoded.iter().zip(&types) {
        crate::json::bind(*ty, v).map_err(|_| wrong(&format!("{v} is not a {ty:?} that a page URL can name")))?;
    }
    Ok(url(&resource.name, &segment))
}
