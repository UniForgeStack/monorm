//! Prepared writes: a held `update` or `delete` ([`PreparedWrite`]) and the writes of rows
//! (`insert`, `insert_many`, upsert and `update_many`: [`PreparedRows`]).
//!
//! A single statement is planned once with recording parameters, like a read. A statement
//! of rows depends on the row count and on which cells are set, so it is planned once for
//! each such shape, with sentinel cells, and kept. Each call still runs the work that
//! depends on its values (client defaults, hooks, the clock, scope) before it binds.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use sea_query::Value;

use crate::db::{self, Executor};
use crate::error::{query_err, Error, Result};
use crate::exec::{ChainedRows, Conflict, EmptyRows, Outcome};
use crate::params::{Params, Scoped};
use crate::plan::{self, InsertInput, Plan, Returned};
use crate::prepared::{pass, resolve, statements, untyped_error, Arg, Call, Recorder, Resolved, Slot, Template};
use monorm_core::behavior::ResultShape;
use monorm_core::client_default::ClientDefault;
use monorm_core::dialect::{Dialect, Target};
use monorm_core::ir::{Bypass, ColType, Expr, Operation, ValueType};
use monorm_core::schema::{Model, Schema};
use monorm_core::writes::AssignValue;

/// An `insert_many` of at least this many rows on PostgreSQL binds one array for each column.
pub const UNNEST_ROWS: usize = 32;
/// The first sentinel of a cell of the rows: cell `i` is `CELL_BASE + i`.
const CELL_BASE: i64 = 0x4CE1_0000_0000_0000;
/// Shapes kept for one prepared write; the cache starts again when it is full.
const MAX_SHAPES: usize = 64;

/// Parameters of a statement that has none: what the shape of a write is planned with where
/// no value is read.
struct NoParams;

impl Params for NoParams {
    fn len(&self) -> usize {
        0
    }
    fn value(&self, i: usize, _: Option<ValueType>) -> Result<Value> {
        Err(Error::query(format!("parameter {i} out of range")))
    }
    fn text(&self, i: usize) -> Result<String> {
        Err(Error::query(format!("parameter {i} out of range")))
    }
    fn count(&self, i: usize) -> Result<u64> {
        Err(Error::query(format!("parameter {i} out of range")))
    }
}

/// A hook that computes the value of a field at plan time cannot run for a statement that is
/// planned once.
fn refuse_plan_time_hooks(m: &Model) -> Result<()> {
    let assigned = m.assignments.iter().any(|a| matches!(a.value, AssignValue::Hook { .. } | AssignValue::ExprHook { .. }));
    let defaulted = m.client_defaults.iter().any(|(_, d)| matches!(d, ClientDefault::Hook(_)));
    if assigned || defaulted {
        return Err(Error::query(format!("prepare(): {} has a field whose value a hook gives at plan time; run the write without prepare()", m.ir.name)));
    }
    Ok(())
}

fn computed_of(returned: &Option<Returned>) -> bool {
    returned.as_ref().is_some_and(|r| r.3.iter().any(|c| c.reads_context()))
}

fn outcome(returned: &Option<Returned>, rows: Box<dyn crate::db::RowSet>) -> Outcome {
    let (model, types, shape, _) = returned.as_ref().expect("a returning write");
    Outcome::Rows { model: *model, rows, types: types.clone(), shape: shape.clone() as Option<ResultShape> }
}

/// The statements of a shape with the recorded reads.
type Built = (Vec<(String, Vec<Value>)>, Recorder);

// -- update and delete ----------------------------------------------------------------

/// A held `update` or `delete`: one statement with its slots.
pub struct PreparedWrite {
    target: Target,
    main: Template,
    slots: Vec<Slot>,
    scope: BTreeMap<String, usize>,
    returned: Option<Returned>,
    inspection: String,
    context: bool,
}

