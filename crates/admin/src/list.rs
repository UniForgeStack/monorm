//! A keyset page: the planner input, the engine run and the rows as JSON.
//!
//! The list is one statement: a to-one column is a join, so a page costs the same
//! number of statements for one row or a hundred. The order ends with the primary key, so
//! a cursor names exactly one position.

use monorm_core::dialect::{Dialect, Target};
use monorm_core::ir::{ColType, EnumStorage, Operation, ValueType};
use monorm_core::schema::Schema;
use monorm_engine::db::Executor;
use monorm_engine::exec::{self, Outcome};
use monorm_engine::plan::{Plan, Planner};
use monorm_engine::{Error, Params, Result as EngineResult};
use sea_query::Value as Bind;
use serde_json::{json, Value};

use crate::json;
use crate::key;
use crate::site::{Column, Filter, FilterKind, Mode, OrderColumn, Related, Resource, Site, Source, COUNT_CAP, LARGE_TABLE_ROWS, MAX_PAGE_SIZE};

/// Why a list failed: the request is wrong (`Bad`) or a value in it does not fit (`Invalid`),
/// the engine failed, or a statement ran past its time limit.
#[derive(Debug)]
pub enum ListError {
    Bad(String),
    Invalid(String),
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
    /// The columns whose values a host function gives: the row has a null there until the caller fills it.
    pub batch: Vec<usize>,
    /// The `can` rules of each row, as an object: `{"change": true, "delete": false}`.
    pub flags: Vec<Value>,
}

/// What limits the rows of one request beyond the list itself: the `queryset` filter, bound to the
/// values of this request, and the named read defaults that a staff user switches off.
/// Every route that reads rows takes its row limits from this one type.
#[derive(Debug, Clone, Default)]
pub struct RowScope {
    /// Filter expressions: a `param` with index `i` reads `values[i]`.
    pub filters: Vec<Value>,
    pub values: Vec<Bind>,
    /// The named read defaults that the request turns off.
    pub without: Vec<String>,
}

impl RowScope {
    /// The filters, with their parameters moved to the end of `binds`.
    pub(crate) fn push(&self, binds: &mut Binds) -> Vec<Value> {
        let base = binds.0.len();
        binds.0.extend(self.values.iter().cloned());
        self.filters.iter().map(|f| shift(f, base)).collect()
    }

    /// Turns the read defaults of the request off in a select or a count.
    pub(crate) fn apply(&self, op: &mut Value) {
        if !self.without.is_empty() {
            op["without_defaults"] = json!(self.without);
        }
    }
}

/// The expression with the index of every parameter moved up by `base`.
pub(crate) fn shift(expr: &Value, base: usize) -> Value {
    match expr {
        Value::Object(map) => {
            let moved = matches!(map.get("t").and_then(Value::as_str), Some("param" | "in_param"));
            Value::Object(map.iter().map(|(k, v)| (k.clone(), if moved && k == "i" { json!(v.as_u64().map_or(0, |i| i as usize + base)) } else { shift(v, base) })).collect())
        }
        Value::Array(items) => Value::Array(items.iter().map(|v| shift(v, base)).collect()),
        other => other.clone(),
    }
}

