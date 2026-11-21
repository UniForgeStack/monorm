//! Engine-managed writes for a model that stores its fields in several tables joined by shared primary keys.
use crate::{
    db::{self, Executor, Transaction},
    error::{query_err, Error, Result},
    exec::{ChainedRows, Outcome},
    params::NoParams,
    plan,
};
use monorm_core::{
    behavior::{OwnerId, PreparedOwnerLink},
    dialect::Target,
    ir::{Bypass, Delete, FieldIr, Select, Update, ValueType},
    schema::Schema,
    writes::{AssignValue, WriteKind},
};
use sea_query::{
    Alias, Expr, ExprTrait, JoinType, LockType, Query, SelectStatement, SimpleExpr, Value,
};

struct OwnerInsert {
    table: String,
    fields: Vec<FieldIr>,
    values: Vec<Option<Value>>,
    assigned: std::collections::BTreeMap<String, SimpleExpr>,
    key: FieldIr,
}
/// Immutable bound insert input, owned independently of the defining registry.
pub struct Insert {
    rows: Vec<Vec<OwnerInsert>>,
    read: SelectStatement,
    table: String,
    key: FieldIr,
    model: usize,
    types: Vec<ValueType>,
    shape: Option<monorm_core::behavior::ResultShape>,
    computed: Vec<crate::hooks::Computed>,
    /// One multi-row statement for each level, with the keys taken first (see [`run_bulk`]).
    bulk: bool,
}

impl Insert {
    /// Fixed input rows determine the owner insert templates; identity reads
    /// contain the generated joins of the logical model.
    pub(crate) fn inspection(&self) -> Vec<crate::inspect::Statement> {
        use sea_query::QueryStatementBuilder;
        // A bulk insert runs one statement for each level, whatever the row count.
        let batching = self.rows.len() > 1 && !self.bulk;
        let mut statements = vec![];
        for row in self.rows.iter().take(if self.bulk { 1 } else { usize::MAX }) {
            for (index, step) in row.iter().enumerate() {
                let mut insert = Query::insert();
                insert.into_table(Alias::new(&step.table));
                let mut fields = vec![];
                let mut values = vec![];
                for (field, value) in step.fields.iter().zip(&step.values) {
                    if let Some(value) = value.clone().map(|v| plan::bind(v, Some(field))).or_else(|| step.assigned.get(&field.column).cloned()) {
                        fields.push(Alias::new(&field.column));
                        values.push(value);
                    }
                }
                if index > 0 {
                    fields.push(Alias::new(&step.key.column));
                    values.push(Expr::val(0)); // identity supplied by the preceding owner
                }
                if fields.is_empty() { insert.or_default_values(); }
                else { insert.columns(fields).values_panic(values); }
                insert.returning(Query::returning().expr(plan::returning_col(&step.key)));
                statements.push(crate::inspect::statement(&insert.into_sub_query_statement(), batching));
            }
            statements.push(crate::inspect::statement(&self.read.clone().into_sub_query_statement(), batching));
        }
        statements
    }
}

pub fn is_multi_table(schema: &Schema, model: &str) -> Result<bool> {
    let m = schema.model(schema.model_idx(model).map_err(query_err)?);
    Ok(schema.owner_links.iter().any(|l| l.child == m.owner))
}

/// The owner links from `owner` up to the root, leaf first.
fn owner_chain(schema: &Schema, owner: OwnerId) -> Vec<&PreparedOwnerLink> {
    let mut chain = vec![];
    let mut current = owner;
    while let Some(link) = schema.owner_links.iter().find(|l| l.child == current) {
        chain.push(link);
        current = link.parent;
    }
    chain
}

/// Every field of the model, in order, and no related load: the identity capture of a
/// mutation reads whole rows (the key and the assigned values sit at fixed columns), with
/// the default filters of `without_defaults` and `without_root_filter`.
pub(crate) fn matched_select(schema: &Schema, model: &str, without_defaults: Bypass, without_root_filter: bool) -> Result<Select> {
    let logical = schema.model(schema.model_idx(model).map_err(query_err)?);
    let names: Vec<_> = logical.fields().iter().map(|f| &f.name).collect();
    serde_json::from_value(serde_json::json!({
        "model": model, "model_fields": names, "without_defaults": without_defaults,
        "without_root_filter": without_root_filter, "without_related": true,
    }))
    .map_err(|e| query_err(e.to_string()))
}

/// The read-back of a write: the named fields, else the default selection, with every
/// read default bypassed so that a filter cannot hide the row that was just written.
fn returned_read(
    schema: &Schema,
    target: Target,
    model: &str,
    model_fields: Option<&[String]>,
    without_defaults: &Bypass,
) -> Result<plan::SelectPlan> {
    let logical = schema.model(schema.model_idx(model).map_err(query_err)?);
    let names = model_fields.or_else(|| plan::active_fields(logical, without_defaults));
    let select: Select = serde_json::from_value(serde_json::json!({
        "model": model, "model_fields": names, "without_defaults": true, "without_related": true,
    }))
    .map_err(|e| query_err(e.to_string()))?;
    plan::plan_select(schema, target, &select, &NoParams)
}