impl PreparedWrite {
    /// Plans `op` (an update or a delete) once. `len` is the number of parameters with the
    /// scope values after them; `declared` holds the declared type of each parameter.
    pub fn prepare(schema: &Schema, target: Target, op: &Operation, len: usize, declared: &[Option<ValueType>]) -> Result<Self> {
        let (model, set, scope, filters) = match op {
            Operation::Update(u) => (&u.model, u.set.as_slice(), u.scope.clone(), format!("{:?}{:?}", u.filters, u.with)),
            Operation::Delete(d) => (&d.model, &[][..], d.scope.clone(), format!("{:?}{:?}", d.filters, d.with)),
            _ => return Err(Error::query("prepare() takes an update or a delete")),
        };
        let m = schema.model(schema.model_idx(model).map_err(query_err)?);
        refuse_plan_time_hooks(m)?;
        if matches!(op, Operation::Update(_)) && m.hooks.as_deref().is_some_and(monorm_core::hooks::HookPlan::changes_set) {
            return Err(Error::query(format!("prepare(): {} has change-set hooks; run the update without prepare()", m.ir.name)));
        }
        let base = pass(schema, target, op, len, declared, vec![])?;
        if let Some(i) = base.untyped.first() {
            return Err(untyped_error(*i));
        }
        let texts = statements(&base.plan, target);
        if texts.len() != 1 {
            return Err(Error::query("prepare(): this write runs several statements; run it without prepare()"));
        }
        let flip = |k: usize| -> Result<Vec<(String, Vec<Value>)>> { Ok(statements(&pass(schema, target, op, len, declared, vec![k])?.plan, target)) };
        let Resolved { mut slots, mut args } = resolve(&base.reads, &texts, &flip, target, &scope, false, &|_| None)?;
        // A set value that is a parameter on its own may be NULL; a parameter used in a filter may not.
        let direct: Vec<usize> = set.iter().filter_map(|a| if let Expr::Param { i } = &a.value { Some(*i) } else { None }).collect();
        for s in &mut slots {
            s.nullable = direct.contains(&s.param) && !filters.contains(&format!("Param {{ i: {} }}", s.param));
        }
        let inspection = serde_json::to_string(&crate::inspect::Inspection::plans(std::slice::from_ref(&base.plan), false)).map_err(|e| Error::query(e.to_string()))?;
        let returned = match base.plan {
            Plan::Update(_, r) | Plan::Delete(_, r) => r,
            Plan::RowUpdate(_, r) => r,
            _ => None,
        };
        let (sql, _) = texts.into_iter().next().expect("a statement");
        let main = Template { sql, args: args.remove(0) };
        let context = computed_of(&returned);
        Ok(Self { target, main, slots, scope, returned, inspection, context })
    }

    /// Converts the call's parameters: the slot values, and the context a hook reads.
    pub fn call(&self, params: &dyn Params) -> Result<Call> {
        let values = self.slots.iter().map(|s| s.convert(params)).collect::<Result<Vec<_>>>()?;
        let context = if self.context { Some(crate::hooks::context(&Scoped { params, scope: &self.scope })?) } else { None };
        Ok(Call { values, context })
    }

    pub fn statement(&self, call: &Call) -> (String, Vec<Value>) {
        self.main.bind(&call.values, None)
    }

    pub fn sql_text(&self) -> &str {
        &self.main.sql
    }

    pub fn sql(&self, call: &Call) -> String {
        let (sql, args) = self.statement(call);
        crate::prepared::inline(self.target, &sql, &args)
    }

    pub fn inspection(&self) -> &str {
        &self.inspection
    }

    pub async fn run(&self, conn: &dyn Executor, call: Call) -> Result<Outcome> {
        let (sql, args) = self.statement(&call);
        Ok(match &self.returned {
            None => Outcome::Affected(conn.execute(sql, args).await?),
            Some((_, _, _, computed)) => {
                let rows = crate::hooks::compute_with(conn.query(sql, args).await?, computed, call.context.as_ref())?;
                outcome(&self.returned, rows)
            }
        })
    }
}

// -- rows -----------------------------------------------------------------------------

pub enum RowsKind {
    /// `insert` and `insert_many`, with an upsert's conflict clause.
    Insert(Option<Conflict>),
    /// `update_many`: the filters that restrict the rows.
    UpdateMany(Vec<Expr>),
}

pub struct RowsSpec {
    pub kind: RowsKind,
    pub model: String,
    pub fields: Vec<String>,
    pub returning: bool,
    pub model_fields: Option<Vec<String>>,
    pub without_defaults: Bypass,
    pub scope: BTreeMap<String, usize>,
    pub batch_size: Option<usize>,
    /// The number of parameters with the scope values after them.
    pub len: usize,
    pub declared: Vec<Option<ValueType>>,
}

