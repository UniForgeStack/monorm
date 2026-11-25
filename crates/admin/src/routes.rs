//! Custom routes: `admin.route(method, path, fn, permission=...)`. A route lives at `<prefix>/routes/<path>`.
//! Rust runs the session, the CSRF check (the guard) and the named permission, then calls the host function
//! `route.<METHOD>.<path>` over the bridge with the request. The function answers with JSON or a stream.
//!
//! A stream is pulled: Rust calls `route.__next` with the stream id and gets one chunk, so a chunk
//! waits in memory only while the client takes the one before it. `route.__close` ends the stream on
//! every path (done, error, idle timeout, client gone). The host holds the iterator under the id.

use std::collections::BTreeSet;
use std::io;
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{header, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use http_body_util::channel::{Channel, Sender};
use serde_json::{json, Map, Value};

use crate::bridge::CallContext;
use crate::permissions::Strategy;
use crate::server::{authorize, deny, reject, Shared, State_, User};
use crate::service::{ErrorNote, Logger, RequestId};
use crate::error::{code, Fault};
use crate::site::{check_name, Site, err_code};
use crate::write::{meta_of, Meta, MAX_BODY};
use crate::site_config::{RouteConfig};

/// The methods of a custom route.
pub const METHODS: [&str; 5] = ["GET", "POST", "PUT", "PATCH", "DELETE"];
/// The host function that gives the next chunk of a stream, and the one that ends it.
pub const NEXT: &str = "route.__next";
pub const CLOSE: &str = "route.__close";
/// How many chunks wait for the client while the host makes the next one.
const WAITING_CHUNKS: usize = 2;

#[derive(Debug, Clone, PartialEq)]
enum Seg {
    Lit(String),
    Param(String),
}

#[derive(Debug, Clone)]
pub struct Route {
    pub method: String,
    /// The path as declared, without a leading slash: `reports/{id}/summary`.
    pub path: String,
    segments: Vec<Seg>,
    /// The full name `<label>.<name>`; the gate alone when none.
    pub permission: Option<String>,
}

impl Route {
    /// The name of the host function.
    pub fn function(&self) -> String {
        format!("route.{}.{}", self.method, self.path)
    }

    fn literals(&self) -> usize {
        self.segments.iter().filter(|s| matches!(s, Seg::Lit(_))).count()
    }

    fn matches(&self, parts: &[&str]) -> Option<Map<String, Value>> {
        if parts.len() != self.segments.len() {
            return None;
        }
        let mut params = Map::new();
        for (seg, part) in self.segments.iter().zip(parts) {
            match seg {
                Seg::Lit(l) if l == part => {}
                Seg::Lit(_) => return None,
                Seg::Param(name) => {
                    params.insert(name.clone(), Value::String((*part).to_owned()));
                }
            }
        }
        Some(params)
    }

    fn shape(&self) -> String {
        let parts: Vec<&str> = self.segments.iter().map(|s| if let Seg::Lit(l) = s { l.as_str() } else { "{}" }).collect();
        format!("{} {}", self.method, parts.join("/"))
    }
}

fn parse_path(at: &str, path: &str) -> Result<Vec<Seg>, Fault> {
    let path = path.strip_prefix('/').unwrap_or(path);
    if path.is_empty() {
        return err_code(code::ROUTE, format!("{at}: the path is empty"));
    }
    let mut out = Vec::new();
    let mut names = BTreeSet::new();
    for part in path.split('/') {
        if let Some(name) = part.strip_prefix('{').and_then(|p| p.strip_suffix('}')) {
            check_name(&format!("{at}: parameter"), name)?;
            if !names.insert(name) {
                return err_code(code::ROUTE, format!("{at}: parameter {name:?} is used twice"));
            }
            out.push(Seg::Param(name.to_owned()));
        } else if !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'~' | b'-')) && part != "." && part != ".." {
            out.push(Seg::Lit(part.to_owned()));
        } else {
            return err_code(code::ROUTE, format!("{at}: path segment {part:?} must be letters, digits, _ . ~ - or a {{parameter}}"));
        }
    }
    Ok(out)
}

