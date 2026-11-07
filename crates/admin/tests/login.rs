//! Login, sessions and HTTP security through the served router, with a mock host behind the bridge.
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::ConnectInfo;
use axum::http::Request;
use axum::Router;
use monorm_admin::service::{service, Options};
use monorm_admin::{AuthConfig, BoxFuture, BridgeError, CallContext, HostBridge, Security, SecurityOptions, Site};
use monorm_core::{dsl, migrate, schema::Schema};
use monorm_engine::db::{self, Driver};
use serde_json::{json, Value};
use tower::ServiceExt;

const APP: &str = include_str!("../../../apps/admin/schema.prisma");
const SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";
const LOGIN: &str = "/admin/api/login";

/// A host with two users: `ann` (active, superuser) and `bob` (inactive). Both have the password `pw`.
/// The tests flip the flags of `ann` to play a change that happens outside the admin service.
struct Host {
    password: Mutex<String>,
    authenticate_calls: AtomicUsize,
    user_calls: AtomicUsize,
    ann_active: AtomicBool,
    ann_superuser: AtomicBool,
    fail_user: AtomicBool,
}

impl Host {
    fn new() -> Host {
        Host {
            password: Mutex::new("pw".into()),
            authenticate_calls: AtomicUsize::new(0),
            user_calls: AtomicUsize::new(0),
            ann_active: AtomicBool::new(true),
            ann_superuser: AtomicBool::new(true),
            fail_user: AtomicBool::new(false),
        }
    }

    fn ann(&self) -> Value {
        let hash = format!("hash:{}", self.password.lock().unwrap());
        json!({"key": 1, "username": "ann", "is_superuser": self.ann_superuser.load(Ordering::SeqCst), "is_active": self.ann_active.load(Ordering::SeqCst), "display": "Ann", "password": "leak", "password_hash": hash})
    }
}

impl HostBridge for Host {
    fn call(&self, name: &str, payload: Value, _ctx: CallContext) -> BoxFuture<Result<Value, BridgeError>> {
        let result = match name {
            "auth.authenticate" => {
                self.authenticate_calls.fetch_add(1, Ordering::SeqCst);
                let right = payload["password"] == json!(self.password.lock().unwrap().clone());
                Ok(match (payload["username"].as_str(), right) {
                    (Some("ann"), true) => self.ann(),
                    (Some("bob"), true) => json!({"key": 2, "username": "bob", "is_superuser": false, "is_active": false, "password_hash": "hash:bob"}),
                    _ => Value::Null,
                })
            }
            "auth.user" => {
                self.user_calls.fetch_add(1, Ordering::SeqCst);
                if self.fail_user.load(Ordering::SeqCst) {
                    Err(BridgeError::Host { kind: "ValueError".into(), message: "boom".into() })
                } else {
                    Ok(if payload["key"] == json!(1) { self.ann() } else { Value::Null })
                }
            }
            "auth.change_password" => {
                let mut current = self.password.lock().unwrap();
                if payload["old_password"] == json!(current.clone()) {
                    *current = payload["new_password"].as_str().unwrap().to_string();
                    Ok(json!({"changed": true, "password_hash": format!("hash:{current}")}))
                } else {
                    Ok(json!({"changed": false, "reason": "the old password is wrong"}))
                }
            }
            other => Err(BridgeError::Bridge(format!("no host function {other}"))),
        };
        Box::pin(async move { result })
    }
}

struct Rig {
    router: Router,
    driver: Arc<dyn Driver>,
    security: Arc<Security>,
    host: Arc<Host>,
}

async fn rig_with(options: SecurityOptions) -> Rig {
    let driver = db::connect("sqlite://:memory:", 1).await.unwrap();
    rig_on(driver, "sqlite", options).await
}

async fn rig_on(driver: Arc<dyn Driver>, provider: &str, options: SecurityOptions) -> Rig {
    build(driver, provider, options, true).await
}

/// A second server process over the database of `first`: same secret, own memory and clock.
async fn sibling(first: &Rig, provider: &str) -> Rig {
    build(first.driver.clone(), provider, SecurityOptions::default(), false).await
}