impl RowsSpec {
    /// The spec a binding passes as JSON. `options` is the insert options (`kind` "insert"), or
    /// `{"filters": ..., "model_fields": ..., "without_defaults": ...}` (`kind` "update_many").
    #[allow(clippy::too_many_arguments)]
    pub fn from_options(kind: &str, model: &str, fields: Vec<String>, options: &str, returning: bool, batch_size: Option<usize>,
        len: usize, declared: Vec<Option<ValueType>>) -> Result<Self> {
        let parse = |e: serde_json::Error| query_err(e.to_string());
        let (kind, scope, model_fields, without_defaults) = match kind {
            "insert" => {
                let opts: crate::inspect::InsertOptions = serde_json::from_str(options).map_err(parse)?;
                let index = crate::exec::index_target(opts.indexes);
                let conflict = opts.conflict.map(|target| match opts.update {
                    Some(update) => Conflict::Update { target, index, update, set: opts.set },
                    None => Conflict::Nothing { target, index },
                });
                (RowsKind::Insert(conflict), opts.scope, opts.model_fields, opts.without_defaults)
            }
            "update_many" => {
                let opts: serde_json::Value = serde_json::from_str(options).map_err(parse)?;
                let filters = opts.get("filters").ok_or_else(|| query_err("update_many options need filters".into()))?;
                let (filters, scope) = crate::params::scoped_filters(&filters.to_string())?;
                let field = |name: &str| opts.get(name).filter(|v| !v.is_null()).cloned();
                let model_fields = field("model_fields").map(serde_json::from_value).transpose().map_err(parse)?;
                let without_defaults = field("without_defaults").map(serde_json::from_value).transpose().map_err(parse)?.unwrap_or_default();
                (RowsKind::UpdateMany(filters), scope, model_fields, without_defaults)
            }
            other => return Err(query_err(format!("unknown rows kind {other:?}"))),
        };
        Ok(RowsSpec { kind, model: model.to_string(), fields, returning, model_fields, without_defaults, scope, batch_size, len, declared })
    }
}

/// A statement for a row count and a pattern of set cells, with the slots of its parameters.
struct Shape {
    template: Template,
    slots: Vec<Slot>,
}

/// The `UNNEST` statement of a batch of rows, and the type of each column's array.
struct Unnest {
    sql: Arc<str>,
    types: Vec<ColType>,
}

impl Unnest {
    /// The statement with an array of the values of each column.
    fn bind(&self, rows: &[Vec<Option<Value>>]) -> (String, Vec<Value>) {
        let columns = self.types.iter().enumerate().map(|(c, ty)| {
            let items = rows.iter().map(|r| r[c].clone().expect("a cell with a value")).collect();
            Value::Array(crate::params::array_type(*ty), Some(Box::new(items)))
        });
        (self.sql.to_string(), columns.collect())
    }
}

/// The SQL type an array of a column is cast to; text for a string, as its column checks the length.
fn unnest_type(ty: ColType) -> &'static str {
    match ty {
        ColType::BigInt => "bigint",
        ColType::Int => "integer",
        ColType::Float => "double precision",
        ColType::Bool => "boolean",
        ColType::String | ColType::Text => "text",
        ColType::DateTime => "timestamp with time zone",
        ColType::Date => "date",
        ColType::Uuid => "uuid",
        ColType::Json => "jsonb",
        ColType::Decimal => "numeric",
    }
}

/// The arrays of the columns, as `$1::bigint[], $2::text[]`.
fn unnest_list(types: &[ColType]) -> String {
    types.iter().enumerate().map(|(i, ty)| format!("${}::{}[]", i + 1, unnest_type(*ty))).collect::<Vec<_>>().join(", ")
}

/// A held write of rows.
pub struct PreparedRows {
    schema: Arc<Schema>,
    target: Target,
    spec: RowsSpec,
    multi_table: bool,
    returned: Option<Returned>,
    context: bool,
    inspection: String,
    types: Vec<ValueType>,
    shapes: Mutex<HashMap<Vec<u8>, Arc<Shape>>>,
    unnests: Mutex<HashMap<Vec<u8>, Option<Arc<str>>>>,
}

type Rows = Vec<Vec<Option<Value>>>;

/// What one call of a [`PreparedRows`] runs.
enum Work {
    Empty,
    Multi(Vec<Plan>),
    Statements { statements: Vec<(String, Vec<Value>)>, context: Option<monorm::abi::Context> },
}

/// One call of a [`PreparedRows`], converted: it holds no reference to the parameters.
pub struct RowsCall {
    work: Work,
}

impl RowsCall {
    /// The text of each statement of the call, as sent.
    pub fn statements(&self) -> Vec<&str> {
        match &self.work {
            Work::Statements { statements, .. } => statements.iter().map(|(sql, _)| sql.as_str()).collect(),
            _ => vec![],
        }
    }
}

