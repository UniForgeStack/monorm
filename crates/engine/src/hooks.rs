//! Change-set hooks at the write seams. Each seam reads `Model.hooks` once per statement and
//! runs the transforms, the validators and the record validators of `HookPlan` on the supplied values.
use crate::error::{Error, Result};
use orm_contracts::abi::{Arg, Context, Value as Abi};
use orm_core::{hooks::{HookPlan, HookRef}, schema::Model};
use sea_query::Value;

/// Map a logical field position to its supplied row slot, once per operation.
pub fn input_map(width: usize, positions: &[usize]) -> Result<Vec<Option<usize>>> {
    let mut map = vec![None; width];
    for (slot, &field) in positions.iter().enumerate() {
        if map[field].is_some() { return Err(Error::query("duplicate supplied field")); }
        map[field] = Some(slot);
    }
    Ok(map)
}

/// One supplied row. `None` is a database expression or an omitted value.
pub trait Slots {
    fn slot(&mut self, index: usize) -> Option<&mut Value>;
    fn width(&self) -> usize;
}
impl Slots for [Value] {
    fn slot(&mut self, index: usize) -> Option<&mut Value> { self.get_mut(index) }
    fn width(&self) -> usize { self.len() }
}
impl Slots for [Option<Value>] {
    fn slot(&mut self, index: usize) -> Option<&mut Value> { self.get_mut(index).and_then(Option::as_mut) }
    fn width(&self) -> usize { self.len() }
}

const RESTRICTED: &str = "native validated fields require supplied values; database defaults and expressions need a constraint or explicit transaction strategy";
const PARTIAL: &str = "record validation requires all non-null supplied dependencies; use an explicit transaction for partial records";

fn to_abi(model: &Model, position: usize, value: &Value) -> Result<Abi> {
    use chrono::{DateTime, NaiveDate};
    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).expect("epoch");
    let micros = |t: DateTime<chrono::Utc>| Abi::DateTime(t.timestamp_micros());
    Ok(match value {
        Value::Bool(v) => v.map_or(Abi::Null, Abi::Bool),
        Value::TinyInt(v) => v.map_or(Abi::Null, |v| Abi::Int(v.into())),
        Value::SmallInt(v) => v.map_or(Abi::Null, |v| Abi::Int(v.into())),
        Value::Int(v) => v.map_or(Abi::Null, |v| Abi::Int(v.into())),
        Value::BigInt(v) => v.map_or(Abi::Null, Abi::Int),
        Value::Float(v) => v.map_or(Abi::Null, |v| Abi::Float(v.into())),
        Value::Double(v) => v.map_or(Abi::Null, Abi::Float),
        Value::String(v) => v.as_ref().map_or(Abi::Null, |v| Abi::Text(v.clone())),
        Value::Bytes(v) => v.as_ref().map_or(Abi::Null, |v| Abi::Bytes(v.clone())),
        Value::Json(v) => v.as_ref().map_or(Abi::Null, |v| Abi::Json(v.to_string())),
        Value::Uuid(v) => v.map_or(Abi::Null, |v| Abi::Uuid(*v.as_bytes())),
        Value::BigDecimal(v) => v.as_ref().map_or(Abi::Null, |v| Abi::Decimal(v.to_string())),
        Value::ChronoDate(v) => v.map_or(Abi::Null, |v| Abi::Date(v.signed_duration_since(epoch).num_days() as i32)),
        Value::ChronoDateTimeUtc(v) => v.map_or(Abi::Null, micros),
        Value::ChronoDateTimeWithTimeZone(v) => v.map_or(Abi::Null, |v| micros(v.to_utc())),
        Value::ChronoDateTime(v) => v.map_or(Abi::Null, |v| micros(v.and_utc())),
        _ => return Err(Error::query(format!("{}.{}: hooks do not support this field type", model.ir.name, model.fields()[position].name))),
    })
}

/// The database value for what a hook returned. The original value gives the type.
fn from_abi(model: &Model, position: usize, original: &Value, out: Abi) -> Result<Value> {
    use chrono::DateTime;
    let field = &model.fields()[position];
    let bad = |what: &str| Error::query(format!("{}.{}: hook returned {what} for this field", model.ir.name, field.name));
    if matches!(out, Abi::Null) {
        if !field.nullable { return Err(bad("null")); }
        return Ok(original.as_null());
    }
    let int = |v: i64, lo: i64, hi: i64| if (lo..=hi).contains(&v) { Ok(v) } else { Err(bad("an integer out of range")) };
    Ok(match (original, out) {
        (Value::Bool(_), Abi::Bool(v)) => Value::Bool(Some(v)),
        (Value::TinyInt(_), Abi::Int(v)) => Value::TinyInt(Some(int(v, i8::MIN.into(), i8::MAX.into())? as i8)),
        (Value::SmallInt(_), Abi::Int(v)) => Value::SmallInt(Some(int(v, i16::MIN.into(), i16::MAX.into())? as i16)),
        (Value::Int(_), Abi::Int(v)) => Value::Int(Some(int(v, i32::MIN.into(), i32::MAX.into())? as i32)),
        (Value::BigInt(_), Abi::Int(v)) => Value::BigInt(Some(v)),
        (Value::Float(_), Abi::Float(v)) => Value::Float(Some(v as f32)),
        (Value::Double(_), Abi::Float(v)) => Value::Double(Some(v)),
        (Value::String(_), Abi::Text(v)) => Value::String(Some(v)),
        (Value::Bytes(_), Abi::Bytes(v)) => Value::Bytes(Some(v)),
        (Value::Json(_), Abi::Json(v)) => Value::Json(Some(Box::new(serde_json::from_str(&v).map_err(|_| bad("invalid JSON"))?))),
        (Value::Uuid(_), Abi::Uuid(v)) => Value::Uuid(Some(uuid::Uuid::from_bytes(v))),
        (Value::BigDecimal(_), Abi::Decimal(v)) => Value::BigDecimal(Some(Box::new(v.parse().map_err(|_| bad("an invalid decimal"))?))),
        (Value::ChronoDate(_), Abi::Date(v)) => Value::ChronoDate(Some(chrono::NaiveDate::from_ymd_opt(1970, 1, 1).expect("epoch") + chrono::Duration::days(v.into()))),
        (Value::ChronoDateTimeUtc(_), Abi::DateTime(v)) => Value::ChronoDateTimeUtc(Some(DateTime::from_timestamp_micros(v).ok_or_else(|| bad("a time out of range"))?)),
        (Value::ChronoDateTimeWithTimeZone(_), Abi::DateTime(v)) => Value::ChronoDateTimeWithTimeZone(Some(DateTime::from_timestamp_micros(v).ok_or_else(|| bad("a time out of range"))?.fixed_offset())),
        (Value::ChronoDateTime(_), Abi::DateTime(v)) => Value::ChronoDateTime(Some(DateTime::from_timestamp_micros(v).ok_or_else(|| bad("a time out of range"))?.naive_utc())),
        _ => return Err(bad("a value of another type")),
    })
}