/// Checks the routes of a config. A name that does not belong to a known label is an error, so a typo never opens or closes a route silently.
pub(crate) fn build(configs: &[RouteConfig], site: &Site) -> Result<Vec<Route>, Fault> {
    let labels = site.labels();
    let mut out: Vec<Route> = Vec::new();
    let mut shapes = BTreeSet::new();
    for c in configs {
        let at = format!("route {} {}", c.method, c.path);
        let method = c.method.to_ascii_uppercase();
        if !METHODS.contains(&method.as_str()) {
            return err_code(code::ROUTE, format!("{at}: the method is GET, POST, PUT, PATCH or DELETE"));
        }
        let segments = parse_path(&at, &c.path)?;
        let permission = match &c.permission {
            None => None,
            Some(_) if site.strategy == Strategy::Access => return err_code(code::ROUTE, format!("{at}: permission needs the page or model strategy: the access strategy has no grant to check")),
            Some(full) => {
                let Some((label, name)) = full.split_once('.') else {
                    return err_code(code::ROUTE, format!("{at}: permission {full:?} is not <label>.<name>"));
                };
                if !labels.iter().any(|l| l == label) {
                    return err_code(code::ROUTE, format!("{at}: permission {full:?} has an unknown label {label:?}; known labels: {}", labels.join(", ")));
                }
                check_name(&format!("{at}: permission name"), name)?;
                Some(full.clone())
            }
        };
        let route = Route { method, path: c.path.strip_prefix('/').unwrap_or(&c.path).to_owned(), segments, permission };
        if !shapes.insert(route.shape()) {
            return err_code(code::ROUTE, format!("{at}: another route has the same method and path shape"));
        }
        out.push(route);
    }
    Ok(out)
}

impl Site {
    /// The route of a request: the match with the most literal segments. `Err(allowed)` lists the methods
    /// of the routes that match the path when none matches the method (405), or nothing (404).
    pub fn route_for(&self, method: &str, path: &str) -> Result<(&Route, Map<String, Value>), Vec<&str>> {
        let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
        let mut best: Option<(&Route, Map<String, Value>)> = None;
        let mut allowed = Vec::new();
        for r in &self.routes {
            let Some(params) = r.matches(&parts) else { continue };
            if r.method == method {
                if best.as_ref().is_none_or(|(b, _)| r.literals() > b.literals()) {
                    best = Some((r, params));
                }
            } else if !allowed.contains(&r.method.as_str()) {
                allowed.push(r.method.as_str());
            }
        }
        best.ok_or(allowed)
    }
}

/// The query string as an object: a name that repeats is an array.
fn query_of(raw: &str) -> Value {
    let mut out: Map<String, Value> = Map::new();
    for (k, v) in form_urlencoded::parse(raw.as_bytes()) {
        let v = Value::String(v.into_owned());
        match out.get_mut(k.as_ref()) {
            None => {
                out.insert(k.into_owned(), v);
            }
            Some(Value::Array(a)) => a.push(v),
            Some(old) => *old = Value::Array(vec![old.take(), v]),
        }
    }
    Value::Object(out)
}