impl PreparedRows {
    /// Plans the statement of one row, to refuse what cannot be held and to fix the
    /// returned columns.
    pub fn prepare(schema: Arc<Schema>, target: Target, spec: RowsSpec) -> Result<Self> {
        let idx = schema.model_idx(&spec.model).map_err(query_err)?;
        let m = schema.model(idx);
        let types = crate::exec::field_types(&schema, &spec.model, &spec.fields)?;
        let multi_table = crate::multi_table::is_multi_table(&schema, &spec.model)?;
        let mut this = Self { schema: schema.clone(), target, spec, multi_table, returned: None, context: false, inspection: String::new(), types, shapes: Mutex::new(HashMap::new()), unnests: Mutex::new(HashMap::new()) };
        if multi_table {
            return Ok(this);
        }
        refuse_plan_time_hooks(m)?;
        db::require_dialect(target.dialect).map_err(Error::from)?;
        match &this.spec.kind {
            RowsKind::Insert(_) => {
                let pattern = vec![0u8; this.spec.fields.len()];
                let (stmt, columns) = this.insert_statement(&this.spec.fields, 1, &pattern, vec![])?.0;
                let plan = Plan::Insert(stmt, None);
                this.inspection = serde_json::to_string(&crate::inspect::Inspection::plans(std::slice::from_ref(&plan), false)).map_err(|e| Error::query(e.to_string()))?;
                this.returned = this.returned_of(idx, columns)?;
            }
            RowsKind::UpdateMany(_) => {
                let positions = plan::field_positions(m, &this.spec.fields)?;
                if positions.iter().skip(1).any(|p| m.computed(*p)) {
                    return Err(Error::query("computed fields are read-only"));
                }
                let columns = this.update_many_statement(1, vec![])?.1;
                this.returned = this.returned_of(idx, columns)?;
            }
        }
        this.context = computed_of(&this.returned);
        Ok(this)
    }

    fn returned_of(&self, idx: usize, columns: Option<plan::ReturnColumns>) -> Result<Option<Returned>> {
        let Some((types, shape)) = columns else { return Ok(None) };
        let m = self.schema.model(idx);
        let positions = plan::shape_positions(m, shape.as_ref()).to_vec();
        Ok(Some((idx, types, shape, crate::hooks::computations(m, 0, &positions, &NoParams)?)))
    }

    /// The value types of the row fields: what a binding converts the cells by.
    pub fn field_types(&self) -> &[ValueType] {
        &self.types
    }

    pub fn inspection(&self) -> &str {
        &self.inspection
    }

    fn model(&self) -> &Model {
        self.schema.model(self.schema.model_idx(&self.spec.model).expect("a prepared model"))
    }

    fn recorder(&self, flags: Vec<usize>) -> Recorder {
        Recorder { len: self.spec.len, declared: self.spec.declared.clone(), reads: RefCell::new(vec![]), flags, untyped: RefCell::new(vec![]) }
    }

    fn on_conflict(&self) -> Option<plan::OnConflict> {
        match &self.spec.kind {
            RowsKind::Insert(Some(Conflict::Nothing { target, index })) => Some(plan::OnConflict::Nothing(target.clone(), index.clone())),
            RowsKind::Insert(Some(Conflict::Update { target, index, update, set })) => Some(plan::OnConflict::Update(target.clone(), index.clone(), update.clone(), set.clone())),
            _ => None,
        }
    }

    /// The insert statement of `n` rows of sentinel cells; `pattern` has a code for each cell
    /// (0 set, 1 omitted, 2 a NULL of a nested field). The reads are recorded in `reads`.
    fn insert_statement(&self, fields: &[String], n: usize, pattern: &[u8], flags: Vec<usize>) -> Result<((sea_query::InsertStatement, Option<plan::ReturnColumns>), Recorder)> {
        let recorder = self.recorder(flags);
        let ncols = fields.len();
        let cell = |r: usize, c: usize| match pattern[(r * ncols + c).min(pattern.len() - 1)] {
            0 => Some(Value::BigInt(Some(CELL_BASE + (r * ncols + c) as i64))),
            _ => None,
        };
        let rows: Rows = (0..n).map(|r| (0..ncols).map(|c| cell(r, c)).collect()).collect();
        let forced = (0..n).map(|r| (0..ncols).map(|c| pattern[(r * ncols + c).min(pattern.len() - 1)] == 2).collect()).collect();
        let scoped = Scoped { params: &recorder, scope: &self.spec.scope };
        let input = InsertInput { fields: fields.to_vec(), rows, forced };
        let built = plan::insert_statement(&self.schema, self.target, &self.spec.model, input, self.on_conflict(), &scoped, self.spec.returning, self.spec.model_fields.as_deref(), &self.spec.without_defaults)?;
        Ok((built, recorder))
    }

