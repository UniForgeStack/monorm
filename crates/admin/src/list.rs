//! A keyset page: the planner input, the engine run and the rows as JSON.
//!
//! The list is one statement: a to-one column is a join, so a page costs the same
//! number of statements for one row or a hundred. The order ends with the primary key, so
//! a cursor names exactly one position.

use monorm_core::dialect::Target;
use monorm_core::ir::{Operation, ValueType};
use monorm_engine::db::Executor;
use monorm_engine::exec::{self, Outcome};
use monorm_engine::plan::{Plan, Planner};
use monorm_engine::{Error, Params, Result as EngineResult};
use sea_query::Value as Bind;
use serde_json::{json, Value};

use crate::json;
use crate::key;
use crate::site::{Column, Resource, Site};

/// Why a list failed: the request is wrong, or the engine failed.
#[derive(Debug)]
pub enum ListError {
    Bad(String),
    Engine(String),
}

pub struct Page {
    pub rows: Vec<Value>,
    /// The encoded key of each row: the path segment of its detail page.
    pub keys: Vec<String>,
    pub next: Option<String>,
}

struct Binds(Vec<Bind>);

impl Params for Binds {
    fn len(&self) -> usize {
        self.0.len()
    }
    fn value(&self, i: usize, _: Option<ValueType>) -> EngineResult<Bind> {
        self.0.get(i).cloned().ok_or_else(|| Error::query(format!("parameter {i} out of range")))
    }
    fn text(&self, i: usize) -> EngineResult<String> {
        Err(Error::query(format!("parameter {i} is not text")))
    }
    fn count(&self, i: usize) -> EngineResult<u64> {
        Err(Error::query(format!("parameter {i} is not a count")))
    }
}

fn col(c: &Column) -> Value {
    json!({"t": "col", "path": c.path, "name": c.field})
}

/// Rows after the cursor: for each order column in turn, the earlier ones equal and this one past it.
fn after(resource: &Resource, cursor: &[Value]) -> Result<(Value, Vec<Bind>), ListError> {
    let order = &resource.order;
    if cursor.len() != order.len() {
        return Err(ListError::Bad("the cursor does not belong to this list".into()));
    }
    let mut binds = Vec::new();
    for (o, v) in order.iter().zip(cursor) {
        binds.push(json::bind(o.column.ty.ty, v).map_err(ListError::Bad)?);
    }
    let alternatives: Vec<Value> = (0..order.len())
        .map(|i| {
            let mut items: Vec<Value> = (0..i).map(|j| json!({"t": "cmp", "op": "eq", "l": col(&order[j].column), "r": {"t": "param", "i": j}})).collect();
            let op = if order[i].desc { "lt" } else { "gt" };
            items.push(json!({"t": "cmp", "op": op, "l": col(&order[i].column), "r": {"t": "param", "i": i}}));
            json!({"t": "and", "items": items})
        })
        .collect();
    Ok((json!({"t": "or", "items": alternatives}), binds))
}

fn encode(values: &[Value]) -> String {
    serde_json::to_string(values).unwrap_or_default().bytes().map(|b| format!("{b:02x}")).collect()
}

fn decode(cursor: &str) -> Option<Vec<Value>> {
    if !cursor.len().is_multiple_of(2) {
        return None;
    }
    let bytes: Option<Vec<u8>> = (0..cursor.len()).step_by(2).map(|i| u8::from_str_radix(cursor.get(i..i + 2)?, 16).ok()).collect();
    serde_json::from_slice(&bytes?).ok()
}

