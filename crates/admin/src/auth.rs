//! Login, logout, the session and the password change. The password check is the host's: the
//! `auth.authenticate` and `auth.change_password` functions of the auth app, called over the bridge.
//! This module never hashes or compares a password.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use monorm_engine::db::Executor;
use serde_json::{json, Map, Value};

use crate::audit::{self, Entry};
use crate::bridge::{BridgeError, CallContext, HostBridge};
use crate::revoke;
use crate::scope::Scoped;
use crate::security::{self, Security, Session};
use crate::server::{reject, User};
use crate::service::{ErrorNote, RequestId};
use crate::site::Site;

/// The host function that checks a username and a password: `{username, password}` gives the user or null.
pub const AUTHENTICATE: &str = "auth.authenticate";
/// The host function that changes a password: `{key, old_password, new_password}` gives `{changed, reason?}`.
pub const CHANGE_PASSWORD: &str = "auth.change_password";
/// The host function that reads a user again at a session renewal: `{key}` gives the user or null.
pub const USER: &str = "auth.user";

const MAX_USERNAME: usize = 150;
const MAX_PASSWORD: usize = 1024;
const MAX_BODY: usize = 16 * 1024;
/// The fields of the user that the session cookie keeps. The password hash is not one of them: the cookie keeps a keyed tag of it.
const USER_FIELDS: [&str; 4] = ["key", "username", "is_superuser", "display"];

/// What the served router needs for login: the signing and limit state, and the host.
#[derive(Clone)]
pub struct AuthConfig {
    pub security: Arc<Security>,
    pub bridge: Arc<dyn HostBridge>,
}

/// The session of the request, set by the guard.
#[derive(Clone, Debug)]
pub struct SessionInfo(pub Session);

struct Ctx {
    auth: AuthConfig,
    site: Arc<Site>,
    conn: Arc<dyn Executor>,
}

pub fn routes(auth: AuthConfig, site: Arc<Site>, conn: Arc<dyn Executor>) -> Router {
    let ctx = Arc::new(Ctx { auth, site, conn });
    Router::new()
        .route("/login", get(login_page).post(login))
        .route("/logout", post(logout))
        .route("/me", get(me))
        .route("/password", post(password))
        .with_state(ctx)
}

pub(crate) fn with_cookies(mut response: Response, cookies: &[String]) -> Response {
    for cookie in cookies {
        if let Ok(v) = HeaderValue::from_str(cookie) {
            response.headers_mut().append(header::SET_COOKIE, v);
        }
    }
    response
}

/// The client of a request for the login limits: the socket peer, or, when the peer is a trusted proxy,
/// the first address of `X-Forwarded-For` (read from the right) that is not a trusted proxy itself.
/// A header from any other peer, and an entry that is not an address, is not believed.
pub(crate) fn client_ip(req: &Request, trusted: &[IpAddr]) -> String {
    let Some(peer) = req.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip()) else {
        return "unknown".into();
    };
    if !trusted.contains(&peer) {
        return peer.to_string();
    }
    let entries: Vec<&str> = req.headers().get_all("x-forwarded-for").iter().filter_map(|v| v.to_str().ok()).flat_map(|v| v.split(',')).map(str::trim).collect();
    for entry in entries.into_iter().rev() {
        match entry.parse::<IpAddr>() {
            Ok(ip) if trusted.contains(&ip) => continue,
            Ok(ip) => return ip.to_string(),
            Err(_) => break,
        }
    }
    peer.to_string()
}

async fn read_json(req: Request) -> Result<Map<String, Value>, Response> {
    let bytes = axum::body::to_bytes(req.into_body(), MAX_BODY).await.map_err(|_| reject(StatusCode::PAYLOAD_TOO_LARGE, "PayloadTooLarge", "the body is too large"))?;
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(Value::Object(map)) => Ok(map),
        _ => Err(reject(StatusCode::BAD_REQUEST, "BadRequest", "the body must be a JSON object")),
    }
}

// The error is the finished HTTP response, returned at once.
#[allow(clippy::result_large_err)]
fn text<'a>(body: &'a Map<String, Value>, key: &str, max: usize) -> Result<&'a str, Response> {
    match body.get(key).and_then(Value::as_str) {
        Some(s) if !s.is_empty() && s.len() <= max => Ok(s),
        _ => Err(reject(StatusCode::BAD_REQUEST, "BadRequest", &format!("`{key}` must be a non-empty string of at most {max} bytes"))),
    }
}