fn failure(model: &Model, position: Option<usize>, hook: &HookRef, message: String) -> Error {
    match (hook, position) {
        (HookRef::Static(_), _) => Error::query(message),
        (_, Some(p)) => Error::query(format!("{}.{}: {message}", model.ir.name, model.fields()[p].name)),
        (_, None) => Error::query(format!("{}: {message}", model.ir.name)),
    }
}

/// Runs the hooks of one field on `value`.
fn field(model: &Model, plan: &HookPlan, position: usize, value: &mut Value, context: &Context) -> Result<()> {
    let name = &model.fields()[position].name;
    let mut abi = to_abi(model, position, value)?;
    for (_, hook) in plan.transforms.iter().filter(|(p, _)| *p == position) {
        hook.rewrite(name, &mut abi, context).map_err(|m| failure(model, Some(position), hook, m))?;
    }
    for (_, hook) in plan.validators.iter().filter(|(p, _)| *p == position) {
        hook.check(name, &abi, context).map_err(|m| failure(model, Some(position), hook, m))?;
    }
    if !plan.transforms.iter().any(|(p, _)| *p == position) { return Ok(()); }
    *value = from_abi(model, position, value, abi)?;
    Ok(())
}

/// Runs the hooks on one supplied row. `skip` is the number of leading slots that no hook reads
/// (the key slot of `update_many`).
fn row<R: Slots + ?Sized>(model: &Model, plan: &HookPlan, map: &[Option<usize>], row: &mut R, skip: usize, context: &Context) -> Result<()> {
    let slot = |p: usize| map.get(p).copied().flatten().filter(|s| *s >= skip);
    for &p in plan.required.iter().filter(|p| plan.watches(**p)) {
        let Some(s) = slot(p) else { continue };
        match row.slot(s) {
            Some(value) => field(model, plan, p, value, context)?,
            None => return Err(Error::query(RESTRICTED)),
        }
    }
    for record in &plan.records {
        if !record.positions.iter().any(|p| slot(*p).is_some()) { continue; }
        let mut args = Vec::with_capacity(record.positions.len());
        for &p in &record.positions {
            let value = slot(p).and_then(|s| row.slot(s)).ok_or_else(|| Error::query(PARTIAL))?;
            args.push(Arg { name: model.fields()[p].name.clone(), value: to_abi(model, p, value)? });
        }
        record.hook.check_record(&args, context).map_err(|m| failure(model, None, &record.hook, m))?;
    }
    Ok(())
}

/// `SET` values in `set` order, with `None` for a database expression.
pub fn update_set(model: &Model, positions: &[usize], values: &mut [Option<Value>]) -> Result<()> {
    let Some(plan) = model.hooks.as_deref() else { return Ok(()) };
    let map = input_map(model.fields().len(), positions)?;
    row(model, plan, &map, values, 0, &Context::default())
}

/// The statement-level checks of an insert: the supplied fields, and the upsert.
fn insert_fields(model: &Model, plan: &HookPlan, map: &[Option<usize>], upsert: bool) -> Result<()> {
    if upsert && plan.no_upsert {
        return Err(Error::query("native validation does not support upsert conflict outcomes; use an explicit transaction or database constraint"));
    }
    for &p in &plan.required {
        if map.get(p).copied().flatten().is_some() { continue; }
        return Err(Error::query(if plan.watches(p) {
            format!("{}.{}: native validation requires an explicitly supplied insert value", model.ir.name, model.fields()[p].name)
        } else {
            "record validation requires all insert dependencies".to_owned()
        }));
    }
    Ok(())
}

/// Runs the hooks on the rows of an insert.
pub fn insert_rows(model: &Model, map: &[Option<usize>], rows: &mut [Vec<Option<Value>>], upsert: bool) -> Result<()> {
    let Some(plan) = model.hooks.as_deref() else { return Ok(()) };
    insert_fields(model, plan, map, upsert)?;
    let context = Context::default();
    rows.iter_mut().try_for_each(|r| row(model, plan, map, r.as_mut_slice(), 0, &context))
}

/// Runs the hooks on the rows of an `update_many`; slot 0 is the key.
pub fn update_rows(model: &Model, map: &[Option<usize>], rows: &mut [Vec<Value>]) -> Result<()> {
    let Some(plan) = model.hooks.as_deref() else { return Ok(()) };
    let context = Context::default();
    rows.iter_mut().try_for_each(|r| row(model, plan, map, r.as_mut_slice(), 1, &context))
}
