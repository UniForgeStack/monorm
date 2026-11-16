//! JSON helpers that any extension or feature can use: a canonical form with a content hash,
//! and the difference of two JSON objects.
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

/// The canonical text of `value`: object keys sorted by byte order, no spaces.
pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 { out.push(','); }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                write_canonical(&map[key], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 { out.push(','); }
                write_canonical(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// The SHA-256 of the canonical text of `value`, as 64 lowercase hex digits.
pub fn canonical_hash(value: &Value) -> String {
    format!("{:x}", Sha256::digest(canonical_json(value).as_bytes()))
}

/// The keys whose values differ between two objects, as `{key: {"old": ..., "new": ...}}`.
/// A key that only one side has shows `null` for the other side. Values compare by their canonical text.
pub fn json_diff(old: &Map<String, Value>, new: &Map<String, Value>) -> Map<String, Value> {
    let mut keys: Vec<&String> = old.keys().chain(new.keys()).collect();
    keys.sort();
    keys.dedup();
    let mut out = Map::new();
    for key in keys {
        let (a, b) = (old.get(key), new.get(key));
        let same = match (a, b) {
            (Some(a), Some(b)) => canonical_json(a) == canonical_json(b),
            _ => false,
        };
        if !same {
            let mut change = Map::new();
            change.insert("old".into(), a.cloned().unwrap_or(Value::Null));
            change.insert("new".into(), b.cloned().unwrap_or(Value::Null));
            out.insert(key.clone(), Value::Object(change));
        }
    }
    out
}

/// The text forms that the bindings call: JSON in, text out.
pub mod text {
    use super::*;

    fn parse(text: &str) -> Result<Value, String> {
        serde_json::from_str(text).map_err(|e| format!("invalid JSON: {e}"))
    }

    fn object(text: &str) -> Result<Map<String, Value>, String> {
        match parse(text)? {
            Value::Object(map) => Ok(map),
            _ => Err("expected a JSON object".into()),
        }
    }

    /// The content hash of a JSON text.
    pub fn hash(json: &str) -> Result<String, String> { Ok(canonical_hash(&parse(json)?)) }

    /// The diff of two JSON object texts, as a JSON object text.
    pub fn diff(old: &str, new: &str) -> Result<String, String> {
        Ok(Value::Object(json_diff(&object(old)?, &object(new)?)).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_canonical_text_sorts_keys_and_drops_spaces() {
        let value = json!({"b": [1, {"z": null, "a": "x y"}], "a": true, "é": 1.5});
        assert_eq!(canonical_json(&value), r#"{"a":true,"b":[1,{"a":"x y","z":null}],"é":1.5}"#);
    }

    #[test]
    fn the_hash_ignores_key_order_and_matches_the_fixed_vectors() {
        assert_eq!(canonical_hash(&json!({"a": 1, "b": 2})), canonical_hash(&json!({"b": 2, "a": 1})));
        // The same vectors sit in the Python and TypeScript tests.
        assert_eq!(canonical_hash(&json!({})), "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a");
        assert_eq!(canonical_hash(&json!({"b": [1, 2], "a": "x"})), "721ef82f2d6c0997bffb7a8ab3f40f8fb45b0b52ce2af3afa6b0f05efbdc317f");
    }

    #[test]
    fn the_diff_lists_changed_added_and_removed_keys_only() {
        let old = json!({"a": 1, "b": [1, 2], "c": "gone", "same": {"x": 1, "y": 2}});
        let new = json!({"a": 2, "b": [1, 2], "d": "new", "same": {"y": 2, "x": 1}});
        let diff = json_diff(old.as_object().unwrap(), new.as_object().unwrap());
        assert_eq!(Value::Object(diff), json!({
            "a": {"old": 1, "new": 2},
            "c": {"old": "gone", "new": null},
            "d": {"old": null, "new": "new"},
        }));
    }

    #[test]
    fn the_text_forms_reject_bad_input() {
        assert!(text::hash("{").unwrap_err().starts_with("invalid JSON"));
        assert_eq!(text::diff("[]", "{}").unwrap_err(), "expected a JSON object");
        assert_eq!(text::diff(r#"{"a":1}"#, r#"{"a":1}"#).unwrap(), "{}");
    }
}