pub(crate) fn host_error(e: &BridgeError) -> Response {
    let mut response = reject(StatusCode::INTERNAL_SERVER_ERROR, "HostError", "the host failed");
    response.extensions_mut().insert(ErrorNote(format!("{}: {}", e.kind(), e.message())));
    response
}

pub(crate) fn database_error(message: &str) -> Response {
    let mut response = reject(StatusCode::INTERNAL_SERVER_ERROR, "DatabaseError", "the database failed");
    response.extensions_mut().insert(ErrorNote(message.to_string()));
    response
}

fn too_many(wait: i64) -> Response {
    let mut response = reject(StatusCode::TOO_MANY_REQUESTS, "TooManyAttempts", "too many failed attempts, try again later");
    if let Ok(v) = HeaderValue::from_str(&wait.to_string()) {
        response.headers_mut().insert(header::RETRY_AFTER, v);
    }
    response
}

/// The user object of the session: the allow-listed fields of what the host returned. None for no user or an inactive one.
pub(crate) fn session_user(value: &Value) -> Option<Value> {
    let map = value.as_object()?;
    if map.get("is_active").and_then(Value::as_bool) == Some(false) {
        return None;
    }
    if !matches!(map.get("key"), Some(Value::String(_) | Value::Number(_))) {
        return None;
    }
    Some(Value::Object(USER_FIELDS.iter().filter_map(|k| map.get(*k).map(|v| (k.to_string(), v.clone()))).collect()))
}

/// The password hash that the host returns beside the user, for the session's password tag.
pub(crate) fn password_hash(value: &Value) -> Option<&str> {
    value.get("password_hash").and_then(Value::as_str)
}

