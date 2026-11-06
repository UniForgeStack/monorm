//! Prepared reads: the statement is planned once, and a call only converts and binds.
//!
//! The planner reads each parameter through [`Params`]. Planning runs once against a
//! [`Recorder`] that answers each read with a unique sentinel value. The sentinels that
//! come back in the built binds mark the slots: a [`Template`] keeps the SQL text and, for
//! each bind, a constant or a slot. A call converts the slots with the binding's own
//! [`Params`] and runs the held statement. Nothing plans again.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::Arc;

use monorm::abi::Context;
use sea_query::Value;

use crate::db::{self, Executor};
use crate::error::{Error, Result};
use crate::exec::{self, Outcome, PrefetchCall, Selected};
use crate::params::{array_type, null_of, Params};
use crate::plan::{self, json_list, Plan, PrefetchPlan, SelectPlan};
use monorm_core::dialect::Target;
use monorm_core::ir::{ColType, Operation, ValueType};
use monorm_core::schema::Schema;

/// How a slot turns a parameter into a bind.
#[derive(Clone, Debug, PartialEq)]
enum Conv {
    /// `Params::value`; `None` infers the type from the value.
    Value(Option<ValueType>),
    /// A list parameter as the JSON text of a database without arrays.
    JsonList(ValueType),
    Text,
    /// Text in lower case (a `LIKE` on a database without `ILIKE`).
    TextLower,
    Count,
}

#[derive(Clone, Debug)]
struct Read {
    param: usize,
    conv: Conv,
}

/// Where a bind of a held statement gets its value.
#[derive(Clone, Debug)]
pub(crate) enum Arg {
    Const(Value),
    Slot(usize),
    /// The parent keys of a prefetch as one array.
    Keys,
}

/// A slot: one parameter of the call, converted once and bound wherever the statement uses it.
#[derive(Clone, Debug)]
struct Slot {
    param: usize,
    conv: Conv,
    /// `scope.<name>`: a NULL value fails the call.
    scope: Option<String>,
}

impl Slot {
    fn convert(&self, params: &dyn Params) -> Result<Value> {
        if self.param >= params.len() {
            return Err(Error::query(format!("parameter {} out of range", self.param)));
        }
        let value = match &self.conv {
            Conv::Value(ty) => params.value(self.param, *ty)?,
            Conv::JsonList(ty) => json_list(params.value(self.param, Some(*ty))?)?,
            Conv::Text => Value::String(Some(params.text(self.param)?)),
            Conv::TextLower => Value::String(Some(params.text(self.param)?.to_lowercase())),
            Conv::Count => Value::BigUnsigned(Some(params.count(self.param)?)),
        };
        if let Some(name) = &self.scope {
            if !value.is_some() {
                return Err(Error::query(format!("scope.{name} is None/null: a NULL scope value would match no rows; use a filter on the column for NULL rows")));
            }
        }
        Ok(value)
    }
}

/// A statement with its SQL text and the source of each bind.
#[derive(Debug)]
pub(crate) struct Template {
    sql: String,
    args: Vec<Arg>,
}

impl Template {
    fn bind(&self, values: &[Value], keys: Option<Value>) -> (String, Vec<Value>) {
        let mut keys = keys;
        let args = self.args.iter().map(|a| match a {
            Arg::Const(v) => v.clone(),
            Arg::Slot(s) => values[*s].clone(),
            Arg::Keys => keys.take().expect("a keys bind has keys"),
        });
        (self.sql.clone(), args.collect())
    }
}

/// A prefetch held for calls.
pub struct PrefetchRun {
    pub(crate) plan: Arc<PrefetchPlan>,
    /// `key = ANY($n)` with all the parent keys: one statement.
    pub(crate) any: Option<Template>,
    /// For the chunked statement: each sentinel in its binds and the slot that replaces it.
    pub(crate) swaps: Vec<(Value, usize)>,
    pub(crate) children: Vec<PrefetchRun>,
}

impl PrefetchRun {
    /// The statement of all `keys` in one array.
    pub(crate) fn has_any(&self) -> bool {
        self.any.is_some()
    }

    pub(crate) fn any_statement(&self, values: &[Value], keys: Vec<Vec<Value>>) -> (String, Vec<Value>) {
        let ty = self.plan.key_type[0].ty;
        let list = Value::Array(array_type(ty), Some(Box::new(keys.into_iter().flatten().collect())));
        self.any.as_ref().expect("an ANY statement").bind(values, Some(list))
    }

