//! The axum router. A handler reads the request it is given and nothing else: the user is
//! a request extension that the caller sets, and the path prefix is a config value.

use std::sync::Arc;

use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::{Request, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use monorm_core::dialect::{Dialect, Target};
use monorm_engine::db::Executor;
use serde::Deserialize;
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::audit::{self, Entry};
use crate::auth::{self, AuthConfig};
use crate::bridge::{CallContext, HostBridge};
use crate::key;
use crate::list::{self, FilterClause, FilterOp, ListError, ListRequest, Page, Position, RowScope};
use crate::permissions::{self, Grants, Op};
use crate::scope::Scoped;
use crate::service::ErrorNote;
use crate::site::{Related, Resource, Site, Slot, Source};

/// The user of a request: a JSON object with `key`, `is_superuser` and, when it is false, `is_active`.
#[derive(Clone, Debug)]
pub struct User(pub Value);

struct State_ {
    site: Arc<Site>,
    conn: Arc<dyn Executor>,
    target: Target,
    bridge: Option<(Arc<dyn HostBridge>, AuthConfig)>,
}

type Shared = State<Arc<State_>>;

/// The admin routes without a fallback, so a caller can merge other routes first.
pub fn api(site: Arc<Site>, conn: Arc<dyn Executor>) -> Router {
    api_with(site, conn, None)
}

/// `api`, plus the login, logout, session and password routes when `auth` is set.
pub fn api_with(site: Arc<Site>, conn: Arc<dyn Executor>, auth: Option<AuthConfig>) -> Router {
    let target = Target::new(conn.dialect());
    let prefix = site.prefix.clone();
    let routes = Router::new().route("/resources", get(resources)).route("/resources/{name}", get(rows)).route("/resources/{name}/{key}", get(detail)).route("/resources/{name}/{key}/{rel}", get(tab_rows)).route("/counts/{name}", get(counts)).route("/counts/{name}/{key}/{rel}", get(tab_counts));
    let bridge = auth.clone().map(|a| (a.bridge.clone(), a));
    let extra = auth.map(|auth| auth::routes(auth, site.clone(), conn.clone()));
    let state = Arc::new(State_ { site, conn, target, bridge });
    let mut routes = routes.with_state(state);
    if let Some(extra) = extra {
        routes = routes.merge(extra);
    }
    if prefix.is_empty() { routes } else { Router::new().nest(&prefix, routes) }
}

pub fn router(site: Arc<Site>, conn: Arc<dyn Executor>) -> Router {
    api(site, conn).fallback(not_found)
}

pub async fn not_found() -> Response {
    reject(StatusCode::NOT_FOUND, "NotFound", "no such path")
}

/// Sends one request to the router in memory. `user` is the JSON of the user, or none.
pub async fn call(site: Arc<Site>, conn: Arc<dyn Executor>, method: &str, uri: &str, user: Option<Value>) -> (u16, String) {
    call_with(site, conn, method, uri, user, None).await
}

/// `call` with the host bridge of `auth`, so a `search_with` function runs. No login runs: `user` is the user.
pub async fn call_with(site: Arc<Site>, conn: Arc<dyn Executor>, method: &str, uri: &str, user: Option<Value>, auth: Option<AuthConfig>) -> (u16, String) {
    let built = Request::builder().method(method).uri(uri).body(axum::body::Body::empty());
    let mut request = match built {
        Ok(r) => r,
        Err(e) => return (400, failure("BadRequest", &e.to_string())),
    };
    if let Some(user) = user {
        request.extensions_mut().insert(User(user));
    }
    let response = match api_with(site, conn, auth).fallback(not_found).oneshot(request).await {
        Ok(r) => r,
        Err(never) => match never {},
    };
    let status = response.status().as_u16();
    let bytes = http_body_util::BodyExt::collect(response.into_body()).await.map(|b| b.to_bytes()).unwrap_or_default();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn failure(kind: &str, message: &str) -> String {
    json!({"ok": false, "error": {"kind": kind, "message": message}}).to_string()
}

pub(crate) fn reject(status: StatusCode, kind: &str, message: &str) -> Response {
    (status, Json(json!({"ok": false, "error": {"kind": kind, "message": message}}))).into_response()
}

/// 401 for no user: before any lookup, so a path leaks nothing.
fn require_user(user: Option<Extension<User>>) -> Result<User, Box<Response>> {
    user.map(|Extension(u)| u).ok_or_else(|| Box::new(reject(StatusCode::UNAUTHORIZED, "Unauthorized", "sign in")))
}

/// 403, and one audit row that commits with the request. `changes` says why: `needs` (the names of
/// which one is enough) or `reason`. A failed audit write does not change the answer.
async fn deny(site: &Site, scope: Scoped, user: &User, resource: &str, changes: Value) -> Response {
    let key = match &user.0["key"] {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    };
    let entry = Entry { user_key: &key, action: "permission_denied", resource, record_key: "", record_label: "", changes: Some(changes) };
    match audit::record(site, scope.executor(), entry).await {
        Ok(()) => {
            let _ = scope.commit().await;
        }
        Err(_) => scope.rollback().await,
    }
    reject(StatusCode::FORBIDDEN, "Forbidden", "you do not have the permission for this")
}

/// The preamble of every route: the user (401), a scope, the grants of the user and the site gate (403).
async fn authorize(s: &State_, user: Option<Extension<User>>) -> Result<(User, Scoped, Grants), Box<Response>> {
    let user = require_user(user)?;
    let scope = Scoped::begin(s.conn.as_ref()).await.map_err(|e| Box::new(database_error(&e.message)))?;
    // SQLite has no statement timeout: the request timeout of the service covers it.
    if let (Some(limit), Dialect::Postgres) = (s.site.statement_timeout, s.conn.dialect()) {
        if let Err(e) = scope.executor().execute(format!("SET LOCAL statement_timeout = {}", limit.as_millis()), vec![]).await {
            scope.rollback().await;
            return Err(Box::new(database_error(&e.message)));
        }
    }
    let grants = match permissions::load(&s.site, scope.executor(), &user.0).await {
        Ok(g) => g,
        Err(m) => {
            scope.rollback().await;
            return Err(Box::new(database_error(&m)));
        }
    };
    if !s.site.gate(&grants) {
        return Err(Box::new(deny(&s.site, scope, &user, "", json!({"needs": [permissions::GATE]})).await));
    }
    Ok((user, scope, grants))
}

/// The user key as the host functions and the `user` slots see it.
fn user_key(user: &User) -> Value {
    user.0["key"].clone()
}

/// What one request reads: the row scope of the resource.
struct Reach {
    rows: RowScope,
}

/// A JSON value from a slot as a bound value.
fn slot_bind(v: &Value) -> Result<sea_query::Value, ListError> {
    match v {
        Value::Bool(b) => Ok(sea_query::Value::Bool(Some(*b))),
        Value::Number(n) => match n.as_i64() {
            Some(i) => Ok(sea_query::Value::BigInt(Some(i))),
            None => n.as_f64().map(|f| sea_query::Value::Double(Some(f))).ok_or_else(|| ListError::Bad(format!("{n} does not fit a slot"))),
        },
        Value::String(t) => Ok(sea_query::Value::String(Some(t.clone()))),
        other => Err(ListError::Bad(format!("{other} does not fit a slot"))),
    }
}

/// The names in `without=` (comma-separated, repeatable).
fn without_of(uri: &Uri) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (k, v) in form_urlencoded::parse(uri.query().unwrap_or_default().as_bytes()) {
        if k == "without" {
            for name in v.split(',').map(str::trim).filter(|n| !n.is_empty()) {
                if !out.iter().any(|o| o == name) {
                    out.push(name.to_owned());
                }
            }
        }
    }
    out
}

/// The row scope of one request on `resource`: the `queryset` filter (never bypassed), the read defaults that read a scope value
/// (always off: the admin sets no scope value) and the read defaults that the user may switch off (`names`).
async fn reach(s: &State_, resource: &Resource, user: &User, grants: &Grants, names: Vec<String>, scope: Scoped) -> Result<(Scoped, Reach), Box<Response>> {
    for n in &names {
        if !resource.bypass.contains(n) {
            scope.rollback().await;
            return Err(Box::new(reject(StatusCode::BAD_REQUEST, "BadRequest", &format!("{:?} has no read default {n:?}", resource.name))));
        }
    }
    let allowed = s.site.bypassable(grants, resource);
    if let Some(n) = names.iter().find(|n| !allowed.contains(n)) {
        return Err(Box::new(deny(&s.site, scope, user, &resource.name, json!({"needs": [permissions::bypass_name(resource, n)]})).await));
    }
    let mut without = resource.scope_defaults.clone();
    for n in names {
        if !without.contains(&n) {
            without.push(n);
        }
    }
    let mut rows = RowScope { without, ..RowScope::default() };
    if let Some(q) = &resource.queryset {
        let built = queryset_rows(s, resource, q, user, &scope, &mut rows).await;
        if let Err(e) = built {
            scope.rollback().await;
            return Err(Box::new(error_response(e)));
        }
    }
    Ok((scope, Reach { rows }))
}

async fn queryset_rows(s: &State_, resource: &Resource, q: &crate::site::Queryset, user: &User, scope: &Scoped, rows: &mut RowScope) -> Result<(), ListError> {
    if let Some(filter) = &q.filter {
        for slot in &q.slots {
            rows.values.push(match slot {
                Slot::User => slot_bind(&user_key(user))?,
            });
        }
        rows.filters.push(filter.clone());
    }
    if let Some(function) = &q.function {
        let Some((bridge, auth)) = &s.bridge else { return Err(ListError::Engine("a queryset function needs the host bridge".into())) };
        let ctx = CallContext { request_id: auth.security.call_id(), tenant: None, scope: json!({}), tx: Some(scope.transaction()) };
        let payload = json!({"resource": resource.name, "user": user_key(user)});
        let answer = bridge.call(function, payload, ctx).await.map_err(|e| ListError::Engine(format!("{}: {}", e.kind(), e.message())))?;
        let filter = answer.get("filter").filter(|f| f.is_object()).cloned().ok_or_else(|| ListError::Engine("the queryset function returns {\"filter\": expression}".into()))?;
        if crate::site::max_param(&filter).is_some() || crate::site::has_scope(&filter) {
            return Err(ListError::Engine("the queryset function returns a filter with values inline: no params and no scope".into()));
        }
        rows.filters.push(filter);
    }
    Ok(())
}

async fn resources(State(s): Shared, user: Option<Extension<User>>) -> Response {
    let (_, scope, grants) = match authorize(&s, user).await {
        Ok(t) => t,
        Err(r) => return *r,
    };
    if let Err(e) = scope.commit().await {
        return database_error(&e.message);
    }
    let meta = s.site.metadata(&grants);
    let visible: Vec<&str> = meta["resources"].as_array().map(|a| a.iter().filter_map(|r| r["name"].as_str()).collect()).unwrap_or_default();
    Json(json!({"ok": true, "brand": s.site.brand, "nav": s.site.nav(&visible), "resources": meta["resources"], "permissions": meta["permissions"]})).into_response()
}

#[derive(Deserialize)]
struct ListQuery {
    cursor: Option<String>,
    before: Option<String>,
    last: Option<String>,
    page: Option<usize>,
    limit: Option<usize>,
    sort: Option<String>,
    q: Option<String>,
    exact: Option<String>,
}

fn flag(v: &Option<String>) -> bool {
    matches!(v.as_deref(), Some("1" | "true"))
}

/// The preamble of a read route: authorize, find the resource, check the read permission.
async fn open_resource<'a>(s: &'a State_, user: Option<Extension<User>>, name: &str, uri: &Uri) -> Result<(Scoped, &'a Resource, Reach), Box<Response>> {
    let (user, scope, grants) = authorize(s, user).await?;
    let Some(resource) = s.site.resource(name) else {
        scope.rollback().await;
        return Err(Box::new(reject(StatusCode::NOT_FOUND, "NotFound", &format!("no resource {name:?}"))));
    };
    if !resource.allow.contains(&Op::Read) {
        return Err(Box::new(deny(&s.site, scope, &user, &resource.name, json!({"reason": "allow"})).await));
    }
    if !s.site.allows(&grants, resource, Op::Read) {
        return Err(Box::new(deny(&s.site, scope, &user, &resource.name, json!({"needs": s.site.requires(resource, Op::Read)})).await));
    }
    let (scope, reach) = reach(s, resource, &user, &grants, without_of(uri), scope).await?;
    Ok((scope, resource, reach))
}

