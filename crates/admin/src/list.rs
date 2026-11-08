//! A keyset page: the planner input, the engine run and the rows as JSON.
//!
//! The list is one statement: a to-one column is a join, so a page costs the same
//! number of statements for one row or a hundred. The order ends with the primary key, so
//! a cursor names exactly one position.

use monorm_core::dialect::{Dialect, Target};
use monorm_core::ir::{ColType, Operation, ValueType};
use monorm_engine::db::Executor;
use monorm_engine::exec::{self, Outcome};
use monorm_engine::plan::{Plan, Planner};
use monorm_engine::{Error, Params, Result as EngineResult};
use sea_query::Value as Bind;
use serde_json::{json, Value};

use crate::json;
use crate::key;
use crate::site::{Column, Mode, OrderColumn, Resource, Site, COUNT_CAP, LARGE_TABLE_ROWS, MAX_PAGE_SIZE};

/// Why a list failed: the request is wrong, the engine failed, or a statement ran past its time limit.
#[derive(Debug)]
pub enum ListError {
    Bad(String),
    Engine(String),
    Timeout(String),
}

pub struct Page {
    pub rows: Vec<Value>,
    /// The encoded key of each row: the path segment of its detail page.
    pub keys: Vec<String>,
    /// The cursor of the page after this one (`cursor=`), or none on the last page.
    pub next: Option<String>,
    /// The cursor of the page before this one (`before=`), or none on the first page.
    pub prev: Option<String>,
}

/// Which page of the keyset order a request asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Position {
    First,
    Last,
    After(String),
    Before(String),
    /// The 1-based page, by offset.
    Page(usize),
}

#[derive(Debug, Clone)]
pub struct ListRequest {
    pub position: Position,
    pub limit: Option<usize>,
    /// `-rank,name`: the sort columns. None: the order of the resource.
    pub sort: Option<String>,
    /// The search text.
    pub q: Option<String>,
    /// The keys that a `search_with` function found, in place of the search fields.
    pub found: Option<Vec<Vec<Value>>>,
}

pub struct Total {
    pub count: usize,
    /// The count stopped at the cap: the real count is higher.
    pub capped: bool,
}

#[derive(Default)]
struct Binds(Vec<Bind>);

impl Binds {
    fn push(&mut self, v: Bind) -> Value {
        self.0.push(v);
        json!({"t": "param", "i": self.0.len() - 1})
    }
}

impl Params for Binds {
    fn len(&self) -> usize {
        self.0.len()
    }
    fn value(&self, i: usize, _: Option<ValueType>) -> EngineResult<Bind> {
        self.0.get(i).cloned().ok_or_else(|| Error::query(format!("parameter {i} out of range")))
    }
    fn text(&self, i: usize) -> EngineResult<String> {
        match self.0.get(i) {
            Some(Bind::String(Some(s))) => Ok(s.to_string()),
            _ => Err(Error::query(format!("parameter {i} is not text"))),
        }
    }
    fn count(&self, i: usize) -> EngineResult<u64> {
        Err(Error::query(format!("parameter {i} is not a count")))
    }
}

fn col(c: &Column) -> Value {
    json!({"t": "col", "path": c.path, "name": c.field})
}

fn engine(e: Error) -> ListError {
    let message = e.to_string();
    let lower = message.to_lowercase();
    if lower.contains("statement timeout") || lower.contains("canceling statement") || lower.contains("57014") || lower.contains("timed out") || lower.contains("deadline") {
        ListError::Timeout(message)
    } else {
        ListError::Engine(message)
    }
}