async fn build(driver: Arc<dyn Driver>, provider: &str, options: SecurityOptions, fresh: bool) -> Rig {
    let source = format!("datasource db {{\n  provider = \"{provider}\"\n}}\n\nmodel Note {{\n  id Int @id\n}}\n{APP}");
    let schema: Arc<Schema> = Arc::new(dsl::check(dsl::compile(&source, None).unwrap()).unwrap().1);
    if fresh {
        for step in migrate::plan(&schema, &Default::default()).unwrap().up {
            driver.batch(step.sql).await.unwrap();
        }
    }
    let site = Arc::new(Site::build(schema, &json!({"resources": [{"model": "Note"}]}).to_string()).unwrap());
    let security = Security::new(SECRET, options).unwrap();
    let host = Arc::new(Host::new());
    let mut opts = Options::new(None);
    opts.auth = Some(AuthConfig { security: security.clone(), bridge: host.clone() });
    opts.log = monorm_admin::service::Logger::new(|_| {});
    Rig { router: service(site, driver.clone(), opts), driver, security, host }
}

async fn rig() -> Rig {
    rig_with(SecurityOptions::default()).await
}

struct Reply {
    status: u16,
    headers: axum::http::HeaderMap,
    body: Value,
}

impl Reply {
    fn cookies(&self) -> Vec<String> {
        self.headers.get_all("set-cookie").iter().map(|v| v.to_str().unwrap().to_string()).collect()
    }
    fn cookie(&self, name: &str) -> Option<String> {
        self.cookies().into_iter().find(|c| c.starts_with(&format!("{name}=")))
    }
}

/// A browser: it keeps the cookies and sends the CSRF header the way the front end does.
struct Client {
    router: Router,
    ip: &'static str,
    jar: BTreeMap<String, String>,
}

impl Client {
    fn new(router: &Router, ip: &'static str) -> Client {
        Client { router: router.clone(), ip, jar: BTreeMap::new() }
    }

    async fn raw(&mut self, method: &str, uri: &str, headers: &[(&str, &str)], body: Option<Value>) -> Reply {
        let mut req = Request::builder().method(method).uri(uri).header("host", "localhost");
        let cookie = self.jar.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; ");
        if !cookie.is_empty() {
            req = req.header("cookie", cookie);
        }
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let body = match body {
            Some(b) => axum::body::Body::from(b.to_string()),
            None => axum::body::Body::empty(),
        };
        let mut req = req.body(body).unwrap();
        req.extensions_mut().insert(ConnectInfo(format!("{}:4000", self.ip).parse::<SocketAddr>().unwrap()));
        let response = self.router.clone().oneshot(req).await.unwrap();
        let (parts, body) = response.into_parts();
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        let reply = Reply { status: parts.status.as_u16(), headers: parts.headers, body: serde_json::from_slice(&bytes).unwrap_or(Value::Null) };
        for cookie in reply.cookies() {
            let (pair, _) = cookie.split_once(';').unwrap_or((&cookie, ""));
            let (k, v) = pair.split_once('=').unwrap();
            if cookie.contains("Max-Age=0") || v.is_empty() {
                self.jar.remove(k);
            } else {
                self.jar.insert(k.to_string(), v.to_string());
            }
        }
        reply
    }

    async fn get(&mut self, uri: &str) -> Reply {
        self.raw("GET", uri, &[], None).await
    }

    /// A write with the CSRF token of the jar, the Origin of this host and a JSON body.
    async fn post(&mut self, uri: &str, body: Value) -> Reply {
        let token = self.jar.get("__Host-monorm-csrf").cloned().unwrap_or_default();
        self.raw("POST", uri, &[("x-csrf-token", &token), ("origin", "http://localhost"), ("content-type", "application/json")], Some(body)).await
    }

    async fn login(&mut self, username: &str, password: &str) -> Reply {
        self.login_with(username, password, &[]).await
    }

    /// A login with extra request headers, for example `x-forwarded-for`.
    async fn login_with(&mut self, username: &str, password: &str, extra: &[(&str, &str)]) -> Reply {
        self.raw("GET", LOGIN, extra, None).await;
        let token = self.jar.get("__Host-monorm-csrf").cloned().unwrap_or_default();
        let mut headers = vec![("x-csrf-token", token.as_str()), ("origin", "http://localhost"), ("content-type", "application/json")];
        headers.extend_from_slice(extra);
        self.raw("POST", LOGIN, &headers, Some(json!({"username": username, "password": password}))).await
    }
}

async fn audit(rig: &Rig) -> Vec<Vec<String>> {
    let rows = rig.driver.query_text("SELECT user_key, action, record_label FROM \"AdminLogEntry\" ORDER BY id".into()).await.unwrap();
    rows.into_iter().map(|r| r.into_iter().map(Option::unwrap_or_default).collect()).collect()
}