fn returned_shape(read: &mut plan::SelectPlan) -> Option<monorm_core::behavior::ResultShape> {
    match &mut read.output {
        plan::Output::Instances { shape, .. } => shape.take(),
        plan::Output::Rows { .. } => None,
    }
}

/// Ends the own transaction `tx` by the result. Without one (`conn` is a transaction of
/// the caller) nothing opens a savepoint: a failed statement marks the caller's transaction.
async fn finish<T>(tx: Option<std::sync::Arc<dyn Transaction>>, result: Result<T>) -> Result<T> {
    let Some(tx) = tx else { return result };
    match result {
        Ok(out) => {
            tx.commit().await?;
            Ok(out)
        }
        Err(error) => {
            tx.rollback().await?;
            Err(error)
        }
    }
}

/// Complete-child creation. The physical identity is allocated only at the root.
#[allow(clippy::too_many_arguments)]
pub fn prepare_insert(
    schema: &Schema,
    target: Target,
    model: &str,
    fields: &[String],
    rows: Vec<Vec<Option<Value>>>,
    params: &dyn crate::Params,
    model_fields: Option<&[String]>,
    without_defaults: &Bypass,
) -> Result<Insert> {
    prepare_insert_steps(schema, target, model, fields, rows, params, model_fields, without_defaults, true)
}

#[allow(clippy::too_many_arguments)]
fn prepare_insert_steps(
    schema: &Schema,
    target: Target,
    model: &str,
    fields: &[String],
    rows: Vec<Vec<Option<Value>>>,
    params: &dyn crate::Params,
    model_fields: Option<&[String]>,
    without_defaults: &Bypass,
    complete: bool,
) -> Result<Insert> {
    let model_idx = schema.model_idx(model).map_err(query_err)?;
    let logical = schema.model(model_idx);
    let has_assignments = logical.assignments.iter().any(|a| a.applies_to(WriteKind::Insert));
    // The scope column lives on the root level; every level's insert shares the one fill.
    let (mut fields, mut rows) = if complete || has_assignments {
        let (fields, rows) = crate::client_default::fill(logical, fields, rows)?;
        (fields.into_owned(), rows)
    } else {
        (fields.to_vec(), rows)
    };
    if !logical.scoped.is_empty() {
        if rows.iter().any(|r| r.len() != fields.len()) {
            return Err(Error::query("insert row length does not match fields"));
        }
        crate::scoped::fill(logical, &mut fields, &mut rows, params)?;
    }
    let positions = fields
        .iter()
        .map(|f| logical.field_pos(f).map_err(query_err))
        .collect::<Result<Vec<_>>>()?;
    if positions
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        != positions.len()
    {
        return Err(Error::query("duplicate insert field"));
    }
    if rows.iter().any(|r| r.len() != positions.len()) {
        return Err(Error::query("insert row length does not match fields"));
    }
    if positions
        .iter()
        .any(|p| logical.computed(*p))
    {
        return Err(Error::query("computed fields are read-only"));
    }
    if logical.hooks.is_some() {
        let map = crate::hooks::input_map(logical.fields().len(), &positions)?;
        crate::hooks::insert_rows(logical, &map, &mut rows, false, params)?;
    }
    let mut expressions = std::collections::BTreeMap::new();
    if has_assignments || (complete && !logical.client_defaults.is_empty()) {
        let mut planner = plan::Planner::new(schema, &[], target, model, None, params, vec![], 0)?;
        let is_set = |name: &str| {
            fields.iter().position(|f| f == name).is_none_or(|i| rows.iter().all(|row| row[i].is_some()))
        };
        for (column, expression) in plan::maintained(logical, WriteKind::Insert, is_set, &mut planner)? {
            if let Some(field) = logical.fields().iter().find(|f| f.column == column) {
                expressions.insert(field.name.as_str(), plan::write_expr(expression, field));
            }
        }
        if complete {
            for (position, default) in &logical.client_defaults {
                let field = &logical.fields()[*position];
                if is_set(&field.name) || expressions.contains_key(field.name.as_str()) {
                    continue;
                }
                if let Some(expression) = plan::insert_default_expression(logical, *position, default, &mut planner)? {
                    expressions.insert(field.name.as_str(), expression);
                }
            }
        }
    }
    let mut chain = vec![logical.owner];
    chain.extend(owner_chain(schema, logical.owner).iter().map(|l| l.parent));
    chain.reverse();
    let owners = &schema.physical().models;
    let mut prepared = vec![];
    for row in rows {
        let mut steps = vec![];
        for (index, &owner) in chain.iter().enumerate() {
            let physical = &owners[owner.0];
            let mut columns = vec![];
            let mut values = vec![];
            let mut assigned = std::collections::BTreeMap::new();
            for (&position, value) in positions.iter().zip(&row) {
                let resolved = &logical.resolved_fields[position];
                let field = if position == logical.pk && index == 0 {
                    physical.pk_field()
                } else if position != logical.pk && resolved.storage.owner == owner {
                    &physical.fields()[resolved.storage.column]
                } else {
                    continue;
                };
                if value.is_none() {
                    if let Some(expression) = expressions.get(logical.fields()[position].name.as_str()) {
                        assigned.insert(field.column.clone(), expression.clone());
                    }
                }
                columns.push(field.clone());
                values.push(value.clone());
            }
            steps.push(OwnerInsert {
                table: physical.table().into(),
                fields: columns,
                values,
                assigned,
                key: physical.pk_field().clone(),
            });
        }
        prepared.push(steps);
    }
    let mut read = returned_read(schema, target, model, model_fields, without_defaults)?;
    let shape = returned_shape(&mut read);
    let positions = plan::shape_positions(logical, shape.as_ref()).to_vec();
    Ok(Insert {
        rows: prepared,
        table: logical.table().into(),
        key: logical.pk_field().clone(),
        model: model_idx,
        types: read.types,
        computed: crate::hooks::computations(logical, 0, &positions, params)?,
        read: read.stmt,
        shape,
        bulk: false,
    })
}