/// The most keys that a search function may return.
const FOUND_KEYS: usize = 1000;

/// The keys that the `search_with` function of the resource finds for the text, or none when the resource searches its own fields.
async fn found_keys(s: &State_, resource: &Resource, scope: &Scoped, q: Option<&str>) -> Result<Option<Vec<Vec<Value>>>, ListError> {
    let (Some(name), Some(text)) = (resource.search_with.as_deref(), q.map(str::trim).filter(|t| !t.is_empty())) else { return Ok(None) };
    let Some((bridge, auth)) = &s.bridge else { return Err(ListError::Engine("search_with needs the host bridge".into())) };
    let ctx = CallContext { request_id: auth.security.call_id(), tenant: None, scope: json!({}), tx: Some(scope.transaction()) };
    let answer = bridge.call(name, json!({"q": text, "limit": FOUND_KEYS}), ctx).await.map_err(|e| ListError::Engine(format!("{}: {}", e.kind(), e.message())))?;
    let Value::Array(items) = answer else { return Err(ListError::Engine("the search function returns a list of keys".into())) };
    if items.len() > FOUND_KEYS {
        return Err(ListError::Engine(format!("the search function returns at most {FOUND_KEYS} keys")));
    }
    let width = resource.key_columns.len();
    let keys = items
        .into_iter()
        .map(|item| match item {
            Value::Array(parts) => parts,
            single => vec![single],
        })
        .collect::<Vec<_>>();
    if keys.iter().any(|k| k.len() != width) {
        return Err(ListError::Engine("a key from the search function does not fit the resource".into()));
    }
    Ok(Some(keys))
}