#[tokio::test]
async fn a_login_sets_a_signed_http_only_cookie_and_the_user_is_filtered() {
    let rig = rig().await;
    let mut c = Client::new(&rig.router, "10.0.0.1");
    let reply = c.login("ann", "pw").await;
    assert_eq!(reply.status, 200, "{:?}", reply.body);
    let session = reply.cookie("__Host-monorm-session").expect("a session cookie");
    for flag in ["HttpOnly", "Secure", "SameSite=Lax", "Path=/"] {
        assert!(session.contains(flag), "{flag} in {session}");
    }
    assert!(!session.contains("Domain"), "a __Host- cookie has no Domain: {session}");
    assert!(reply.cookie("__Host-monorm-csrf").is_some());
    assert_eq!(reply.body["user"]["username"], "ann");
    assert!(reply.body["user"].get("password").is_none(), "only the allow-listed fields are kept");
    let me = c.get("/admin/api/me").await;
    assert_eq!(me.status, 200);
    assert_eq!(me.body["user"]["key"], 1);
    assert_eq!(audit(&rig).await, vec![vec!["1".to_string(), "login".into(), "ann".into()]]);
}

#[tokio::test]
async fn a_wrong_password_and_an_unknown_user_give_the_same_401_and_a_failed_login_row() {
    let rig = rig().await;
    let mut c = Client::new(&rig.router, "10.0.0.1");
    let wrong = c.login("ann", "nope").await;
    let unknown = c.login("zed", "nope").await;
    assert_eq!((wrong.status, unknown.status), (401, 401));
    assert_eq!(wrong.body, unknown.body);
    assert!(wrong.cookie("__Host-monorm-session").is_none());
    let rows = audit(&rig).await;
    assert_eq!(rows.iter().map(|r| r[1].as_str()).collect::<Vec<_>>(), ["login_failed", "login_failed"]);
    assert_eq!(rows[1][0], "", "a failed login names no user key: the typed text is not an account");
    assert_eq!(rows[1][2], "zed", "the typed name is the label");
    assert_eq!(rig.host.authenticate_calls.load(Ordering::SeqCst), 2, "an unknown user goes through the host too");
}

#[tokio::test]
async fn an_inactive_user_cannot_log_in() {
    let rig = rig().await;
    let mut c = Client::new(&rig.router, "10.0.0.1");
    let reply = c.login("bob", "pw").await;
    assert_eq!(reply.status, 401);
    assert_eq!(c.get("/admin/api/me").await.status, 401);
}

#[tokio::test]
async fn five_failures_for_a_user_from_one_ip_answer_429_but_another_ip_still_signs_in() {
    let rig = rig().await;
    for _ in 0..5 {
        assert_eq!(Client::new(&rig.router, "10.0.1.1").login("ann", "nope").await.status, 401);
    }
    let blocked = Client::new(&rig.router, "10.0.1.1").login("ann", "pw").await;
    assert_eq!(blocked.status, 429);
    assert!(blocked.headers.get("retry-after").unwrap().to_str().unwrap().parse::<i64>().unwrap() > 0);
    assert_eq!(Client::new(&rig.router, "10.0.1.1").login("other", "nope").await.status, 401, "another user is not blocked");
    assert_eq!(Client::new(&rig.router, "10.0.1.9").login("ann", "pw").await.status, 200, "an attacker cannot lock the owner out from one address");
}

#[tokio::test]
async fn fifty_failures_for_one_name_over_many_ips_answer_429_for_every_ip() {
    let rig = rig().await;
    for i in 0..10 {
        let ip: &'static str = Box::leak(format!("10.0.4.{i}").into_boxed_str());
        for _ in 0..5 {
            assert_eq!(Client::new(&rig.router, ip).login("ann", "nope").await.status, 401);
        }
    }
    assert_eq!(Client::new(&rig.router, "10.0.4.99").login("ann", "pw").await.status, 429, "the global limit of one name");
    assert_eq!(Client::new(&rig.router, "10.0.4.99").login("other", "nope").await.status, 401);
}