fn user_key(user: &Value) -> String {
    match &user["key"] {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

impl Ctx {
    fn context(&self, scope: &Scoped) -> CallContext {
        CallContext { request_id: self.auth.security.call_id(), tenant: None, scope: json!({}), tx: Some(scope.transaction()) }
    }

    async fn audit(&self, scope: &Scoped, entry: Entry<'_>) -> Result<(), String> {
        audit::record(&self.site, scope.executor(), entry).await
    }
}

fn session_response(ctx: &Ctx, session: Session) -> Response {
    let sec = &ctx.auth.security;
    let csrf = sec.csrf_token(&session.sid);
    let max_age = sec.remaining(&session);
    let cookies = [security::session_cookie(&sec.sign_session(&session), max_age), security::csrf_cookie(&csrf, max_age)];
    with_cookies(Json(json!({"ok": true, "user": session.user, "csrf": csrf})).into_response(), &cookies)
}

/// Which login limit refused an attempt.
#[derive(Clone, Copy)]
enum Blocked {
    Name,
    User,
    Ip,
}

impl Blocked {
    fn label(self) -> &'static str {
        match self {
            Blocked::Name => "name",
            Blocked::User => "user",
            Blocked::Ip => "ip",
        }
    }
}

/// The three login limit keys of one attempt.
struct LoginKeys {
    name: String,
    user: String,
    ip: String,
}

impl LoginKeys {
    fn new(username: &str, ip: &str) -> Self {
        let lower = username.to_lowercase();
        LoginKeys { name: format!("name:{lower}"), user: format!("user:{lower}|ip:{ip}"), ip: format!("ip:{ip}") }
    }

    /// Counts the attempt on all three limits, or on none: gives the limit that refused and the seconds to wait.
    fn reserve(&self, sec: &Security) -> Result<(), (Blocked, i64)> {
        let now = sec.now();
        if let Some(wait) = sec.name_limit().reserve(&self.name, now) {
            return Err((Blocked::Name, wait));
        }
        if let Some(wait) = sec.user_limit().reserve(&self.user, now) {
            sec.name_limit().release(&self.name);
            return Err((Blocked::User, wait));
        }
        if let Some(wait) = sec.ip_limit().reserve(&self.ip, now) {
            sec.name_limit().release(&self.name);
            sec.user_limit().release(&self.user);
            return Err((Blocked::Ip, wait));
        }
        Ok(())
    }

    fn release(&self, sec: &Security) {
        sec.name_limit().release(&self.name);
        sec.user_limit().release(&self.user);
        sec.ip_limit().release(&self.ip);
    }

    /// A good login frees the pair, and gives back the attempt on the shared limits (the failures of others stay).
    fn succeeded(&self, sec: &Security) {
        sec.name_limit().release(&self.name);
        sec.user_limit().clear(&self.user);
        sec.ip_limit().release(&self.ip);
    }

    fn key(&self, blocked: Blocked) -> &str {
        match blocked {
            Blocked::Name => &self.name,
            Blocked::User => &self.user,
            Blocked::Ip => &self.ip,
        }
    }
}

impl Ctx {
    /// The audit row of a refused attempt, once per limit key and minute. The 429 does not fail with it.
    async fn log_block(&self, keys: &LoginKeys, blocked: Blocked, username: &str, ip: &str, request_id: &str) {
        let sec = &self.auth.security;
        if !sec.log_block(&format!("{}|{}", blocked.label(), keys.key(blocked))) {
            return;
        }
        let Ok(scope) = Scoped::begin(self.conn.as_ref()).await else { return };
        let changes = Some(json!({"ip": ip, "request_id": request_id, "limit": blocked.label()}));
        let entry = Entry { user_key: "", action: "login_blocked", resource: "user", record_key: "", record_label: username, changes };
        match self.audit(&scope, entry).await {
            Ok(()) => {
                let _ = scope.commit().await;
            }
            Err(_) => scope.rollback().await,
        }
    }
}

/// A visitor with no session gets a CSRF token here, to sign in with.
async fn login_page(State(ctx): State<Arc<Ctx>>, session: Option<Extension<SessionInfo>>) -> Response {
    let sec = &ctx.auth.security;
    let sid = session.as_ref().map(|s| s.0 .0.sid.as_str()).unwrap_or("");
    let csrf = sec.csrf_token(sid);
    let max_age = session.as_ref().map(|s| sec.remaining(&s.0 .0)).unwrap_or(sec.options.idle_secs);
    let body = json!({"ok": true, "csrf": csrf, "authenticated": session.is_some()});
    with_cookies(Json(body).into_response(), &[security::csrf_cookie(&csrf, max_age)])
}

async fn login(State(ctx): State<Arc<Ctx>>, req: Request) -> Response {
    let sec = &ctx.auth.security;
    let ip = client_ip(&req, &sec.options.trusted_proxies);
    let request_id = req.extensions().get::<RequestId>().map(|r| r.0.clone()).unwrap_or_default();
    let body = match read_json(req).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let (username, password) = match (text(&body, "username", MAX_USERNAME), text(&body, "password", MAX_PASSWORD)) {
        (Ok(u), Ok(p)) => (u, p),
        (Err(r), _) | (_, Err(r)) => return r,
    };
    let keys = LoginKeys::new(username, &ip);
    if let Err((blocked, wait)) = keys.reserve(sec) {
        ctx.log_block(&keys, blocked, username, &ip, &request_id).await;
        return too_many(wait);
    }
    let scope = match Scoped::begin(ctx.conn.as_ref()).await {
        Ok(s) => s,
        Err(e) => {
            keys.release(sec);
            return database_error(&e.message);
        }
    };
    let (found, hash) = match ctx.auth.bridge.call(AUTHENTICATE, json!({"username": username, "password": password}), ctx.context(&scope)).await {
        Ok(v) => (session_user(&v), password_hash(&v).map(str::to_owned)),
        Err(e) => {
            scope.rollback().await;
            keys.release(sec);
            return host_error(&e);
        }
    };
    let changes = Some(json!({"ip": ip, "request_id": request_id}));
    // A failed login names no user: the typed name is only a label, so a guess cannot pose as a real user in the log.
    let entry = match &found {
        Some(user) => Entry { user_key: &user_key(user), action: "login", resource: "user", record_key: &user_key(user), record_label: username, changes },
        None => Entry { user_key: "", action: "login_failed", resource: "user", record_key: "", record_label: username, changes },
    };
    if let Err(e) = ctx.audit(&scope, entry).await {
        scope.rollback().await;
        keys.release(sec);
        return database_error(&e);
    }
    if let Err(e) = scope.commit().await {
        keys.release(sec);
        return database_error(&e.message);
    }
    match found {
        Some(user) => {
            keys.succeeded(sec);
            session_response(&ctx, sec.new_session(user, sec.pwd_tag(hash.as_deref())))
        }
        None => reject(StatusCode::UNAUTHORIZED, "InvalidCredentials", "wrong username or password"),
    }
}

/// Ends the session in every process: a revocation row, which each process reads at its next renewal, and the memory of this one.
async fn end_session(ctx: &Ctx, session: &Session) -> Result<(), Response> {
    let sec = &ctx.auth.security;
    let scope = Scoped::begin(ctx.conn.as_ref()).await.map_err(|e| database_error(&e.message))?;
    if let Err(e) = revoke::write(&ctx.site, scope.executor(), &session.sid, session.iat + sec.options.absolute_secs, sec.now()).await {
        scope.rollback().await;
        return Err(database_error(&e));
    }
    scope.commit().await.map_err(|e| database_error(&e.message))?;
    sec.revoke(session);
    Ok(())
}

async fn logout(State(ctx): State<Arc<Ctx>>, Extension(session): Extension<SessionInfo>) -> Response {
    if let Err(r) = end_session(&ctx, &session.0).await {
        return r;
    }
    with_cookies(Json(json!({"ok": true})).into_response(), &security::ended_cookies())
}

async fn me(State(ctx): State<Arc<Ctx>>, Extension(session): Extension<SessionInfo>, Extension(user): Extension<User>) -> Response {
    let sec = &ctx.auth.security;
    let csrf = sec.csrf_token(&session.0.sid);
    let cookie = security::csrf_cookie(&csrf, sec.remaining(&session.0));
    with_cookies(Json(json!({"ok": true, "user": user.0, "csrf": csrf})).into_response(), &[cookie])
}

async fn password(State(ctx): State<Arc<Ctx>>, Extension(session): Extension<SessionInfo>, req: Request) -> Response {
    let ip = client_ip(&req, &ctx.auth.security.options.trusted_proxies);
    let request_id = req.extensions().get::<RequestId>().map(|r| r.0.clone()).unwrap_or_default();
    let body = match read_json(req).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let (old, new) = match (text(&body, "old_password", MAX_PASSWORD), text(&body, "new_password", MAX_PASSWORD)) {
        (Ok(o), Ok(n)) => (o, n),
        (Err(r), _) | (_, Err(r)) => return r,
    };
    let sec = &ctx.auth.security;
    let key = user_key(&session.0.user);
    let limit = format!("password:{key}");
    if let Some(wait) = sec.user_limit().reserve(&limit, sec.now()) {
        return too_many(wait);
    }
    let scope = match Scoped::begin(ctx.conn.as_ref()).await {
        Ok(s) => s,
        Err(e) => {
            sec.user_limit().release(&limit);
            return database_error(&e.message);
        }
    };
    let payload = json!({"key": session.0.user["key"], "old_password": old, "new_password": new});
    let out = match ctx.auth.bridge.call(CHANGE_PASSWORD, payload, ctx.context(&scope)).await {
        Ok(v) => v,
        Err(e) => {
            scope.rollback().await;
            sec.user_limit().release(&limit);
            return host_error(&e);
        }
    };
    let label = session.0.user["username"].as_str().unwrap_or_default().to_string();
    let changed = out.get("changed").and_then(Value::as_bool) == Some(true);
    let action = if changed { "password_change" } else { "password_change_failed" };
    let entry = Entry { user_key: &key, action, resource: "user", record_key: &key, record_label: &label, changes: Some(json!({"ip": ip, "request_id": request_id})) };
    if let Err(e) = ctx.audit(&scope, entry).await {
        scope.rollback().await;
        sec.user_limit().release(&limit);
        return database_error(&e);
    }
    // The old cookie ends with the change, in the same transaction: both happen or neither.
    if changed {
        let old = &session.0;
        if let Err(e) = revoke::write(&ctx.site, scope.executor(), &old.sid, old.iat + sec.options.absolute_secs, sec.now()).await {
            scope.rollback().await;
            sec.user_limit().release(&limit);
            return database_error(&e);
        }
    }
    if let Err(e) = scope.commit().await {
        sec.user_limit().release(&limit);
        return database_error(&e.message);
    }
    if changed {
        sec.user_limit().clear(&limit);
        sec.revoke(&session.0);
        // This browser keeps its sign-in under a new session id and a tag of the new hash. Other sessions of the user end at their next renewal.
        return session_response(&ctx, sec.rotate(&session.0, sec.pwd_tag(password_hash(&out))));
    }
    let reason = out.get("reason").and_then(Value::as_str).unwrap_or("the password was not changed");
    reject(StatusCode::BAD_REQUEST, "InvalidPassword", reason)
}
