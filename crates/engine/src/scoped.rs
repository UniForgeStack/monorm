//! Scope columns: a field declared by an extension as holding the value of `scope.<key>`.
//! An insert fills it, a write that sets it to another value fails.
//! `unscoped(key)` turns both off for that key.

use crate::error::{Error, Result};
use crate::params::Params;
use monorm_core::schema::Model;
use monorm_core::writes::PreparedScoped;
use sea_query::Value;

type Rows = Vec<Vec<Option<Value>>>;

/// The scope value of `s` for a write of `m`, coerced to the column type; `None` under `unscoped(key)`.
pub(crate) fn value(m: &Model, s: &PreparedScoped, params: &dyn Params) -> Result<Option<Value>> {
    if params.unscoped(&s.key) { return Ok(None); }
    let field = &m.fields()[s.field];
    let Some(i) = params.scope(&s.key) else {
        return Err(Error::ScopeMissing {
            model: m.ir.name.clone(),
            key: s.key.clone(),
            message: format!("{} writes {}, which holds scope.{}, and the scope has no value for it: set it with scope(), or use unscoped({:?}) to write across scopes", m.ir.name, field.name, s.key, s.key),
        });
    };
    if i >= params.len() { return Err(Error::query(format!("parameter {i} out of range"))); }
    params.value(i, Some(field.value_type())).map(Some)
}

/// An insert: an omitted cell takes the scope value, an explicit other value fails, an absent column is added.
pub(crate) fn fill(m: &Model, fields: &mut Vec<String>, rows: &mut Rows, params: &dyn Params) -> Result<()> {
    for s in &m.scoped {
        let Some(scope) = value(m, s, params)? else { continue };
        let name = &m.fields()[s.field].name;
        let slot = match fields.iter().position(|f| f == name) {
            Some(slot) => slot,
            None => {
                fields.push(name.clone());
                for row in rows.iter_mut() { row.push(None); }
                fields.len() - 1
            }
        };
        for row in rows.iter_mut() {
            let Some(cell) = row.get_mut(slot) else { return Err(Error::query("insert row length does not match fields")) };
            match cell {
                None => *cell = Some(scope.clone()),
                Some(v) if *v == scope => {}
                Some(v) => return Err(mismatch(m, s, &scope, Some(v))),
            }
        }
    }
    Ok(())
}

/// An update: the scope column may be set to the scope value only.
/// `set` is the value of the assignment of `field` (a parameter, or SQL `NULL`); any other expression is refused.
pub(crate) fn check_set(m: &Model, field: &str, set: Assigned, params: &dyn Params) -> Result<()> {
    let Some(s) = m.scoped.iter().find(|s| m.fields()[s.field].name == field) else { return Ok(()) };
    let Some(scope) = value(m, s, params)? else { return Ok(()) };
    let ty = m.fields()[s.field].value_type();
    match set {
        Assigned::Param(i) => {
            if i >= params.len() { return Err(Error::query(format!("parameter {i} out of range"))); }
            let v = params.value(i, Some(ty))?;
            if v == scope { Ok(()) } else { Err(mismatch(m, s, &scope, Some(&v))) }
        }
        Assigned::Null => Err(mismatch(m, s, &scope, None)),
        Assigned::NonLiteral => Err(Error::query(format!("{}.{field} holds scope.{}: assign it a plain value equal to the scope, or use unscoped({:?})", m.ir.name, s.key, s.key))),
    }
}

/// `update_many` rows: the cells of the scope columns must equal the scope value.
pub(crate) fn check_rows<'a>(m: &Model, fields: &[String], rows: impl Iterator<Item = &'a [Value]>, params: &dyn Params) -> Result<()> {
    let rows: Vec<&[Value]> = rows.collect();
    for s in &m.scoped {
        let name = &m.fields()[s.field].name;
        let Some(slot) = fields.iter().position(|f| f == name) else { continue };
        let Some(scope) = value(m, s, params)? else { continue };
        if let Some(v) = rows.iter().map(|r| &r[slot]).find(|v| **v != scope) {
            return Err(mismatch(m, s, &scope, Some(v)));
        }
    }
    Ok(())
}

/// What an update assigns to a field.
pub(crate) enum Assigned {
    Param(usize),
    Null,
    NonLiteral,
}

/// An upsert's `DO UPDATE` columns without the scope columns: the existing row keeps its scope.
pub(crate) fn keep_scope(m: &Model, update: &[String], params: &dyn Params) -> Vec<String> {
    let mut kept = update.to_vec();
    for s in m.scoped.iter().filter(|s| !params.unscoped(&s.key)) {
        let name = &m.fields()[s.field].name;
        kept.retain(|u| u != name);
    }
    kept
}

fn mismatch(m: &Model, s: &PreparedScoped, expected: &Value, actual: Option<&Value>) -> Error {
    Error::ScopeMismatch {
        model: m.ir.name.clone(),
        key: s.key.clone(),
        expected: show(Some(expected)),
        actual: show(actual),
    }
}

/// A value as the caller wrote it, for the message: a number bare, a string quoted, a null as `NULL`.
pub(crate) fn show(v: Option<&Value>) -> String {
    match v {
        None => "NULL".into(),
        Some(Value::Bool(Some(b))) => b.to_string(),
        Some(Value::TinyInt(Some(n))) => n.to_string(),
        Some(Value::SmallInt(Some(n))) => n.to_string(),
        Some(Value::Int(Some(n))) => n.to_string(),
        Some(Value::BigInt(Some(n))) => n.to_string(),
        Some(Value::TinyUnsigned(Some(n))) => n.to_string(),
        Some(Value::SmallUnsigned(Some(n))) => n.to_string(),
        Some(Value::Unsigned(Some(n))) => n.to_string(),
        Some(Value::BigUnsigned(Some(n))) => n.to_string(),
        Some(Value::String(Some(t))) => format!("{:?}", t.as_str()),
        Some(Value::Uuid(Some(u))) => u.to_string(),
        Some(Value::Bool(None) | Value::TinyInt(None) | Value::SmallInt(None) | Value::Int(None) | Value::BigInt(None) | Value::TinyUnsigned(None) | Value::SmallUnsigned(None) | Value::Unsigned(None) | Value::BigUnsigned(None) | Value::String(None) | Value::Uuid(None)) => "NULL".into(),
        // Any other column type gets the readable `Debug` text.
        Some(other) => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn show_prints_a_value_as_the_caller_wrote_it() {
        let u = uuid::Uuid::nil();
        assert_eq!(show(Some(&Value::Int(Some(7)))), "7");
        assert_eq!(show(Some(&Value::BigInt(Some(-3)))), "-3");
        assert_eq!(show(Some(&Value::BigUnsigned(Some(5)))), "5");
        assert_eq!(show(Some(&Value::Bool(Some(true)))), "true");
        assert_eq!(show(Some(&Value::String(Some("a\"b".into())))), "\"a\\\"b\"");
        assert_eq!(show(Some(&Value::Uuid(Some(u)))), u.to_string());
        assert_eq!(show(Some(&Value::Int(None))), "NULL");
        assert_eq!(show(Some(&Value::String(None))), "NULL");
        assert_eq!(show(None), "NULL");
    }
}