    /// `args` of a chunked statement with the sentinels replaced by this call's values.
    pub(crate) fn swap(&self, values: &[Value], args: &mut [Value]) {
        if self.swaps.is_empty() {
            return;
        }
        for a in args.iter_mut() {
            if let Some((_, s)) = self.swaps.iter().find(|(sentinel, _)| sentinel == a) {
                *a = values[*s].clone();
            }
        }
    }
}

enum ReadKind {
    Select { plan: Arc<SelectPlan>, runs: Vec<PrefetchRun> },
    Count,
    Exists,
}

/// A read planned once: the held statements and the slot map.
pub struct PreparedRead {
    target: Target,
    kind: ReadKind,
    main: Template,
    slots: Vec<Slot>,
    scope: BTreeMap<String, usize>,
    inspection: String,
    /// A computed field reads the request context.
    context: bool,
}

/// What a call converted: the slot values, and the request context for hooks.
pub struct Call {
    values: Vec<Value>,
    context: Option<Context>,
}

// -- the recording parameters ------------------------------------------------------

const BASE: i64 = 0x5EED_0000_0000_0000;
/// The read index of the parent keys of a prefetch.
const KEYS: usize = 0x3FFF_FFFF;

fn scalar(ty: ColType, k: usize, flag: bool) -> Value {
    // The parent keys read index would overflow the 32 bit and the date sentinels.
    let k = if k == KEYS { 999_999 } else { k };
    let n = k as i64;
    match ty {
        ColType::BigInt => Value::BigInt(Some(BASE + n)),
        ColType::Int => Value::Int(Some(1_900_000_000 + n as i32)),
        ColType::Float => Value::Double(Some(123_456_789.062_5 + n as f64)),
        ColType::Bool => Value::Bool(Some(flag)),
        ColType::String | ColType::Text => Value::String(Some(format!("\u{1}S{k}\u{1}"))),
        ColType::DateTime => Value::ChronoDateTimeWithTimeZone(chrono::DateTime::from_timestamp(4_000_000_000 + n, 0).map(|d| d.fixed_offset())),
        ColType::Date => Value::ChronoDate(chrono::NaiveDate::from_num_days_from_ce_opt(2_000_000 + k as i32)),
        ColType::Uuid => Value::Uuid(Some(uuid::Uuid::from_u128((0x5EED_u128 << 96) | k as u128))),
        ColType::Json => Value::Json(Some(Box::new(serde_json::json!({ "$s": k })))),
        ColType::Decimal => Value::BigDecimal(Some(Box::new(format!("7777777.{k:06}").parse().expect("a decimal")))),
    }
}

fn sentinel(ty: Option<ValueType>, k: usize, flag: bool) -> Value {
    match ty {
        None => scalar(ColType::Text, k, false),
        Some(t) if t.array => Value::Array(array_type(t.ty), Some(Box::new(vec![scalar(t.ty, k, flag)]))),
        Some(t) => scalar(t.ty, k, flag),
    }
}

/// The parameters of a planning pass that records each read and answers it with a sentinel.
struct Recorder {
    len: usize,
    declared: Vec<Option<ValueType>>,
    reads: RefCell<Vec<Read>>,
    /// Reads that answer `true` in this pass (bool parameters have two values only).
    flags: Vec<usize>,
    /// Parameters a `CASE` of parameters needed a type for.
    untyped: RefCell<Vec<usize>>,
}

impl Recorder {
    fn read(&self, param: usize, conv: Conv) -> usize {
        let mut reads = self.reads.borrow_mut();
        match reads.iter().position(|r| r.param == param && r.conv == conv) {
            Some(k) => k,
            None => {
                reads.push(Read { param, conv });
                reads.len() - 1
            }
        }
    }
}

impl Params for Recorder {
    fn len(&self) -> usize {
        self.len
    }
    fn value(&self, i: usize, ty: Option<ValueType>) -> Result<Value> {
        let ty = ty.or(self.declared.get(i).copied().flatten());
        let k = self.read(i, Conv::Value(ty));
        Ok(sentinel(ty, k, self.flags.contains(&k)))
    }
    fn text(&self, i: usize) -> Result<String> {
        let k = self.read(i, Conv::Text);
        Ok(format!("\u{1}S{k}\u{1}"))
    }
    fn count(&self, i: usize) -> Result<u64> {
        Ok(BASE as u64 + self.read(i, Conv::Count) as u64)
    }
    fn probe(&self, i: usize) -> Option<Value> {
        match self.declared.get(i).copied().flatten() {
            Some(t) => Some(sentinel(Some(t), 0, false)),
            None => {
                self.untyped.borrow_mut().push(i);
                None
            }
        }
    }
}