/// `insert_many` on a model of several tables: the keys first, then one multi-row insert
/// for each table, and one read in input order. Only PostgreSQL has the key statement.
#[allow(clippy::too_many_arguments)]
pub fn prepare_bulk_insert(
    schema: &Schema,
    target: Target,
    model: &str,
    fields: &[String],
    rows: Vec<Vec<Option<Value>>>,
    params: &dyn crate::Params,
    model_fields: Option<&[String]>,
    without_defaults: &Bypass,
) -> Result<Insert> {
    debug_assert!(target.dialect == monorm_core::dialect::Dialect::Postgres);
    let mut insert = prepare_insert(schema, target, model, fields, rows, params, model_fields, without_defaults)?;
    insert.bulk = insert.rows.iter().flatten().all(|step| step.assigned.is_empty());
    Ok(insert)
}

pub async fn run_insert(conn: &dyn Executor, target: Target, insert: Insert, own_tx: bool) -> Result<Outcome> {
    let tx = if own_tx { Some(conn.begin().await?) } else { None };
    let exec: &dyn Executor = match &tx {
        Some(t) => t.as_ref(),
        None => conn,
    };
    if insert.bulk {
        let result = run_bulk(exec, target, insert).await;
        return finish(tx, result).await;
    }
    let result = async {
        let mut fetched = vec![];
        for row in insert.rows {
            let mut identity = None;
            for step in row {
                let mut columns = vec![];
                let mut values = vec![];
                for (field, value) in step.fields.iter().zip(step.values) {
                    // Omitting a column uses its actual database default on both backends.
                    if let Some(value) = value.map(|v| plan::bind(v, Some(field))).or_else(|| step.assigned.get(&field.column).cloned()) {
                        columns.push(Alias::new(&field.column));
                        values.push(value);
                    }
                }
                if let Some(value) = identity.take() {
                    columns.push(Alias::new(&step.key.column));
                    values.push(plan::bind(value, Some(&step.key)));
                }
                let mut statement = Query::insert();
                statement.into_table(Alias::new(&step.table));
                if columns.is_empty() {
                    statement.or_default_values();
                } else {
                    statement
                        .columns(columns)
                        .values(values)
                        .map_err(|e| query_err(e.to_string()))?;
                }
                statement.returning(Query::returning().expr(plan::returning_col(&step.key)));
                let (sql, args) = db::build(target.dialect, &statement);
                let returned = exec.query(sql, args).await?;
                if returned.len() != 1 {
                    return Err(Error::query("multi-table insert must return one identity"));
                }
                identity = Some(returned.value(0, 0, step.key.value_type())?);
            }
            let mut read = insert.read.clone();
            read.and_where(
                Expr::col((Alias::new(&insert.table), Alias::new(&insert.key.column)))
                    .eq(plan::bind(identity.unwrap(), Some(&insert.key))),
            );
            let (sql, args) = db::build(target.dialect, &read);
            let returned = exec.query(sql, args).await?;
            if returned.len() != 1 {
                return Err(Error::query(
                    "multi-table insert could not read its stored row",
                ));
            }
            fetched.push(returned);
        }
        Ok(Outcome::Rows {
            model: insert.model,
            types: insert.types,
            rows: crate::hooks::compute(Box::new(ChainedRows::new(fetched)), &insert.computed)?,
            shape: insert.shape,
        })
    }
    .await;
    finish(tx, result).await
}

/// The position of the primary key column among the fields of a step.
fn key_position(step: &OwnerInsert) -> Option<usize> {
    step.fields.iter().position(|f| f.primary_key)
}

/// Puts `key` last among the columns of a step, so that every row of one level has the same columns.
fn set_key(step: &mut OwnerInsert, key: Value) {
    if let Some(at) = key_position(step) {
        step.fields.remove(at);
        step.values.remove(at);
    }
    step.fields.push(step.key.clone());
    step.values.push(Some(key));
}

