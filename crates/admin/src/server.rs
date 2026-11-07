//! The axum router. A handler reads the request it is given and nothing else: the user is
//! a request extension that the caller sets, and the path prefix is a config value.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use monorm_core::dialect::Target;
use monorm_engine::db::Executor;
use serde::Deserialize;
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::audit::{self, Entry};
use crate::auth::{self, AuthConfig};
use crate::list::{self, ListError};
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
    let routes = Router::new().route("/resources", get(resources)).route("/resources/{name}", get(rows));
    let extra = auth.map(|auth| auth::routes(auth, site.clone(), conn.clone()));
    let state = Arc::new(State_ { site, conn, target });
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
    let built = Request::builder().method(method).uri(uri).body(axum::body::Body::empty());
    let mut request = match built {
        Ok(r) => r,
        Err(e) => return (400, failure("BadRequest", &e.to_string())),
    };
    if let Some(user) = user {
        request.extensions_mut().insert(User(user));
    }
    let response = match router(site, conn).oneshot(request).await {
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

fn columns(r: &Resource) -> Vec<Value> {
    r.columns.iter().map(|c| json!({"label": c.label, "type": c.ty.ty, "nullable": c.nullable, "array": c.ty.array})).collect()
}

pub(crate) fn describe(site: &Site, r: &Resource) -> Value {
    json!({
        "name": r.name,
        "columns": columns(r),
        "order": r.order.iter().map(|o| json!({"column": o.column.label, "desc": o.desc})).collect::<Vec<_>>(),
        "page_size": r.page_size,
        "permissions": ["read"],
        "table": site.schema.model(r.model).table(),
    })
}

/// The preamble of every route: the user (401), a scope, the grants of the user, and the site gate (403).
async fn authorize(s: &State_, user: Option<Extension<User>>) -> Result<(User, Scoped, Grants), Box<Response>> {
    let user = require_user(user)?;
    let scope = Scoped::begin(s.conn.as_ref()).await.map_err(|e| Box::new(database_error(&e.message)))?;
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
    Json(json!({"ok": true, "resources": meta["resources"], "permissions": meta["permissions"]})).into_response()
}

#[derive(Deserialize)]
struct ListQuery {
    cursor: Option<String>,
    limit: Option<usize>,
}

async fn rows(State(s): Shared, user: Option<Extension<User>>, Path(name): Path<String>, Query(q): Query<ListQuery>) -> Response {
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
    let out = list::list(&s.site, resource, scope.executor(), s.target, q.cursor.as_deref(), q.limit).await;
    match out {
        Ok(page) => {
            if let Err(e) = scope.commit().await {
                return database_error(&e.message);
            }
            Json(json!({"ok": true, "resource": resource.name, "columns": columns(resource), "rows": page.rows, "next": page.next})).into_response()
        }
        Err(ListError::Bad(m)) => {
            scope.rollback().await;
            reject(StatusCode::BAD_REQUEST, "BadRequest", &m)
        }
        Err(ListError::Engine(m)) => {
            scope.rollback().await;
            database_error(&m)
        }
    }
}

fn database_error(message: &str) -> Response {
    let mut response = reject(StatusCode::INTERNAL_SERVER_ERROR, "DatabaseError", message);
    response.extensions_mut().insert(ErrorNote(message.to_string()));
    response
}