    /// The `update_many` statement of `n` rows of sentinel cells.
    fn update_many_statement(&self, n: usize, flags: Vec<usize>) -> Result<(sea_query::UpdateStatement, Option<plan::ReturnColumns>, Recorder)> {
        let RowsKind::UpdateMany(filters) = &self.spec.kind else { unreachable!("an update_many") };
        let recorder = self.recorder(flags);
        let ncols = self.spec.fields.len();
        let rows: Vec<Vec<Value>> = (0..n).map(|r| (0..ncols).map(|c| Value::BigInt(Some(CELL_BASE + (r * ncols + c) as i64))).collect()).collect();
        let scoped = Scoped { params: &recorder, scope: &self.spec.scope };
        let (mut stmts, columns) = plan::plan_update_many(&self.schema, self.target, &self.spec.model, &self.spec.fields, &rows, n.max(1), filters, &scoped, self.spec.returning, self.spec.model_fields.as_deref(), &self.spec.without_defaults)?;
        if stmts.len() != 1 {
            return Err(Error::query("update_many: the rows of a batch must make one statement"));
        }
        Ok((stmts.remove(0), columns, recorder))
    }

    /// The held statement of a shape, planned on the first call of it.
    fn shape(&self, key: Vec<u8>, build: &dyn Fn(Vec<usize>) -> Result<Built>, cells: usize) -> Result<Arc<Shape>> {
        if let Some(shape) = self.shapes.lock().expect("shape cache").get(&key) {
            return Ok(shape.clone());
        }
        let (base, recorder) = build(vec![])?;
        let reads = recorder.reads.into_inner();
        if let Some(i) = recorder.untyped.into_inner().first() {
            return Err(untyped_error(*i));
        }
        let flip = |k: usize| -> Result<Vec<(String, Vec<Value>)>> { Ok(build(vec![k])?.0) };
        let cell = |v: &Value| match v {
            Value::BigInt(Some(x)) if (CELL_BASE..CELL_BASE + cells as i64).contains(x) => Some(Arg::Cell((x - CELL_BASE) as usize)),
            _ => None,
        };
        let Resolved { slots, mut args } = resolve(&reads, &base, &flip, self.target, &self.spec.scope, false, &cell)?;
        let (sql, _) = base.into_iter().next().expect("a statement");
        let shape = Arc::new(Shape { template: Template { sql, args: args.remove(0) }, slots });
        let mut shapes = self.shapes.lock().expect("shape cache");
        if shapes.len() >= MAX_SHAPES {
            shapes.clear();
        }
        shapes.insert(key, shape.clone());
        Ok(shape)
    }

    /// The statement of a shape with the cells and slot values of one call.
    fn bind(shape: &Shape, values: &[Value], ncols: usize, rows: &[Vec<Option<Value>>]) -> (String, Vec<Value>) {
        let args = shape.template.args.iter().map(|a| match a {
            Arg::Const(v) => v.clone(),
            Arg::Slot(s) => values[*s].clone(),
            Arg::Cell(i) => rows[i / ncols][i % ncols].clone().expect("a cell with a value"),
            Arg::Keys => unreachable!("a row statement has no arrays"),
        });
        (shape.template.sql.clone(), args.collect())
    }

    /// Runs the write of `rows` (a value for each of the fields; `None` is the default of the
    /// column). `own_tx` is false when `conn` is already a transaction.
    pub async fn run(&self, conn: &dyn Executor, params: &dyn Params, rows: Rows, own_tx: bool) -> Result<Outcome> {
        let call = self.call(params, rows)?;
        self.execute(conn, call, own_tx).await
    }

    /// Does the work of one call that reads the values: client defaults, hooks, the clock and
    /// the binds of each statement. The result holds no reference to `params`.
    pub fn call(&self, params: &dyn Params, rows: Rows) -> Result<RowsCall> {
        let scoped = Scoped { params, scope: &self.spec.scope };
        let work = match &self.spec.kind {
            RowsKind::Insert(conflict) => self.insert_work(&scoped, conflict.clone(), rows)?,
            RowsKind::UpdateMany(_) => self.update_many_work(&scoped, rows)?,
        };
        Ok(RowsCall { work })
    }