fn quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// The statement that takes `count` new root keys: the next values of the key's sequence, or
/// its default expression evaluated for each row.
fn key_statement(table: &str, key: &FieldIr, count: usize) -> Result<String> {
    let source = if key.auto_increment {
        let table = quoted(table).replace('\'', "''");
        format!("nextval(pg_get_serial_sequence('{table}', '{}'))", key.column.replace('\'', "''"))
    } else if let Some(expr) = &key.default_sql {
        expr.clone()
    } else {
        return Err(Error::query(format!(
            "multi-table insert_many needs a value for {table}.{} in every row: the key has no sequence or default",
            key.column
        )));
    };
    // `nextval` gives a bigint; the cast keeps the key in the type of its column.
    Ok(format!("SELECT ({source})::{} FROM generate_series(1, {count})", monorm_core::migrate::model::sql_type(key)))
}

/// `insert_many` on PostgreSQL, and attach for many parents: the root keys in one statement
/// (rows with a key of their own skip it), one multi-row `INSERT` for each level in level
/// order, then one read by key that returns the rows in input order.
async fn run_bulk(exec: &dyn Executor, target: Target, insert: Insert) -> Result<Outcome> {
    let Insert { mut rows, read, table, key, model, types, shape, computed, .. } = insert;
    let max_params = target.caps.max_params.max(4);
    let missing: Vec<usize> = (0..rows.len()).filter(|&i| rows[i][0].fields.iter().zip(&rows[i][0].values).all(|(f, v)| !f.primary_key || v.is_none())).collect();
    if !missing.is_empty() {
        let root = &rows[missing[0]][0];
        let sql = key_statement(&root.table, &root.key, missing.len())?;
        let keys = exec.query(sql, vec![]).await?;
        if keys.len() != missing.len() {
            return Err(Error::query("multi-table insert could not take its keys"));
        }
        let key_type = root.key.value_type();
        for (n, &i) in missing.iter().enumerate() {
            let value = keys.value(n, 0, key_type).map_err(|e| Error::query(format!("multi-table insert_many: cannot read the new key: {e}")))?;
            set_key(&mut rows[i][0], value);
        }
    }
    let mut keys = Vec::with_capacity(rows.len());
    for row in &mut rows {
        let at = key_position(&row[0]).ok_or_else(|| Error::query("multi-table insert has no key"))?;
        let value = row[0].values[at].clone().ok_or_else(|| Error::query("multi-table insert has no key"))?;
        for step in row.iter_mut() {
            set_key(step, value.clone());
        }
        keys.push(value);
    }
    for level in 0..rows.first().map_or(0, Vec::len) {
        let first = &rows[0][level];
        let used: Vec<usize> = (0..first.fields.len())
            .filter(|&c| rows.iter().any(|r| r[level].values[c].is_some()))
            .collect();
        let per_statement = (max_params / used.len().max(1)).max(1);
        for chunk in rows.chunks(per_statement) {
            let mut statement = Query::insert();
            statement.into_table(Alias::new(&first.table));
            statement.columns(used.iter().map(|&c| Alias::new(&first.fields[c].column)));
            for row in chunk {
                let step = &row[level];
                let cells: Vec<SimpleExpr> = used
                    .iter()
                    .map(|&c| match &step.values[c] {
                        Some(v) => plan::bind(v.clone(), Some(&step.fields[c])),
                        None => SimpleExpr::Keyword(sea_query::Keyword::Default),
                    })
                    .collect();
                statement.values(cells).map_err(|e| query_err(e.to_string()))?;
            }
            let (sql, args) = db::build(target.dialect, &statement);
            exec.execute(sql, args).await?;
        }
    }
    let mut fetched = vec![];
    for chunk in keys.chunks(((max_params - 1) / 2).max(1)) {
        let key_column = || Expr::col((Alias::new(&table), Alias::new(&key.column)));
        let mut read = read.clone();
        read.clear_order_by();
        read.and_where(key_column().is_in(chunk.iter().map(|k| plan::bind(k.clone(), Some(&key)))));
        let mut order = sea_query::CaseStatement::new();
        for (position, k) in chunk.iter().enumerate() {
            order = order.case(key_column().eq(plan::bind(k.clone(), Some(&key))), Expr::val(position as i64));
        }
        read.order_by_expr(order.into(), sea_query::Order::Asc);
        let (sql, args) = db::build(target.dialect, &read);
        let returned = exec.query(sql, args).await?;
        if returned.len() != chunk.len() {
            return Err(Error::query("multi-table insert could not read its stored rows"));
        }
        fetched.push(returned);
    }
    Ok(Outcome::Rows {
        model,
        types,
        rows: crate::hooks::compute(Box::new(ChainedRows::new(fetched)), &computed)?,
        shape,
    })
}

