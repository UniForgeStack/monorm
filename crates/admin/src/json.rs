//! Cells as JSON, bind values as the JSON of a cell, and cursor values back as bind values.
//!
//! Big integers and decimals are JSON strings, so a browser keeps every digit. Dates and
//! times are ISO 8601; a time carries its offset.

use bigdecimal::BigDecimal;
use chrono::{DateTime, SecondsFormat, Utc};
use std::str::FromStr;
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
        // A bridge function returns plain integers for a BigInt key; a row cell shows the string form.
        ColType::BigInt => Bind::BigInt(Some(match v.as_i64() {
            Some(n) => n,
            None => v.as_str().ok_or_else(bad)?.parse().map_err(|_| bad())?,
        })),
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

/// A bind value as the JSON of a row cell of type `ty`: the form a stored value of that type has.
pub fn bind_json(schema: &Schema, ty: ValueType, b: &Bind) -> Value {
    cell(schema, ty, bind_cell(b))
}

fn bind_cell(b: &Bind) -> Cell<'_> {
    match b {
        Bind::Bool(Some(x)) => Cell::Bool(*x),
        Bind::Int(Some(x)) => Cell::Int(*x),
        Bind::BigInt(Some(x)) => Cell::BigInt(*x),
        Bind::Double(Some(x)) => Cell::Float(*x),
        Bind::String(Some(x)) => Cell::Text(x),
        Bind::ChronoDateTimeWithTimeZone(Some(x)) => Cell::DateTime(x.with_timezone(&Utc)),
        Bind::ChronoDate(Some(x)) => Cell::Date(*x),
        Bind::Uuid(Some(x)) => Cell::Uuid(*x),
        Bind::Json(Some(x)) => Cell::Json((**x).clone()),
        Bind::BigDecimal(Some(x)) => Cell::Decimal(x.to_string()),
        Bind::Array(_, Some(items)) => Cell::Array(items.iter().map(bind_cell).collect()),
        _ => Cell::Null,
    }
}

/// Two JSON cells of one column type hold the same value. A decimal compares by number, so `5` and `"5.00"` match.
pub fn same(ty: ColType, a: &Value, b: &Value) -> bool {
    if a == b {
        return true;
    }
    match (ty, a, b) {
        (ColType::Decimal, Value::String(x), Value::String(y)) => matches!((BigDecimal::from_str(x), BigDecimal::from_str(y)), (Ok(x), Ok(y)) if x == y),
        _ => false,
    }
}