#[tokio::test]
async fn x_forwarded_for_counts_only_from_a_trusted_proxy() {
    let rig = rig().await;
    for i in 0..20 {
        let xff: &'static str = Box::leak(format!("198.51.100.{i}").into_boxed_str());
        assert_eq!(Client::new(&rig.router, "10.0.5.1").login_with(&format!("user{i}"), "nope", &[("x-forwarded-for", xff)]).await.status, 401);
    }
    let spoofed = Client::new(&rig.router, "10.0.5.1").login_with("ann", "pw", &[("x-forwarded-for", "198.51.100.200")]).await;
    assert_eq!(spoofed.status, 429, "an untrusted peer cannot choose its own address: the header is ignored");

    let proxy = "10.0.5.9".parse().unwrap();
    let behind = rig_with(SecurityOptions { trusted_proxies: vec![proxy], ..Default::default() }).await;
    for _ in 0..5 {
        let reply = Client::new(&behind.router, "10.0.5.9").login_with("ann", "nope", &[("x-forwarded-for", "203.0.113.5")]).await;
        assert_eq!(reply.status, 401);
    }
    let same = Client::new(&behind.router, "10.0.5.9").login_with("ann", "pw", &[("x-forwarded-for", "203.0.113.5")]).await;
    assert_eq!(same.status, 429, "the client behind the proxy is the one that fails");
    let other = Client::new(&behind.router, "10.0.5.9").login_with("ann", "pw", &[("x-forwarded-for", "203.0.113.6")]).await;
    assert_eq!(other.status, 200, "another client behind the same proxy");
    let rows = behind.driver.query_text("SELECT changes FROM \"AdminLogEntry\" ORDER BY id LIMIT 1".into()).await.unwrap();
    assert!(rows[0][0].clone().unwrap().contains("203.0.113.5"), "the audit row has the client address, not the proxy");
    let spoof_through_proxy = Client::new(&behind.router, "10.0.5.9").login_with("ann", "pw", &[("x-forwarded-for", "203.0.113.7, 10.0.5.9")]).await;
    assert_eq!(spoof_through_proxy.status, 200, "a trusted proxy hop on the right is skipped");
}

#[tokio::test]
async fn a_blocked_login_writes_one_audit_row_a_minute() {
    let rig = rig().await;
    let mut c = Client::new(&rig.router, "10.0.6.1");
    for _ in 0..5 {
        c.login("ann", "nope").await;
    }
    for _ in 0..3 {
        assert_eq!(c.login("ann", "pw").await.status, 429);
    }
    let blocked = |rows: &[Vec<String>]| rows.iter().filter(|r| r[1] == "login_blocked").cloned().collect::<Vec<_>>();
    let rows = audit(&rig).await;
    assert_eq!(blocked(&rows), vec![vec!["".to_string(), "login_blocked".into(), "ann".into()]], "three attempts, one row");
    rig.security.advance(61);
    assert_eq!(c.login("ann", "pw").await.status, 429);
    assert_eq!(blocked(&audit(&rig).await).len(), 2, "a minute later the next blocked attempt writes a row");
    assert_eq!(rig.host.authenticate_calls.load(Ordering::SeqCst), 5, "a blocked attempt never reaches the host");
}

#[tokio::test]
async fn parallel_wrong_passwords_cannot_pass_the_limit_before_the_first_verdict() {
    let rig = rig().await;
    let tasks: Vec<_> = (0..12)
        .map(|_| {
            let router = rig.router.clone();
            tokio::spawn(async move { Client::new(&router, "10.0.3.1").login("ann", "nope").await.status })
        })
        .collect();
    let mut statuses = Vec::new();
    for t in tasks {
        statuses.push(t.await.unwrap());
    }
    assert_eq!(statuses.iter().filter(|s| **s == 401).count(), 5, "{statuses:?}");
    assert_eq!(statuses.iter().filter(|s| **s == 429).count(), 7, "{statuses:?}");
    assert_eq!(rig.host.authenticate_calls.load(Ordering::SeqCst), 5, "a limited attempt never reaches the host");
}

#[tokio::test]
async fn twenty_failures_from_one_ip_answer_429_for_any_user() {
    let rig = rig().await;
    let mut c = Client::new(&rig.router, "10.0.2.1");
    for i in 0..20 {
        assert_eq!(c.login(&format!("user{i}"), "nope").await.status, 401);
    }
    assert_eq!(c.login("ann", "pw").await.status, 429);
    assert_eq!(Client::new(&rig.router, "10.0.2.2").login("ann", "pw").await.status, 200);
}

#[tokio::test]
async fn a_request_without_a_session_gets_401_before_404_and_before_the_routes() {
    let rig = rig().await;
    let mut c = Client::new(&rig.router, "10.0.0.1");
    for uri in ["/admin/api/resources", "/admin/api/resources/Note", "/admin/api/nothing", "/elsewhere", "/admin/api/me"] {
        assert_eq!(c.get(uri).await.status, 401, "{uri}");
    }
    assert_eq!(c.get("/healthz").await.status, 200, "health needs no session");
    c.login("ann", "pw").await;
    assert_eq!(c.get("/admin/api/nothing").await.status, 404);
    assert_eq!(c.get("/admin/api/resources").await.status, 200);
}