async fn finish<T>(scope: Scoped, out: Result<T, ListError>, ok: impl FnOnce(T) -> Response) -> Response {
    match out {
        Ok(value) => {
            if let Err(e) = scope.commit().await {
                return database_error(&e.message);
            }
            ok(value)
        }
        Err(e) => {
            scope.rollback().await;
            error_response(e)
        }
    }
}

fn error_response(e: ListError) -> Response {
    match e {
        ListError::Bad(m) => reject(StatusCode::BAD_REQUEST, "BadRequest", &m),
        ListError::Invalid(m) => reject(StatusCode::UNPROCESSABLE_ENTITY, "Invalid", &m),
        ListError::Engine(m) => database_error(&m),
        ListError::Timeout(m) => timeout_error(m),
    }
}

fn timeout_error(note: String) -> Response {
    let mut response = reject(StatusCode::GATEWAY_TIMEOUT, "Timeout", "the query took too long: narrow the search or the sort");
    response.extensions_mut().insert(ErrorNote(note));
    response
}

/// The query of a list route, once the user may read the resource: a malformed value is 400 in the usual envelope, after the 401, 404 and 403.
async fn query(scope: Scoped, q: Result<Query<ListQuery>, QueryRejection>) -> Result<(Scoped, ListQuery), Response> {
    match q {
        Ok(Query(q)) => Ok((scope, q)),
        Err(e) => {
            scope.rollback().await;
            Err(reject(StatusCode::BAD_REQUEST, "BadRequest", &e.body_text()))
        }
    }
}