/// Where an owner column gets its new value.
enum Source {
    /// A column of the matched row after the model's fields (the value of a `set` item).
    Matched(usize),
    /// A value fixed at plan time (the current time).
    Value(Value),
    /// The column plus a number, in SQL.
    Add(i64),
}

struct Assignment {
    table: String,
    key: FieldIr,
    field: FieldIr,
    source: Source,
}
/// One captured identity set governs every table mutation.
pub struct Mutation {
    matched: SelectStatement,
    read: SelectStatement,
    table: String,
    key: FieldIr,
    /// Where a delete goes: the chain root row, and its foreign keys remove every level below it.
    delete_table: String,
    delete_key: FieldIr,
    key_position: usize,
    model: usize,
    types: Vec<ValueType>,
    shape: Option<monorm_core::behavior::ResultShape>,
    computed: Vec<crate::hooks::Computed>,
    assignments: Vec<Assignment>,
    returning: bool,
    delete: bool,
    /// The level keys that the root delete removes through `ON DELETE CASCADE`.
    cascades: Cascades,
    /// A hard delete: the root trigger of database mode can soft-delete a live row, so the delete may affect no row.
    hard: bool,
}

impl Mutation {
    /// Owner writes and optional identity reads repeat for captured parent rows,
    /// whose number is known only after executing `matched`.
    pub(crate) fn inspection(&self) -> Vec<crate::inspect::Statement> {
        use sea_query::QueryStatementBuilder;
        let mut statements = vec![crate::inspect::statement(&self.matched.clone().into_sub_query_statement(), false)];
        let read = || crate::inspect::statement(&self.read.clone().into_sub_query_statement(), true);
        if self.delete {
            // A delete reads its rows before it removes them.
            if self.returning {
                statements.push(read());
            }
            let mut delete = Query::delete();
            delete.from_table(Alias::new(&self.delete_table)).and_where(Expr::col(Alias::new(&self.delete_key.column)).eq(0));
            statements.push(crate::inspect::statement(&delete.into_sub_query_statement(), true));
        } else {
            let mut writes = std::collections::BTreeMap::new();
            for assignment in &self.assignments {
                let update = writes.entry(&assignment.table).or_insert_with(|| {
                    let mut update = Query::update();
                    update.table(Alias::new(&assignment.table)).and_where(Expr::col(Alias::new(&assignment.key.column)).eq(0));
                    update
                });
                update.value(Alias::new(&assignment.field.column), match &assignment.source {
                    Source::Add(n) => Expr::col(Alias::new(&assignment.field.column)).add(*n),
                    _ => plan::bind(Value::Int(Some(0)), Some(&assignment.field)),
                });
            }
            for update in writes.into_values() {
                statements.push(crate::inspect::statement(&update.into_sub_query_statement(), true));
            }
            if self.returning {
                statements.push(read());
            }
        }
        statements
    }
}

#[allow(clippy::too_many_arguments)]
fn mutation(
    schema: &Schema,
    target: Target,
    model: &str,
    filters: Vec<monorm_core::ir::Expr>,
    with: Vec<monorm_core::ir::Cte>,
    params: &dyn crate::Params,
    returning: bool,
    model_fields: Option<&[String]>,
    delete: bool,
    without_defaults: monorm_core::ir::Bypass,
    without_root_filter: bool,
    prepared_match: Option<SelectStatement>,
) -> Result<Mutation> {
    let model_idx = schema.model_idx(model).map_err(query_err)?;
    let logical = schema.model(model_idx);
    let mut read = returned_read(schema, target, model, model_fields, &without_defaults)?;
    let shape = returned_shape(&mut read);
    let mut query = matched_select(schema, model, without_defaults, without_root_filter)?;
    query.filters = filters;
    query.with = with;
    let mut matched = match prepared_match {
        Some(statement) => statement,
        None => plan::plan_select(schema, target, &query, params)?.stmt,
    };
    // Lock the whole owner chain on PostgreSQL. SQLite's read transaction pins
    // the matched snapshot; an incompatible concurrent writer causes rollback.
    if target.dialect == monorm_core::dialect::Dialect::Postgres {
        let owners = &schema.physical().models;
        let mut alias = logical.table().to_owned();
        let mut locks = vec![Alias::new(&alias)];
        for link in owner_chain(schema, logical.owner) {
            let parent = &owners[link.parent.0];
            let next_alias = format!("__write_owner_{}", link.parent.0);
            matched.join_as(
                JoinType::InnerJoin,
                Alias::new(parent.table()),
                Alias::new(&next_alias),
                Expr::col((
                    Alias::new(&next_alias),
                    Alias::new(&parent.fields()[link.parent_key].column),
                ))
                .eq(Expr::col((
                    Alias::new(&alias),
                    Alias::new(&owners[link.child.0].fields()[link.child_key].column),
                ))),
            );
            locks.push(Alias::new(&next_alias));
            alias = next_alias;
        }
        matched.lock_with_tables(LockType::Update, locks);
    }
    let positions = plan::shape_positions(logical, shape.as_ref()).to_vec();
    Ok(Mutation {
        matched,
        read: read.stmt,
        table: logical.table().into(),
        key: logical.pk_field().clone(),
        delete_table: logical.table().into(),
        delete_key: logical.pk_field().clone(),
        key_position: logical.pk,
        model: model_idx,
        types: read.types,
        shape,
        computed: if returning { crate::hooks::computations(logical, 0, &positions, params)? } else { vec![] },
        assignments: vec![],
        returning,
        delete,
        cascades: Cascades::default(),
        hard: false,
    })
}