/// One record: the cells of the detail fields, and its `can` rules.
pub struct Record {
    pub cells: Vec<Value>,
    pub flags: Value,
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
    /// The filters of the filter bar: all of them hold.
    pub filters: Vec<FilterClause>,
    /// `(filter, choice)` of the named filters. A named filter not in the list takes its default.
    pub named: Vec<(String, String)>,
    /// Fixed equalities, as the JSON of a row cell: the rows of one record's tab.
    pub pins: Vec<(Column, Value)>,
    /// The `queryset` filter and the read defaults that the user switched off.
    pub scope: RowScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterOp {
    Eq,
    Gt,
    Gte,
    Lt,
    Lte,
    Empty,
    NotEmpty,
}

impl FilterOp {
    pub fn parse(text: &str) -> Option<FilterOp> {
        Some(match text {
            "eq" => FilterOp::Eq,
            "gt" => FilterOp::Gt,
            "gte" => FilterOp::Gte,
            "lt" => FilterOp::Lt,
            "lte" => FilterOp::Lte,
            "empty" => FilterOp::Empty,
            "notempty" => FilterOp::NotEmpty,
            _ => return None,
        })
    }
}

/// One condition of the filter bar: `f.status=a&f.status=b` is one clause with two values.
#[derive(Debug, Clone)]
pub struct FilterClause {
    pub name: String,
    pub op: FilterOp,
    pub values: Vec<String>,
}

impl ListRequest {
    pub fn new(position: Position) -> ListRequest {
        ListRequest { position, limit: None, sort: None, q: None, found: None, filters: Vec::new(), named: Vec::new(), pins: Vec::new(), scope: RowScope::default() }
    }
}

pub struct Total {
    pub count: usize,
    /// The count stopped at the cap: the real count is higher.
    pub capped: bool,
}

#[derive(Default)]
pub(crate) struct Binds(pub(crate) Vec<Bind>);

impl Binds {
    pub(crate) fn push(&mut self, v: Bind) -> Value {
        self.0.push(v);
        json!({"t": "param", "i": self.0.len() - 1})
    }
}

impl Params for Binds {
    fn len(&self) -> usize {
        self.0.len()
    }
    fn value(&self, i: usize, ty: Option<ValueType>) -> EngineResult<Bind> {
        let v = self.0.get(i).cloned().ok_or_else(|| Error::query(format!("parameter {i} out of range")))?;
        Ok(match (ty, &v) {
            // A slot value comes from JSON: it has the widest type, and the column decides the real one.
            (Some(t), Bind::BigInt(Some(n))) if !t.array && t.ty == monorm_core::ir::ColType::Int => i32::try_from(*n).map(|n| Bind::Int(Some(n))).unwrap_or(v),
            (Some(t), Bind::String(Some(s))) if !t.array && matches!(t.ty, monorm_core::ir::ColType::Uuid | monorm_core::ir::ColType::Date | monorm_core::ir::ColType::DateTime | monorm_core::ir::ColType::BigInt | monorm_core::ir::ColType::Int) => {
                json::bind(t.ty, &Value::String(s.clone())).unwrap_or(v)
            }
            _ => v,
        })
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

/// A column as an expression: a field, or the expression of a computed column.
pub(crate) fn col(c: &Column) -> Value {
    match &c.source {
        Source::Expr(e) => e.clone(),
        _ => json!({"t": "col", "path": c.path, "name": c.field}),
    }
}

pub(crate) fn engine(e: Error) -> ListError {
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

fn invalid(m: String) -> ListError {
    ListError::Invalid(m)
}

fn parse_error(text: &str, what: &str) -> String {
    format!("{text:?} is not {what}")
}

/// The text of a filter value as a bind value of the column type.
fn filter_bind(schema: &Schema, ty: ValueType, text: &str) -> Result<Bind, String> {
    if let Some(e) = ty.enum_idx.and_then(|i| schema.enums.get(i as usize)) {
        let Some(v) = e.values.iter().find(|v| v.name == text) else { return Err(format!("{text:?} is not a value of {}", e.name)) };
        return match e.storage {
            EnumStorage::Int => v.value.as_i64().and_then(|n| i32::try_from(n).ok()).map(|n| Bind::Int(Some(n))).ok_or_else(|| parse_error(text, "an integer enum value")),
            _ => Ok(Bind::String(Some(v.value.as_str().unwrap_or(&v.name).to_owned()))),
        };
    }
    let t = text.trim();
    Ok(match ty.ty {
        ColType::Int => Bind::Int(Some(t.parse().map_err(|_| parse_error(text, "an integer"))?)),
        ColType::BigInt => Bind::BigInt(Some(t.parse().map_err(|_| parse_error(text, "an integer"))?)),
        ColType::Float => Bind::Double(Some(t.parse().map_err(|_| parse_error(text, "a number"))?)),
        ColType::Bool => Bind::Bool(Some(match t {
            "true" | "1" => true,
            "false" | "0" => false,
            _ => return Err(parse_error(text, "true or false")),
        })),
        ColType::String | ColType::Text => Bind::String(Some(text.to_owned())),
        ColType::DateTime => {
            let at = chrono::DateTime::parse_from_rfc3339(t).ok().or_else(|| {
                let day = chrono::NaiveDate::parse_from_str(t, "%Y-%m-%d").ok()?;
                Some(day.and_hms_opt(0, 0, 0)?.and_utc().fixed_offset())
            });
            Bind::ChronoDateTimeWithTimeZone(Some(at.ok_or_else(|| parse_error(text, "a date and time (2026-01-31T10:00:00Z) or a date (2026-01-31)"))?))
        }
        ColType::Date => Bind::ChronoDate(Some(chrono::NaiveDate::parse_from_str(t, "%Y-%m-%d").map_err(|_| parse_error(text, "a date (2026-01-31)"))?)),
        ColType::Uuid => Bind::Uuid(Some(t.parse().map_err(|_| parse_error(text, "a UUID"))?)),
        ColType::Json | ColType::Decimal => return Err(format!("a filter does not take a {:?} field", ty.ty)),
    })
}

pub fn ranged(ty: ValueType) -> bool {
    !ty.array && ty.enum_idx.is_none() && matches!(ty.ty, ColType::Int | ColType::BigInt | ColType::Float | ColType::DateTime | ColType::Date)
}

pub fn is_text(ty: ValueType) -> bool {
    ty.enum_idx.is_none() && matches!(ty.ty, ColType::String | ColType::Text)
}

fn null_of(item: Value, neg: bool) -> Value {
    json!({"t": "is_null", "item": item, "neg": neg})
}

fn not(item: Value) -> Value {
    json!({"t": "not", "item": item})
}

/// The condition of one clause on a column.
fn field_expr(site: &Site, f: &Filter, c: &Column, clause: &FilterClause, binds: &mut Binds) -> Result<Value, ListError> {
    let item = col(c);
    let one = || match clause.values.as_slice() {
        [v] => Ok(v.as_str()),
        _ => Err(invalid(format!("filter {:?}: {} takes one value", f.name, clause_op(clause.op)))),
    };
    let bind = |binds: &mut Binds, text: &str| filter_bind(&site.schema, c.ty, text).map(|b| binds.push(b)).map_err(|m| invalid(format!("filter {:?}: {m}", f.name)));
    Ok(match clause.op {
        FilterOp::Eq => {
            if clause.values.is_empty() {
                return Err(invalid(format!("filter {:?} has no value", f.name)));
            }
            let mut params = Vec::new();
            for v in &clause.values {
                params.push(bind(binds, v)?);
            }
            if params.len() == 1 {
                json!({"t": "cmp", "op": "eq", "l": item, "r": params[0]})
            } else {
                json!({"t": "in", "item": item, "values": params})
            }
        }
        FilterOp::Gt | FilterOp::Gte | FilterOp::Lt | FilterOp::Lte => {
            if !ranged(c.ty) {
                return Err(invalid(format!("filter {:?}: {} is for numbers and dates", f.name, clause_op(clause.op))));
            }
            let op = match clause.op {
                FilterOp::Gt => "gt",
                FilterOp::Gte => "ge",
                FilterOp::Lt => "lt",
                _ => "le",
            };
            json!({"t": "cmp", "op": op, "l": item, "r": bind(binds, one()?)?})
        }
        FilterOp::Empty | FilterOp::NotEmpty => {
            if !clause.values.is_empty() {
                return Err(invalid(format!("filter {:?}: {} takes no value", f.name, clause_op(clause.op))));
            }
            if !c.nullable && !is_text(c.ty) {
                return Err(invalid(format!("filter {:?}: the field is never empty", f.name)));
            }
            let empty = if is_text(c.ty) {
                json!({"t": "or", "items": [null_of(item.clone(), false), {"t": "cmp", "op": "eq", "l": item, "r": binds.push(Bind::String(Some(String::new())))}]})
            } else {
                null_of(item, false)
            };
            if clause.op == FilterOp::Empty { empty } else { not(empty) }
        }
    })
}

fn clause_op(op: FilterOp) -> &'static str {
    match op {
        FilterOp::Eq => "eq",
        FilterOp::Gt => "gt",
        FilterOp::Gte => "gte",
        FilterOp::Lt => "lt",
        FilterOp::Lte => "lte",
        FilterOp::Empty => "empty",
        FilterOp::NotEmpty => "notempty",
    }
}

/// The condition of one clause on a to-one relation: the value is an encoded key of the target.
fn relation_expr(f: &Filter, from: &[Column], key: &[Column], clause: &FilterClause, binds: &mut Binds) -> Result<Value, ListError> {
    let empty = || json!({"t": "and", "items": from.iter().map(|c| null_of(col(c), false)).collect::<Vec<_>>()});
    match clause.op {
        FilterOp::Eq => {
            if clause.values.is_empty() {
                return Err(invalid(format!("filter {:?} has no value", f.name)));
            }
            let types: Vec<ColType> = key.iter().map(|c| c.ty.ty).collect();
            let mut alternatives = Vec::new();
            for raw in &clause.values {
                let parts = key::decode(&types, raw).map_err(|m| invalid(format!("filter {:?}: {m}", f.name)))?;
                let mut items = Vec::new();
                for ((c, k), v) in from.iter().zip(key).zip(&parts) {
                    let b = json::bind(k.ty.ty, v).map_err(|m| invalid(format!("filter {:?}: {m}", f.name)))?;
                    items.push(json!({"t": "cmp", "op": "eq", "l": col(c), "r": binds.push(b)}));
                }
                alternatives.push(json!({"t": "and", "items": items}));
            }
            Ok(json!({"t": "or", "items": alternatives}))
        }
        FilterOp::Empty | FilterOp::NotEmpty if clause.values.is_empty() => Ok(if clause.op == FilterOp::Empty { empty() } else { not(empty()) }),
        _ => Err(invalid(format!("filter {:?}: a relation filter takes values, or empty or notempty", f.name))),
    }
}

/// The conditions of the filter bar, the named filters (with their defaults) and the pins of a request.
/// No database: a wrong filter fails the request before it runs.
fn filter_exprs(site: &Site, resource: &Resource, request: &ListRequest, binds: &mut Binds) -> Result<Vec<Value>, ListError> {
    let mut out = Vec::new();
    for clause in &request.filters {
        let Some(f) = resource.filters.iter().find(|f| f.name == clause.name) else {
            return Err(ListError::Bad(format!("filter: {:?} is not a filter of this list", clause.name)));
        };
        out.push(match &f.kind {
            FilterKind::Field(c) => field_expr(site, f, c, clause, binds)?,
            FilterKind::Relation { from, key, .. } => relation_expr(f, from, key, clause, binds)?,
        });
    }
    for (name, _) in &request.named {
        if !resource.named_filters.iter().any(|n| n.name == *name) {
            return Err(ListError::Bad(format!("filter: {name:?} is not a named filter of this list")));
        }
    }
    for nf in &resource.named_filters {
        let choice = match request.named.iter().find(|(n, _)| *n == nf.name) {
            Some((_, c)) if c == "all" => continue,
            Some((_, c)) => c.as_str(),
            None => match &nf.default {
                Some(d) => d.as_str(),
                None => continue,
            },
        };
        let Some(c) = nf.choices.iter().find(|c| c.name == choice) else {
            return Err(invalid(format!("filter {:?}: {choice:?} is not a choice", nf.name)));
        };
        out.push(c.expr.clone());
    }
    for (c, v) in &request.pins {
        if v.is_null() {
            // A parent without a value for the key joins no child row.
            out.push(json!({"t": "and", "items": [null_of(col(c), false), null_of(col(c), true)]}));
            continue;
        }
        let b = json::bind(c.ty.ty, v).map_err(ListError::Engine)?;
        out.push(json!({"t": "cmp", "op": "eq", "l": col(c), "r": binds.push(b)}));
    }
    Ok(out)
}

/// Every condition of a request: the search, then the filters. `None`: no row matches.
fn where_filters(site: &Site, resource: &Resource, request: &ListRequest, binds: &mut Binds) -> Result<Option<Vec<Value>>, ListError> {
    let Some(mut filters) = search_filters(site, resource, request.q.as_deref(), request.found.as_deref(), binds)? else { return Ok(None) };
    filters.extend(filter_exprs(site, resource, request, binds)?);
    filters.extend(request.scope.push(binds));
    Ok(Some(filters))
}

pub(crate) async fn run_select(site: &Site, conn: &dyn Executor, target: Target, op: Value, binds: Binds) -> Result<Box<exec::Selected>, ListError> {
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

/// The count of the rows that match the search and the filters of the request, capped at `COUNT_CAP`, or exact.
pub async fn count(site: &Site, resource: &Resource, conn: &dyn Executor, target: Target, request: &ListRequest, exact: bool) -> Result<Total, ListError> {
    let mut binds = Binds::default();
    let Some(filters) = where_filters(site, resource, request, &mut binds)? else { return Ok(Total { count: 0, capped: false }) };
    let model = &site.schema.model(resource.model).ir.name;
    let mut op = json!({"op": "count", "model": model, "filters": filters});
    request.scope.apply(&mut op);
    if !exact {
        op["limit"] = json!(COUNT_CAP + 1);
    }
    let n = run_count(site, conn, target, op, binds).await? as usize;
    Ok(if !exact && n > COUNT_CAP { Total { count: COUNT_CAP, capped: true } } else { Total { count: n, capped: false } })
}

/// Whether the rows that this request may see are more than `LARGE_TABLE_ROWS`.
/// Without a row scope the answer is the table size, and the resource caches it.
/// A scope changes the rows, so a scoped answer is a capped count of those rows and no cache holds it.
async fn is_large(site: &Site, resource: &Resource, conn: &dyn Executor, target: Target, scope: &RowScope) -> Result<bool, ListError> {
    let model = &site.schema.model(resource.model).ir.name;
    let measure = || async {
        let mut binds = Binds::default();
        let mut op = json!({"op": "count", "model": model, "limit": LARGE_TABLE_ROWS + 1});
        if !scope.filters.is_empty() {
            op["filters"] = json!(scope.push(&mut binds));
        }
        scope.apply(&mut op);
        let n = run_count(site, conn, target, op, binds).await?;
        Ok(n as usize > LARGE_TABLE_ROWS)
    };
    if scope.filters.is_empty() && scope.without.is_empty() {
        resource.large(measure).await
    } else {
        measure().await
    }
}

/// The checks of a request that need no database, so that a bad request never reaches the bridge.
pub fn check_request(site: &Site, resource: &Resource, request: &ListRequest) -> Result<(), ListError> {
    page_limit(resource, request)?;
    effective_order(resource, request.sort.as_deref())?;
    if request.q.as_deref().is_some_and(|q| q.trim().chars().count() > crate::site::MAX_SEARCH_LEN) {
        return Err(ListError::Bad(format!("the search text has at most {} characters", crate::site::MAX_SEARCH_LEN)));
    }
    // The values bind to a throwaway list: the check needs the errors only.
    filter_exprs(site, resource, request, &mut Binds::default()).map(|_| ())
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
        if let Some(o) = first.filter(|o| o.column.source == Source::Field) {
            if is_large(site, resource, conn, target, &request.scope).await? {
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
            let total = count(site, resource, conn, target, request, false).await?;
            if total.capped {
                return Err(ListError::Bad(format!("page needs an exact count: more than {COUNT_CAP} rows match")));
            }
        }
    }
    let batch: Vec<usize> = resource.columns.iter().enumerate().filter(|(_, c)| matches!(c.source, Source::Batch(_))).map(|(i, _)| i).collect();
    let Some(search) = where_filters(site, resource, request, &mut binds)? else {
        return Ok(Page { rows: vec![], keys: vec![], next: None, prev: None, batch, flags: vec![] });
    };
    filters.extend(search);
    let model = &site.schema.model(resource.model).ir.name;
    let selected: Vec<usize> = (0..resource.columns.len()).filter(|i| !batch.contains(i)).collect();
    let mut columns: Vec<Value> = selected.iter().map(|&i| json!({"t": "expr", "expr": col(&resource.columns[i])})).collect();
    columns.extend(resource.can.iter().map(|(_, expr)| json!({"t": "expr", "expr": expr})));
    let mut op = json!({
        "op": "select",
        "model": model,
        "columns": columns,
        "order": used.iter().map(|o| json!({"expr": col(&o.column), "desc": o.desc})).collect::<Vec<_>>(),
        "filters": filters,
        "limit": limit + 1,
    });
    request.scope.apply(&mut op);
    if offset > 0 {
        op["offset"] = json!(offset);
    }
    let fetched = run_select(site, conn, target, op, binds).await?;
    let types = &fetched.plan.types;
    let total = fetched.rows.len();
    let more = total > limit;
    let mut rows = Vec::with_capacity(total.min(limit));
    let mut flags = Vec::with_capacity(total.min(limit));
    for r in 0..total.min(limit) {
        let mut row = vec![Value::Null; resource.columns.len()];
        let mut rules = serde_json::Map::new();
        for (c, ty) in types.iter().enumerate() {
            let cell = fetched.rows.cell(r, c, *ty).map_err(|e| ListError::Engine(e.to_string()))?;
            let value = json::cell(&site.schema, *ty, cell);
            match selected.get(c) {
                Some(&at) => row[at] = value,
                None => {
                    rules.insert(resource.can[c - selected.len()].0.clone(), json!(value.as_bool().unwrap_or(false)));
                }
            }
        }
        rows.push(Value::Array(row));
        flags.push(Value::Object(rules));
    }
    if flip {
        rows.reverse();
        flags.reverse();
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
    Ok(Page { rows, keys, next, prev, batch, flags })
}

/// The values of the columns that a to-many relation joins on, in the record with this key, or none when no row has the key.
pub async fn parent_values(site: &Site, parent: &Resource, related: &Related<'_>, conn: &dyn Executor, target: Target, key: &[Value], scope: &RowScope) -> Result<Option<Vec<Value>>, ListError> {
    if key.len() != parent.key_columns.len() {
        return Err(ListError::Bad("the key does not fit this resource".into()));
    }
    let mut binds = Binds::default();
    for (c, v) in parent.key_columns.iter().zip(key) {
        binds.0.push(json::bind(c.ty.ty, v).map_err(ListError::Bad)?);
    }
    let items: Vec<Value> = parent.key_columns.iter().enumerate().map(|(i, c)| json!({"t": "cmp", "op": "eq", "l": col(c), "r": {"t": "param", "i": i}})).collect();
    let mut filters = vec![json!({"t": "and", "items": items})];
    filters.extend(scope.push(&mut binds));
    let model = &site.schema.model(parent.model).ir.name;
    let mut op = json!({
        "op": "select",
        "model": model,
        "columns": related.parent_from.iter().map(|c| json!({"t": "expr", "expr": col(c)})).collect::<Vec<_>>(),
        "filters": filters,
        "limit": 1,
    });
    scope.apply(&mut op);
    let selected = run_select(site, conn, target, op, binds).await?;
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

/// The filter that picks the record with this key; its values go to `binds`.
pub(crate) fn key_filter(resource: &Resource, key: &[Value], binds: &mut Binds) -> Result<Value, ListError> {
    let mut items = Vec::new();
    for (c, v) in resource.key_columns.iter().zip(key) {
        let param = binds.push(json::bind(c.ty.ty, v).map_err(ListError::Bad)?);
        items.push(json!({"t": "cmp", "op": "eq", "l": col(c), "r": param}));
    }
    Ok(json!({"t": "and", "items": items}))
}

/// The detail columns of the record with this key, as JSON cells, or none when no row has the key.
pub async fn detail(site: &Site, resource: &Resource, conn: &dyn Executor, target: Target, key: &[Value], scope: &RowScope) -> Result<Option<Record>, ListError> {
    read_record(site, resource, conn, target, key, scope, false).await
}

/// `detail`; with `lock` the select takes the row lock of a write (`FOR UPDATE`, PostgreSQL only: SQLite has no row locks).
pub(crate) async fn read_record(site: &Site, resource: &Resource, conn: &dyn Executor, target: Target, key: &[Value], scope: &RowScope, lock: bool) -> Result<Option<Record>, ListError> {
    if key.len() != resource.key_columns.len() {
        return Err(ListError::Bad("the key does not fit this resource".into()));
    }
    let mut binds = Binds::default();
    let mut filters = vec![key_filter(resource, key, &mut binds)?];
    filters.extend(scope.push(&mut binds));
    let model = &site.schema.model(resource.model).ir.name;
    let mut columns: Vec<Value> = resource.detail.iter().map(|c| json!({"t": "expr", "expr": col(c)})).collect();
    columns.extend(resource.can.iter().map(|(_, expr)| json!({"t": "expr", "expr": expr})));
    let mut op = json!({
        "op": "select",
        "model": model,
        "columns": columns,
        "filters": filters,
        "limit": 1,
    });
    if lock && target.dialect == monorm_core::dialect::Dialect::Postgres {
        op["lock"] = json!({"exclusive": true});
    }
    scope.apply(&mut op);
    let selected = run_select(site, conn, target, op, binds).await?;
    if selected.rows.is_empty() {
        return Ok(None);
    }
    let mut cells = Vec::new();
    let mut flags = serde_json::Map::new();
    for (c, ty) in selected.plan.types.iter().enumerate() {
        let cell = selected.rows.cell(0, c, *ty).map_err(|e| ListError::Engine(e.to_string()))?;
        let value = json::cell(&site.schema, *ty, cell);
        match resource.detail.get(c) {
            Some(_) => cells.push(value),
            None => {
                flags.insert(resource.can[c - resource.detail.len()].0.clone(), json!(value.as_bool().unwrap_or(false)));
            }
        }
    }
    Ok(Some(Record { cells, flags: Value::Object(flags) }))
}
