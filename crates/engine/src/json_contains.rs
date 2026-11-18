//! PostgreSQL's `jsonb @> jsonb` for SQLite, which has no containment operator.
//! SQLite takes it as the scalar function `monorm_json_contains(l, r)`; its SQL does not depend on the
//! operand, so a `param()` in a prepared query works. The rules are those of `JsonbDeepContains`.
use serde_json::{Number, Value};

/// The SQL name of the function.
pub const SQL_NAME: &str = "monorm_json_contains";

/// `l @> r`, top level: an array contains a scalar that is one of its elements.
pub fn contains(l: &Value, r: &Value) -> bool {
    match (l, r) {
        (Value::Array(items), scalar) if is_scalar(scalar) => items.iter().any(|i| scalar_eq(i, scalar)),
        _ => nested(l, r),
    }
}

/// A value inside a container: the types must match, so `{"a": [1]}` does not contain `{"a": 1}`.
fn nested(l: &Value, r: &Value) -> bool {
    match (l, r) {
        (Value::Object(l), Value::Object(r)) => r.iter().all(|(k, rv)| l.get(k).is_some_and(|lv| nested(lv, rv))),
        (Value::Array(l), Value::Array(r)) => r.iter().all(|rv| l.iter().any(|lv| nested(lv, rv))),
        (l, r) if is_scalar(l) && is_scalar(r) => scalar_eq(l, r),
        _ => false,
    }
}

fn is_scalar(v: &Value) -> bool {
    !matches!(v, Value::Array(_) | Value::Object(_))
}

fn scalar_eq(l: &Value, r: &Value) -> bool {
    match (l, r) {
        (Value::Number(l), Value::Number(r)) => number_eq(l, r),
        _ => l == r,
    }
}

/// `1` equals `1.0`, as `numeric` does. Integers beyond 2^53 against floats compare as `f64`.
fn number_eq(l: &Number, r: &Number) -> bool {
    match (l.as_i64(), r.as_i64(), l.as_u64(), r.as_u64()) {
        (Some(a), Some(b), ..) => a == b,
        (_, _, Some(a), Some(b)) => a == b,
        _ => l.as_f64() == r.as_f64(),
    }
}

#[cfg(feature = "sqlite")]
pub fn register(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    use rusqlite::functions::FunctionFlags;
    use rusqlite::types::ValueRef;
    conn.create_scalar_function(SQL_NAME, 2, FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC, |ctx| {
        let mut docs = [None, None];
        for (i, doc) in docs.iter_mut().enumerate() {
            *doc = match ctx.get_raw(i) {
                ValueRef::Null => return Ok(None),
                ValueRef::Text(t) => Some(serde_json::from_slice::<Value>(t).map_err(|e| rusqlite::Error::UserFunctionError(format!("malformed JSON: {e}").into()))?),
                _ => return Err(rusqlite::Error::UserFunctionError("json containment takes JSON text".into())),
            };
        }
        let [Some(l), Some(r)] = docs else { return Ok(None) };
        Ok(Some(contains(&l, &r)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(l: &str, r: &str) -> bool {
        contains(&serde_json::from_str(l).unwrap(), &serde_json::from_str(r).unwrap())
    }

    /// Every pair was checked against PostgreSQL 18's `jsonb @>`.
    #[test]
    fn matches_postgres() {
        let yes = [
            (r#"{"a":1,"b":2}"#, r#"{"a":1}"#), (r#"{"a":1}"#, r#"{"a":1.0}"#), (r#"{"a":{"b":1,"c":2}}"#, r#"{"a":{"b":1}}"#),
            (r#"{"a":[1,2]}"#, r#"{"a":[1]}"#), ("[1,2,3]", "2"), ("[1,2,3]", "[2,3]"), ("[1]", "[1,1]"), ("[1]", "1"), (r#""a""#, r#""a""#),
            (r#"["a"]"#, r#""a""#), ("[[1,2]]", "[[1]]"), ("[1,[2]]", "[[2]]"), (r#"[{"a":1,"b":2}]"#, r#"[{"a":1}]"#),
            (r#"{"a":null}"#, r#"{"a":null}"#), ("{}", "{}"), ("[]", "[]"), (r#"{"a":1}"#, "{}"), ("[1]", "[]"), ("null", "null"),
            ("[null]", "null"), ("true", "true"), ("[1,2]", "[1.0]"), (r#"{"a":[[1]]}"#, r#"{"a":[[]]}"#), ("[[]]", "[[]]"), ("[[1]]", "[]"),
        ];
        let no = [
            (r#"{"a":[1,2]}"#, r#"{"a":1}"#), ("1", "[1]"), (r#""a""#, r#"["a"]"#), ("[1,[2]]", "[2]"), (r#"[{"a":1},{"b":2}]"#, r#"[{"a":1,"b":2}]"#),
            ("[]", "{}"), (r#"{"a":true}"#, r#"{"a":1}"#), (r#"{"a":"1"}"#, r#"{"a":1}"#), ("[1]", "[[]]"),
        ];
        for (l, r) in yes {
            assert!(c(l, r), "{l} @> {r}");
        }
        for (l, r) in no {
            assert!(!c(l, r), "{l} @> {r}");
        }
    }
}
