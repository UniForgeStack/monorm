//! Cells as JSON, and cursor values back as bind values.
//!
//! Big integers and decimals are JSON strings, so a browser keeps every digit. Dates and
//! times are ISO 8601; a time carries its offset.

use chrono::{DateTime, SecondsFormat};
use monorm_core::ir::{ColType, EnumStorage, ValueType};
use monorm_core::schema::Schema;
use monorm_engine::db::Cell;
use sea_query::Value as Bind;
use serde_json::{json, Value};

pub fn cell(schema: &Schema, ty: ValueType, cell: Cell<'_>) -> Value {
    match cell {
        Cell::Null => Value::Null,
        Cell::Bool(b) => json!(b),
        Cell::Int(n) => enum_label(schema, ty, i64::from(n)).map_or_else(|| json!(n), Value::String),
        Cell::BigInt(n) => json!(n.to_string()),
        Cell::Float(f) => serde_json::Number::from_f64(f).map_or_else(|| json!(f.to_string()), Value::Number),
        Cell::Text(s) => json!(s),
        Cell::DateTime(t) => json!(t.to_rfc3339_opts(SecondsFormat::AutoSi, false)),
        Cell::Date(d) => json!(d.format("%Y-%m-%d").to_string()),
        Cell::Uuid(u) => json!(u.to_string()),
        Cell::Json(v) => v,
        Cell::Decimal(s) => json!(s),
        Cell::Array(items) => Value::Array(items.into_iter().map(|c| self::cell(schema, ty.element(), c)).collect()),
    }
}

/// The schema name of an integer-stored enum value.
fn enum_label(schema: &Schema, ty: ValueType, n: i64) -> Option<String> {
    let e = schema.enums.get(ty.enum_idx? as usize)?;
    if e.storage != EnumStorage::Int {
        return None;
    }
    e.values.iter().find(|v| v.value.as_i64() == Some(n)).map(|v| v.name.clone())
}

/// A cursor value, as the JSON of a row cell, as a bind value of the column type.
pub fn bind(ty: ColType, v: &Value) -> Result<Bind, String> {
    let bad = || format!("a cursor value {v} is not a {ty:?}");
    Ok(match ty {
        ColType::BigInt => Bind::BigInt(Some(v.as_str().ok_or_else(bad)?.parse().map_err(|_| bad())?)),
        ColType::Int => Bind::Int(Some(i32::try_from(v.as_i64().ok_or_else(bad)?).map_err(|_| bad())?)),
        ColType::Float => Bind::Double(Some(v.as_f64().ok_or_else(bad)?)),
        ColType::Bool => Bind::Bool(Some(v.as_bool().ok_or_else(bad)?)),
        ColType::String | ColType::Text => Bind::String(Some(v.as_str().ok_or_else(bad)?.to_owned())),
        ColType::DateTime => Bind::ChronoDateTimeWithTimeZone(Some(DateTime::parse_from_rfc3339(v.as_str().ok_or_else(bad)?).map_err(|_| bad())?)),
        ColType::Date => Bind::ChronoDate(Some(chrono::NaiveDate::parse_from_str(v.as_str().ok_or_else(bad)?, "%Y-%m-%d").map_err(|_| bad())?)),
        ColType::Uuid => Bind::Uuid(Some(v.as_str().ok_or_else(bad)?.parse().map_err(|_| bad())?)),
        ColType::Json | ColType::Decimal => return Err(bad()),
    })
}
