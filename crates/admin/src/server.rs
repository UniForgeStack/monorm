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

use crate::auth::{self, AuthConfig};
use crate::list::{self, ListError};
use crate::scope::Scoped;
use crate::service::ErrorNote;
use crate::site::{Resource, Site};

/// The user of a request: a JSON object. Only `is_superuser: true` gets in for now.
#[derive(Clone, Debug)]
pub struct User(pub Value);

impl User {
    fn is_superuser(&self) -> bool {
        self.0.get("is_superuser").and_then(Value::as_bool) == Some(true)
    }
}

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

/// 401 for no user, 403 for a user who is not a superuser: before any lookup, so a path leaks nothing.
fn check_user(user: Option<&Extension<User>>) -> Option<Response> {
    match user {
        None => Some(reject(StatusCode::UNAUTHORIZED, "Unauthorized", "sign in")),
        Some(u) if !u.0.is_superuser() => Some(reject(StatusCode::FORBIDDEN, "Forbidden", "superusers only")),
        Some(_) => None,
    }
}

fn describe(site: &Site, r: &Resource) -> Value {
    json!({
        "name": r.name,
        "columns": r.columns.iter().map(|c| json!({"label": c.label, "type": c.ty.ty, "nullable": c.nullable, "array": c.ty.array})).collect::<Vec<_>>(),
        "order": r.order.iter().map(|o| json!({"column": o.column.label, "desc": o.desc})).collect::<Vec<_>>(),
        "page_size": r.page_size,
        "permissions": ["read"],
        "table": site.schema.model(r.model).table(),
    })
}

async fn resources(State(s): Shared, user: Option<Extension<User>>) -> Response {
    if let Some(r) = check_user(user.as_ref()) {
        return r;
    }
    let all: Vec<Value> = s.site.resources.iter().map(|r| describe(&s.site, r)).collect();
    Json(json!({"ok": true, "resources": all})).into_response()
}

#[derive(Deserialize)]
struct ListQuery {
    cursor: Option<String>,
    limit: Option<usize>,
}

async fn rows(State(s): Shared, user: Option<Extension<User>>, Path(name): Path<String>, Query(q): Query<ListQuery>) -> Response {
    if let Some(r) = check_user(user.as_ref()) {
        return r;
    }
    let Some(resource) = s.site.resource(&name) else {
        return reject(StatusCode::NOT_FOUND, "NotFound", &format!("no resource {name:?}"));
    };
    let scope = match Scoped::begin(s.conn.as_ref()).await {
        Ok(scope) => scope,
        Err(e) => return database_error(&e.message),
    };
    let out = list::list(&s.site, resource, scope.executor(), s.target, q.cursor.as_deref(), q.limit).await;
    match out {
        Ok(page) => {
            if let Err(e) = scope.commit().await {
                return database_error(&e.message);
            }
            let meta = describe(&s.site, resource);
            Json(json!({"ok": true, "resource": meta["name"], "columns": meta["columns"], "rows": page.rows, "next": page.next})).into_response()
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