pub fn prepare_delete(
    schema: &Schema,
    target: Target,
    query: &Delete,
    params: &dyn crate::Params,
) -> Result<Mutation> {
    let logical = schema.model(schema.model_idx(&query.model).map_err(query_err)?);
    // A delete rewrite of the root (a soft delete) updates only the root row; any other delete removes every level.
    let rewrite = logical.delete_rewrite.as_ref().filter(|_| !query.hard);
    let mut filters = query.filters.clone();
    if let Some(rewrite) = rewrite { filters.push(rewrite.guard.clone()); }
    let mut plan = mutation(
        schema,
        target,
        &query.model,
        filters,
        query.with.clone(),
        params,
        query.returning,
        query.model_fields.as_deref(),
        rewrite.is_none(),
        query.without_defaults.clone(),
        query.without_root_filter,
        None,
    )?;
    if rewrite.is_none() {
        plan.hard = query.hard;
        let owners = &schema.physical().models;
        let root = owner_chain(schema, logical.owner).last().map_or(logical.owner, |link| link.parent);
        plan.delete_table = owners[root.0].table().into();
        plan.delete_key = owners[root.0].pk_field().clone();
        plan.cascades = Cascades::under(schema, root);
    }
    if let Some(rewrite) = rewrite {
        let owners = &schema.physical().models;
        let storage = logical.resolved_fields[rewrite.field].storage;
        let owner = &owners[storage.owner.0];
        let field = owner.fields()[storage.column].clone();
        plan.assignments.push(Assignment { table: owner.table().into(), key: owner.pk_field().clone(), source: Source::Value(params.now(field.ty)), field });
        plan.assignments.extend(maintained(schema, logical, WriteKind::DeleteRewrite, |_| false, params)?);
    }
    Ok(plan)
}

/// The columns the root maintains on a write of kind `kind` that the caller does not set: the current time or `column + 1`.
/// A composed model keeps them on the owner that stores the field, which is the root.
fn maintained(schema: &Schema, logical: &monorm_core::schema::Model, kind: WriteKind, is_set: impl Fn(&str) -> bool, params: &dyn crate::Params) -> Result<Vec<Assignment>> {
    let owners = &schema.physical().models;
    let mut out = vec![];
    for a in logical.assignments.iter().filter(|a| a.applies_to(kind)) {
        let name = &logical.fields()[a.field].name;
        if is_set(name) { continue; }
        let storage = logical.resolved_fields[a.field].storage;
        let owner = &owners[storage.owner.0];
        let field = owner.fields()[storage.column].clone();
        let source = match a.value {
            AssignValue::Now => Source::Value(params.now(field.ty)),
            AssignValue::Increment => Source::Add(1),
            _ => return Err(Error::query(format!("{}.{name}: a hook or expression assignment is not supported on a composed model", logical.ir.name))),
        };
        out.push(Assignment { table: owner.table().into(), key: owner.pk_field().clone(), field, source });
    }
    Ok(out)
}

pub fn prepare_update(
    schema: &Schema,
    target: Target,
    query: &Update,
    params: &dyn crate::Params,
    expressions: Vec<SimpleExpr>,
    matched: SelectStatement,
) -> Result<Mutation> {
    let mut plan = mutation(
        schema,
        target,
        &query.model,
        query.filters.clone(),
        query.with.clone(),
        params,
        query.returning,
        query.model_fields.as_deref(),
        false,
        query.without_defaults.clone(),
        query.without_root_filter,
        Some(matched),
    )?;
    let logical = schema.model(plan.model);
    let owners = &schema.physical().models;
    for (offset, (assignment, expression)) in query.set.iter().zip(expressions).enumerate() {
        let position = logical.field_pos(&assignment.field).map_err(query_err)?;
        if position == logical.pk {
            return Err(Error::query(
                "multi-table update cannot change the shared primary key",
            ));
        }
        let storage = logical.resolved_fields[position].storage;
        let owner = &owners[storage.owner.0];
        let field = owner.fields()[storage.column].clone();
        let expression = match &field.read_sql {
            Some(t) => SimpleExpr::cust_with_expr(t.replace("{}", "$1"), expression),
            None => expression,
        };
        plan.matched.expr(expression);
        plan.assignments.push(Assignment {
            table: owner.table().into(),
            key: owner.pk_field().clone(),
            field,
            source: Source::Matched(logical.fields().len() + offset),
        });
    }
    // A change on any level touches the root: its time and version columns change even when only a child level does.
    let assigned: Vec<&str> = query.set.iter().map(|a| a.field.as_str()).collect();
    plan.assignments.extend(maintained(schema, logical, WriteKind::Update, |name| assigned.contains(&name), params)?);
    Ok(plan)
}