pub async fn list(site: &Site, resource: &Resource, conn: &dyn Executor, target: Target, cursor: Option<&str>, limit: Option<usize>) -> Result<Page, ListError> {
    let limit = limit.unwrap_or(resource.page_size);
    if limit == 0 || limit > crate::site::MAX_PAGE_SIZE {
        return Err(ListError::Bad(format!("limit is between 1 and {}", crate::site::MAX_PAGE_SIZE)));
    }
    let (filters, binds) = match cursor {
        Some(c) => {
            let values = decode(c).ok_or_else(|| ListError::Bad("the cursor is not valid".into()))?;
            let (filter, binds) = after(resource, &values)?;
            (vec![filter], binds)
        }
        None => (vec![], vec![]),
    };
    let model = &site.schema.model(resource.model).ir.name;
    let op = json!({
        "op": "select",
        "model": model,
        "columns": resource.columns.iter().map(|c| json!({"t": "expr", "expr": col(c)})).collect::<Vec<_>>(),
        "order": resource.order.iter().map(|o| json!({"expr": col(&o.column), "desc": o.desc})).collect::<Vec<_>>(),
        "filters": filters,
        "limit": limit + 1,
    });
    let op: Operation = serde_json::from_value(op).map_err(|e| ListError::Engine(e.to_string()))?;
    let engine = |e: Error| ListError::Engine(e.to_string());
    let plan = Planner::plan(&site.schema, target, &op, &Binds(binds)).map_err(engine)?;
    let Plan::Select(_) = &plan else { return Err(ListError::Engine("a list plans a select".into())) };
    let Outcome::Select(selected) = exec::run(conn, target, plan).await.map_err(engine)? else {
        return Err(ListError::Engine("a list runs a select".into()));
    };
    let types = &selected.plan.types;
    let total = selected.rows.len();
    let mut rows = Vec::with_capacity(total.min(limit));
    for r in 0..total.min(limit) {
        let mut row = Vec::with_capacity(types.len());
        for (c, ty) in types.iter().enumerate() {
            let cell = selected.rows.cell(r, c, *ty).map_err(|e| ListError::Engine(e.to_string()))?;
            row.push(json::cell(&site.schema, *ty, cell));
        }
        rows.push(Value::Array(row));
    }
    let next = (total > limit).then(|| {
        let last = rows.last().and_then(Value::as_array);
        let values: Vec<Value> = resource.order_positions.iter().map(|&p| last.map_or(Value::Null, |r| r[p].clone())).collect();
        encode(&values)
    });
    let keys = rows.iter().map(|r| key::encode(&resource.key_positions.iter().map(|&p| r[p].clone()).collect::<Vec<_>>())).collect();
    Ok(Page { rows, keys, next })
}

/// The detail columns of the record with this key, as JSON cells, or none when no row has the key.
pub async fn detail(site: &Site, resource: &Resource, conn: &dyn Executor, target: Target, key: &[Value]) -> Result<Option<Vec<Value>>, ListError> {
    if key.len() != resource.key_columns.len() {
        return Err(ListError::Bad("the key does not fit this resource".into()));
    }
    let mut binds = Vec::new();
    for (c, v) in resource.key_columns.iter().zip(key) {
        binds.push(json::bind(c.ty.ty, v).map_err(ListError::Bad)?);
    }
    let items: Vec<Value> = resource.key_columns.iter().enumerate().map(|(i, c)| json!({"t": "cmp", "op": "eq", "l": col(c), "r": {"t": "param", "i": i}})).collect();
    let model = &site.schema.model(resource.model).ir.name;
    let op = json!({
        "op": "select",
        "model": model,
        "columns": resource.detail.iter().map(|c| json!({"t": "expr", "expr": col(c)})).collect::<Vec<_>>(),
        "filters": [{"t": "and", "items": items}],
        "limit": 1,
    });
    let op: Operation = serde_json::from_value(op).map_err(|e| ListError::Engine(e.to_string()))?;
    let engine = |e: Error| ListError::Engine(e.to_string());
    let plan = Planner::plan(&site.schema, target, &op, &Binds(binds)).map_err(engine)?;
    let Plan::Select(_) = &plan else { return Err(ListError::Engine("a detail plans a select".into())) };
    let Outcome::Select(selected) = exec::run(conn, target, plan).await.map_err(engine)? else {
        return Err(ListError::Engine("a detail runs a select".into()));
    };
    if selected.rows.is_empty() {
        return Ok(None);
    }
    let mut row = Vec::new();
    for (c, ty) in selected.plan.types.iter().enumerate() {
        let cell = selected.rows.cell(0, c, *ty).map_err(|e| ListError::Engine(e.to_string()))?;
        row.push(json::cell(&site.schema, *ty, cell));
    }
    Ok(Some(row))
}
