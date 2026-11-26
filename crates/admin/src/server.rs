//! The axum router. A handler reads the request it is given and nothing else: the user is
//! a request extension that the caller sets, and the path prefix is a config value.

use std::sync::Arc;

use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::{Request, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
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
use crate::sight::Sight;
use crate::site::{Related, Resource, Site, Slot, Source};

/// The user of a request: a JSON object with `key`, `is_superuser` and, when it is false, `is_active`.
#[derive(Clone, Debug)]
pub struct User(pub Value);

pub(crate) struct State_ {
    pub(crate) site: Arc<Site>,
    pub(crate) conn: Arc<dyn Executor>,
    pub(crate) target: Target,
    pub(crate) bridge: Option<(Arc<dyn HostBridge>, AuthConfig)>,
}

impl State_ {
    /// The host bridge that converts the cells of a view column; none when the API runs without auth.
    pub(crate) fn viewer(&self) -> Option<&AuthConfig> {
        self.bridge.as_ref().map(|(_, a)| a)
    }
}

pub(crate) type Shared = State<Arc<State_>>;

/// The admin routes without a fallback, so a caller can merge other routes first.
pub fn api(site: Arc<Site>, conn: Arc<dyn Executor>) -> Router {
    api_with(site, conn, None)
}

/// `api`, plus the login, logout, session and password routes when `auth` is set.
pub fn api_with(site: Arc<Site>, conn: Arc<dyn Executor>, auth: Option<AuthConfig>) -> Router {
    let target = Target::new(conn.dialect());
    let prefix = site.prefix.clone();
    let routes = Router::new().route("/resources", get(resources)).route("/resources/{name}", get(rows).post(crate::write::create).patch(crate::listwrite::save)).route("/resources/{name}/{key}", get(detail).patch(crate::write::update).delete(crate::write::destroy)).route("/resources/{name}/{key}/{rel}", get(tab_rows))
        .route("/resources/{name}/{key}/inlines/{inline}", post(crate::inlinewrite::row_create))
        .route("/resources/{name}/{key}/inlines/{inline}/{row}", patch(crate::inlinewrite::row_update).delete(crate::inlinewrite::row_destroy)).route("/forms/{name}", get(form_module)).route("/counts/{name}", get(counts)).route("/dates/{name}", get(dates)).route("/export/{name}", get(crate::export::export)).route("/counts/{name}/{key}/{rel}", get(tab_counts)).route("/autocomplete/{name}/{rel}", get(crate::relations::autocomplete)).route("/links/{name}/{key}/{rel}", get(crate::relations::links)).route("/files/{name}/{key}/{field}", get(crate::files::download).post(crate::files::upload))
        .route("/site", get(resources))
        .route("/embeds/{name}", get(embed))
        .route("/routes/{*path}", axum::routing::any(crate::routes::handle))
        .route("/packages/{pkg}/{*file}", get(crate::packages::serve))
        .route("/actions/{name}/{action}", post(crate::actions::on_list))
        .route("/tab-actions/{name}/{key}/{rel}/{action}", post(crate::actions::on_tab));
    let bridge = auth.clone().map(|a| (a.bridge.clone(), a));
    let extra = auth.map(|auth| auth::routes(auth, site.clone(), conn.clone()));
    let state = Arc::new(State_ { site, conn, target, bridge });
    let mut routes = routes.with_state(state);
    if let Some(extra) = extra {
        routes = routes.merge(extra);
    }
    Router::new().nest(&prefix, routes)
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
    call_body(site, conn, method, uri, user, auth, None).await
}

/// `call_with` with a JSON body.
pub async fn call_body(site: Arc<Site>, conn: Arc<dyn Executor>, method: &str, uri: &str, user: Option<Value>, auth: Option<AuthConfig>, body: Option<&str>) -> (u16, String) {
    let json = body.map(|b| ("application/json", b.as_bytes().to_vec()));
    let (status, bytes) = call_raw(site, conn, method, uri, user, auth, json.as_ref().map(|(t, b)| (*t, b.clone()))).await;
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// `call_with` with a body of any content type (an upload) and the bytes of the answer (a download). No guard runs.
pub async fn call_raw(site: Arc<Site>, conn: Arc<dyn Executor>, method: &str, uri: &str, user: Option<Value>, auth: Option<AuthConfig>, body: Option<(&str, Vec<u8>)>) -> (u16, Vec<u8>) {
    let (status, _, bytes) = call_full(site, conn, method, uri, user, auth, body).await;
    (status, bytes)
}

/// `call_raw` with the headers of the answer.
pub async fn call_full(site: Arc<Site>, conn: Arc<dyn Executor>, method: &str, uri: &str, user: Option<Value>, auth: Option<AuthConfig>, body: Option<(&str, Vec<u8>)>) -> (u16, Vec<(String, String)>, Vec<u8>) {
    let mut built = Request::builder().method(method).uri(uri);
    let bytes = match body {
        Some((kind, bytes)) => {
            built = built.header("content-type", kind);
            bytes
        }
        None => Vec::new(),
    };
    let mut request = match built.body(axum::body::Body::from(bytes)) {
        Ok(r) => r,
        Err(e) => return (400, Vec::new(), failure("BadRequest", &e.to_string()).into_bytes()),
    };
    if let Some(user) = user {
        request.extensions_mut().insert(User(user));
    }
    let response = match api_with(site, conn, auth).fallback(not_found).oneshot(request).await {
        Ok(r) => r,
        Err(never) => match never {},
    };
    let status = response.status().as_u16();
    let headers = response.headers().iter().map(|(k, v)| (k.as_str().to_owned(), String::from_utf8_lossy(v.as_bytes()).into_owned())).collect();
    // A body that fails while it streams (a cut export) is an error here, not an empty answer.
    match http_body_util::BodyExt::collect(response.into_body()).await {
        Ok(b) => (status, headers, b.to_bytes().to_vec()),
        Err(e) => (500, headers, failure("Internal", &e.to_string()).into_bytes()),
    }
}

fn failure(kind: &str, message: &str) -> String {
    json!({"ok": false, "error": {"kind": kind, "message": message}}).to_string()
}

pub(crate) fn reject(status: StatusCode, kind: &str, message: &str) -> Response {
    (status, Json(json!({"ok": false, "error": {"kind": kind, "message": message}}))).into_response()
}

/// 401 for no user: before any lookup, so a path leaks nothing.
pub(crate) fn require_user(user: Option<Extension<User>>) -> Result<User, Box<Response>> {
    user.map(|Extension(u)| u).ok_or_else(|| Box::new(reject(StatusCode::UNAUTHORIZED, "Unauthorized", "sign in")))
}

/// 403, and one audit row that commits with the request. `changes` says why: `needs` (the names of
/// which one is enough) or `reason`. A failed audit write does not change the answer.
pub(crate) async fn deny(site: &Site, scope: Scoped, user: &User, resource: &str, changes: Value) -> Response {
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
    forbidden()
}

fn forbidden() -> Response {
    reject(StatusCode::FORBIDDEN, "Forbidden", "you do not have the permission for this")
}

/// A transaction with the statement timeout of the site set.
pub(crate) async fn begin_scope(s: &State_) -> Result<Scoped, String> {
    let scope = Scoped::begin(s.conn.as_ref()).await.map_err(|e| e.message)?;
    // SQLite has no statement timeout: the request timeout of the service covers the head of a request, and an export bounds each later batch itself.
    if let (Some(limit), Dialect::Postgres) = (s.site.statement_timeout, s.conn.dialect()) {
        if let Err(e) = scope.executor().execute(format!("SET LOCAL statement_timeout = {}", limit.as_millis()), vec![]).await {
            scope.rollback().await;
            return Err(e.message);
        }
    }
    Ok(scope)
}

/// The preamble of every route: the user (401), a scope, the grants of the user and the site gate (403).
pub(crate) async fn authorize(s: &State_, user: Option<Extension<User>>) -> Result<(User, Scoped, Grants), Box<Response>> {
    authorize_with(s, user, true).await
}

async fn authorize_with(s: &State_, user: Option<Extension<User>>, audit_denial: bool) -> Result<(User, Scoped, Grants), Box<Response>> {
    let user = require_user(user)?;
    let scope = begin_scope(s).await.map_err(|m| Box::new(database_error(&m)))?;
    let grants = match permissions::load(&s.site, scope.executor(), &user.0).await {
        Ok(g) => g,
        Err(m) => {
            scope.rollback().await;
            return Err(Box::new(database_error(&m)));
        }
    };
    if !s.site.gate(&grants) {
        if !audit_denial {
            scope.rollback().await;
            return Err(Box::new(forbidden()));
        }
        return Err(Box::new(deny(&s.site, scope, &user, "", json!({"needs": [permissions::GATE]})).await));
    }
    Ok((user, scope, grants))
}

/// The user key as the host functions and the `user` slots see it.
pub(crate) fn user_key(user: &User) -> Value {
    user.0["key"].clone()
}

/// What one request reads: the row scope of the resource.
pub(crate) struct Reach {
    pub(crate) rows: RowScope,
    /// The positions in `Resource::refs` that the user may read: a list shows the label of the related record there.
    pub(crate) labels: Vec<usize>,
    /// The user may update rows of the page: the `change` flag of each row of a list with editable cells includes it.
    pub(crate) change: bool,
}

/// A JSON value from a slot as a bound value.
pub(crate) fn slot_bind(v: &Value) -> Result<sea_query::Value, ListError> {
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
pub(crate) fn without_of(uri: &Uri) -> Vec<String> {
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

/// The row scope of a child page inside an inline: the `queryset` filter and the read defaults that read a scope value.
pub(crate) async fn child_rows(s: &State_, resource: &Resource, user: &User, scope: &Scoped) -> Result<RowScope, ListError> {
    let mut rows = RowScope { without: resource.scope_defaults.clone(), user_key: user_key(user), ..RowScope::default() };
    if let Some(q) = &resource.queryset {
        queryset_rows(s, resource, q, user, scope, &mut rows).await?;
    }
    Ok(rows)
}

/// The row scope of one request on `resource`: the `queryset` filter (never bypassed), the read defaults that read a scope value
/// (always off: the admin sets no scope value) and the read defaults that the user may switch off (`names`).
pub(crate) async fn reach(s: &State_, resource: &Resource, user: &User, grants: &Grants, names: Vec<String>, scope: Scoped) -> Result<(Scoped, Reach), Box<Response>> {
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
    let mut rows = RowScope { without, user_key: user_key(user), ..RowScope::default() };
    if let Some(q) = &resource.queryset {
        let built = queryset_rows(s, resource, q, user, &scope, &mut rows).await;
        if let Err(e) = built {
            scope.rollback().await;
            return Err(Box::new(error_response(e)));
        }
    }
    let labels = (0..resource.refs.len()).filter(|&i| s.site.resources.iter().find(|t| t.name == resource.refs[i].target).is_some_and(|t| s.site.allows(grants, t, Op::Read))).collect();
    let change = !resource.editable.is_empty() && s.site.allows(grants, resource, Op::Update) && crate::write::write_reason(&s.site.schema, resource, Op::Update).is_none() && (s.bridge.is_some() || crate::hostwrite::route(&s.site.schema, resource, Op::Update).is_none());
    Ok((scope, Reach { rows, labels, change }))
}

pub(crate) async fn queryset_rows(s: &State_, resource: &Resource, q: &crate::site::Queryset, user: &User, scope: &Scoped, rows: &mut RowScope) -> Result<(), ListError> {
    if let Some(filter) = &q.filter {
        let base = rows.values.len();
        for slot in &q.slots {
            rows.values.push(match slot {
                Slot::User => slot_bind(&user_key(user))?,
            });
        }
        rows.filters.push(list::shift(filter, base));
    }
    if let Some(function) = &q.function {
        let Some((bridge, auth)) = &s.bridge else { return Err(ListError::Engine("a queryset function needs the host bridge".into())) };
        let ctx = CallContext { request_id: auth.security.call_id(), scope: json!({}), tx: Some(scope.transaction()) };
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
    let mut meta = s.site.metadata(&grants);
    if s.bridge.is_none() {
        close_host_forms(&mut meta);
    }
    let visible: Vec<&str> = meta["resources"].as_array().map(|a| a.iter().filter_map(|r| r["name"].as_str()).collect()).unwrap_or_default();
    Json(json!({"ok": true, "brand": s.site.brand, "nav": s.site.nav(&visible), "resources": meta["resources"], "permissions": meta["permissions"], "admin_major": crate::packages::ADMIN_MAJOR, "packages": s.site.packages.meta(), "frame_cards": s.site.frame_cards})).into_response()
}

/// With no host, a resource whose write needs host code cannot save: say so in its form metadata.
fn close_host_forms(meta: &mut Value) {
    for r in meta["resources"].as_array_mut().into_iter().flatten().filter(|r| r["host_write"] == true && r["writable"] == true) {
        r["writable"] = json!(false);
        r["read_only_reason"] = json!("this write needs host code, and this admin has no host");
    }
}

#[derive(Deserialize)]
pub(crate) struct ListQuery {
    cursor: Option<String>,
    before: Option<String>,
    last: Option<String>,
    page: Option<usize>,
    limit: Option<usize>,
    pub(crate) sort: Option<String>,
    pub(crate) q: Option<String>,
    exact: Option<String>,
    /// `dates` only: the period edges, comma separated.
    edges: Option<String>,
}

fn flag(v: &Option<String>) -> bool {
    matches!(v.as_deref(), Some("1" | "true"))
}

/// The preamble of a read route: authorize, find the resource, check the read permission.
async fn open_resource<'a>(s: &'a State_, user: Option<Extension<User>>, name: &str, uri: &Uri) -> Result<(Scoped, Sight<'a>, Reach), Box<Response>> {
    let (user, scope, grants) = authorize(s, user).await?;
    open_resource_with(s, user, scope, grants, name, uri).await.map(|(scope, resource, reach, _)| (scope, resource, reach))
}

/// `open_resource` for a caller that authorized already and needs the user and the grants after it.
pub(crate) async fn open_resource_with<'a>(s: &'a State_, user: User, scope: Scoped, grants: Grants, name: &str, uri: &Uri) -> Result<(Scoped, Sight<'a>, Reach, (User, Grants)), Box<Response>> {
    let Some(resource) = s.site.sight(name, &grants) else {
        scope.rollback().await;
        return Err(Box::new(reject(StatusCode::NOT_FOUND, "NotFound", &format!("no resource {name:?}"))));
    };
    if !resource.allow.contains(&Op::Read) {
        return Err(Box::new(deny(&s.site, scope, &user, &resource.name, json!({"reason": "allow"})).await));
    }
    if !s.site.allows(&grants, &resource, Op::Read) {
        return Err(Box::new(deny(&s.site, scope, &user, &resource.name, json!({"needs": s.site.requires(&resource, Op::Read)})).await));
    }
    let (scope, reach) = reach(s, &resource, &user, &grants, without_of(uri), scope).await?;
    Ok((scope, resource, reach, (user, grants)))
}

/// The module that replaces the form of a resource, for a user who may read the resource.
async fn form_module(State(s): Shared, user: Option<Extension<User>>, Path(name): Path<String>, req: Request<axum::body::Body>) -> Response {
    let (scope, resource, _) = match open_resource(&s, user, &name, req.uri()).await {
        Ok(x) => x,
        Err(r) => return *r,
    };
    scope.rollback().await;
    match &resource.form.component {
        Some(source) => (StatusCode::OK, [("content-type", "text/javascript; charset=utf-8"), ("cache-control", "no-store")], source.clone()).into_response(),
        None => reject(StatusCode::NOT_FOUND, "NotFound", &format!("{name} has no form module")),
    }
}

/// The most keys that a search function may return.
const FOUND_KEYS: usize = 1000;

/// The keys that the `search_with` function of the resource finds for the text, or none when the resource searches its own fields.
pub(crate) async fn found_keys(s: &State_, resource: &Resource, scope: &Scoped, q: Option<&str>) -> Result<Option<Vec<Vec<Value>>>, ListError> {
    let (Some(name), Some(text)) = (resource.search_with.as_deref(), q.map(str::trim).filter(|t| !t.is_empty())) else { return Ok(None) };
    let Some((bridge, auth)) = &s.bridge else { return Err(ListError::Engine("search_with needs the host bridge".into())) };
    let ctx = CallContext { request_id: auth.security.call_id(), scope: json!({}), tx: Some(scope.transaction()) };
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

pub(crate) async fn finish<T>(scope: Scoped, out: Result<T, ListError>, ok: impl FnOnce(T) -> Response) -> Response {
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

pub(crate) fn error_response(e: ListError) -> Response {
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
pub(crate) async fn query(scope: Scoped, q: Result<Query<ListQuery>, QueryRejection>) -> Result<(Scoped, ListQuery), Response> {
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
pub(crate) fn filters_of(uri: &Uri) -> Result<Filters, ListError> {
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
pub(crate) async fn fill_batch(s: &State_, resource: &Resource, scope: &Scoped, page: &mut Page) -> Result<(), ListError> {
    // Every page of a list or an export passes here: the audit log drops the fields that the reader may not see.
    crate::sight::redact_log(resource, &resource.columns, &mut page.rows);
    if page.batch.is_empty() || page.rows.is_empty() {
        return Ok(());
    }
    let Some((bridge, auth)) = &s.bridge else { return Err(ListError::Engine("a computed column with a function needs the host bridge".into())) };
    let keys: Vec<Value> = page.rows.iter().map(|r| Value::Array(resource.key_positions.iter().map(|&p| r[p].clone()).collect())).collect();
    for &i in &page.batch {
        let c = &resource.columns[i];
        let Source::Batch(name) = &c.source else { continue };
        let ctx = CallContext { request_id: auth.security.call_id(), scope: json!({}), tx: Some(scope.transaction()) };
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
    let change = reach.change;
    let mut request = ListRequest { limit: q.limit, sort: q.sort, q: q.q, filters, named, pins, scope: reach.rows, labels: reach.labels, ..ListRequest::new(position) };
    let out = async {
        list::check_request(&s.site, resource, &request)?;
        request.found = found_keys(s, resource, &scope, request.q.as_deref()).await?;
        let mut page = list::list(&s.site, resource, scope.executor(), s.target, s.viewer(), &request).await?;
        fill_batch(s, resource, &scope, &mut page).await?;
        if !resource.editable.is_empty() {
            for f in page.flags.iter_mut().filter_map(Value::as_object_mut) {
                let rule = f.get("change").and_then(Value::as_bool).unwrap_or(true);
                f.insert("change".into(), json!(rule && change));
            }
        }
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
        let mut body = json!({"ok": true, "resource": resource.name, "columns": resource.meta["columns"], "rows": page.rows, "flags": page.flags, "labels": page.labels, "keys": page.keys, "next": page.next, "prev": page.prev});
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
    run_rows(&s, &resource, scope, reach, q, &uri, Vec::new()).await
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
    run_counts(&s, &resource, scope, reach, q, &uri, Vec::new()).await
}

/// The bar of the date drill-down: the range of the field, or the counts per period, over the rows of the list.
async fn dates(State(s): Shared, user: Option<Extension<User>>, Path(name): Path<String>, uri: Uri, q: Result<Query<ListQuery>, QueryRejection>) -> Response {
    let (user, scope, grants) = match authorize(&s, user).await {
        Ok(t) => t,
        Err(r) => return *r,
    };
    let (scope, resource, reach, (user, _)) = match open_resource_with(&s, user, scope, grants, &name, &uri).await {
        Ok(t) => t,
        Err(r) => return *r,
    };
    let (scope, q) = match query(scope, q).await {
        Ok(t) => t,
        Err(r) => return r,
    };
    let Some(dh) = &resource.date_hierarchy else {
        scope.rollback().await;
        return reject(StatusCode::NOT_FOUND, "NotFound", &format!("{name:?} has no date_hierarchy"));
    };
    // A user view drops the filters of the fields that the user may not see.
    if !resource.filters.iter().any(|f| f.name == dh.field) {
        return deny(&s.site, scope, &user, &resource.name, json!({"reason": "field", "field": dh.field})).await;
    }
    let (filters, named) = match filters_of(&uri) {
        Ok(f) => f,
        Err(e) => {
            scope.rollback().await;
            return error_response(e);
        }
    };
    let edges: Option<Vec<String>> = q.edges.as_deref().map(|e| e.split(',').map(str::to_owned).collect());
    let mut request = ListRequest { sort: None, q: q.q, filters, named, scope: reach.rows, ..ListRequest::new(Position::First) };
    let out = async {
        list::check_request(&s.site, &resource, &request)?;
        request.found = found_keys(&s, &resource, &scope, request.q.as_deref()).await?;
        list::dates(&s.site, &resource, scope.executor(), s.target, &request, edges.as_deref()).await
    }
    .await;
    finish(scope, out, |mut body| {
        body["ok"] = json!(true);
        Json(body).into_response()
    })
    .await
}

/// The preamble of a tab route: the parent (read permission), the tab (404), the child (read permission),
/// and the pins that fix the child rows to the parent record.
pub(crate) type Tab<'a> = (Scoped, Related<'a>, Vec<(crate::site::Column, Value)>, Reach);

async fn open_tab<'a>(s: &'a State_, user: Option<Extension<User>>, name: &str, raw: &str, rel: &str, uri: &Uri) -> Result<Tab<'a>, Box<Response>> {
    let (user, scope, grants) = authorize(s, user).await?;
    open_tab_with(s, &user, scope, &grants, name, raw, rel, uri).await
}

/// `open_tab` for a caller that authorized already.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn open_tab_with<'a>(s: &'a State_, user: &User, scope: Scoped, grants: &Grants, name: &str, raw: &str, rel: &str, uri: &Uri) -> Result<Tab<'a>, Box<Response>> {
    let Some(parent) = s.site.sight(name, grants) else {
        scope.rollback().await;
        return Err(Box::new(reject(StatusCode::NOT_FOUND, "NotFound", &format!("no resource {name:?}"))));
    };
    for r in [Some(parent.clone()), s.site.plan_related(&parent, rel, grants).map(|r| r.child)].into_iter().flatten() {
        if !r.allow.contains(&Op::Read) {
            return Err(Box::new(deny(&s.site, scope, user, &r.name, json!({"reason": "allow"})).await));
        }
        if !s.site.allows(grants, &r, Op::Read) {
            return Err(Box::new(deny(&s.site, scope, user, &r.name, json!({"needs": s.site.requires(&r, Op::Read)})).await));
        }
    }
    let Some(related) = s.site.plan_related(&parent, rel, grants) else {
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
    let (scope, parent_reach) = reach(s, &parent, user, grants, Vec::new(), scope).await?;
    let (scope, child_reach) = reach(s, &related.child, user, grants, without_of(uri), scope).await?;
    match list::parent_values(&s.site, &parent, &related, scope.executor(), s.target, &key, &parent_reach.rows).await {
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
    let raw = segment(&uri, 1).to_owned();
    let (scope, related, pins, reach) = match open_tab(&s, user, &name, &raw, &rel, &uri).await {
        Ok(t) => t,
        Err(r) => return *r,
    };
    let (scope, q) = match query(scope, q).await {
        Ok(t) => t,
        Err(r) => return r,
    };
    run_rows(&s, &related.child, scope, reach, q, &uri, pins).await
}

async fn tab_counts(State(s): Shared, user: Option<Extension<User>>, Path((name, _, rel)): Path<(String, String, String)>, uri: Uri, q: Result<Query<ListQuery>, QueryRejection>) -> Response {
    let raw = segment(&uri, 1).to_owned();
    let (scope, related, pins, reach) = match open_tab(&s, user, &name, &raw, &rel, &uri).await {
        Ok(t) => t,
        Err(r) => return *r,
    };
    let (scope, q) = match query(scope, q).await {
        Ok(t) => t,
        Err(r) => return r,
    };
    run_counts(&s, &related.child, scope, reach, q, &uri, pins).await
}

/// The raw segment of the path, `back` segments from the end. `Path` decodes a segment, and then a `,` in a key value cannot be told from the separator.
pub(crate) fn segment(uri: &Uri, back: usize) -> &str {
    uri.path().rsplit('/').nth(back).unwrap_or_default()
}


/// The rows of each inline of the record that the user may read: the child rows that the record fixes, inside the `queryset` of the inline page.
async fn inline_rows(s: &State_, parent: &Resource, user: &User, grants: &Grants, key: &[Value], parent_reach: &Reach, mut scope: Scoped) -> Result<(Scoped, Value), Box<Response>> {
    let mut out = serde_json::Map::new();
    for inline in &parent.inlines {
        if !s.site.inline_allows(grants, parent, inline, Op::Read) {
            continue;
        }
        let related = s.site.inline_related(inline);
        let values = match list::parent_values(&s.site, parent, &related, scope.executor(), s.target, key, &parent_reach.rows).await {
            Ok(Some(v)) => v,
            Ok(None) => {
                scope.rollback().await;
                return Err(Box::new(reject(StatusCode::NOT_FOUND, "NotFound", "no such record")));
            }
            Err(e) => {
                scope.rollback().await;
                return Err(Box::new(error_response(e)));
            }
        };
        let pins: Vec<_> = inline.child_to.iter().cloned().zip(values).collect();
        let (next, child_reach) = reach(s, &inline.resource, user, grants, Vec::new(), scope).await?;
        scope = next;
        let rows = match list::read_pinned(&s.site, &inline.resource, scope.executor(), s.target, s.viewer(), &pins, &child_reach.rows, inline.max + 1, false).await {
            Ok(r) => r,
            Err(e) => {
                scope.rollback().await;
                return Err(Box::new(error_response(e)));
            }
        };
        let truncated = rows.len() > inline.max;
        let link = inline.link.as_deref().filter(|l| s.site.resource(l).is_some_and(|t| s.site.allows(grants, t, Op::Read)));
        let shown: Vec<Value> = rows.into_iter().take(inline.max).map(|r| json!({"key": r.key, "values": r.cells, "flags": r.flags})).collect();
        out.insert(inline.name.clone(), json!({"rows": shown, "truncated": truncated, "link": link}));
    }
    Ok((scope, Value::Object(out)))
}

const BANNER_LEVELS: &[&str] = &["info", "warning", "danger"];
const BANNERS_MAX: usize = 20;
const BANNER_TEXT_MAX: usize = 500;

fn danger_banner(text: &str) -> Value {
    json!([{"level": "danger", "text": text}])
}

/// The banners of one record: one host call. Any failure, or an answer of the wrong shape, is one danger banner: the page stays whole.
/// An answer over a limit is cut, not refused: the first `BANNERS_MAX` banners, each text at `BANNER_TEXT_MAX` characters.
async fn record_banners(s: &State_, resource: &Resource, scope: &Scoped, values: &[Value]) -> Value {
    let Some(name) = &resource.banners else { return json!([]) };
    let Some((bridge, auth)) = &s.bridge else { return danger_banner("The banners of this record could not load: the host bridge is missing.") };
    let record: serde_json::Map<String, Value> = resource.detail.iter().zip(values).map(|(c, v)| (c.label.clone(), v.clone())).collect();
    let ctx = CallContext { request_id: auth.security.call_id(), scope: json!({}), tx: Some(scope.transaction()) };
    let answer = match bridge.call(name, json!({"record": record}), ctx).await {
        Ok(answer) => answer,
        Err(e) => return danger_banner(&format!("The banners of this record could not load ({}).", e.kind())),
    };
    let wrong_value_banner = || danger_banner("The banners of this record could not load: the function returned a wrong value.");
    let Value::Array(items) = answer else { return wrong_value_banner() };
    let mut out = Vec::with_capacity(items.len().min(BANNERS_MAX));
    for item in items.into_iter().take(BANNERS_MAX) {
        let level = item.get("level").and_then(Value::as_str).filter(|l| BANNER_LEVELS.contains(l));
        let text = item.get("text").and_then(Value::as_str).filter(|t| !t.is_empty());
        let (Some(level), Some(text)) = (level, text) else { return wrong_value_banner() };
        out.push(json!({"level": level, "text": text.chars().take(BANNER_TEXT_MAX).collect::<String>()}));
    }
    Value::Array(out)
}

/// One record: its fields, and a link to each to-one target that the user may read.
async fn detail(State(s): Shared, user: Option<Extension<User>>, Path((name, _)): Path<(String, String)>, uri: Uri) -> Response {
    let (user, scope, grants) = match authorize(&s, user).await {
        Ok(t) => t,
        Err(r) => return *r,
    };
    let Some(resource) = s.site.sight(&name, &grants) else {
        scope.rollback().await;
        return reject(StatusCode::NOT_FOUND, "NotFound", &format!("no resource {name:?}"));
    };
    if !resource.allow.contains(&Op::Read) {
        return deny(&s.site, scope, &user, &resource.name, json!({"reason": "allow"})).await;
    }
    if !s.site.allows(&grants, &resource, Op::Read) {
        return deny(&s.site, scope, &user, &resource.name, json!({"needs": s.site.requires(&resource, Op::Read)})).await;
    }
    let raw = segment(&uri, 0);
    let types: Vec<_> = resource.key_columns.iter().map(|c| c.ty.ty).collect();
    let key = match key::decode(&types, raw) {
        Ok(k) => k,
        Err(m) => {
            scope.rollback().await;
            return reject(StatusCode::BAD_REQUEST, "BadRequest", &m);
        }
    };
    let (scope, reach) = match reach(&s, &resource, &user, &grants, without_of(&uri), scope).await {
        Ok(t) => t,
        Err(r) => return *r,
    };
    let mut projection = resource.detail.clone();
    for column in resource.frames.iter().flat_map(|frame| frame.columns()) {
        if !projection.iter().any(|c| c.label == column.label) && crate::frames::visible(&s.site, &resource, &grants, column) {
            projection.push(column.clone());
        }
    }
    match list::read_projection(&s.site, &resource, scope.executor(), s.target, s.viewer(), list::RecordFilter::Key(&key), &reach.rows, false, &projection, true).await {
        Ok(Some(record)) => {
            let mut values = record.cells;
            crate::sight::redact_row(&resource, &projection, &mut values);
            let frames: Vec<_> = resource.frames.iter().map(|frame| frame.payload(&s.site.base_path, |column| {
                projection.iter().position(|c| c.label == column.label).map(|i| &values[i])
            })).collect();
            values.truncate(resource.detail.len());
            let (scope, inlines) = match inline_rows(&s, &resource, &user, &grants, &key, &reach, scope).await {
                Ok(t) => t,
                Err(r) => return *r,
            };
            let password_target = if resource.set_password && resource.actions.iter().any(|a| a.name == crate::set_password::NAME && s.site.action_allowed(&grants, &resource, a)) {
                match (s.viewer(), key.as_slice()) {
                    (Some(auth), [id]) => match crate::set_password::target(auth, &scope, id).await {
                        Ok(target) if target.is_object() => {
                            let sso_only = auth.security.options.oidc.is_some() && auth::password_hash(&target).is_some_and(|hash| hash.starts_with('!'));
                            Some(json!({"display": target["display"], "sso_only": sso_only}))
                        }
                        Ok(_) => None,
                        Err(e) => {
                            scope.rollback().await;
                            return auth::host_error(&e);
                        }
                    },
                    _ => None,
                }
            } else {
                None
            };
            let banners = record_banners(&s, &resource, &scope, &values).await;
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
            let mut body = json!({"ok": true, "resource": resource.name, "key": raw, "fields": resource.meta["fields"], "values": values, "flags": record.flags, "links": links, "inlines": inlines, "frames": frames, "banners": banners});
            if let Some(target) = password_target {
                body["password_target"] = target;
            }
            Json(body).into_response()
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

/// One scoped record and one read-only detail tab. Embed URLs have no default bypass controls.
async fn embed(State(s): Shared, user: Option<Extension<User>>, Path(name): Path<String>, uri: Uri) -> Response {
    use crate::frames::EmbedTab;

    let (user, scope, grants) = match authorize_with(&s, user, false).await {
        Ok(t) => t,
        Err(r) => return *r,
    };
    let no_record = || reject(StatusCode::NOT_FOUND, "NotFound", "no such record");
    let Some(embed) = s.site.embeds.get(&name) else {
        scope.rollback().await;
        return reject(StatusCode::NOT_FOUND, "NotFound", "no such embed");
    };
    let Some(resource) = s.site.sight(&embed.resource, &grants) else {
        scope.rollback().await;
        return no_record();
    };
    if !resource.allow.contains(&Op::Read) || !s.site.allows(&grants, &resource, Op::Read) {
        scope.rollback().await;
        return forbidden();
    }
    // Looking up a hidden value would let a reader test guesses about it.
    if resource.hidden.contains(&embed.lookup.label) {
        scope.rollback().await;
        return no_record();
    }
    let mut values = form_urlencoded::parse(uri.query().unwrap_or_default().as_bytes())
        .filter_map(|(name, value)| (name == embed.lookup.label).then_some(value));
    let lookup = match (values.next(), values.next()) {
        (Some(value), None) => value,
        _ => {
            scope.rollback().await;
            return reject(StatusCode::BAD_REQUEST, "BadRequest", &format!("supply one {:?} query value", embed.lookup.label));
        }
    };
    let (scope, reach) = match reach(&s, &resource, &user, &grants, Vec::new(), scope).await {
        Ok(t) => t,
        Err(r) => return *r,
    };
    let mut projection = resource.key_columns.clone();
    let key_len = projection.len();
    let (title, fields) = match &embed.tab {
        EmbedTab::Fields(set) => (
            set.title.clone().unwrap_or_else(|| crate::meta::humanize(&set.name)),
            set.fields.iter().filter_map(|name| resource.detail.iter().find(|c| &c.label == name)).cloned().collect::<Vec<_>>(),
        ),
        EmbedTab::Package { title, .. } => (title.clone(), resource.detail.clone()),
    };
    projection.extend(fields.iter().cloned());
    let selected = list::read_projection(&s.site, &resource, scope.executor(), s.target, s.viewer(), list::RecordFilter::Lookup(&embed.lookup, &lookup), &reach.rows, false, &projection, false).await;
    finish(scope, selected, |record| {
        let Some(record) = record else { return no_record() };
        let key = key::encode(&record.cells[..key_len]);
        let mut values = record.cells[key_len..].to_vec();
        crate::sight::redact_row(&resource, &fields, &mut values);
        let metadata: Vec<_> = fields.iter().filter_map(|c| resource.meta["fields"].as_array()?.iter().find(|f| f["label"] == c.label).cloned()).collect();
        let mut tab = json!({"fields": metadata, "values": values});
        if let EmbedTab::Package { name, .. } = &embed.tab {
            tab["package"] = s.site.packages.tab_package(name);
            tab["name"] = json!(name);
            tab["resource"] = json!(resource.name);
        }
        Json(json!({"title": title, "key": key, "tab": tab})).into_response()
    }).await
}

pub(crate) fn database_error(message: &str) -> Response {
    let mut response = reject(StatusCode::INTERNAL_SERVER_ERROR, "DatabaseError", message);
    response.extensions_mut().insert(ErrorNote(message.to_string()));
    response
}
