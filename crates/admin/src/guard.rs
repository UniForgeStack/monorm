//! The check every request passes before routing: the session (401 before 404), the Origin check,
//! the CSRF token and the JSON content type. At each renewal of the session cookie it also asks the host
//! for the user again and reads the revocation table, so a deactivated user, a demotion, a changed password
//! and a logout in another process end the session within `renew_after` seconds.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header, Method, StatusCode};
use axum::middleware::Next;
use axum::response::Response;

use axum::response::IntoResponse;
use monorm_engine::db::Executor;
use serde_json::json;

use crate::auth::{database_error, host_error, password_hash, session_user, with_cookies, AuthConfig, SessionInfo, USER};
use crate::bridge::CallContext;
use crate::revoke;
use crate::scope::Scoped;
use crate::security::{self, cookie_value, Session};
use crate::server::{reject, User};
use crate::site::Site;

pub struct Guard {
    pub auth: AuthConfig,
    /// The path of the one route that needs no session.
    pub login_path: String,
    pub site: Arc<Site>,
    pub conn: Arc<dyn Executor>,
}

/// The verdict of a renewal check.
enum Renewal {
    /// The session goes on, with the user as the host has it now.
    Live(Session),
    Ended,
}

/// Reads the revocation table and the user again, in one short transaction of its own (committed before the handler runs).
/// An error is the finished 500 response: the cookie stays, and the next request tries again, so a failure never keeps a session alive on its own.
async fn renew(g: &Guard, s: &Session, request_id: u64) -> Result<Renewal, Response> {
    let sec = &g.auth.security;
    let scope = Scoped::begin(g.conn.as_ref()).await.map_err(|e| database_error(&e.message))?;
    let revoked = match revoke::is_revoked(&g.site, scope.executor(), &s.sid).await {
        Ok(r) => r,
        Err(e) => {
            scope.rollback().await;
            return Err(database_error(&e));
        }
    };
    if revoked {
        scope.rollback().await;
        return Ok(Renewal::Ended);
    }
    let ctx = CallContext { request_id, tenant: None, scope: json!({}), tx: Some(scope.transaction()) };
    let fresh = match g.auth.bridge.call(USER, json!({"key": s.user["key"]}), ctx).await {
        Ok(v) => v,
        Err(e) => {
            scope.rollback().await;
            return Err(host_error(&e));
        }
    };
    scope.commit().await.map_err(|e| database_error(&e.message))?;
    let Some(user) = session_user(&fresh) else { return Ok(Renewal::Ended) };
    let demoted = s.user["is_superuser"].as_bool() == Some(true) && user["is_superuser"].as_bool() != Some(true);
    if demoted || !sec.pwd_matches(s, password_hash(&fresh)) {
        return Ok(Renewal::Ended);
    }
    Ok(Renewal::Live(Session { user, seen: sec.now(), ..s.clone() }))
}

const CSRF_HEADER: &str = security::CSRF_HEADER;

fn is_safe(method: &Method) -> bool {
    matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

/// The one route that takes a multipart body: `POST <prefix>/files/<resource>/<key>/<field>`.
fn is_upload(path: &str, prefix: &str) -> bool {
    path.strip_prefix(prefix).and_then(|p| p.strip_prefix("/files/")).is_some_and(|rest| rest.split('/').count() == 3 && rest.split('/').all(|s| !s.is_empty()))
}

fn header_text(req: &Request, name: impl header::AsHeaderName) -> Option<&str> {
    req.headers().get(name).and_then(|v| v.to_str().ok())
}

pub async fn guard(State(g): State<Arc<Guard>>, mut req: Request, next: Next) -> Response {
    let path = req.uri().path();
    if path == "/healthz" || path == "/readyz" {
        return next.run(req).await;
    }
    // The page and its files hold no data: the API calls of the page need the session.
    if matches!(*req.method(), Method::GET | Method::HEAD) && crate::ui::is_ui(path, &g.site.prefix) {
        return next.run(req).await;
    }
    let sec = &g.auth.security;
    let mut session = header_text(&req, header::COOKIE).and_then(|c| cookie_value(c, security::SESSION_COOKIE)).and_then(|t| sec.read_session(t));
    if session.is_none() && path != g.login_path {
        return reject(StatusCode::UNAUTHORIZED, "Unauthorized", "sign in");
    }
    if !is_safe(req.method()) {
        let host = header_text(&req, header::HOST).or_else(|| req.uri().authority().map(|a| a.as_str()));
        if !sec.origin_ok(header_text(&req, header::ORIGIN), header_text(&req, header::REFERER), host) {
            return reject(StatusCode::FORBIDDEN, "BadOrigin", "the Origin or Referer does not match this server");
        }
        let sid = session.as_ref().map(|s| s.sid.as_str()).unwrap_or("");
        let cookie = header_text(&req, header::COOKIE).and_then(|c| cookie_value(c, security::CSRF_COOKIE));
        if !sec.csrf_ok(sid, cookie, header_text(&req, CSRF_HEADER)) {
            return reject(StatusCode::FORBIDDEN, "BadCsrfToken", "send the CSRF token of the csrf cookie in the x-csrf-token header");
        }
        let json = header_text(&req, header::CONTENT_TYPE).is_some_and(|t| t.split(';').next().is_some_and(|m| m.trim().eq_ignore_ascii_case("application/json")));
        let upload = *req.method() == Method::POST && is_upload(path, &g.site.prefix) && header_text(&req, header::CONTENT_TYPE).is_some_and(|t| t.split(';').next().is_some_and(|m| m.trim().eq_ignore_ascii_case("multipart/form-data")));
        if matches!(*req.method(), Method::POST | Method::PUT | Method::PATCH) && !json && !upload {
            return reject(StatusCode::UNSUPPORTED_MEDIA_TYPE, "UnsupportedMediaType", "the body must be application/json");
        }
    }
    let mut renewed = false;
    if let Some(s) = &session {
        if sec.now() - s.seen >= sec.renew_after() {
            match renew(&g, s, sec.call_id()).await {
                Ok(Renewal::Live(fresh)) => {
                    session = Some(fresh);
                    renewed = true;
                }
                Ok(Renewal::Ended) if path == g.login_path => session = None,
                Ok(Renewal::Ended) => return with_cookies(reject(StatusCode::UNAUTHORIZED, "Unauthorized", "sign in").into_response(), &security::ended_cookies()),
                Err(response) => return response,
            }
        }
    }
    if let Some(s) = &session {
        req.extensions_mut().insert(User(s.user.clone()));
        req.extensions_mut().insert(SessionInfo(s.clone()));
    }
    let mut response = next.run(req).await;
    if let (true, Some(s)) = (renewed, &session) {
        if !sets_session(&response) {
            let cookie = security::session_cookie(&sec.sign_session(s), sec.remaining(s));
            if let Ok(v) = header::HeaderValue::from_str(&cookie) {
                response.headers_mut().append(header::SET_COOKIE, v);
            }
        }
    }
    response
}

/// True when the response already sets or ends the session cookie (login, logout).
fn sets_session(response: &Response) -> bool {
    response.headers().get_all(header::SET_COOKIE).iter().any(|v| v.to_str().is_ok_and(|c| c.starts_with(security::SESSION_COOKIE)))
}