    /// Runs the statements of a call, in one transaction when there are several and `own_tx`.
    pub async fn execute(&self, conn: &dyn Executor, call: RowsCall, own_tx: bool) -> Result<Outcome> {
        match call.work {
            Work::Empty => Ok(self.empty()),
            Work::Multi(plans) => crate::exec::run_inserts(conn, self.target, plans, own_tx, self.spec.returning).await,
            Work::Statements { statements, context } => self.run_statements(conn, statements, context, own_tx).await,
        }
    }

    fn empty(&self) -> Outcome {
        if self.returned.is_some() {
            outcome(&self.returned, Box::new(EmptyRows))
        } else {
            Outcome::Affected(0)
        }
    }

    fn insert_work(&self, params: &dyn Params, conflict: Option<Conflict>, rows: Rows) -> Result<Work> {
        let spec = &self.spec;
        if spec.batch_size == Some(0) {
            return Err(Error::query("batch_size must be at least 1"));
        }
        if self.multi_table {
            let plans = crate::exec::plan_inserts(&self.schema, self.target, &spec.model, &spec.fields, rows, conflict, params, spec.batch_size, spec.returning, spec.model_fields.as_deref(), &spec.without_defaults)?;
            return Ok(Work::Multi(plans));
        }
        if rows.is_empty() {
            return Ok(Work::Empty);
        }
        let upsert = matches!(conflict, Some(Conflict::Update { .. }));
        let InsertInput { fields, rows, forced } = plan::insert_input(&self.schema, &spec.model, &spec.fields, rows, upsert, params)?;
        let m = self.model();
        // A `write_sql` template binds its value once for each `{}`.
        let width = fields.iter().map(|f| Ok(plan::param_template(m.field(f).map_err(query_err)?).map_or(1, |t| t.matches("{}").count().max(1)))).sum::<Result<usize>>()?;
        let total = rows.len();
        let unnest = if total >= UNNEST_ROWS { self.unnest(&fields, &rows)? } else { None };
        // One array a column binds the same number of parameters for any row count.
        let mut chunk = if unnest.is_some() { total } else { self.target.caps.max_params.saturating_sub(params.len()) / width.max(1) };
        if fields.is_empty() {
            chunk = 1;
        }
        if let Some(n) = spec.batch_size {
            chunk = chunk.min(n);
        }
        let chunk = chunk.max(1);
        let context = if self.context { Some(crate::hooks::context(params)?) } else { None };
        let mut statements = vec![];
        let mut start = 0;
        while start < total {
            let end = (start + chunk).min(total);
            statements.push(self.insert_chunk(&fields, &rows[start..end], &forced[start..end], unnest.as_ref(), params)?);
            start = end;
        }
        Ok(Work::Statements { statements, context })
    }

    /// Runs the chunk statements in order; a returning write chains the rows.
    async fn run_statements(&self, conn: &dyn Executor, statements: Vec<(String, Vec<Value>)>, context: Option<monorm::abi::Context>, own_tx: bool) -> Result<Outcome> {
        let tx = if own_tx && statements.len() > 1 { Some(conn.begin().await?) } else { None };
        let exec: &dyn Executor = match &tx {
            Some(t) => t.as_ref(),
            None => conn,
        };
        let done = async {
            let mut parts = vec![];
            let mut affected = 0u64;
            for (sql, args) in statements {
                match &self.returned {
                    Some((_, _, _, computed)) => parts.push(crate::hooks::compute_with(exec.query(sql, args).await?, computed, context.as_ref())?),
                    None => affected += exec.execute(sql, args).await?,
                }
            }
            Ok::<_, Error>((parts, affected))
        }
        .await;
        if let Some(t) = tx {
            match &done {
                Ok(_) => t.commit().await?,
                Err(_) => {
                    let _ = t.rollback().await;
                }
            }
        }
        let (mut parts, affected) = done?;
        Ok(if self.returned.is_some() {
            let rows: Box<dyn crate::db::RowSet> = if parts.len() == 1 { parts.remove(0) } else { Box::new(ChainedRows::new(parts)) };
            outcome(&self.returned, rows)
        } else {
            Outcome::Affected(affected)
        })
    }

    /// The held statement of `n` rows with a pattern of set cells.
    fn row_shape(&self, fields: &[String], n: usize, mut pattern: Vec<u8>) -> Result<Arc<Shape>> {
        let ncols = fields.len();
        let mut key = fields.join("\0").into_bytes();
        key.push(1);
        key.extend((n as u64).to_le_bytes());
        if pattern.iter().all(|c| *c == 0) {
            key.push(0);
            pattern.truncate(1);
        } else {
            key.push(1);
            key.extend(&pattern);
        }
        let build = |flags: Vec<usize>| -> Result<Built> {
            let ((stmt, _), recorder) = self.insert_statement(fields, n, &pattern, flags)?;
            Ok((vec![db::build(self.target.dialect, &stmt)], recorder))
        };
        self.shape(key, &build, n * ncols)
    }

