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
use crate::list::{self, ListError, ListRequest, Position};
use crate::permissions::{self, Grants, Op};
use crate::scope::Scoped;
use crate::service::ErrorNote;
use crate::site::{Resource, Site};

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
    let routes = Router::new().route("/resources", get(resources)).route("/resources/{name}", get(rows)).route("/resources/{name}/{key}", get(detail)).route("/counts/{name}", get(counts));
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

/// The preamble of every route: the user (401), a scope, the grants of the user, and the site gate (403).
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
async fn open_resource<'a>(s: &'a State_, user: Option<Extension<User>>, name: &str) -> Result<(Scoped, &'a Resource), Box<Response>> {
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
    Ok((scope, resource))
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
        Err(ListError::Bad(m)) => {
            scope.rollback().await;
            reject(StatusCode::BAD_REQUEST, "BadRequest", &m)
        }
        Err(ListError::Engine(m)) => {
            scope.rollback().await;
            database_error(&m)
        }
        Err(ListError::Timeout(m)) => {
            scope.rollback().await;
            timeout_error(m)
        }
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

async fn rows(State(s): Shared, user: Option<Extension<User>>, Path(name): Path<String>, q: Result<Query<ListQuery>, QueryRejection>) -> Response {
    let (scope, resource) = match open_resource(&s, user, &name).await {
        Ok(t) => t,
        Err(r) => return *r,
    };
    let (scope, q) = match query(scope, q).await {
        Ok(t) => t,
        Err(r) => return r,
    };
    let position = match (&q.cursor, &q.before, flag(&q.last), q.page) {
        (None, None, false, None) => Position::First,
        (Some(c), None, false, None) => Position::After(c.clone()),
        (None, Some(c), false, None) => Position::Before(c.clone()),
        (None, None, true, None) => Position::Last,
        (None, None, false, Some(n)) => Position::Page(n),
        _ => {
            scope.rollback().await;
            return reject(StatusCode::BAD_REQUEST, "BadRequest", "use one of cursor, before, last and page");
        }
    };
    let mut request = ListRequest { position, limit: q.limit, sort: q.sort, q: q.q, found: None };
    let out = async {
        list::check_request(resource, &request)?;
        request.found = found_keys(&s, resource, &scope, request.q.as_deref()).await?;
        let page = list::list(&s.site, resource, scope.executor(), s.target, &request).await?;
        // The function ran once: its keys give the count too, so the count request need not call it again.
        let total = match &request.found {
            Some(found) => Some(list::count(&s.site, resource, scope.executor(), s.target, None, Some(found), true).await?),
            None => None,
        };
        Ok::<_, ListError>((page, total))
    }
    .await;
    finish(scope, out, |(page, total)| {
        let mut body = json!({"ok": true, "resource": resource.name, "columns": resource.meta["columns"], "rows": page.rows, "keys": page.keys, "next": page.next, "prev": page.prev});
        if let Some(total) = total {
            body["count"] = json!(total.count);
            body["capped"] = json!(total.capped);
        }
        Json(body).into_response()
    })
    .await
}

/// The count of a list, a separate request after the rows: capped at 10,000, or exact when `exact=1`.
async fn counts(State(s): Shared, user: Option<Extension<User>>, Path(name): Path<String>, q: Result<Query<ListQuery>, QueryRejection>) -> Response {
    let (scope, resource) = match open_resource(&s, user, &name).await {
        Ok(t) => t,
        Err(r) => return *r,
    };
    let (scope, q) = match query(scope, q).await {
        Ok(t) => t,
        Err(r) => return r,
    };
    let found = match found_keys(&s, resource, &scope, q.q.as_deref()).await {
        Ok(f) => f,
        Err(e) => return finish(scope, Err::<(), _>(e), |_| unreachable!()).await,
    };
    let out = list::count(&s.site, resource, scope.executor(), s.target, q.q.as_deref(), found.as_deref(), flag(&q.exact)).await;
    finish(scope, out, |total| Json(json!({"ok": true, "resource": resource.name, "count": total.count, "capped": total.capped})).into_response()).await
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
    match list::detail(&s.site, resource, scope.executor(), s.target, &key).await {
        Ok(Some(values)) => {
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
            Json(json!({"ok": true, "resource": resource.name, "key": raw, "fields": resource.meta["fields"], "values": values, "links": links})).into_response()
        }
        Ok(None) => {
            scope.rollback().await;
            reject(StatusCode::NOT_FOUND, "NotFound", "no such record")
        }
        Err(ListError::Bad(m)) => {
            scope.rollback().await;
            reject(StatusCode::BAD_REQUEST, "BadRequest", &m)
        }
        Err(ListError::Engine(m)) => {
            scope.rollback().await;
            database_error(&m)
        }
        Err(ListError::Timeout(m)) => {
            scope.rollback().await;
            timeout_error(m)
        }
    }
}

fn database_error(message: &str) -> Response {
    let mut response = reject(StatusCode::INTERNAL_SERVER_ERROR, "DatabaseError", message);
    response.extensions_mut().insert(ErrorNote(message.to_string()));
    response
}