/// The filter clauses and the named-filter choices of a query string.
type Filters = (Vec<FilterClause>, Vec<(String, String)>);

/// The filter bar of a request: `f.<name>=v` (repeated for several values), `f.<name>:gte=v` and `n.<name>=<choice>`.
fn filters_of(uri: &Uri) -> Result<Filters, ListError> {
    let mut clauses: Vec<FilterClause> = Vec::new();
    let mut named: Vec<(String, String)> = Vec::new();
    for (key, value) in form_urlencoded::parse(uri.query().unwrap_or_default().as_bytes()) {
        if let Some(rest) = key.strip_prefix("f.") {
            let (name, op) = match rest.rsplit_once(':') {
                Some((name, op)) => (name, FilterOp::parse(op).ok_or_else(|| ListError::Bad(format!("filter: {op:?} is not an operator (gt, gte, lt, lte, empty, notempty)")))?),
                None => (rest, FilterOp::Eq),
            };
            let value = matches!(op, FilterOp::Eq | FilterOp::Gt | FilterOp::Gte | FilterOp::Lt | FilterOp::Lte).then(|| value.into_owned());
            match clauses.iter_mut().find(|c| c.name == name && c.op == op) {
                Some(c) => c.values.extend(value),
                None => clauses.push(FilterClause { name: name.to_owned(), op, values: value.into_iter().collect() }),
            }
        } else if let Some(name) = key.strip_prefix("n.") {
            if named.iter().any(|(n, _)| n == name) {
                return Err(ListError::Bad(format!("filter: {name:?} has two choices")));
            }
            named.push((name.to_owned(), value.into_owned()));
        }
    }
    Ok((clauses, named))
}