    /// What binds an array for each column, when the rows allow it: PostgreSQL, every cell set,
    /// plain columns of a scalar type, and a statement with no parameter but the cells.
    fn unnest(&self, fields: &[String], rows: &[Vec<Option<Value>>]) -> Result<Option<Unnest>> {
        if self.target.dialect != Dialect::Postgres || fields.is_empty() {
            return Ok(None);
        }
        let m = self.model();
        let mut types = vec![];
        for name in fields {
            let f = m.field(name).map_err(query_err)?;
            if f.array || f.db_type.is_some() || f.write_sql.is_some() || f.enum_name.is_some() {
                return Ok(None);
            }
            types.push(f.ty);
        }
        let fits = |ty: ColType, v: &Value| match ty {
            ColType::BigInt => matches!(v, Value::BigInt(_)),
            ColType::Int => matches!(v, Value::Int(_)),
            ColType::Float => matches!(v, Value::Double(_)),
            ColType::Bool => matches!(v, Value::Bool(_)),
            ColType::String | ColType::Text => matches!(v, Value::String(_)),
            ColType::DateTime => matches!(v, Value::ChronoDateTimeWithTimeZone(_)),
            ColType::Date => matches!(v, Value::ChronoDate(_)),
            ColType::Uuid => matches!(v, Value::Uuid(_)),
            ColType::Json => matches!(v, Value::Json(_)),
            ColType::Decimal => matches!(v, Value::BigDecimal(_)),
        };
        let all = rows.iter().all(|row| row.iter().zip(&types).all(|(cell, ty)| cell.as_ref().is_some_and(|v| fits(*ty, v))));
        if !all {
            return Ok(None);
        }
        let mut key = fields.join("\0").into_bytes();
        key.push(if matches!(self.spec.kind, RowsKind::Insert(_)) { 3 } else { 4 });
        let cached = self.unnests.lock().expect("unnest cache").get(&key).cloned();
        let sql = match cached {
            Some(sql) => sql,
            None => {
                let sql = if matches!(self.spec.kind, RowsKind::Insert(_)) { self.unnest_insert_sql(fields, &types)? } else { self.unnest_update_sql(&types)? };
                self.unnests.lock().expect("unnest cache").insert(key, sql.clone());
                sql
            }
        };
        Ok(sql.map(|sql| Unnest { sql, types }))
    }

    /// The statement of one row, with its `VALUES` list replaced by `UNNEST` of an array for each column.
    fn unnest_insert_sql(&self, fields: &[String], types: &[ColType]) -> Result<Option<Arc<str>>> {
        let ncols = fields.len();
        let shape = self.row_shape(fields, 1, vec![0; ncols])?;
        let only_cells = shape.slots.is_empty() && shape.template.args.len() == ncols && shape.template.args.iter().enumerate().all(|(i, a)| matches!(a, Arg::Cell(c) if *c == i));
        if !only_cells {
            return Ok(None);
        }
        let group = format!("({})", (1..=ncols).map(|i| format!("${i}")).collect::<Vec<_>>().join(", "));
        let marker = format!(" VALUES {group}");
        let sql = &shape.template.sql;
        let Some(at) = sql.find(&marker) else { return Ok(None) };
        let (head, tail) = (&sql[..at], &sql[at + marker.len()..]);
        if tail.contains('$') {
            return Ok(None);
        }
        // An explicit cast to `varchar(n)` cuts a long text; the column's assignment cast refuses it.
        let list = unnest_list(types);
        // Without it, `ON` after the `FROM` item could read as a join condition.
        let guard = if tail.starts_with(" ON CONFLICT") { " WHERE true" } else { "" };
        Ok(Some(format!("{head} SELECT * FROM UNNEST({list}){guard}{tail}").into()))
    }

    /// The held `update_many` statement of `n` rows.
    fn update_shape(&self, n: usize) -> Result<Arc<Shape>> {
        let build = |flags: Vec<usize>| -> Result<Built> {
            let (stmt, _, recorder) = self.update_many_statement(n, flags)?;
            Ok((vec![db::build(self.target.dialect, &stmt)], recorder))
        };
        let mut key = (n as u64).to_le_bytes().to_vec();
        key.push(2);
        self.shape(key, &build, n * self.spec.fields.len())
    }

