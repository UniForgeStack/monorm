//! The served router: health routes, one request timeout, one request id and one log line
//! per request. `call` and `test_client` use the bare router and none of this.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use monorm_engine::db::Executor;
use serde_json::{json, Map, Value};

use crate::server;
use crate::site::Site;

pub const REQUEST_ID: &str = "x-request-id";

/// Where a structured log line goes: a JSON object per line.
#[derive(Clone)]
pub struct Logger(Arc<dyn Fn(String) + Send + Sync>);

impl Logger {
    pub fn stderr() -> Self {
        Logger(Arc::new(|line| eprintln!("{line}")))
    }

    pub fn new(sink: impl Fn(String) + Send + Sync + 'static) -> Self {
        Logger(Arc::new(sink))
    }

    pub fn line(&self, level: &str, message: &str, fields: Value) {
        let mut out = Map::new();
        out.insert("ts".into(), chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true).into());
        out.insert("level".into(), level.into());
        out.insert("msg".into(), message.into());
        if let Value::Object(extra) = fields {
            out.extend(extra);
        }
        (self.0)(Value::Object(out).to_string());
    }
}

/// The id of the request, a request extension for handlers.
#[derive(Clone, Debug)]
pub struct RequestId(pub String);

/// A handler sets this on a response that failed inside, so the request line carries the cause.
#[derive(Clone, Debug)]
pub struct ErrorNote(pub String);

#[derive(Clone)]
pub struct Options {
    /// One limit for every request. None: no limit.
    pub timeout: Option<Duration>,
    pub log: Logger,
    /// True once the server stops: `/readyz` then answers 503.
    pub draining: Arc<AtomicBool>,
}

impl Options {
    pub fn new(timeout: Option<Duration>) -> Self {
        Options { timeout, log: Logger::stderr(), draining: Arc::new(AtomicBool::new(false)) }
    }
}

/// Health routes at the root (outside the prefix and before any user check) and the admin routes.
pub fn service(site: Arc<Site>, conn: Arc<dyn Executor>, options: Options) -> Router {
    let health = Router::new()
        .route("/healthz", get(|| async { Json(json!({"ok": true})) }))
        .route("/readyz", get(ready))
        .with_state((conn.clone(), options.draining.clone()));
    let app = health.merge(server::api(site, conn)).fallback(server::not_found);
    let app = match options.timeout {
        Some(limit) => with_timeout(app, limit),
        None => app,
    };
    with_request_log(app, options.log)
}

async fn ready(State((conn, draining)): State<(Arc<dyn Executor>, Arc<AtomicBool>)>) -> Response {
    if draining.load(Ordering::SeqCst) {
        return failure(StatusCode::SERVICE_UNAVAILABLE, "Draining", "the server is stopping");
    }
    match tokio::time::timeout(Duration::from_secs(2), conn.query_text("SELECT 1".into())).await {
        Ok(Ok(_)) => Json(json!({"ok": true})).into_response(),
        Ok(Err(e)) => failure(StatusCode::SERVICE_UNAVAILABLE, "DatabaseError", &e.message),
        Err(_) => failure(StatusCode::SERVICE_UNAVAILABLE, "Timeout", "the database did not answer in 2 s"),
    }
}

fn failure(status: StatusCode, kind: &str, message: &str) -> Response {
    (status, Json(json!({"ok": false, "error": {"kind": kind, "message": message}}))).into_response()
}

/// The one global request timeout. A request that runs past `limit` gets 504 and its future
/// is dropped; a `Scoped` it holds rolls back (see `scope`).
pub fn with_timeout(app: Router, limit: Duration) -> Router {
    app.layer(middleware::from_fn_with_state(limit, |State(limit): State<Duration>, req: Request, next: Next| async move {
        match tokio::time::timeout(limit, next.run(req)).await {
            Ok(response) => response,
            Err(_) => {
                let mut response = failure(StatusCode::GATEWAY_TIMEOUT, "Timeout", "request timeout");
                response.extensions_mut().insert(ErrorNote(format!("no answer in {} ms", limit.as_millis())));
                response
            }
        }
    }))
}

/// A request id (the caller's `x-request-id` when it is a plain token, else a new UUID) on
/// the request, the response and the one log line of the request. Outermost layer.
pub fn with_request_log(app: Router, log: Logger) -> Router {
    app.layer(middleware::from_fn_with_state(log, |State(log): State<Logger>, mut req: Request, next: Next| async move {
        let id = req
            .headers()
            .get(REQUEST_ID)
            .and_then(|v| v.to_str().ok())
            .filter(|v| !v.is_empty() && v.len() <= 128 && v.bytes().all(|b| b.is_ascii_graphic()))
            .map(str::to_string)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        req.extensions_mut().insert(RequestId(id.clone()));
        let method = req.method().to_string();
        let path = req.uri().path().to_string();
        let started = Instant::now();
        let mut response = next.run(req).await;
        if let Ok(value) = HeaderValue::from_str(&id) {
            response.headers_mut().insert(REQUEST_ID, value);
        }
        let status = response.status();
        let mut fields = json!({"request_id": id, "method": method, "path": path, "status": status.as_u16(), "ms": started.elapsed().as_millis() as u64});
        if let Some(ErrorNote(note)) = response.extensions().get::<ErrorNote>() {
            fields["error"] = note.clone().into();
        }
        log.line(if status.is_server_error() { "error" } else { "info" }, "request", fields);
        response
    }))
}