// -- planning once -----------------------------------------------------------------

/// Every statement of a plan in a fixed order: the main one, then each prefetch, depth first.
/// A prefetch is the `ANY` statement when it has one, else the chunked one for a dummy key.
fn statements(plan: &Plan, target: Target) -> Vec<(String, Vec<Value>)> {
    let d = target.dialect;
    match plan {
        Plan::Select(p) => {
            let mut out = vec![db::build(d, &p.stmt)];
            fn walk(plans: &[Arc<PrefetchPlan>], out: &mut Vec<(String, Vec<Value>)>, d: monorm_core::dialect::Dialect) {
                for p in plans {
                    out.push(db::build(d, &prefetch_statement(p)));
                    walk(&p.children, out, d);
                }
            }
            walk(&p.prefetch, &mut out, d);
            out
        }
        Plan::Count(s) | Plan::Exists(s) => vec![db::build(d, s)],
        _ => vec![],
    }
}

fn prefetch_statement(p: &PrefetchPlan) -> sea_query::SelectStatement {
    let list = Value::Array(array_type(p.key_type[0].ty), Some(Box::new(vec![scalar(p.key_type[0].ty, KEYS, false)])));
    p.statement_any(list).unwrap_or_else(|| p.statement(vec![p.key_type.iter().map(|t| null_of(Some(*t))).collect()]))
}

struct Pass {
    plan: Plan,
    reads: Vec<Read>,
    untyped: Vec<usize>,
}

fn pass(schema: &Schema, target: Target, op: &Operation, len: usize, declared: &[Option<ValueType>], flags: Vec<usize>) -> Result<Pass> {
    let recorder = Recorder { len, declared: declared.to_vec(), reads: RefCell::new(vec![]), flags, untyped: RefCell::new(vec![]) };
    let plan = match plan::Planner::plan(schema, target, op, &recorder) {
        Ok(plan) => plan,
        Err(_) if !recorder.untyped.borrow().is_empty() => return Err(untyped_error(recorder.untyped.borrow()[0])),
        Err(e) => return Err(e),
    };
    Ok(Pass { plan, reads: recorder.reads.into_inner(), untyped: recorder.untyped.into_inner() })
}

fn untyped_error(i: usize) -> Error {
    Error::query(format!("parameter {i} is the only source of a CASE value type: declare its type with param(name, type=...)"))
}

fn is_bool(read: &Read) -> bool {
    matches!(read.conv, Conv::Value(Some(t)) if t.ty == ColType::Bool)
}

/// The values a read leaves in the binds, with how to convert them again.
fn candidates(read: &Read, k: usize, any_array: bool) -> Vec<(Value, Conv)> {
    match &read.conv {
        Conv::Value(ty) => {
            let mut out = vec![(sentinel(*ty, k, false), read.conv.clone())];
            if let Some(t) = ty.filter(|t| t.array && !any_array) {
                if let Ok(json) = json_list(sentinel(Some(t), k, false)) {
                    out.push((json, Conv::JsonList(t)));
                }
            }
            out
        }
        Conv::Text => {
            let s = format!("\u{1}S{k}\u{1}");
            vec![(Value::String(Some(s.clone())), Conv::Text), (Value::String(Some(s.to_lowercase())), Conv::TextLower)]
        }
        Conv::Count => vec![(Value::BigUnsigned(Some(BASE as u64 + k as u64)), Conv::Count)],
        _ => vec![],
    }
}