/// The returned form of each captured row, read by key (`what`: the verb of the error).
async fn read_rows(exec: &dyn Executor, target: Target, plan: &Mutation, keys: &[Value], what: &str) -> Result<Vec<Box<dyn crate::db::RowSet>>> {
    let mut rows = vec![];
    for key in keys {
        let mut read = plan.read.clone();
        read.and_where(Expr::col((Alias::new(&plan.table), Alias::new(&plan.key.column))).eq(plan::bind(key.clone(), Some(&plan.key))));
        let (sql, args) = db::build(target.dialect, &read);
        let row = exec.query(sql, args).await?;
        if row.len() != 1 {
            return Err(Error::query(format!("{what} multi-table identity disappeared")));
        }
        rows.push(row);
    }
    Ok(rows)
}

/// The level keys that a delete of a chain root removes through `ON DELETE CASCADE`.
#[derive(Default)]
pub struct Cascades {
    keys: Vec<crate::introspect::LevelKey>,
    /// The entry of the check in the cascade cache.
    id: String,
}

impl Cascades {
    /// Every level under `root`, sibling levels included, goes with the root row.
    pub fn under(schema: &Schema, root: OwnerId) -> Self {
        let owners = &schema.physical().models;
        let under_root = |link: &&PreparedOwnerLink| owner_chain(schema, link.child).last().map_or(link.parent, |top| top.parent) == root;
        let keys: Vec<_> = schema.owner_links.iter().filter(under_root).map(|link| crate::introspect::LevelKey {
            table: owners[link.child.0].table().into(),
            column: owners[link.child.0].fields()[link.child_key].column.clone(),
            parent: owners[link.parent.0].table().into(),
        }).collect();
        let id = keys.iter().map(|k| format!("{}.{}.{}", k.table, k.column, k.parent)).collect::<Vec<_>>().join(",");
        Cascades { keys, id }
    }

    /// The cascades of a delete of `model`: none unless it is the root of a composition.
    pub fn of_model(schema: &Schema, model: &str) -> Result<Option<Self>> {
        let logical = schema.model(schema.model_idx(model).map_err(query_err)?);
        let root = owner_chain(schema, logical.owner).last().map_or(logical.owner, |link| link.parent);
        Ok(Some(Self::under(schema, root)).filter(|c| !c.keys.is_empty()))
    }

    /// A delete removes the other levels only through `ON DELETE CASCADE`: tables made by hand or by
    /// Django have none, so the delete fails with a raw error or leaves rows behind.
    /// `conn` holds the cache of passed checks; `exec` runs the reads.
    pub async fn check(&self, conn: &dyn Executor, exec: &dyn Executor) -> Result<()> {
        if self.keys.is_empty() { return Ok(()); }
        let cache = conn.cascade_cache();
        if cache.is_some_and(|c| c.contains(&self.id)) { return Ok(()); }
        let missing = crate::introspect::missing_cascades(exec, &self.keys).await?;
        if !missing.is_empty() {
            return Err(Error::query(format!(
                "a composed delete needs ON DELETE CASCADE on each level key, and none is in force on {}; add it, for example ALTER TABLE <table> DROP CONSTRAINT <fkey>, ADD FOREIGN KEY (<column>) REFERENCES <parent> (<key>) ON DELETE CASCADE (on SQLite, PRAGMA foreign_keys = ON)",
                missing.join(", "))));
        }
        if let Some(cache) = cache { cache.insert(self.id.clone()); }
        Ok(())
    }
}