fn position_of(q: &ListQuery) -> Option<Position> {
    match (&q.cursor, &q.before, flag(&q.last), q.page) {
        (None, None, false, None) => Some(Position::First),
        (Some(c), None, false, None) => Some(Position::After(c.clone())),
        (None, Some(c), false, None) => Some(Position::Before(c.clone())),
        (None, None, true, None) => Some(Position::Last),
        (None, None, false, Some(n)) => Some(Position::Page(n)),
        _ => None,
    }
}

/// A value that a host function returned for a computed column, in the protocol encoding of its type.
fn batch_value(c: &crate::site::Column, v: Value) -> Result<Value, ListError> {
    let bad = |m: String| ListError::Engine(format!("the function of column {:?} returned a value that does not fit: {m}", c.label));
    if v.is_null() {
        return if c.nullable { Ok(v) } else { Err(bad("null in a column that is not nullable".into())) };
    }
    match c.ty.ty {
        monorm_core::ir::ColType::Json => Ok(v),
        monorm_core::ir::ColType::Decimal => match &v {
            Value::String(_) => Ok(v),
            Value::Number(n) => Ok(Value::String(n.to_string())),
            _ => Err(bad(format!("{v} is not a decimal"))),
        },
        monorm_core::ir::ColType::BigInt => match crate::json::bind(c.ty.ty, &v).map_err(bad)? {
            sea_query::Value::BigInt(Some(n)) => Ok(Value::String(n.to_string())),
            _ => Err(bad(format!("{v} is not an integer"))),
        },
        ty => crate::json::bind(ty, &v).map(|_| v).map_err(bad),
    }
}