impl PreparedRead {
    /// Plans `op` (a select, count or exists) once. `len` is the number of parameters with
    /// the scope values after them; `declared` holds the declared type of each parameter.
    pub fn prepare(schema: &Schema, target: Target, op: &Operation, len: usize, declared: &[Option<ValueType>]) -> Result<Self> {
        if !matches!(op, Operation::Select(_) | Operation::Count(_) | Operation::Exists(_)) {
            return Err(Error::query("prepare() takes a read query"));
        }
        let base = pass(schema, target, op, len, declared, vec![])?;
        if let Some(i) = base.untyped.first() {
            return Err(untyped_error(*i));
        }
        let statements_of = |plan: &Plan| statements(plan, target);
        let base_statements = statements_of(&base.plan);
        let flat: Vec<&Value> = base_statements.iter().flat_map(|(_, b)| b).collect();
        // A bool parameter has two values only: the binds that change when it flips are its own.
        let mut owner: Vec<Option<usize>> = vec![None; flat.len()];
        for (k, read) in base.reads.iter().enumerate().filter(|(_, r)| is_bool(r)) {
            let flipped = pass(schema, target, op, len, declared, vec![k])?;
            let other = statements_of(&flipped.plan);
            if other.len() != base_statements.len() || other.iter().zip(&base_statements).any(|(a, b)| a.0 != b.0) {
                return Err(Error::query(format!("parameter {} changes the SQL text: a bool parameter must only be bound", read.param)));
            }
            for (p, v) in other.iter().flat_map(|(_, b)| b).enumerate() {
                if v != flat[p] {
                    owner[p] = Some(k);
                }
            }
        }
        // Match every bind with a read.
        let any_array = target.caps.any_array;
        let table: Vec<(usize, Value, Conv)> = base.reads.iter().enumerate().filter(|(_, r)| !is_bool(r))
            .flat_map(|(k, r)| candidates(r, k, any_array).into_iter().map(move |(v, c)| (k, v, c))).collect();
        let scope = op.scope().clone();
        let names: BTreeMap<usize, String> = scope.iter().map(|(n, i)| (*i, n.clone())).collect();
        let mut slots: Vec<Slot> = vec![];
        let mut slot_of: Vec<(usize, Conv, usize)> = vec![];
        let mut matched = vec![false; base.reads.len()];
        let mut slot_for = |k: usize, conv: Conv, slots: &mut Vec<Slot>, matched: &mut Vec<bool>| -> usize {
            matched[k] = true;
            if let Some((_, _, s)) = slot_of.iter().find(|(kk, c, _)| *kk == k && *c == conv) {
                return *s;
            }
            let param = base.reads[k].param;
            slots.push(Slot { param, conv: conv.clone(), scope: names.get(&param).cloned() });
            slot_of.push((k, conv, slots.len() - 1));
            slots.len() - 1
        };
        let mut position = 0;
        let mut args: Vec<Vec<Arg>> = vec![];
        let keys_array = base_statements.len() > 1;
        for (_, binds) in &base_statements {
            let mut row = Vec::with_capacity(binds.len());
            for v in binds {
                let arg = if let Some(k) = owner[position] {
                    Arg::Slot(slot_for(k, base.reads[k].conv.clone(), &mut slots, &mut matched))
                } else if let Some((k, _, conv)) = table.iter().find(|(_, s, _)| s == v) {
                    Arg::Slot(slot_for(*k, conv.clone(), &mut slots, &mut matched))
                } else if keys_array && matches!(v, Value::Array(_, Some(list)) if list.len() == 1 && list[0] == scalar_of(v, KEYS)) {
                    Arg::Keys
                } else {
                    Arg::Const(v.clone())
                };
                row.push(arg);
                position += 1;
            }
            args.push(row);
        }
        if let Some(k) = base.reads.iter().enumerate().position(|(k, r)| !matched[k] && !matches!(r.conv, Conv::Value(None))) {
            let r = &base.reads[k];
            return Err(Error::query(format!("parameter {} ({:?}) is not bound as a value in the statement: a prepared query cannot use it here", r.param, r.conv)));
        }
        // Build the held statements.
        let inspection = match &base.plan {
            Plan::Select(_) | Plan::Count(_) | Plan::Exists(_) => serde_json::to_string(&crate::inspect::Inspection::plans(std::slice::from_ref(&base.plan), false)).map_err(|e| Error::query(e.to_string()))?,
            _ => String::new(),
        };
        let mut texts = base_statements.into_iter().zip(args);
        let ((sql, _), main_args) = texts.next().expect("a main statement");
        let main = Template { sql, args: main_args };
        let mut context = false;
        let kind = match base.plan {
            Plan::Select(p) => {
                context = p.computed.iter().any(|c| c.reads_context());
                let runs = runs(&p.prefetch, &mut texts, &slots, &mut context)?;
                ReadKind::Select { plan: Arc::new(p), runs }
            }
            Plan::Count(_) => ReadKind::Count,
            _ => ReadKind::Exists,
        };
        Ok(Self { target, kind, main, slots, scope, inspection, context })
    }

    /// Converts the call's parameters: the slot values, and the context a hook reads.
    pub fn call(&self, params: &dyn Params) -> Result<Call> {
        let values = self.slots.iter().map(|s| s.convert(params)).collect::<Result<Vec<_>>>()?;
        let context = if self.context { Some(crate::hooks::context(&crate::params::Scoped { params, scope: &self.scope })?) } else { None };
        Ok(Call { values, context })
    }

    /// The statement of a call with its binds, as `Executor::query` takes it.
    pub fn statement(&self, call: &Call) -> (String, Vec<Value>) {
        self.main.bind(&call.values, None)
    }