pub async fn run_mutation(conn: &dyn Executor, target: Target, plan: Mutation, own_tx: bool) -> Result<Outcome> {
    let tx = if own_tx { Some(conn.begin().await?) } else { None };
    let exec: &dyn Executor = match &tx {
        Some(t) => t.as_ref(),
        None => conn,
    };
    let result = async {
        if plan.delete { plan.cascades.check(conn, exec).await?; }
        let (sql, args) = db::build(target.dialect, &plan.matched);
        let matched = exec.query(sql, args).await?;
        let mut keys = vec![];
        for row in 0..matched.len() {
            keys.push(matched.value(row, plan.key_position, plan.key.value_type())?);
        }
        // A deleted row is read before it goes.
        let before = if plan.delete && plan.returning { read_rows(exec, target, &plan, &keys, "deleted").await? } else { vec![] };
        let mut removed = vec![false; keys.len()];
        for (row, key) in keys.iter().enumerate() {
            if plan.delete {
                // A hard delete goes to the root row, and its foreign keys remove the other levels. In database
                // mode the root trigger soft-deletes a live row, and the delete affects no row.
                let mut delete = Query::delete();
                delete.from_table(Alias::new(&plan.delete_table)).and_where(
                    Expr::col(Alias::new(&plan.delete_key.column))
                        .eq(plan::bind(key.clone(), Some(&plan.delete_key))),
                );
                let (sql, args) = db::build(target.dialect, &delete);
                let count = exec.execute(sql, args).await?;
                if count > 0 || !plan.hard {
                    if count != 1 {
                        return Err(Error::query("captured multi-table identity disappeared"));
                    }
                    removed[row] = true;
                }
            } else {
                let mut writes = std::collections::BTreeMap::new();
                for assignment in &plan.assignments {
                    let value = match &assignment.source {
                        Source::Matched(position) => Some(matched.value(row, *position, assignment.field.value_type())?),
                        Source::Value(value) => Some(value.clone()),
                        Source::Add(_) => None,
                    };
                    let statement = writes.entry(assignment.table.clone()).or_insert_with(|| {
                        let mut update = Query::update();
                        update.table(Alias::new(&assignment.table)).and_where(
                            Expr::col(Alias::new(&assignment.key.column))
                                .eq(plan::bind(key.clone(), Some(&assignment.key))),
                        );
                        update
                    });
                    match (&assignment.source, value) {
                        (Source::Add(n), _) => statement.value(Alias::new(&assignment.field.column), Expr::col(Alias::new(&assignment.field.column)).add(*n)),
                        (_, Some(value)) => statement.value(Alias::new(&assignment.field.column), plan::bind(value, Some(&assignment.field))),
                        _ => unreachable!("a value source has a value"),
                    };
                }
                for statement in writes.into_values() {
                    let (sql, args) = db::build(target.dialect, &statement);
                    if exec.execute(sql, args).await? != 1 {
                        return Err(Error::query("captured multi-table owner disappeared"));
                    }
                }
            }
        }
        let affected = if plan.delete { removed.iter().filter(|gone| **gone).count() } else { keys.len() };
        if !plan.returning {
            return Ok(Outcome::Affected(affected as u64));
        }
        let returned = if plan.delete {
            before.into_iter().zip(&removed).filter_map(|(rows, gone)| gone.then_some(rows)).collect()
        } else {
            read_rows(exec, target, &plan, &keys, "updated").await?
        };
        Ok(Outcome::Rows {
            model: plan.model,
            types: plan.types,
            rows: crate::hooks::compute(Box::new(ChainedRows::new(returned)), &plan.computed)?,
            shape: plan.shape,
        })
    }
    .await;
    finish(tx, result).await
}

/// Attach only local columns to a physically existing immediate parent. The FK
/// checks existence and races; no ancestor insert or update is executed.
pub fn prepare_attach(
    schema: &Schema,
    target: Target,
    model: &str,
    identity: Value,
    fields: &[String],
    rows: Vec<Vec<Option<Value>>>,
) -> Result<Insert> {
    let rows = rows.into_iter().map(|row| (identity.clone(), row)).collect();
    prepare_attach_rows(schema, target, model, fields, rows)
}

/// The child rows of several existing parents, each row with its parent identity: one
/// multi-row insert for the child table, and one read in input order.
pub fn prepare_attach_many(
    schema: &Schema,
    target: Target,
    model: &str,
    fields: &[String],
    rows: Vec<(Value, Vec<Option<Value>>)>,
) -> Result<Insert> {
    let mut insert = prepare_attach_rows(schema, target, model, fields, rows)?;
    insert.bulk = true;
    Ok(insert)
}

fn prepare_attach_rows(
    schema: &Schema,
    target: Target,
    model: &str,
    fields: &[String],
    rows: Vec<(Value, Vec<Option<Value>>)>,
) -> Result<Insert> {
    if !is_multi_table(schema, model)? {
        return Err(Error::query("attach requires a child model of several tables"));
    }
    let logical = schema.model(schema.model_idx(model).map_err(query_err)?);
    for field in fields {
        let position = logical.field_pos(field).map_err(query_err)?;
        if position == logical.pk
            || logical.resolved_fields[position].storage.owner != logical.owner
        {
            return Err(Error::query(
                "attach accepts only local child values; parent values cannot be overwritten",
            ));
        }
    }
    let mut fields = fields.to_vec();
    fields.push(logical.pk_field().name.clone());
    let rows = rows
        .into_iter()
        .map(|(identity, mut row)| {
            row.push(Some(identity));
            row
        })
        .collect();
    let mut insert = prepare_insert_steps(schema, target, model, &fields, rows, &NoParams, None, &Bypass::All(true), false)?;
    for row in &mut insert.rows {
        let mut leaf = row.pop().expect("prepared multi-table leaf");
        // Complete creates place a supplied shared key at the root. Attach places
        // that same validated value directly in the sole child statement.
        let root = row.first().expect("multi-table ancestor");
        let value = root
            .fields
            .iter()
            .position(|f| f.primary_key)
            .and_then(|p| root.values[p].clone())
            .ok_or_else(|| Error::query("attach requires an explicit parent identity"))?;
        leaf.fields.push(leaf.key.clone());
        leaf.values.push(Some(value));
        *row = vec![leaf];
    }
    Ok(insert)
}