#[tokio::test]
async fn a_write_needs_the_csrf_token_the_origin_and_json() {
    let rig = rig().await;
    let mut c = Client::new(&rig.router, "10.0.0.1");
    c.login("ann", "pw").await;
    let token = c.jar["__Host-monorm-csrf"].clone();
    let body = || Some(json!({"old_password": "pw", "new_password": "pw"}));
    let url = "/admin/api/password";
    let json_type = ("content-type", "application/json");
    let ok_origin = ("origin", "http://localhost");

    let no_header = c.raw("POST", url, &[ok_origin, json_type], body()).await;
    assert_eq!((no_header.status, no_header.body["error"]["kind"].as_str()), (403, Some("BadCsrfToken")));
    let wrong_token = c.raw("POST", url, &[("x-csrf-token", "abc.def"), ok_origin, json_type], body()).await;
    assert_eq!(wrong_token.status, 403);
    let no_origin = c.raw("POST", url, &[("x-csrf-token", &token), json_type], body()).await;
    assert_eq!((no_origin.status, no_origin.body["error"]["kind"].as_str()), (403, Some("BadOrigin")));
    let foreign = c.raw("POST", url, &[("x-csrf-token", &token), ("origin", "https://evil.example"), json_type], body()).await;
    assert_eq!(foreign.status, 403);
    let referer = c.raw("POST", url, &[("x-csrf-token", &token), ("referer", "http://localhost/admin/x?y=1"), json_type], body()).await;
    assert_eq!(referer.status, 200, "a matching Referer replaces the Origin");
    let token = c.jar["__Host-monorm-csrf"].clone(); // the password change issued a new session, so a new token
    let form = c.raw("POST", url, &[("x-csrf-token", &token), ok_origin, ("content-type", "application/x-www-form-urlencoded")], body()).await;
    assert_eq!((form.status, form.body["error"]["kind"].as_str()), (415, Some("UnsupportedMediaType")));
    let other_session = {
        let mut d = Client::new(&rig.router, "10.0.0.2");
        d.login("ann", "pw").await;
        d.jar["__Host-monorm-csrf"].clone()
    };
    let swapped = c.raw("POST", url, &[("x-csrf-token", &other_session), ok_origin, json_type], body()).await;
    assert_eq!(swapped.status, 403, "a token is bound to its session (the cookie differs from the header)");
}

#[tokio::test]
async fn a_configured_origin_is_accepted_and_the_login_post_needs_csrf_too() {
    let rig = rig_with(SecurityOptions { origins: vec!["https://admin.example.com".into()], ..Default::default() }).await;
    let mut c = Client::new(&rig.router, "10.0.0.1");
    c.get(LOGIN).await;
    let token = c.jar["__Host-monorm-csrf"].clone();
    let body = Some(json!({"username": "ann", "password": "pw"}));
    let bare = c.raw("POST", LOGIN, &[("origin", "http://localhost"), ("content-type", "application/json")], body.clone()).await;
    assert_eq!(bare.status, 403, "no CSRF header on the login");
    let proxied = c.raw("POST", LOGIN, &[("x-csrf-token", &token), ("origin", "https://admin.example.com"), ("content-type", "application/json")], body).await;
    assert_eq!(proxied.status, 200);
}

#[tokio::test]
async fn every_response_has_the_security_headers() {
    let rig = rig().await;
    let mut c = Client::new(&rig.router, "10.0.0.1");
    let mut replies = vec![c.get("/admin/api/resources").await, c.get("/healthz").await, c.get(LOGIN).await];
    c.login("ann", "pw").await;
    replies.push(c.get("/admin/api/nothing").await);
    replies.push(c.get("/admin/api/resources").await);
    replies.push(c.raw("POST", "/admin/api/password", &[], None).await);
    for r in replies {
        let h = &r.headers;
        assert!(h["content-security-policy"].to_str().unwrap().contains("default-src 'none'"), "{}", r.status);
        assert!(h["content-security-policy"].to_str().unwrap().contains("frame-ancestors 'none'"));
        assert_eq!(h["x-frame-options"], "DENY");
        assert_eq!(h["referrer-policy"], "same-origin");
        assert_eq!(h["cache-control"], "no-store");
        assert_eq!(h["x-content-type-options"], "nosniff");
    }
}