/// One call to each host function of a computed column for the whole page: never one call per row.
async fn fill_batch(s: &State_, resource: &Resource, scope: &Scoped, page: &mut Page) -> Result<(), ListError> {
    if page.batch.is_empty() || page.rows.is_empty() {
        return Ok(());
    }
    let Some((bridge, auth)) = &s.bridge else { return Err(ListError::Engine("a computed column with a function needs the host bridge".into())) };
    let keys: Vec<Value> = page.rows.iter().map(|r| Value::Array(resource.key_positions.iter().map(|&p| r[p].clone()).collect())).collect();
    for &i in &page.batch {
        let c = &resource.columns[i];
        let Source::Batch(name) = &c.source else { continue };
        let ctx = CallContext { request_id: auth.security.call_id(), tenant: None, scope: json!({}), tx: Some(scope.transaction()) };
        let answer = bridge.call(name, json!({"keys": keys, "column": c.label}), ctx).await.map_err(|e| ListError::Engine(format!("{}: {}", e.kind(), e.message())))?;
        let Value::Array(values) = answer else { return Err(ListError::Engine(format!("the function of column {:?} returns a list with one value for each key", c.label))) };
        if values.len() != page.rows.len() {
            return Err(ListError::Engine(format!("the function of column {:?} returned {} values for {} keys", c.label, values.len(), page.rows.len())));
        }
        for (row, v) in page.rows.iter_mut().zip(values) {
            row[i] = batch_value(c, v)?;
        }
    }
    Ok(())
}

async fn run_rows(s: &State_, resource: &Resource, scope: Scoped, reach: Reach, q: ListQuery, uri: &Uri, pins: Vec<(crate::site::Column, Value)>) -> Response {
    let Some(position) = position_of(&q) else {
        scope.rollback().await;
        return reject(StatusCode::BAD_REQUEST, "BadRequest", "use one of cursor, before, last and page");
    };
    let (filters, named) = match filters_of(uri) {
        Ok(f) => f,
        Err(e) => {
            scope.rollback().await;
            return error_response(e);
        }
    };
    let mut request = ListRequest { limit: q.limit, sort: q.sort, q: q.q, filters, named, pins, scope: reach.rows, ..ListRequest::new(position) };
    let out = async {
        list::check_request(&s.site, resource, &request)?;
        request.found = found_keys(s, resource, &scope, request.q.as_deref()).await?;
        let mut page = list::list(&s.site, resource, scope.executor(), s.target, &request).await?;
        fill_batch(s, resource, &scope, &mut page).await?;
        // The function ran once: its keys give the count too, so the count request need not call it again.
        let total = match &request.found {
            Some(found) => {
                let counted = ListRequest { q: None, found: Some(found.clone()), ..request.clone() };
                Some(list::count(&s.site, resource, scope.executor(), s.target, &counted, true).await?)
            }
            None => None,
        };
        Ok::<_, ListError>((page, total))
    }
    .await;
    finish(scope, out, |(page, total)| {
        let mut body = json!({"ok": true, "resource": resource.name, "columns": resource.meta["columns"], "rows": page.rows, "flags": page.flags, "keys": page.keys, "next": page.next, "prev": page.prev});
        if let Some(total) = total {
            body["count"] = json!(total.count);
            body["capped"] = json!(total.capped);
        }
        Json(body).into_response()
    })
    .await
}

async fn run_counts(s: &State_, resource: &Resource, scope: Scoped, reach: Reach, q: ListQuery, uri: &Uri, pins: Vec<(crate::site::Column, Value)>) -> Response {
    let (filters, named) = match filters_of(uri) {
        Ok(f) => f,
        Err(e) => {
            scope.rollback().await;
            return error_response(e);
        }
    };
    let mut request = ListRequest { sort: None, q: q.q, filters, named, pins, scope: reach.rows, ..ListRequest::new(Position::First) };
    let out = async {
        list::check_request(&s.site, resource, &request)?;
        request.found = found_keys(s, resource, &scope, request.q.as_deref()).await?;
        list::count(&s.site, resource, scope.executor(), s.target, &request, flag(&q.exact)).await
    }
    .await;
    finish(scope, out, |total| Json(json!({"ok": true, "resource": resource.name, "count": total.count, "capped": total.capped})).into_response()).await
}

async fn rows(State(s): Shared, user: Option<Extension<User>>, Path(name): Path<String>, uri: Uri, q: Result<Query<ListQuery>, QueryRejection>) -> Response {
    let (scope, resource, reach) = match open_resource(&s, user, &name, &uri).await {
        Ok(t) => t,
        Err(r) => return *r,
    };
    let (scope, q) = match query(scope, q).await {
        Ok(t) => t,
        Err(r) => return r,
    };
    run_rows(&s, resource, scope, reach, q, &uri, Vec::new()).await
}