    /// The SQL text with the placeholders, for `inspect` of the shape.
    pub fn sql_text(&self) -> &str {
        &self.main.sql
    }

    /// The statement of a call with its values inlined: what `sql()` shows.
    pub fn sql(&self, call: &Call) -> String {
        let (sql, args) = self.statement(call);
        inline(self.target, &sql, &args)
    }

    /// The structure counts of the statement (`inspect()`); they do not depend on the values.
    pub fn inspection(&self) -> &str {
        &self.inspection
    }

    pub async fn run(&self, conn: &dyn Executor, call: Call) -> Result<Outcome> {
        let (sql, args) = self.statement(&call);
        Ok(match &self.kind {
            ReadKind::Select { plan, runs } => {
                let rows = conn.query(sql, args).await?;
                let pc = PrefetchCall { runs, values: &call.values, context: call.context.as_ref() };
                let prefetched = exec::run_prefetch(conn, self.target, rows.as_ref(), plan.prefetch.clone(), Some(pc)).await?;
                let rows = crate::hooks::compute_with(rows, &plan.computed, call.context.as_ref())?;
                Outcome::Select(Box::new(Selected { rows, plan: plan.clone(), prefetched }))
            }
            ReadKind::Count => {
                let rows = conn.query(sql, args).await?;
                Outcome::Count(if rows.is_empty() { 0 } else { rows.get_i64(0, 0)? })
            }
            ReadKind::Exists => {
                let rows = conn.query(sql, args).await?;
                Outcome::Exists(!rows.is_empty() && rows.get_bool(0, 0)?)
            }
        })
    }
}

/// The element sentinel of the keys array `v`, for the comparison with the `KEYS` sentinel.
fn scalar_of(v: &Value, k: usize) -> Value {
    match v {
        Value::Array(ty, _) => match ty {
            sea_query::ArrayType::BigInt => scalar(ColType::BigInt, k, false),
            sea_query::ArrayType::Int => scalar(ColType::Int, k, false),
            sea_query::ArrayType::String => scalar(ColType::Text, k, false),
            sea_query::ArrayType::Uuid => scalar(ColType::Uuid, k, false),
            sea_query::ArrayType::ChronoDate => scalar(ColType::Date, k, false),
            sea_query::ArrayType::ChronoDateTimeWithTimeZone => scalar(ColType::DateTime, k, false),
            _ => Value::Bool(None),
        },
        _ => Value::Bool(None),
    }
}

fn runs(plans: &[Arc<PrefetchPlan>], texts: &mut impl Iterator<Item = ((String, Vec<Value>), Vec<Arg>)>, slots: &[Slot], context: &mut bool) -> Result<Vec<PrefetchRun>> {
    let mut out = Vec::with_capacity(plans.len());
    for p in plans {
        let ((sql, binds), args) = texts.next().expect("a statement for each prefetch");
        *context |= p.computed.iter().any(|c| c.reads_context());
        let any = p.statement_any(Value::Bool(None)).is_some();
        let (any, swaps) = if any {
            (Some(Template { sql, args }), vec![])
        } else {
            let mut swaps = vec![];
            for (v, a) in binds.iter().zip(&args) {
                match a {
                    Arg::Slot(s) if matches!(slots[*s].conv, Conv::Count) => return Err(Error::query("a prefetch with a parameter in its limit or offset cannot be prepared")),
                    Arg::Slot(s) if matches!(v, Value::Bool(_)) => {
                        let _ = s;
                        return Err(Error::query("a bool parameter in a prefetch of this shape cannot be prepared"));
                    }
                    Arg::Slot(s) => swaps.push((v.clone(), *s)),
                    _ => {}
                }
            }
            (None, swaps)
        };
        let children = runs(&p.children, texts, slots, context)?;
        out.push(PrefetchRun { plan: p.clone(), any, swaps, children });
    }
    Ok(out)
}

/// `sql` with each placeholder replaced by its value.
pub fn inline(target: Target, sql: &str, args: &[Value]) -> String {
    match target.dialect {
        #[cfg(feature = "postgres")]
        monorm_core::dialect::Dialect::Postgres => sea_query::inject_parameters(sql, args, &sea_query::PostgresQueryBuilder),
        #[cfg(feature = "sqlite")]
        monorm_core::dialect::Dialect::Sqlite => sea_query::inject_parameters(sql, args, &sea_query::SqliteQueryBuilder),
        #[allow(unreachable_patterns)]
        _ => sql.to_string(),
    }
}