#[tokio::test]
async fn two_servers_with_the_same_secret_accept_one_session_and_a_tampered_cookie_fails() {
    let a = rig().await;
    let b = rig().await;
    let mut c = Client::new(&a.router, "10.0.0.1");
    c.login("ann", "pw").await;
    let mut on_b = Client::new(&b.router, "10.0.0.1");
    on_b.jar = c.jar.clone();
    assert_eq!(on_b.get("/admin/api/me").await.status, 200);

    let other = Security::new(b"another-secret-another-secret-123", SecurityOptions::default()).unwrap();
    let session = other.new_session(json!({"key": 1}), String::new());
    let mut forged = Client::new(&a.router, "10.0.0.1");
    forged.jar.insert("__Host-monorm-session".into(), other.sign_session(&session));
    assert_eq!(forged.get("/admin/api/me").await.status, 401, "a cookie signed with another secret");

    let token = c.jar["__Host-monorm-session"].clone();
    let mut parts: Vec<String> = token.split('.').map(str::to_string).collect();
    parts[1].insert(0, 'A');
    let mut tampered = Client::new(&a.router, "10.0.0.1");
    tampered.jar.insert("__Host-monorm-session".into(), parts.join("."));
    assert_eq!(tampered.get("/admin/api/me").await.status, 401);
}

#[tokio::test]
async fn the_idle_timeout_ends_a_session_and_use_renews_it() {
    let rig = rig_with(SecurityOptions { idle_secs: 100, absolute_secs: 10_000, ..Default::default() }).await;
    let mut c = Client::new(&rig.router, "10.0.0.1");
    c.login("ann", "pw").await;
    rig.security.advance(60);
    let renewed = c.get("/admin/api/me").await;
    assert_eq!(renewed.status, 200);
    rig.security.advance(60);
    assert_eq!(c.get("/admin/api/me").await.status, 200, "120 s after the login, 60 s after the last request");
    rig.security.advance(101);
    assert_eq!(c.get("/admin/api/me").await.status, 401);
}

#[tokio::test]
async fn the_absolute_timeout_ends_a_session_that_is_in_use() {
    let rig = rig_with(SecurityOptions { idle_secs: 100, absolute_secs: 250, ..Default::default() }).await;
    let mut c = Client::new(&rig.router, "10.0.0.1");
    c.login("ann", "pw").await;
    for _ in 0..2 {
        rig.security.advance(90);
        assert_eq!(c.get("/admin/api/me").await.status, 200);
    }
    rig.security.advance(90);
    assert_eq!(c.get("/admin/api/me").await.status, 401, "270 s after the login");
}

async fn count_revocations(rig: &Rig) -> i64 {
    let rows = rig.driver.query_text("SELECT COUNT(*) FROM \"AdminSessionRevocation\"".into()).await.unwrap();
    rows[0][0].clone().unwrap().parse().unwrap()
}

#[tokio::test]
async fn a_renewal_reads_the_user_again_and_a_live_session_goes_on() {
    let rig = rig().await;
    let mut c = Client::new(&rig.router, "10.0.0.1");
    c.login("ann", "pw").await;
    assert_eq!(c.get("/admin/api/me").await.status, 200);
    assert_eq!(rig.host.user_calls.load(Ordering::SeqCst), 0, "a fresh cookie needs no check");
    rig.security.advance(61);
    let renewed = c.get("/admin/api/me").await;
    assert_eq!(renewed.status, 200);
    assert!(renewed.cookie("__Host-monorm-session").is_some(), "a renewal sets the new cookie");
    assert_eq!(rig.host.user_calls.load(Ordering::SeqCst), 1);
    assert_eq!(c.get("/admin/api/me").await.status, 200);
    assert_eq!(rig.host.user_calls.load(Ordering::SeqCst), 1, "one check per renewal, not per request");
}

#[tokio::test]
async fn a_deactivated_user_loses_the_session_at_the_next_renewal() {
    let rig = rig().await;
    let mut c = Client::new(&rig.router, "10.0.0.1");
    c.login("ann", "pw").await;
    rig.host.ann_active.store(false, Ordering::SeqCst);
    assert_eq!(c.get("/admin/api/me").await.status, 200, "the change shows at the renewal, not before");
    rig.security.advance(61);
    let ended = c.get("/admin/api/me").await;
    assert_eq!(ended.status, 401);
    assert!(ended.cookies().iter().all(|k| k.contains("Max-Age=0")), "the cookies are cleared");
    assert_eq!(c.get("/admin/api/me").await.status, 401);
}

#[tokio::test]
async fn a_demoted_superuser_loses_the_session_at_the_next_renewal() {
    let rig = rig().await;
    let mut c = Client::new(&rig.router, "10.0.0.1");
    c.login("ann", "pw").await;
    rig.host.ann_superuser.store(false, Ordering::SeqCst);
    rig.security.advance(61);
    assert_eq!(c.get("/admin/api/me").await.status, 401);
}