/// The count of a list, a separate request after the rows: capped at 10,000, or exact when `exact=1`.
async fn counts(State(s): Shared, user: Option<Extension<User>>, Path(name): Path<String>, uri: Uri, q: Result<Query<ListQuery>, QueryRejection>) -> Response {
    let (scope, resource, reach) = match open_resource(&s, user, &name, &uri).await {
        Ok(t) => t,
        Err(r) => return *r,
    };
    let (scope, q) = match query(scope, q).await {
        Ok(t) => t,
        Err(r) => return r,
    };
    run_counts(&s, resource, scope, reach, q, &uri, Vec::new()).await
}

/// The preamble of a tab route: the parent (read permission), the tab (404), the child (read permission),
/// and the pins that fix the child rows to the parent record.
type Tab<'a> = (Scoped, Related<'a>, Vec<(crate::site::Column, Value)>, Reach);

async fn open_tab<'a>(s: &'a State_, user: Option<Extension<User>>, name: &str, raw: &str, rel: &str, uri: &Uri) -> Result<Tab<'a>, Box<Response>> {
    let (user, scope, grants) = authorize(s, user).await?;
    let Some(parent) = s.site.resource(name) else {
        scope.rollback().await;
        return Err(Box::new(reject(StatusCode::NOT_FOUND, "NotFound", &format!("no resource {name:?}"))));
    };
    for r in [Some(parent), s.site.plan_related(parent, rel).map(|r| r.child)].into_iter().flatten() {
        if !r.allow.contains(&Op::Read) {
            return Err(Box::new(deny(&s.site, scope, &user, &r.name, json!({"reason": "allow"})).await));
        }
        if !s.site.allows(&grants, r, Op::Read) {
            return Err(Box::new(deny(&s.site, scope, &user, &r.name, json!({"needs": s.site.requires(r, Op::Read)})).await));
        }
    }
    let Some(related) = s.site.plan_related(parent, rel) else {
        scope.rollback().await;
        return Err(Box::new(reject(StatusCode::NOT_FOUND, "NotFound", &format!("{name:?} has no tab {rel:?}"))));
    };
    let types: Vec<_> = parent.key_columns.iter().map(|c| c.ty.ty).collect();
    let key = match key::decode(&types, raw) {
        Ok(k) => k,
        Err(m) => {
            scope.rollback().await;
            return Err(Box::new(reject(StatusCode::BAD_REQUEST, "BadRequest", &m)));
        }
    };
    // The parent record must be inside the queryset of its resource, with its read defaults on.
    let (scope, parent_reach) = reach(s, parent, &user, &grants, Vec::new(), scope).await?;
    let (scope, child_reach) = reach(s, related.child, &user, &grants, without_of(uri), scope).await?;
    match list::parent_values(&s.site, parent, &related, scope.executor(), s.target, &key, &parent_reach.rows).await {
        Ok(Some(values)) => {
            let pins = related.child_to.iter().cloned().zip(values).collect();
            Ok((scope, related, pins, child_reach))
        }
        Ok(None) => {
            scope.rollback().await;
            Err(Box::new(reject(StatusCode::NOT_FOUND, "NotFound", "no such record")))
        }
        Err(e) => {
            scope.rollback().await;
            Err(Box::new(error_response(e)))
        }
    }
}

async fn tab_rows(State(s): Shared, user: Option<Extension<User>>, Path((name, _, rel)): Path<(String, String, String)>, uri: Uri, q: Result<Query<ListQuery>, QueryRejection>) -> Response {
    let raw = tab_key(&uri);
    let (scope, related, pins, reach) = match open_tab(&s, user, &name, &raw, &rel, &uri).await {
        Ok(t) => t,
        Err(r) => return *r,
    };
    let (scope, q) = match query(scope, q).await {
        Ok(t) => t,
        Err(r) => return r,
    };
    run_rows(&s, related.child, scope, reach, q, &uri, pins).await
}

