//! Change-set hooks at the write seams. Each seam reads `Model.hooks` once per statement and
//! runs the transforms, the validators and the record validators of `HookPlan` on the supplied values.
use crate::db::{Cell, DbError, DbResult, RowSet};
use crate::error::{Error, Result};
use crate::params::Params;
use orm_contracts::abi::{Arg, Context, Value as Abi};
use orm_core::{hooks::{HookPlan, HookRef}, ir::ValueType, schema::Model, writes::{AssignValue, WriteKind}};
use sea_query::Value;
use std::cell::OnceCell;

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

const RESTRICTED: &str = "hooked fields require supplied values; database defaults and expressions need a constraint or explicit transaction strategy";
const PARTIAL: &str = "record validation requires all non-null supplied dependencies; use an explicit transaction for partial records";

fn abi_of(value: &Value) -> Option<Abi> {
    use chrono::{DateTime, NaiveDate};
    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).expect("epoch");
    let micros = |t: DateTime<chrono::Utc>| Abi::DateTime(t.timestamp_micros());
    Some(match value {
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
        _ => return None,
    })
}

fn to_abi(model: &Model, position: usize, value: &Value) -> Result<Abi> {
    abi_of(value).ok_or_else(|| Error::query(format!("{}.{}: hooks do not support this field type", model.ir.name, model.fields()[position].name)))
}

/// The request context of one statement. A hook that reads it gets the scope values, built when the first such hook runs; the others get none.
pub struct Contexts<'a> { params: &'a dyn Params, full: OnceCell<Context>, empty: Context }