#[tokio::test]
async fn a_password_changed_outside_the_service_ends_the_session_at_the_next_renewal() {
    let rig = rig().await;
    let mut c = Client::new(&rig.router, "10.0.0.1");
    c.login("ann", "pw").await;
    *rig.host.password.lock().unwrap() = "changed-elsewhere".into();
    rig.security.advance(61);
    assert_eq!(c.get("/admin/api/me").await.status, 401);
}

#[tokio::test]
async fn a_host_error_at_a_renewal_answers_500_keeps_the_cookie_and_the_next_try_works() {
    let rig = rig().await;
    let mut c = Client::new(&rig.router, "10.0.0.1");
    c.login("ann", "pw").await;
    rig.host.fail_user.store(true, Ordering::SeqCst);
    rig.security.advance(61);
    let failed = c.get("/admin/api/me").await;
    assert_eq!((failed.status, failed.body["error"]["kind"].as_str()), (500, Some("HostError")));
    assert!(failed.cookies().is_empty(), "the session is not ended by an error");
    rig.host.fail_user.store(false, Ordering::SeqCst);
    assert_eq!(c.get("/admin/api/me").await.status, 200);
}

#[tokio::test]
async fn a_logout_in_one_process_ends_the_copy_of_the_cookie_in_another_process() {
    let a = rig().await;
    let b = sibling(&a, "sqlite").await;
    let mut c = Client::new(&a.router, "10.0.0.1");
    c.login("ann", "pw").await;
    let mut other = Client::new(&b.router, "10.0.0.1");
    other.jar = c.jar.clone();
    assert_eq!(other.get("/admin/api/me").await.status, 200, "process b takes the cookie of process a");
    assert_eq!(c.post("/admin/api/logout", json!({})).await.status, 200);
    assert_eq!(count_revocations(&a).await, 1);
    assert_eq!(other.get("/admin/api/me").await.status, 200, "process b learns of the logout at the next renewal");
    b.security.advance(61);
    assert_eq!(other.get("/admin/api/me").await.status, 401);
}

#[tokio::test]
async fn logout_ends_the_session_even_for_a_copy_of_the_cookie() {
    let rig = rig().await;
    let mut c = Client::new(&rig.router, "10.0.0.1");
    c.login("ann", "pw").await;
    let copy = c.jar.clone();
    let out = c.post("/admin/api/logout", json!({})).await;
    assert_eq!(out.status, 200);
    assert!(out.cookies().iter().all(|k| k.contains("Max-Age=0")));
    assert_eq!(c.get("/admin/api/me").await.status, 401);
    let mut thief = Client::new(&rig.router, "10.0.0.1");
    thief.jar = copy;
    assert_eq!(thief.get("/admin/api/me").await.status, 401);
}

#[tokio::test]
async fn a_password_change_needs_the_old_password_and_writes_an_audit_row() {
    let rig = rig().await;
    let mut c = Client::new(&rig.router, "10.0.0.1");
    c.login("ann", "pw").await;
    let wrong = c.post("/admin/api/password", json!({"old_password": "x", "new_password": "new-pw"})).await;
    assert_eq!((wrong.status, wrong.body["error"]["kind"].as_str()), (400, Some("InvalidPassword")));
    let missing = c.post("/admin/api/password", json!({"old_password": "pw"})).await;
    assert_eq!(missing.status, 400);
    let done = c.post("/admin/api/password", json!({"old_password": "pw", "new_password": "new-pw"})).await;
    assert_eq!(done.status, 200);
    assert_eq!(*rig.host.password.lock().unwrap(), "new-pw");
    let actions: Vec<String> = audit(&rig).await.into_iter().map(|r| r[1].clone()).collect();
    assert_eq!(actions, ["login", "password_change_failed", "password_change"]);
    let mut again = Client::new(&rig.router, "10.0.0.2");
    assert_eq!(again.login("ann", "pw").await.status, 401);
    assert_eq!(again.login("ann", "new-pw").await.status, 200);
}