    /// The statement of one row with its `VALUES` list replaced by `UNNEST` of an array for each column.
    fn unnest_update_sql(&self, types: &[ColType]) -> Result<Option<Arc<str>>> {
        let ncols = types.len();
        let shape = self.update_shape(1)?;
        let only_cells = shape.slots.is_empty() && shape.template.args.len() == ncols && shape.template.args.iter().enumerate().all(|(i, a)| matches!(a, Arg::Cell(c) if *c == i));
        if !only_cells {
            return Ok(None);
        }
        let group = format!("(VALUES ({})) AS \"v\"", (1..=ncols).map(|i| format!("${i}")).collect::<Vec<_>>().join(", "));
        let sql = &shape.template.sql;
        let Some(at) = sql.find(&group) else { return Ok(None) };
        let (head, tail) = (&sql[..at], &sql[at + group.len()..]);
        if tail.contains('$') {
            return Ok(None);
        }
        let names = (1..=ncols).map(|i| format!("\"column{i}\"")).collect::<Vec<_>>().join(", ");
        Ok(Some(format!("{head}UNNEST({}) AS \"v\"({names}){tail}", unnest_list(types)).into()))
    }

    /// The statement and binds of one batch of rows.
    fn insert_chunk(&self, fields: &[String], rows: &[Vec<Option<Value>>], forced: &[Vec<bool>], unnest: Option<&Unnest>, params: &dyn Params) -> Result<(String, Vec<Value>)> {
        let ncols = fields.len();
        let n = rows.len();
        if let (Some(u), true) = (unnest, n >= UNNEST_ROWS) {
            return Ok(u.bind(rows));
        }
        let mut pattern = Vec::with_capacity(n * ncols);
        for (row, mask) in rows.iter().zip(forced) {
            for (cell, f) in row.iter().zip(mask) {
                pattern.push(if cell.is_some() { 0 } else if *f { 2 } else { 1 });
            }
        }
        let shape = self.row_shape(fields, n, pattern)?;
        let values = shape.slots.iter().map(|s| s.convert(params)).collect::<Result<Vec<_>>>()?;
        Ok(Self::bind(&shape, &values, ncols.max(1), rows))
    }

    fn update_many_work(&self, params: &dyn Params, rows: Rows) -> Result<Work> {
        let spec = &self.spec;
        let ncols = spec.fields.len();
        let mut rows: Vec<Vec<Value>> = rows.into_iter().map(|r| r.into_iter().map(|v| v.ok_or_else(|| Error::query("update_many takes a value for every field"))).collect::<Result<Vec<_>>>()).collect::<Result<_>>()?;
        if rows.iter().any(|r| r.len() != ncols) {
            return Err(Error::query("update_many row length does not match fields"));
        }
        if rows.is_empty() {
            return Ok(Work::Empty);
        }
        let m = self.model();
        if m.hooks.is_some() {
            let positions = plan::field_positions(m, &spec.fields)?;
            let map = crate::hooks::input_map(m.fields().len(), &positions)?;
            crate::hooks::update_rows(m, &map, &mut rows, params)?;
        }
        let cells: Vec<Vec<Option<Value>>> = rows.into_iter().map(|r| r.into_iter().map(Some).collect()).collect();
        let total = cells.len();
        let unnest = if total >= UNNEST_ROWS { self.unnest(&spec.fields, &cells)? } else { None };
        let per_row = if self.target.caps.update_from_values { ncols } else { 2 * ncols - 1 };
        let mut chunk = if unnest.is_some() { total } else { self.target.caps.max_params.saturating_sub(params.len()) / per_row.max(1) };
        if let Some(n) = spec.batch_size {
            chunk = chunk.min(n);
        }
        let chunk = chunk.max(1);
        let context = if self.context { Some(crate::hooks::context(params)?) } else { None };
        let mut statements = vec![];
        let mut start = 0;
        while start < total {
            let end = (start + chunk).min(total);
            let n = end - start;
            if let (Some(u), true) = (&unnest, n >= UNNEST_ROWS) {
                statements.push(u.bind(&cells[start..end]));
            } else {
                let shape = self.update_shape(n)?;
                let values = shape.slots.iter().map(|s| s.convert(params)).collect::<Result<Vec<_>>>()?;
                statements.push(Self::bind(&shape, &values, ncols, &cells[start..end]));
            }
            start = end;
        }
        Ok(Work::Statements { statements, context })
    }
}