impl<'a> Contexts<'a> {
    pub fn new(params: &'a dyn Params) -> Self { Self { params, full: OnceCell::new(), empty: Context::default() } }

    pub fn of(&self, hook: &HookRef) -> Result<&Context> {
        if !hook.reads_context() { return Ok(&self.empty); }
        if let Some(full) = self.full.get() { return Ok(full); }
        let built = context(self.params)?;
        Ok(self.full.get_or_init(|| built))
    }
}

pub(crate) fn context(params: &dyn Params) -> Result<Context> {
    let mut values = Vec::new();
    for (name, i) in params.scope_values() {
        let value = params.value(i, None)?;
        let value = abi_of(&value).ok_or_else(|| Error::query(format!("scope.{name}: hooks do not support this value type")))?;
        values.push(Arg { name, value });
    }
    Ok(Context { values })
}

/// The database value for what a hook returned. The original value gives the type.
fn from_abi(model: &Model, position: usize, original: &Value, out: Abi) -> Result<Value> {
    let field = &model.fields()[position];
    convert(&format!("{}.{}", model.ir.name, field.name), field.nullable, original, out)
}

fn convert(label: &str, nullable: bool, original: &Value, out: Abi) -> Result<Value> {
    use chrono::DateTime;
    let bad = |what: &str| Error::query(format!("{label}: hook returned {what} for this field"));
    if matches!(out, Abi::Null) {
        if !nullable { return Err(bad("null")); }
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

fn failure(model: &Model, position: Option<usize>, message: String) -> Error {
    match position {
        Some(p) => Error::query(format!("{}.{}: {message}", model.ir.name, model.fields()[p].name)),
        None => Error::query(format!("{}: {message}", model.ir.name)),
    }
}

/// Runs the hooks of one field on `value`.
fn field(model: &Model, plan: &HookPlan, position: usize, value: &mut Value, contexts: &Contexts) -> Result<()> {
    let mut abi = to_abi(model, position, value)?;
    let mut rewritten = false;
    for (_, hook) in plan.transforms.iter().filter(|(p, _)| *p == position) {
        hook.rewrite(&mut abi, contexts.of(hook)?).map_err(|m| failure(model, Some(position), m))?;
        rewritten = true;
    }
    for (_, hook) in plan.validators.iter().filter(|(p, _)| *p == position) {
        hook.check(&abi, contexts.of(hook)?).map_err(|m| failure(model, Some(position), m))?;
    }
    if rewritten { *value = from_abi(model, position, value, abi)?; }
    Ok(())
}

/// Runs the hooks on one supplied row. `skip` is the number of leading slots that no hook reads
/// (the key slot of `update_many`).
fn row<R: Slots + ?Sized>(model: &Model, plan: &HookPlan, map: &[Option<usize>], row: &mut R, skip: usize, contexts: &Contexts) -> Result<()> {
    let slot = |p: usize| map.get(p).copied().flatten().filter(|s| *s >= skip);
    for &p in &plan.watched {
        let Some(s) = slot(p) else { continue };
        match row.slot(s) {
            Some(value) => field(model, plan, p, value, contexts)?,
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
        record.hook.check_record(&args, contexts.of(&record.hook)?).map_err(|m| failure(model, None, m))?;
    }
    Ok(())
}

/// Gives the value that the caller supplied for an assigned field to its point-4 hook, which keeps, replaces or rejects it.
/// A field that the caller leaves out gets its value from `assigned`.
fn assign_row<R: Slots + ?Sized>(model: &Model, plan: &HookPlan, map: &[Option<usize>], row: &mut R, skip: usize, contexts: &Contexts) -> Result<()> {
    for (p, hook) in &plan.assign {
        let Some(s) = map.get(*p).copied().flatten().filter(|s| *s >= skip) else { continue };
        let on_update = model.assignments.iter().any(|a| a.field == *p && matches!(a.value, AssignValue::Hook { .. }) && a.applies_to(WriteKind::Update));
        if !on_update { continue; }
        let Some(value) = row.slot(s) else {
            return Err(Error::query(format!("{}.{}: a hook assigns this field; supply a value, not a database expression", model.ir.name, model.fields()[*p].name)));
        };
        let mut abi = to_abi(model, *p, value)?;
        hook.rewrite(&mut abi, contexts.of(hook)?).map_err(|m| failure(model, Some(*p), m))?;
        *value = from_abi(model, *p, value, abi)?;
    }
    Ok(())
}

/// `SET` values in `set` order, with `None` for a database expression.
pub fn update_set(model: &Model, positions: &[usize], values: &mut [Option<Value>], params: &dyn Params) -> Result<()> {
    let Some(plan) = model.hooks.as_deref() else { return Ok(()) };
    let map = input_map(model.fields().len(), positions)?;
    let contexts = Contexts::new(params);
    row(model, plan, &map, values, 0, &contexts)?;
    assign_row(model, plan, &map, values, 0, &contexts)
}

/// The statement-level checks of an insert: the supplied fields, and the upsert.
fn insert_fields(model: &Model, plan: &HookPlan, map: &[Option<usize>], upsert: bool) -> Result<()> {
    if upsert && plan.no_upsert {
        return Err(Error::query("hooks do not support upsert conflict outcomes; use an explicit transaction or database constraint"));
    }
    for &p in &plan.required {
        if map.get(p).copied().flatten().is_some() { continue; }
        return Err(Error::query(if plan.watches(p) {
            format!("{}.{}: a hook requires an explicitly supplied insert value", model.ir.name, model.fields()[p].name)
        } else {
            "record validation requires all insert dependencies".to_owned()
        }));
    }
    Ok(())
}

/// Runs the hooks on the rows of an insert.
pub fn insert_rows(model: &Model, map: &[Option<usize>], rows: &mut [Vec<Option<Value>>], upsert: bool, params: &dyn Params) -> Result<()> {
    let Some(plan) = model.hooks.as_deref() else { return Ok(()) };
    insert_fields(model, plan, map, upsert)?;
    let contexts = Contexts::new(params);
    rows.iter_mut().try_for_each(|r| row(model, plan, map, r.as_mut_slice(), 0, &contexts))
}

/// Runs the hooks on the rows of an `update_many`; slot 0 is the key.
pub fn update_rows(model: &Model, map: &[Option<usize>], rows: &mut [Vec<Value>], params: &dyn Params) -> Result<()> {
    let Some(plan) = model.hooks.as_deref() else { return Ok(()) };
    let contexts = Contexts::new(params);
    rows.iter_mut().try_for_each(|r| {
        row(model, plan, map, r.as_mut_slice(), 1, &contexts)?;
        assign_row(model, plan, map, r.as_mut_slice(), 1, &contexts)
    })
}

/// The value that the point-4 hook of field `position` gives to a write that leaves the field out. `None` when the model has no such hook.
pub fn assigned(model: &Model, position: usize, contexts: &Contexts) -> Result<Option<Value>> {
    let Some(hook) = model.hooks.as_deref().and_then(|plan| plan.assigner(position)) else { return Ok(None) };
    let field = &model.fields()[position];
    let out = hook.produce(contexts.of(hook)?).map_err(|m| failure(model, Some(position), m))?;
    let seed = crate::params::null_of(Some(field.value_type()));
    from_abi(model, position, &seed, out).map(Some)
}

/// The expression that the hook of field `position` gives to one statement: point 7 for an insert default, point 6 for an update
/// assignment. The hook runs once for the statement. `None` when the model has no such hook.
pub fn expression(model: &Model, position: usize, insert: bool, contexts: &Contexts) -> Result<Option<orm_core::ir::Expr>> {
    let Some(plan) = model.hooks.as_deref() else { return Ok(None) };
    let Some(hook) = (if insert { plan.expr_defaulter(position) } else { plan.expr_assigner(position) }) else { return Ok(None) };
    let json = hook.expression(contexts.of(hook)?).map_err(|m| failure(model, Some(position), m))?;
    let label = format!("{}.{}", model.ir.name, model.fields()[position].name);
    orm_core::writes::parse_expr(&json, !insert).map(Some).map_err(|e| Error::query(format!("{label}: the expression hook returned an expression that a statement cannot use: {e}")))
}

/// A computed field (point 5) of a read: the hook runs on each returned row, on the dependency columns of the same statement.
pub struct Computed { column: usize, hook: HookRef, ty: ValueType, nullable: bool, label: String, args: Vec<(String, usize, ValueType)>, context: Context }

/// The computed fields among the result columns `positions` of `model`, which start at column `start`.
pub fn computations(model: &Model, start: usize, positions: &[usize], params: &dyn Params) -> Result<Vec<Computed>> {
    let Some(plan) = model.hooks.as_deref().filter(|p| !p.computed.is_empty()) else { return Ok(vec![]) };
    let mut out = vec![];
    for (slot, &position) in positions.iter().enumerate() {
        let Some((hook, dependencies)) = plan.computed_at(position) else { continue };
        let field = &model.fields()[position];
        let args = dependencies.iter().map(|d| {
            let at = positions.iter().position(|p| p == d).ok_or_else(|| Error::query(format!("{}.{}: the read does not select its dependency {}", model.ir.name, field.name, model.fields()[*d].name)))?;
            Ok((model.fields()[*d].name.clone(), start + at, model.fields()[*d].value_type()))
        }).collect::<Result<_>>()?;
        let context = if hook.reads_context() { context(params)? } else { Context::default() };
        out.push(Computed { column: start + slot, hook: hook.clone(), ty: field.value_type(), nullable: field.nullable, label: format!("{}.{}", model.ir.name, field.name), args, context });
    }
    Ok(out)
}

struct ComputedRows {
    inner: Box<dyn RowSet>,
    /// Indexed by result column; `Some` holds the computed values of that column.
    values: Vec<Option<Vec<Value>>>,
}

impl RowSet for ComputedRows {
    fn len(&self) -> usize { self.inner.len() }
    fn cell(&self, row: usize, col: usize, ty: ValueType) -> DbResult<Cell<'_>> {
        let Some(Some(values)) = self.values.get(col) else { return self.inner.cell(row, col, ty) };
        Ok(match &values[row] {
            Value::Bool(v) => v.map_or(Cell::Null, Cell::Bool),
            Value::Int(v) => v.map_or(Cell::Null, Cell::Int),
            Value::BigInt(v) => v.map_or(Cell::Null, Cell::BigInt),
            Value::Double(v) => v.map_or(Cell::Null, Cell::Float),
            Value::String(v) => v.as_deref().map_or(Cell::Null, Cell::Text),
            Value::Uuid(v) => v.map_or(Cell::Null, Cell::Uuid),
            Value::Json(v) => v.as_ref().map_or(Cell::Null, |j| Cell::Json((**j).clone())),
            Value::BigDecimal(v) => v.as_ref().map_or(Cell::Null, |d| Cell::Decimal(d.to_string())),
            Value::ChronoDate(v) => v.map_or(Cell::Null, Cell::Date),
            Value::ChronoDateTimeWithTimeZone(v) => v.map_or(Cell::Null, |d| Cell::DateTime(d.with_timezone(&chrono::Utc))),
            other => return Err(DbError::other(format!("a computed value cannot be {other:?}"))),
        })
    }
    fn value(&self, row: usize, col: usize, ty: ValueType) -> DbResult<Value> {
        match self.values.get(col) { Some(Some(values)) => Ok(values[row].clone()), _ => self.inner.value(row, col, ty) }
    }
    fn get_i64(&self, row: usize, col: usize) -> DbResult<i64> { self.inner.get_i64(row, col) }
    fn get_bool(&self, row: usize, col: usize) -> DbResult<bool> { self.inner.get_bool(row, col) }
}

/// `rows` with the computed columns filled: one hook call for each row.
pub fn compute(rows: Box<dyn RowSet>, computed: &[Computed]) -> DbResult<Box<dyn RowSet>> {
    if computed.is_empty() { return Ok(rows); }
    let mut values = vec![None; computed.iter().map(|c| c.column + 1).max().unwrap_or(0)];
    for c in computed {
        let original = crate::params::null_of(Some(c.ty));
        let mut column = Vec::with_capacity(rows.len());
        for row in 0..rows.len() {
            let mut args = Vec::with_capacity(c.args.len());
            for (name, col, ty) in &c.args {
                let value = rows.value(row, *col, *ty)?;
                let value = abi_of(&value).ok_or_else(|| DbError::other(format!("{}: hooks do not support the type of {name}", c.label)))?;
                args.push(Arg { name: name.clone(), value });
            }
            let out = c.hook.compute(&args, &c.context).map_err(|m| DbError::other(format!("{}: {m}", c.label)))?;
            column.push(convert(&c.label, c.nullable, &original, out).map_err(|e| DbError::other(e.to_string()))?);
        }
        values[c.column] = Some(column);
    }
    Ok(Box::new(ComputedRows { inner: rows, values }))
}