/// `/routes/{*path}`: every custom route.
pub(crate) async fn handle(State(s): Shared, user: Option<Extension<User>>, log: Option<Extension<Logger>>, id: Option<Extension<RequestId>>, Path(path): Path<String>, req: axum::extract::Request) -> Response {
    let (user, scope, grants) = match authorize(&s, user).await {
        Ok(t) => t,
        Err(r) => return *r,
    };
    let method = req.method().as_str().to_owned();
    let (route, params) = match s.site.route_for(&method, &path) {
        Ok(t) => t,
        Err(allowed) => {
            scope.rollback().await;
            if allowed.is_empty() {
                return reject(StatusCode::NOT_FOUND, "NotFound", "no such route");
            }
            let mut response = reject(StatusCode::METHOD_NOT_ALLOWED, "MethodNotAllowed", &format!("this route takes {}", allowed.join(", ")));
            if let Ok(v) = allowed.join(", ").parse() {
                response.headers_mut().insert(header::ALLOW, v);
            }
            return response;
        }
    };
    if let Some(name) = &route.permission {
        if !grants.has(name) {
            return deny(&s.site, scope, &user, &format!("route:{}", route.path), json!({"needs": [name]})).await;
        }
    }
    scope.rollback().await;
    let Some((bridge, auth)) = &s.bridge else {
        return reject(StatusCode::FORBIDDEN, "ReadOnly", "this route needs host code, and this admin has no host");
    };
    let (meta, uri) = meta_of(&s, &req);
    let body = if matches!(req.method(), &Method::GET | &Method::HEAD) {
        Value::Null
    } else {
        let bytes = match axum::body::to_bytes(req.into_body(), MAX_BODY).await {
            Ok(b) => b,
            Err(_) => return reject(StatusCode::PAYLOAD_TOO_LARGE, "PayloadTooLarge", &format!("the body is more than {MAX_BODY} bytes")),
        };
        if bytes.is_empty() {
            Value::Null
        } else {
            match serde_json::from_slice(&bytes) {
                Ok(v) => v,
                Err(e) => return reject(StatusCode::BAD_REQUEST, "BadRequest", &format!("the body is not JSON: {e}")),
            }
        }
    };
    let payload = json!({
        "method": route.method,
        "path": route.path,
        "params": params,
        "query": query_of(uri.query().unwrap_or_default()),
        "body": body,
        "user": user.0,
        "request_id": meta.request_id,
        "ip": meta.ip,
        "user_agent": meta.agent,
    });
    let function = route.function();
    let ctx = CallContext { request_id: auth.security.call_id(), scope: json!({}), tx: None };
    let answer = match bridge.call(&function, payload, ctx).await {
        Ok(v) => v,
        Err(e) => return failure(&meta, &format!("{function}: {}: {}", e.kind(), e.message())),
    };
    let stream = answer.get("stream").filter(|v| !v.is_null());
    if let Err(m) = set_audit_keys(&s, &answer).await {
        if let Some(stream) = stream {
            let _ = bridge.call(CLOSE, json!({"stream": stream}), CallContext { request_id: auth.security.call_id(), scope: json!({}), tx: None }).await;
        }
        return failure(&meta, &format!("{function}: the route ran but its audit key failed: {m}"));
    }
    let status = match answer["status"].as_u64() {
        None => StatusCode::OK,
        Some(n) => match u16::try_from(n).ok().and_then(|n| StatusCode::from_u16(n).ok()) {
            Some(s) if (200..600).contains(&s.as_u16()) => s,
            _ => {
                // The host holds the iterator under the id until it gets a close.
                if let Some(stream) = stream {
                    let _ = bridge.call(CLOSE, json!({"stream": stream}), CallContext { request_id: auth.security.call_id(), scope: json!({}), tx: None }).await;
                }
                return failure(&meta, &format!("{function}: status {n} is not an HTTP status"));
            }
        },
    };
    if let Some(stream) = stream {
        let kind = answer["content_type"].as_str().unwrap_or("application/octet-stream");
        let (tx, body) = Channel::<Bytes, io::Error>::new(WAITING_CHUNKS);
        let trace = log.map(|Extension(l)| (l, id.map(|Extension(i)| i.0).unwrap_or_default()));
        tokio::spawn(pump(s.clone(), stream.clone(), function.clone(), tx, trace));
        let mut response = Response::new(Body::new(body));
        *response.status_mut() = status;
        let headers = response.headers_mut();
        headers.insert(header::CONTENT_TYPE, kind.parse().unwrap_or_else(|_| header::HeaderValue::from_static("application/octet-stream")));
        headers.insert(header::CACHE_CONTROL, header::HeaderValue::from_static("no-store"));
        if let Some(v) = answer["filename"].as_str().and_then(|name| crate::files::disposition(name).parse().ok()) {
            headers.insert(header::CONTENT_DISPOSITION, v);
        }
        return response;
    }
    let mut response = Json(answer.get("json").cloned().unwrap_or(Value::Null)).into_response();
    *response.status_mut() = status;
    response.headers_mut().insert(header::CACHE_CONTROL, header::HeaderValue::from_static("no-store"));
    response
}