async fn tab_counts(State(s): Shared, user: Option<Extension<User>>, Path((name, _, rel)): Path<(String, String, String)>, uri: Uri, q: Result<Query<ListQuery>, QueryRejection>) -> Response {
    let raw = tab_key(&uri);
    let (scope, related, pins, reach) = match open_tab(&s, user, &name, &raw, &rel, &uri).await {
        Ok(t) => t,
        Err(r) => return *r,
    };
    let (scope, q) = match query(scope, q).await {
        Ok(t) => t,
        Err(r) => return r,
    };
    run_counts(&s, related.child, scope, reach, q, &uri, pins).await
}

/// The raw key segment of `/resources/<name>/<key>/<rel>`: `Path` decodes it, and then a `,` in a value cannot be told from the separator.
fn tab_key(uri: &Uri) -> String {
    uri.path().rsplit('/').nth(1).unwrap_or_default().to_owned()
}

/// One record: its fields, and a link to each to-one target that the user may read.
async fn detail(State(s): Shared, user: Option<Extension<User>>, Path((name, _)): Path<(String, String)>, uri: Uri) -> Response {
    let (user, scope, grants) = match authorize(&s, user).await {
        Ok(t) => t,
        Err(r) => return *r,
    };
    let Some(resource) = s.site.resource(&name) else {
        scope.rollback().await;
        return reject(StatusCode::NOT_FOUND, "NotFound", &format!("no resource {name:?}"));
    };
    if !resource.allow.contains(&Op::Read) {
        return deny(&s.site, scope, &user, &resource.name, json!({"reason": "allow"})).await;
    }
    if !s.site.allows(&grants, resource, Op::Read) {
        return deny(&s.site, scope, &user, &resource.name, json!({"needs": s.site.requires(resource, Op::Read)})).await;
    }
    // The raw segment: `Path` decodes it, and then a `,` in a value cannot be told from the separator.
    let raw = uri.path().rsplit('/').next().unwrap_or_default();
    let types: Vec<_> = resource.key_columns.iter().map(|c| c.ty.ty).collect();
    let key = match key::decode(&types, raw) {
        Ok(k) => k,
        Err(m) => {
            scope.rollback().await;
            return reject(StatusCode::BAD_REQUEST, "BadRequest", &m);
        }
    };
    let (scope, reach) = match reach(&s, resource, &user, &grants, without_of(&uri), scope).await {
        Ok(t) => t,
        Err(r) => return *r,
    };
    match list::detail(&s.site, resource, scope.executor(), s.target, &key, &reach.rows).await {
        Ok(Some(record)) => {
            let values = record.cells;
            if let Err(e) = scope.commit().await {
                return database_error(&e.message);
            }
            let links: Vec<Value> = resource
                .links
                .iter()
                .filter_map(|l| {
                    let target = s.site.resource(&l.target)?;
                    if !s.site.allows(&grants, target, Op::Read) {
                        return None;
                    }
                    let parts: Vec<Value> = l.from.iter().map(|&p| values[p].clone()).collect();
                    if parts.iter().any(Value::is_null) {
                        return None;
                    }
                    let title = resource.meta["links"].as_array()?.iter().find(|m| m["name"] == l.name.as_str())?["title"].clone();
                    Some(json!({"name": l.name, "title": title, "resource": target.name, "key": key::encode(&parts)}))
                })
                .collect();
            Json(json!({"ok": true, "resource": resource.name, "key": raw, "fields": resource.meta["fields"], "values": values, "flags": record.flags, "links": links})).into_response()
        }
        Ok(None) => {
            scope.rollback().await;
            reject(StatusCode::NOT_FOUND, "NotFound", "no such record")
        }
        Err(e) => {
            scope.rollback().await;
            error_response(e)
        }
    }
}

fn database_error(message: &str) -> Response {
    let mut response = reject(StatusCode::INTERNAL_SERVER_ERROR, "DatabaseError", message);
    response.extensions_mut().insert(ErrorNote(message.to_string()));
    response
}