#[tokio::test]
async fn a_password_change_ends_the_old_cookie_and_keeps_the_changed_session() {
    let rig = rig().await;
    let mut c = Client::new(&rig.router, "10.0.0.1");
    c.login("ann", "pw").await;
    let old = c.jar.clone();
    let mut elsewhere = Client::new(&rig.router, "10.0.0.2");
    elsewhere.login("ann", "pw").await;
    let done = c.post("/admin/api/password", json!({"old_password": "pw", "new_password": "new-pw"})).await;
    assert_eq!(done.status, 200);
    assert!(done.cookie("__Host-monorm-session").is_some() && done.cookie("__Host-monorm-csrf").is_some(), "the session is issued again");
    assert!(done.body["csrf"].is_string());
    assert_ne!(old["__Host-monorm-session"], c.jar["__Host-monorm-session"]);
    assert_eq!(count_revocations(&rig).await, 1, "the old session id is revoked");
    let mut copy = Client::new(&rig.router, "10.0.0.1");
    copy.jar = old;
    assert_eq!(copy.get("/admin/api/me").await.status, 401, "the old cookie is dead at once");
    assert_eq!(c.get("/admin/api/me").await.status, 200);
    rig.security.advance(61);
    assert_eq!(c.get("/admin/api/me").await.status, 200, "the changed session knows the new hash");
    assert_eq!(elsewhere.get("/admin/api/me").await.status, 401, "another session of the user ends at its renewal");
}

#[tokio::test]
async fn a_host_failure_answers_500_and_leaves_no_audit_row() {
    struct Broken;
    impl HostBridge for Broken {
        fn call(&self, _: &str, _: Value, _: CallContext) -> BoxFuture<Result<Value, BridgeError>> {
            Box::pin(async { Err(BridgeError::Host { kind: "ValueError".into(), message: "boom".into() }) })
        }
    }
    let rig = rig().await;
    let mut opts = Options::new(None);
    opts.log = monorm_admin::service::Logger::new(|_| {});
    opts.auth = Some(AuthConfig { security: rig.security.clone(), bridge: Arc::new(Broken) });
    let site = Arc::new(Site::build(
        Arc::new(dsl::check(dsl::compile(&format!("datasource db {{\n  provider = \"sqlite\"\n}}\n\nmodel Note {{\n  id Int @id\n}}\n{APP}"), None).unwrap()).unwrap().1),
        &json!({"resources": [{"model": "Note"}]}).to_string(),
    ).unwrap());
    let router = service(site, rig.driver.clone(), opts);
    let mut c = Client::new(&router, "10.0.0.1");
    let reply = c.login("ann", "pw").await;
    assert_eq!((reply.status, reply.body["error"]["kind"].as_str()), (500, Some("HostError")));
    assert!(!reply.body.to_string().contains("boom"), "the host message stays in the log");
    assert!(audit(&rig).await.is_empty());
}

#[tokio::test]
async fn login_and_failed_login_rows_are_written_on_postgres() {
    let Ok(url) = std::env::var("ORM_RUST_TEST_POSTGRES_URL") else {
        eprintln!("skipped: ORM_RUST_TEST_POSTGRES_URL is not set");
        return;
    };
    let driver = db::connect(&url, 2).await.unwrap();
    driver.batch("DROP TABLE IF EXISTS adminlogentry; DROP TABLE IF EXISTS adminsessionrevocation; DROP TABLE IF EXISTS userpreference; DROP TABLE IF EXISTS note".into()).await.unwrap();
    let rig = rig_on(driver, "postgresql", SecurityOptions::default()).await;
    let mut c = Client::new(&rig.router, "10.0.0.1");
    assert_eq!(c.login("ann", "nope").await.status, 401);
    assert_eq!(c.login("ann", "pw").await.status, 200);
    let rows = rig.driver.query_text("SELECT action, record_label, changes::text FROM adminlogentry ORDER BY id".into()).await.unwrap();
    let actions: Vec<_> = rows.iter().map(|r| r[0].clone().unwrap()).collect();
    assert_eq!(actions, ["login_failed", "login"]);
    assert!(rows[0][2].clone().unwrap().contains("10.0.0.1"), "{:?}", rows[0]);
    let other = sibling(&rig, "postgresql").await;
    let mut copy = Client::new(&other.router, "10.0.0.1");
    copy.jar = c.jar.clone();
    assert_eq!(c.post("/admin/api/logout", json!({})).await.status, 200);
    other.security.advance(61);
    assert_eq!(copy.get("/admin/api/me").await.status, 401, "the revocation row is read on PostgreSQL");
    let revoked = rig.driver.query_text("SELECT COUNT(*) FROM adminsessionrevocation".into()).await.unwrap();
    assert_eq!(revoked[0][0].as_deref(), Some("1"));
    rig.driver.batch("DROP TABLE adminlogentry; DROP TABLE adminsessionrevocation; DROP TABLE userpreference; DROP TABLE note".into()).await.unwrap();
}