/// The order of a request: the sort columns, then the key; or the order of the resource.
/// Returns the order and its canonical text (the cursor carries it).
fn effective_order(resource: &Resource, sort: Option<&str>) -> Result<(Vec<OrderColumn>, String), ListError> {
    let Some(sort) = sort.map(str::trim).filter(|s| !s.is_empty()) else { return Ok((resource.order.clone(), String::new())) };
    let mut order: Vec<OrderColumn> = Vec::new();
    let mut text = Vec::new();
    for token in sort.split(',') {
        let (desc, name) = match token.trim().strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, token.trim()),
        };
        let Some(found) = resource.sortable.iter().find(|s| s.column.label == name) else {
            return Err(ListError::Bad(format!("sort: {name:?} is not a sort column of this list")));
        };
        if order.iter().any(|o| o.column.label == name) {
            return Err(ListError::Bad(format!("sort: {name:?} is listed twice")));
        }
        order.push(OrderColumn { column: found.column.clone(), desc });
        text.push(format!("{}{name}", if desc { "-" } else { "" }));
    }
    if order.len() > 3 {
        return Err(ListError::Bad("sort takes at most 3 columns".into()));
    }
    for k in &resource.key_columns {
        if !order.iter().any(|o| o.column.label == k.label) {
            order.push(OrderColumn { column: k.clone(), desc: false });
        }
    }
    Ok((order, text.join(",")))
}

/// Rows after the cursor: for each order column in turn, the earlier ones equal and this one past it.
fn after(order: &[OrderColumn], cursor: &[Value], binds: &mut Binds) -> Result<Value, ListError> {
    if cursor.len() != order.len() {
        return Err(ListError::Bad("the cursor does not belong to this list".into()));
    }
    let mut params = Vec::new();
    for (o, v) in order.iter().zip(cursor) {
        let b = json::bind(o.column.ty.ty, v).map_err(ListError::Bad)?;
        params.push(binds.push(b));
    }
    let alternatives: Vec<Value> = (0..order.len())
        .map(|i| {
            let mut items: Vec<Value> = (0..i).map(|j| json!({"t": "cmp", "op": "eq", "l": col(&order[j].column), "r": params[j]})).collect();
            let op = if order[i].desc { "lt" } else { "gt" };
            items.push(json!({"t": "cmp", "op": op, "l": col(&order[i].column), "r": params[i]}));
            json!({"t": "and", "items": items})
        })
        .collect();
    Ok(json!({"t": "or", "items": alternatives}))
}

fn encode(sort: &str, values: &[Value]) -> String {
    serde_json::to_string(&json!([sort, values])).unwrap_or_default().bytes().map(|b| format!("{b:02x}")).collect()
}

fn decode(cursor: &str) -> Option<(String, Vec<Value>)> {
    if !cursor.len().is_multiple_of(2) {
        return None;
    }
    let bytes: Option<Vec<u8>> = (0..cursor.len()).step_by(2).map(|i| u8::from_str_radix(cursor.get(i..i + 2)?, 16).ok()).collect();
    let Value::Array(parts) = serde_json::from_slice(&bytes?).ok()? else { return None };
    let [Value::String(sort), Value::Array(values)] = <[Value; 2]>::try_from(parts).ok()? else { return None };
    Some((sort, values))
}