/// A route writes its `ctx.audit()` rows on the connection of the host, outside any request transaction: Rust sets their key text in a transaction of its own.
async fn set_audit_keys(s: &State_, answer: &Value) -> Result<(), String> {
    if answer.get("audit_keys").and_then(Value::as_array).is_none_or(Vec::is_empty) {
        return Ok(());
    }
    let scope = crate::server::begin_scope(s).await?;
    match crate::audit::set_keys(&s.site, scope.executor(), answer).await {
        Ok(()) => scope.commit().await.map_err(|e| e.message),
        Err(m) => {
            scope.rollback().await;
            Err(m)
        }
    }
}

/// Pulls the chunks and sends them, one at a time. An error or an idle timeout aborts the body, so a cut
/// answer never looks complete; the request log shows the head, so a cut also leaves one `error` line.
async fn pump(s: Arc<State_>, stream: Value, function: String, mut tx: Sender<Bytes, io::Error>, trace: Option<(Logger, String)>) {
    let Some((bridge, auth)) = &s.bridge else { return };
    let idle = s.site.export_idle_timeout;
    let cut = |tx: Sender<Bytes, io::Error>, error: io::Error| {
        if let Some((log, id)) = &trace {
            log.line("error", "route stream cut", json!({"request_id": id, "route": function, "error": error.to_string()}));
        }
        tx.abort(error);
    };
    let ctx = || CallContext { request_id: auth.security.call_id(), scope: json!({}), tx: None };
    loop {
        let pull = bridge.call(NEXT, json!({"stream": stream}), ctx());
        let pulled = match idle {
            Some(limit) => tokio::time::timeout(limit, pull).await.map_err(|_| limit),
            None => Ok(pull.await),
        };
        let chunk = match pulled {
            Err(limit) => {
                cut(tx, io::Error::new(io::ErrorKind::TimedOut, format!("the route made no data for {} ms", limit.as_millis())));
                break;
            }
            Ok(Err(e)) => {
                cut(tx, io::Error::other(format!("{}: {}", e.kind(), e.message())));
                break;
            }
            Ok(Ok(v)) => match v["chunk"].as_str() {
                None => break,
                Some(text) => match B64.decode(text) {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        cut(tx, io::Error::other(format!("a chunk is not base64: {e}")));
                        break;
                    }
                },
            },
        };
        if chunk.is_empty() {
            continue;
        }
        let data = Bytes::from(chunk);
        let sent = match idle {
            Some(limit) => tokio::time::timeout(limit, tx.send_data(data)).await.map_err(|_| limit),
            None => Ok(tx.send_data(data).await),
        };
        match sent {
            Ok(Ok(())) => {}
            // The client is gone: nobody reads an error.
            Ok(Err(_)) => break,
            Err(limit) => {
                cut(tx, io::Error::new(io::ErrorKind::TimedOut, format!("the client took no data for {} ms", limit.as_millis())));
                break;
            }
        }
    }
    let _ = bridge.call(CLOSE, json!({"stream": stream}), ctx()).await;
}

/// A route failed on the host side: the client gets the request id, the cause goes to the log.
fn failure(meta: &Meta, cause: &str) -> Response {
    let id = if meta.request_id.is_empty() { "unknown" } else { meta.request_id.as_str() };
    let mut response = reject(StatusCode::INTERNAL_SERVER_ERROR, "ServerError", &format!("The route failed: an error occurred. Request id: {id}"));
    response.extensions_mut().insert(ErrorNote(cause.to_owned()));
    response
}