/// `%`, `_` and `\` in the text match themselves.
fn escape_like(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

enum Search {
    All,
    /// No row can match: the text fits no search field.
    Nothing,
    Filter(Value),
}

/// The filter of the search text: one alternative for each search field that the text fits.
fn search_filter(site: &Site, resource: &Resource, q: &str, binds: &mut Binds) -> Result<Search, ListError> {
    let q = q.trim();
    if q.is_empty() {
        return Ok(Search::All);
    }
    if q.chars().count() > crate::site::MAX_SEARCH_LEN {
        return Err(ListError::Bad(format!("the search text has at most {} characters", crate::site::MAX_SEARCH_LEN)));
    }
    if resource.search.is_empty() {
        return Err(ListError::Bad("this list has no search".into()));
    }
    let (forced, term) = match q.chars().next() {
        Some('=') => (Some(Mode::Exact), &q[1..]),
        Some('^') => (Some(Mode::Prefix), &q[1..]),
        Some('@') => (Some(Mode::Fulltext), &q[1..]),
        _ => (None, q),
    };
    let term = term.trim();
    if term.is_empty() {
        return Ok(Search::All);
    }
    let postgres = site.schema.dialect == Dialect::Postgres;
    let mut items: Vec<Value> = Vec::new();
    for f in &resource.search {
        let mode = forced.unwrap_or(f.default);
        let column = col(&f.column);
        if !f.text {
            if mode != Mode::Exact {
                continue;
            }
            let value = match f.column.ty.ty {
                ColType::Int => term.parse::<i32>().map(|n| json!(n)).ok(),
                ColType::BigInt => term.parse::<i64>().ok().map(|_| json!(term)),
                _ => Some(json!(term)),
            };
            let Some(bound) = value.and_then(|v| json::bind(f.column.ty.ty, &v).ok()) else { continue };
            items.push(json!({"t": "cmp", "op": "eq", "l": column, "r": binds.push(bound)}));
            continue;
        }
        match mode {
            Mode::Exact => items.push(json!({"t": "cmp", "op": "eq", "l": column, "r": binds.push(Bind::from(term.to_owned()))})),
            Mode::Prefix => items.push(json!({"t": "like", "item": column, "pattern": binds.push(Bind::from(format!("{}%", escape_like(term))))})),
            Mode::Contains => items.push(json!({"t": "like", "item": column, "pattern": binds.push(Bind::from(format!("%{}%", escape_like(term)))), "ci": true})),
            Mode::Fulltext => {
                let Some(config) = f.fulltext.as_ref().filter(|_| postgres) else { continue };
                let vector = json!({"t": "func", "name": "to_tsvector", "args": [{"t": "text", "value": config}, column]});
                let query = json!({"t": "func", "name": "websearch_to_tsquery", "args": [{"t": "text", "value": config}, {"t": "text", "value": term}]});
                items.push(json!({"t": "cmp", "op": "match", "l": vector, "r": query}));
            }
        }
    }
    if forced == Some(Mode::Fulltext) && items.is_empty() {
        return Err(ListError::Bad("no search field of this list has full-text search".into()));
    }
    let number = term.bytes().all(|b| b.is_ascii_digit()) && matches!(forced, None | Some(Mode::Exact));
    if number && resource.key_columns.len() == 1 {
        let k = &resource.key_columns[0];
        let searched = resource.search.iter().any(|f| f.column.label == k.label && !f.text);
        if !searched && matches!(k.ty.ty, ColType::Int | ColType::BigInt) {
            let value = if k.ty.ty == ColType::Int { term.parse::<i32>().ok().map(|n| json!(n)) } else { term.parse::<i64>().ok().map(|_| json!(term)) };
            if let Some(bound) = value.and_then(|v| json::bind(k.ty.ty, &v).ok()) {
                items.push(json!({"t": "cmp", "op": "eq", "l": col(k), "r": binds.push(bound)}));
            }
        }
    }
    Ok(if items.is_empty() { Search::Nothing } else { Search::Filter(json!({"t": "or", "items": items})) })
}

/// The keys that a `search_with` function found, as a filter.
fn found_filter(resource: &Resource, found: &[Vec<Value>], binds: &mut Binds) -> Result<Value, ListError> {
    let mut alternatives = Vec::new();
    for key in found {
        if key.len() != resource.key_columns.len() {
            return Err(ListError::Engine("a key from the search function does not fit this resource".into()));
        }
        let mut items = Vec::new();
        for (c, v) in resource.key_columns.iter().zip(key) {
            let b = json::bind(c.ty.ty, v).map_err(ListError::Engine)?;
            items.push(json!({"t": "cmp", "op": "eq", "l": col(c), "r": binds.push(b)}));
        }
        alternatives.push(json!({"t": "and", "items": items}));
    }
    Ok(json!({"t": "or", "items": alternatives}))
}

/// The filters of the search of a request. `Nothing`: no row matches.
fn search_filters(site: &Site, resource: &Resource, q: Option<&str>, found: Option<&[Vec<Value>]>, binds: &mut Binds) -> Result<Option<Vec<Value>>, ListError> {
    if let Some(found) = found {
        if found.is_empty() {
            return Ok(None);
        }
        return Ok(Some(vec![found_filter(resource, found, binds)?]));
    }
    match search_filter(site, resource, q.unwrap_or_default(), binds)? {
        Search::All => Ok(Some(vec![])),
        Search::Nothing => Ok(None),
        Search::Filter(f) => Ok(Some(vec![f])),
    }
}

async fn run_select(site: &Site, conn: &dyn Executor, target: Target, op: Value, binds: Binds) -> Result<Box<exec::Selected>, ListError> {
    let op: Operation = serde_json::from_value(op).map_err(|e| ListError::Engine(e.to_string()))?;
    let plan = Planner::plan(&site.schema, target, &op, &binds).map_err(engine)?;
    let Plan::Select(_) = &plan else { return Err(ListError::Engine("a list plans a select".into())) };
    match exec::run(conn, target, plan).await.map_err(engine)? {
        Outcome::Select(selected) => Ok(selected),
        _ => Err(ListError::Engine("a list runs a select".into())),
    }
}

async fn run_count(site: &Site, conn: &dyn Executor, target: Target, op: Value, binds: Binds) -> Result<i64, ListError> {
    let op: Operation = serde_json::from_value(op).map_err(|e| ListError::Engine(e.to_string()))?;
    let plan = Planner::plan(&site.schema, target, &op, &binds).map_err(engine)?;
    match exec::run(conn, target, plan).await.map_err(engine)? {
        Outcome::Count(n) => Ok(n),
        _ => Err(ListError::Engine("a count runs a count".into())),
    }
}

/// The count of the rows that match the search, capped at `COUNT_CAP`, or exact.
pub async fn count(site: &Site, resource: &Resource, conn: &dyn Executor, target: Target, q: Option<&str>, found: Option<&[Vec<Value>]>, exact: bool) -> Result<Total, ListError> {
    let mut binds = Binds::default();
    let Some(filters) = search_filters(site, resource, q, found, &mut binds)? else { return Ok(Total { count: 0, capped: false }) };
    let model = &site.schema.model(resource.model).ir.name;
    let mut op = json!({"op": "count", "model": model, "filters": filters});
    if !exact {
        op["limit"] = json!(COUNT_CAP + 1);
    }
    let n = run_count(site, conn, target, op, binds).await? as usize;
    Ok(if !exact && n > COUNT_CAP { Total { count: COUNT_CAP, capped: true } } else { Total { count: n, capped: false } })
}

/// Whether the table has more than `LARGE_TABLE_ROWS` rows (a cached answer).
async fn is_large(site: &Site, resource: &Resource, conn: &dyn Executor, target: Target) -> Result<bool, ListError> {
    let model = &site.schema.model(resource.model).ir.name;
    let measure = || async {
        let op = json!({"op": "count", "model": model, "limit": LARGE_TABLE_ROWS + 1});
        let n = run_count(site, conn, target, op, Binds::default()).await?;
        Ok(n as usize > LARGE_TABLE_ROWS)
    };
    resource.large(measure).await
}

/// The checks of a request that need no database, so that a bad request never reaches the bridge.
pub fn check_request(resource: &Resource, request: &ListRequest) -> Result<(), ListError> {
    page_limit(resource, request)?;
    effective_order(resource, request.sort.as_deref())?;
    if request.q.as_deref().is_some_and(|q| q.trim().chars().count() > crate::site::MAX_SEARCH_LEN) {
        return Err(ListError::Bad(format!("the search text has at most {} characters", crate::site::MAX_SEARCH_LEN)));
    }
    Ok(())
}

fn page_limit(resource: &Resource, request: &ListRequest) -> Result<usize, ListError> {
    let limit = request.limit.unwrap_or(resource.page_size);
    if limit == 0 || limit > MAX_PAGE_SIZE {
        return Err(ListError::Bad(format!("limit is between 1 and {MAX_PAGE_SIZE}")));
    }
    Ok(limit)
}

pub async fn list(site: &Site, resource: &Resource, conn: &dyn Executor, target: Target, request: &ListRequest) -> Result<Page, ListError> {
    let limit = page_limit(resource, request)?;
    let (order, sort_text) = effective_order(resource, request.sort.as_deref())?;
    if !sort_text.is_empty() && !resource.sortable_explicit {
        let indexed = |o: &OrderColumn| resource.sortable.iter().any(|s| s.column.label == o.column.label && s.indexed);
        let first = order.iter().find(|o| !resource.key_columns.iter().any(|k| k.label == o.column.label) && !indexed(o));
        if let Some(o) = first {
            if is_large(site, resource, conn, target).await? {
                return Err(ListError::Bad(format!("sort: {:?} has no index, and the table has more than {LARGE_TABLE_ROWS} rows", o.column.label)));
            }
        }
    }
    let flip = matches!(request.position, Position::Before(_) | Position::Last);
    let used: Vec<OrderColumn> = order.iter().map(|o| OrderColumn { column: o.column.clone(), desc: o.desc != flip }).collect();
    let positions: Vec<usize> = order.iter().map(|o| resource.columns.iter().position(|c| c.label == o.column.label).unwrap_or(0)).collect();
    let mut binds = Binds::default();
    let mut filters = Vec::new();
    match &request.position {
        Position::After(c) | Position::Before(c) => {
            let (cursor_sort, values) = decode(c).ok_or_else(|| ListError::Bad("the cursor is not valid".into()))?;
            if cursor_sort != sort_text {
                return Err(ListError::Bad("the cursor does not belong to this sort".into()));
            }
            filters.push(after(&used, &values, &mut binds)?);
        }
        _ => {}
    }
    let mut offset = 0;
    if let Position::Page(n) = request.position {
        if n == 0 {
            return Err(ListError::Bad("page starts at 1".into()));
        }
        offset = (n - 1).checked_mul(limit).filter(|o| i64::try_from(*o).is_ok()).ok_or_else(|| ListError::Bad("page is too large".into()))?;
        if offset > 0 {
            let total = count(site, resource, conn, target, request.q.as_deref(), request.found.as_deref(), false).await?;
            if total.capped {
                return Err(ListError::Bad(format!("page needs an exact count: more than {COUNT_CAP} rows match")));
            }
        }
    }
    let Some(search) = search_filters(site, resource, request.q.as_deref(), request.found.as_deref(), &mut binds)? else {
        return Ok(Page { rows: vec![], keys: vec![], next: None, prev: None });
    };
    filters.extend(search);
    let model = &site.schema.model(resource.model).ir.name;
    let mut op = json!({
        "op": "select",
        "model": model,
        "columns": resource.columns.iter().map(|c| json!({"t": "expr", "expr": col(c)})).collect::<Vec<_>>(),
        "order": used.iter().map(|o| json!({"expr": col(&o.column), "desc": o.desc})).collect::<Vec<_>>(),
        "filters": filters,
        "limit": limit + 1,
    });
    if offset > 0 {
        op["offset"] = json!(offset);
    }
    let selected = run_select(site, conn, target, op, binds).await?;
    let types = &selected.plan.types;
    let total = selected.rows.len();
    let more = total > limit;
    let mut rows = Vec::with_capacity(total.min(limit));
    for r in 0..total.min(limit) {
        let mut row = Vec::with_capacity(types.len());
        for (c, ty) in types.iter().enumerate() {
            let cell = selected.rows.cell(r, c, *ty).map_err(|e| ListError::Engine(e.to_string()))?;
            row.push(json::cell(&site.schema, *ty, cell));
        }
        rows.push(Value::Array(row));
    }
    if flip {
        rows.reverse();
    }
    let cursor_of = |row: Option<&Value>| {
        let row = row.and_then(Value::as_array)?;
        let values: Vec<Value> = positions.iter().map(|&p| row[p].clone()).collect();
        Some(encode(&sort_text, &values))
    };
    let (first_cursor, last_cursor) = (cursor_of(rows.first()), cursor_of(rows.last()));
    // A cursor page with no rows (the rows after it were deleted) keeps the way back: the cursor itself.
    let (next, prev) = match &request.position {
        Position::First => (more.then_some(last_cursor).flatten(), None),
        Position::After(c) => (more.then_some(last_cursor).flatten(), first_cursor.or_else(|| Some(c.clone()))),
        Position::Page(n) => (more.then_some(last_cursor).flatten(), if *n > 1 { first_cursor } else { None }),
        Position::Before(c) => (last_cursor.or_else(|| Some(c.clone())), more.then_some(first_cursor).flatten()),
        Position::Last => (None, more.then_some(first_cursor).flatten()),
    };
    let keys = rows.iter().map(|r| key::encode(&resource.key_positions.iter().map(|&p| r[p].clone()).collect::<Vec<_>>())).collect();
    Ok(Page { rows, keys, next, prev })
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
