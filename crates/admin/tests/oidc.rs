//! OIDC single sign-on through the served router against a fake provider in this file:
//! discovery, the authorization-code callback with state, nonce and PKCE, the ID-token checks,
//! the roles of the claim, the `login` and `login_failed` audit rows and the `sso_error` redirects.
use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::extract::ConnectInfo;
use axum::http::Request;
use axum::Router;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine;
use monorm_admin::oidc::OidcSettings;
use monorm_admin::service::{service, Options};
use monorm_admin::{AuthConfig, BoxFuture, BridgeError, CallContext, HostBridge, Security, SecurityOptions, Site};
use monorm_core::{dsl, migrate, schema::Schema};
use monorm_engine::db::{self, Driver};
use ring::rand::SystemRandom;
use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tower::ServiceExt;

const APP: &str = include_str!("../../../apps/admin/schema.prisma");
const SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";
const CLIENT_ID: &str = "monorm-admin";
const CLIENT_SECRET: &str = "idp-client-secret";
static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
const START: &str = "/admin/auth/start";
const FLOW_COOKIE: &str = "__Secure-monorm-oidc";

// ---------------------------------------------------------------- the fake provider

struct Grant {
    nonce: String,
    challenge: String,
    redirect_uri: String,
    sub: String,
    claims: Map<String, Value>,
}

#[derive(Default)]
struct IdpState {
    codes: HashMap<String, Grant>,
    /// Every request as `METHOD path`.
    requests: Vec<String>,
    /// The form of the last token request.
    last_form: BTreeMap<String, String>,
    fail_token: bool,
    /// Claims that replace the ones of the next tokens; a `null` removes the claim.
    overrides: Map<String, Value>,
    /// The `issuer` that the discovery document names. None: the real one.
    discovery_issuer: Option<String>,
    /// Sign the next tokens with this key instead of the published one.
    rogue_key: bool,
}

struct Idp {
    issuer: String,
    state: Arc<Mutex<IdpState>>,
    key: Arc<EcdsaKeyPair>,
    rogue: Arc<EcdsaKeyPair>,
}

fn key_pair() -> EcdsaKeyPair {
    let rng = SystemRandom::new();
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
    EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng).unwrap()
}

fn sign(pair: &EcdsaKeyPair, claims: &Value) -> String {
    let head = B64.encode(serde_json::to_vec(&json!({"alg": "ES256", "typ": "JWT", "kid": "k1"})).unwrap());
    let body = B64.encode(serde_json::to_vec(claims).unwrap());
    let sig = pair.sign(&SystemRandom::new(), format!("{head}.{body}").as_bytes()).unwrap();
    format!("{head}.{body}.{}", B64.encode(sig.as_ref()))
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

fn form(text: &str) -> BTreeMap<String, String> {
    form_urlencoded::parse(text.as_bytes()).into_owned().collect()
}

async fn read_request(stream: &mut tokio::net::TcpStream) -> (String, String, String) {
    let mut raw = Vec::new();
    let mut chunk = [0u8; 4096];
    let (head_end, length) = loop {
        let n = stream.read(&mut chunk).await.unwrap();
        raw.extend_from_slice(&chunk[..n]);
        if let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&raw[..end]).to_lowercase();
            let length = head.lines().find_map(|l| l.strip_prefix("content-length:")).and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(0);
            break (end + 4, length);
        }
        assert!(n > 0, "the client closed before the header end");
    };
    while raw.len() < head_end + length {
        let n = stream.read(&mut chunk).await.unwrap();
        assert!(n > 0);
        raw.extend_from_slice(&chunk[..n]);
    }
    let head = String::from_utf8_lossy(&raw[..head_end]).to_string();
    let mut first = head.lines().next().unwrap().split(' ');
    let (method, path) = (first.next().unwrap().to_string(), first.next().unwrap().to_string());
    (method, path, String::from_utf8_lossy(&raw[head_end..head_end + length]).to_string())
}

impl Idp {
    async fn start() -> Idp {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let issuer = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let idp = Idp { issuer, state: Default::default(), key: Arc::new(key_pair()), rogue: Arc::new(key_pair()) };
        let (state, issuer, key, rogue) = (idp.state.clone(), idp.issuer.clone(), idp.key.clone(), idp.rogue.clone());
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else { return };
                let (state, issuer, key, rogue) = (state.clone(), issuer.clone(), key.clone(), rogue.clone());
                tokio::spawn(async move {
                    let (method, path, body) = read_request(&mut stream).await;
                    let (status, answer) = route(&state, &issuer, &key, &rogue, &method, &path, &body);
                    let data = answer.to_string();
                    let reply = format!("HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{data}", data.len());
                    let _ = stream.write_all(reply.as_bytes()).await;
                });
            }
        });
        idp
    }

    fn settings(&self, extra: Value) -> Value {
        let mut value = json!({"issuer": self.issuer, "client_id": CLIENT_ID, "client_secret": CLIENT_SECRET});
        value.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        value
    }

    /// The user signs in at the provider: the callback URI (path and query) for the `Location` of the start.
    fn approve(&self, location: &str, sub: &str, state: Option<&str>, claims: Value) -> String {
        let query = form(location.split_once('?').unwrap().1);
        assert_eq!(query["response_type"], "code");
        assert_eq!(query["code_challenge_method"], "S256");
        let code = format!("code-{}", COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst));
        let grant = Grant { nonce: query["nonce"].clone(), challenge: query["code_challenge"].clone(), redirect_uri: query["redirect_uri"].clone(), sub: sub.into(), claims: claims.as_object().unwrap().clone() };
        self.state.lock().unwrap().codes.insert(code.clone(), grant);
        let path = query["redirect_uri"].split_once("://").unwrap().1.split_once('/').unwrap().1;
        format!("/{path}?code={code}&state={}", state.unwrap_or(&query["state"]))
    }

    fn requests(&self) -> Vec<String> {
        self.state.lock().unwrap().requests.clone()
    }
}

fn route(state: &Mutex<IdpState>, issuer: &str, key: &EcdsaKeyPair, rogue: &EcdsaKeyPair, method: &str, path: &str, body: &str) -> (u16, Value) {
    let mut s = state.lock().unwrap();
    s.requests.push(format!("{method} {path}"));
    match (method, path) {
        ("GET", "/.well-known/openid-configuration") => {
            let named = s.discovery_issuer.clone().unwrap_or_else(|| issuer.to_string());
            (200, json!({"issuer": named, "authorization_endpoint": format!("{issuer}/authorize"), "token_endpoint": format!("{issuer}/token"), "jwks_uri": format!("{issuer}/jwks")}))
        }
        ("GET", "/jwks") => {
            let point = key.public_key().as_ref();
            (200, json!({"keys": [{"kty": "EC", "crv": "P-256", "kid": "k1", "use": "sig", "alg": "ES256", "x": B64.encode(&point[1..33]), "y": B64.encode(&point[33..])}]}))
        }
        ("POST", "/token") => {
            if s.fail_token {
                return (500, json!({"error": "server_error"}));
            }
            let f = form(body);
            s.last_form = f.clone();
            let grant = s.codes.remove(f.get("code").map(String::as_str).unwrap_or(""));
            let challenge = B64.encode(Sha256::digest(f.get("code_verifier").map(String::as_str).unwrap_or("").as_bytes()));
            let Some(g) = grant.filter(|g| {
                f.get("client_id").map(String::as_str) == Some(CLIENT_ID) && f.get("client_secret").map(String::as_str) == Some(CLIENT_SECRET) && f.get("redirect_uri") == Some(&g.redirect_uri) && challenge == g.challenge
            }) else {
                return (400, json!({"error": "invalid_grant"}));
            };
            let at = now();
            let mut claims = json!({"iss": issuer, "aud": CLIENT_ID, "sub": g.sub, "iat": at, "exp": at + 300, "nonce": g.nonce});
            let map = claims.as_object_mut().unwrap();
            map.extend(g.claims);
            map.extend(s.overrides.clone());
            map.retain(|_, v| !v.is_null());
            let signer = if s.rogue_key { rogue } else { key };
            (200, json!({"access_token": "at", "token_type": "Bearer", "id_token": sign(signer, &claims)}))
        }
        _ => (404, json!({})),
    }
}

// ---------------------------------------------------------------- the host

/// A host with one inactive user, `off@example.com`. `auth.sso_login` finds or creates the user by `(issuer, subject)`
/// and sets the roles to the claim: the part of the real host that the admin service relies on.
#[derive(Default)]
struct Host {
    users: Mutex<BTreeMap<String, (i64, Vec<String>)>>,
    calls: Mutex<Vec<Value>>,
    /// `Some(true)`: refuse every login (`user` null). `Some(false)`: fail with a host error.
    mode: Mutex<Option<bool>>,
}

impl Host {
    fn roles_of(&self, username: &str) -> Vec<String> {
        self.users.lock().unwrap().get(username).map(|u| u.1.clone()).unwrap_or_default()
    }
}

impl HostBridge for Host {
    fn call(&self, name: &str, payload: Value, _ctx: CallContext) -> BoxFuture<Result<Value, BridgeError>> {
        let result = if name != "auth.sso_login" {
            Err(BridgeError::Bridge(format!("no host function {name}")))
        } else {
            self.calls.lock().unwrap().push(payload.clone());
            match *self.mode.lock().unwrap() {
                Some(true) => Ok(json!({"user": null, "created": false})),
                Some(false) => Err(BridgeError::Host { kind: "ValueError".into(), message: "boom".into() }),
                None => {
                    let username = payload["username"].as_str().unwrap().to_string();
                    let roles: Vec<String> = payload["roles"].as_array().unwrap().iter().map(|r| r.as_str().unwrap().to_string()).collect();
                    let mut users = self.users.lock().unwrap();
                    if username == "off@example.com" {
                        Ok(json!({"user": {"key": 9, "username": username, "is_active": false}, "created": false}))
                    } else {
                        let created = !users.contains_key(&username);
                        let next = users.len() as i64 + 1;
                        let entry = users.entry(username.clone()).or_insert((next, Vec::new()));
                        entry.1 = roles;
                        Ok(json!({"user": {"key": entry.0, "username": username, "is_superuser": false, "is_active": true}, "created": created}))
                    }
                }
            }
        };
        Box::pin(async move { result })
    }
}

// ---------------------------------------------------------------- the rig and the browser

struct Rig {
    router: Router,
    driver: Arc<dyn Driver>,
    host: Arc<Host>,
    idp: Idp,
}

async fn rig_with(extra: Value) -> Rig {
    let idp = Idp::start().await;
    let driver = db::connect("sqlite://:memory:", 1).await.unwrap();
    let source = format!("datasource db {{\n  provider = \"sqlite\"\n}}\n\nmodel Note {{\n  id Int @id\n}}\n{APP}");
    let schema: Arc<Schema> = Arc::new(dsl::check(dsl::compile(&source, None).unwrap()).unwrap().1);
    for step in migrate::plan(&schema, &Default::default()).unwrap().up {
        driver.batch(step.sql).await.unwrap();
    }
    let site = Arc::new(Site::build(schema, &json!({"resources": [{"model": "Note"}]}).to_string()).unwrap());
    let options = SecurityOptions { oidc: Some(OidcSettings::from_value(&idp.settings(extra)).unwrap()), ..Default::default() };
    let security = Security::new(SECRET, options).unwrap();
    let host = Arc::new(Host::default());
    let mut opts = Options::new(None);
    opts.auth = Some(AuthConfig { security, bridge: host.clone() });
    opts.log = monorm_admin::service::Logger::new(|_| {});
    Rig { router: service(site, driver.clone(), opts), driver, host, idp }
}

async fn rig() -> Rig {
    rig_with(json!({})).await
}

struct Reply {
    status: u16,
    headers: axum::http::HeaderMap,
    body: Value,
}

impl Reply {
    fn location(&self) -> String {
        self.headers.get("location").map(|v| v.to_str().unwrap().to_string()).unwrap_or_default()
    }
    fn cookies(&self) -> Vec<String> {
        self.headers.get_all("set-cookie").iter().map(|v| v.to_str().unwrap().to_string()).collect()
    }
}

/// A browser: it keeps the cookies and sends each one only to the `Path` it was set for.
struct Browser {
    router: Router,
    jar: BTreeMap<String, String>,
    paths: BTreeMap<String, String>,
}

impl Browser {
    fn new(rig: &Rig) -> Browser {
        Browser { router: rig.router.clone(), jar: BTreeMap::new(), paths: BTreeMap::new() }
    }

    async fn get(&mut self, uri: &str) -> Reply {
        let path = uri.split('?').next().unwrap();
        let cookie = self.jar.iter().filter(|(k, _)| self.paths.get(*k).is_none_or(|p| path.starts_with(p.as_str()))).map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; ");
        let mut req = Request::builder().method("GET").uri(uri).header("host", "localhost");
        if !cookie.is_empty() {
            req = req.header("cookie", cookie);
        }
        let mut req = req.body(axum::body::Body::empty()).unwrap();
        req.extensions_mut().insert(ConnectInfo("10.0.0.1:4000".parse::<SocketAddr>().unwrap()));
        let (parts, body) = self.router.clone().oneshot(req).await.unwrap().into_parts();
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        let reply = Reply { status: parts.status.as_u16(), headers: parts.headers, body: serde_json::from_slice(&bytes).unwrap_or(Value::Null) };
        for cookie in reply.cookies() {
            let (pair, _) = cookie.split_once(';').unwrap_or((&cookie, ""));
            let (k, v) = pair.split_once('=').unwrap();
            if cookie.contains("Max-Age=0") || v.is_empty() {
                self.jar.remove(k);
                self.paths.remove(k);
            } else {
                self.jar.insert(k.into(), v.into());
                if let Some(p) = cookie.split(';').find_map(|a| a.trim().strip_prefix("Path=")) {
                    self.paths.insert(k.into(), p.into());
                }
            }
        }
        reply
    }

    /// The start, the provider and the callback. Gives the reply of the callback.
    async fn sign_in(&mut self, idp: &Idp, claims: Value) -> Reply {
        self.sign_in_with(idp, None, claims).await
    }

    async fn sign_in_with(&mut self, idp: &Idp, state: Option<&str>, claims: Value) -> Reply {
        let start = self.get(START).await;
        assert_eq!(start.status, 302, "{:?}", start.body);
        let callback = idp.approve(&start.location(), "u-1", state, claims);
        self.get(&callback).await
    }

    async fn logged_in(&mut self) -> bool {
        self.get("/admin/api/me").await.status == 200
    }
}

/// `(action, label, changes)` of every audit row, oldest first.
async fn audit(rig: &Rig) -> Vec<(String, String, Value)> {
    let rows = rig.driver.query_text("SELECT action, record_label, changes FROM \"AdminLogEntry\" ORDER BY id".into()).await.unwrap();
    rows.into_iter()
        .map(|r| {
            let r: Vec<String> = r.into_iter().map(Option::unwrap_or_default).collect();
            (r[0].clone(), r[1].clone(), serde_json::from_str(&r[2]).unwrap_or(Value::Null))
        })
        .collect()
}

async fn user_keys(rig: &Rig) -> Vec<String> {
    let rows = rig.driver.query_text("SELECT user_key FROM \"AdminLogEntry\" ORDER BY id".into()).await.unwrap();
    rows.into_iter().map(|r| r[0].clone().unwrap_or_default()).collect()
}

// ---------------------------------------------------------------- tests

#[tokio::test]
async fn the_start_reads_the_discovery_and_redirects_with_state_nonce_and_pkce() {
    let rig = rig_with(json!({"scopes": ["groups"]})).await;
    let mut b = Browser::new(&rig);
    let reply = b.get(START).await;
    assert_eq!(reply.status, 302);
    let location = reply.location();
    assert!(location.starts_with(&format!("{}/authorize?", rig.idp.issuer)), "{location}");
    let q = form(location.split_once('?').unwrap().1);
    assert_eq!((q["client_id"].as_str(), q["response_type"].as_str(), q["code_challenge_method"].as_str()), (CLIENT_ID, "code", "S256"));
    assert_eq!(q["scope"], "openid profile email groups");
    assert_eq!(q["redirect_uri"], "http://localhost/admin/auth/callback");
    assert!(q["state"].len() >= 22 && q["nonce"].len() >= 22 && q["state"] != q["nonce"]);
    assert_eq!(q["code_challenge"].len(), 43, "a SHA-256 challenge in base64url");
    let cookie = reply.cookies().into_iter().find(|c| c.starts_with(&format!("{FLOW_COOKIE}="))).expect("a flow cookie");
    for flag in ["HttpOnly", "Secure", "Path=/admin/auth;"] {
        assert!(cookie.contains(flag), "{flag} in {cookie}");
    }
    assert_eq!(rig.idp.requests(), ["GET /.well-known/openid-configuration"]);
    b.get(START).await;
    assert_eq!(rig.idp.requests().len(), 1, "the discovery document is cached");
    assert!(audit(&rig).await.is_empty());
}

#[tokio::test]
async fn a_login_trades_the_code_with_the_verifier_creates_the_user_and_ends_the_flow() {
    let rig = rig().await;
    let mut b = Browser::new(&rig);
    let start = b.get(START).await;
    let challenge = form(start.location().split_once('?').unwrap().1)["code_challenge"].clone();
    let done = b.get(&rig.idp.approve(&start.location(), "u-1", None, json!({"email": "new@example.com", "roles": ["editor", "auditor"]}))).await;
    assert_eq!((done.status, done.location().as_str()), (302, "/admin/"));
    let me = b.get("/admin/api/me").await;
    assert_eq!((me.status, me.body["user"]["username"].as_str()), (200, Some("new@example.com")));
    let form = rig.idp.state.lock().unwrap().last_form.clone();
    assert_eq!((form["grant_type"].as_str(), form["client_secret"].as_str()), ("authorization_code", CLIENT_SECRET));
    assert_eq!(B64.encode(Sha256::digest(form["code_verifier"].as_bytes())), challenge, "the verifier of the flow matches the challenge of the start");
    assert!(!b.jar.contains_key(FLOW_COOKIE), "the flow cookie ends with the callback");
    let calls = rig.host.calls.lock().unwrap().clone();
    assert_eq!(calls, [json!({"issuer": rig.idp.issuer, "subject": "u-1", "username": "new@example.com", "email": "new@example.com", "roles": ["auditor", "editor"]})]);
    let rows = audit(&rig).await;
    assert_eq!(rows.len(), 1);
    let (action, label, changes) = &rows[0];
    assert_eq!((action.as_str(), label.as_str()), ("login", "new@example.com"));
    assert_eq!((changes["method"].as_str(), changes["created"].as_bool(), changes["roles"].clone()), (Some("sso"), Some(true), json!(["auditor", "editor"])));
    assert_eq!(user_keys(&rig).await, ["1"]);
}

#[tokio::test]
async fn the_roles_follow_the_claim_at_each_login() {
    let rig = rig().await;
    let mut b = Browser::new(&rig);
    let who = "ann@example.com";
    b.sign_in(&rig.idp, json!({"email": who, "roles": ["editor", "auditor", "editor"]})).await;
    assert_eq!(rig.host.roles_of(who), ["auditor", "editor"], "sorted, no duplicate");
    b.sign_in(&rig.idp, json!({"email": who, "roles": ["auditor"]})).await;
    assert_eq!(rig.host.roles_of(who), ["auditor"]);
    b.sign_in(&rig.idp, json!({"email": who, "roles": "editor"})).await;
    assert_eq!(rig.host.roles_of(who), ["editor"], "one name instead of a list");
    b.sign_in(&rig.idp, json!({"email": who})).await;
    assert!(rig.host.roles_of(who).is_empty(), "no claim, no role");
    let rows = audit(&rig).await;
    assert_eq!(rows.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(), ["login"; 4]);
    assert_eq!(rows[1].2.get("created"), None, "only the first login creates the user");
    assert!(b.logged_in().await);
}

#[tokio::test]
async fn the_roles_claim_can_be_a_nested_path() {
    let rig = rig_with(json!({"roles_claim": "realm.roles"})).await;
    let mut b = Browser::new(&rig);
    b.sign_in(&rig.idp, json!({"email": "ann@example.com", "roles": ["ignored"], "realm": {"roles": ["editor"]}})).await;
    assert_eq!(rig.host.roles_of("ann@example.com"), ["editor"]);
}

#[tokio::test]
async fn the_username_comes_from_the_configured_claim() {
    let rig = rig_with(json!({"username_claim": "preferred_username"})).await;
    let mut b = Browser::new(&rig);
    let done = b.sign_in(&rig.idp, json!({"email": "a@example.com", "preferred_username": "ann"})).await;
    assert_eq!(done.location(), "/admin/");
    assert_eq!(b.get("/admin/api/me").await.body["user"]["username"], "ann");
    let done = b.sign_in(&rig.idp, json!({"email": "a@example.com"})).await;
    assert_eq!(done.location(), "/admin/?sso_error=failed", "no username claim");
}

#[tokio::test]
async fn an_inactive_user_gets_the_inactive_word_and_one_failed_login_row() {
    let rig = rig().await;
    let mut b = Browser::new(&rig);
    let done = b.sign_in(&rig.idp, json!({"email": "off@example.com", "roles": ["editor"]})).await;
    assert_eq!(done.location(), "/admin/?sso_error=inactive");
    assert!(!b.logged_in().await);
    let rows = audit(&rig).await;
    assert_eq!(rows.len(), 1, "one row, not two");
    assert_eq!((rows[0].0.as_str(), rows[0].1.as_str(), rows[0].2["reason"].as_str()), ("login_failed", "off@example.com", Some("inactive")));
    assert_eq!(user_keys(&rig).await, [""], "a failed login names no user");
}

#[tokio::test]
async fn a_user_that_the_host_refuses_gets_the_failed_word_and_a_row() {
    let rig = rig().await;
    *rig.host.mode.lock().unwrap() = Some(true);
    let mut b = Browser::new(&rig);
    let done = b.sign_in(&rig.idp, json!({"email": "ann@example.com"})).await;
    assert_eq!(done.location(), "/admin/?sso_error=failed");
    assert!(!b.logged_in().await);
    let rows = audit(&rig).await;
    assert_eq!((rows.len(), rows[0].0.as_str(), rows[0].2["reason"].as_str()), (1, "login_failed", Some("refused")));
}

#[tokio::test]
async fn a_host_failure_answers_500_and_leaves_no_audit_row() {
    let rig = rig().await;
    *rig.host.mode.lock().unwrap() = Some(false);
    let mut b = Browser::new(&rig);
    let done = b.sign_in(&rig.idp, json!({"email": "ann@example.com"})).await;
    assert_eq!(done.status, 500);
    assert!(audit(&rig).await.is_empty());
    assert!(!b.logged_in().await);
}

#[tokio::test]
async fn a_wrong_state_is_refused_before_the_token_endpoint_is_called() {
    let rig = rig().await;
    let mut b = Browser::new(&rig);
    let done = b.sign_in_with(&rig.idp, Some("not-the-state"), json!({"email": "ann@example.com"})).await;
    assert_eq!(done.location(), "/admin/?sso_error=failed");
    assert!(!rig.idp.requests().iter().any(|r| r.starts_with("POST")), "{:?}", rig.idp.requests());
    let rows = audit(&rig).await;
    assert_eq!((rows.len(), rows[0].0.as_str(), rows[0].2["reason"].as_str(), rows[0].2["method"].as_str()), (1, "login_failed", Some("bad_state"), Some("sso")));
    assert!(rig.host.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_callback_without_a_flow_or_without_a_code_is_refused() {
    let rig = rig().await;
    let mut b = Browser::new(&rig);
    assert_eq!(b.get("/admin/auth/callback?code=x&state=y").await.location(), "/admin/?sso_error=failed");
    let start = b.get(START).await;
    let state = form(start.location().split_once('?').unwrap().1)["state"].clone();
    assert_eq!(b.get(&format!("/admin/auth/callback?state={state}")).await.location(), "/admin/?sso_error=failed");
    let reasons: Vec<String> = audit(&rig).await.iter().map(|r| r.2["reason"].as_str().unwrap().to_string()).collect();
    assert_eq!(reasons, ["no_flow", "no_code"]);
}

#[tokio::test]
async fn a_flow_cookie_that_the_server_did_not_sign_is_refused() {
    let rig = rig().await;
    let mut b = Browser::new(&rig);
    let start = b.get(START).await;
    let callback = rig.idp.approve(&start.location(), "u-1", None, json!({"email": "ann@example.com"}));
    b.jar.insert(FLOW_COOKIE.into(), format!("x{}", b.jar[FLOW_COOKIE]));
    assert_eq!(b.get(&callback).await.location(), "/admin/?sso_error=failed", "a flow cookie that the server did not sign");
    assert_eq!(audit(&rig).await[0].2["reason"], "no_flow");
}

#[tokio::test]
async fn a_code_works_once() {
    let rig = rig().await;
    let mut b = Browser::new(&rig);
    let start = b.get(START).await;
    let callback = rig.idp.approve(&start.location(), "u-1", None, json!({"email": "ann@example.com"}));
    let flow = b.jar[FLOW_COOKIE].clone();
    assert_eq!(b.get(&callback).await.location(), "/admin/");
    b.jar.clear();
    b.jar.insert(FLOW_COOKIE.into(), flow);
    assert_eq!(b.get(&callback).await.location(), "/admin/?sso_error=unavailable", "the provider answers invalid_grant");
    assert!(!b.logged_in().await);
    let rows = audit(&rig).await;
    assert_eq!((rows[1].0.as_str(), rows[1].2["reason"].as_str()), ("login_failed", Some("exchange_failed")));
}

#[tokio::test]
async fn a_token_with_a_wrong_claim_or_signature_is_refused_with_its_own_reason() {
    let cases: Vec<(&str, Value, &str)> = vec![
        ("nonce", json!({"nonce": "other"}), "bad_nonce"),
        ("audience", json!({"aud": "someone-else"}), "bad_audience"),
        ("issuer", json!({"iss": "https://evil.example.com"}), "bad_issuer"),
        ("expiry", json!({"exp": 1}), "expired"),
        ("subject", json!({"sub": null}), "no_subject"),
        ("email", json!({"email": null}), "no_username"),
        ("unverified email", json!({"email_verified": false}), "email_unverified"),
    ];
    for (name, overrides, reason) in cases {
        let rig = rig().await;
        rig.idp.state.lock().unwrap().overrides = overrides.as_object().unwrap().clone();
        let mut b = Browser::new(&rig);
        let done = b.sign_in(&rig.idp, json!({"email": "ann@example.com", "roles": ["editor"]})).await;
        assert_eq!(done.location(), "/admin/?sso_error=failed", "{name}");
        assert!(!b.logged_in().await, "{name}");
        let rows = audit(&rig).await;
        assert_eq!((rows.len(), rows[0].0.as_str(), rows[0].2["reason"].as_str()), (1, "login_failed", Some(reason)), "{name}");
        assert!(rig.host.calls.lock().unwrap().is_empty(), "{name}: the host is not asked");
    }
    let rig = rig().await;
    rig.idp.state.lock().unwrap().rogue_key = true;
    let mut b = Browser::new(&rig);
    let done = b.sign_in(&rig.idp, json!({"email": "ann@example.com"})).await;
    assert_eq!(done.location(), "/admin/?sso_error=failed");
    assert_eq!(audit(&rig).await[0].2["reason"], "bad_signature");
    let jwks = rig.idp.requests().iter().filter(|r| *r == "GET /jwks").count();
    assert_eq!(jwks, 1, "the key set just fetched is not fetched again inside the refresh window");
}

#[tokio::test]
async fn the_hosted_domain_is_checked_on_the_server() {
    let rig = rig_with(json!({"hd": "example.com"})).await;
    let mut b = Browser::new(&rig);
    assert_eq!(b.sign_in(&rig.idp, json!({"email": "ann@example.com", "hd": "other.com"})).await.location(), "/admin/?sso_error=domain");
    assert_eq!(b.sign_in(&rig.idp, json!({"email": "ann@example.com"})).await.location(), "/admin/?sso_error=domain");
    assert!(!b.logged_in().await);
    assert_eq!(b.sign_in(&rig.idp, json!({"email": "ann@example.com", "hd": "example.com"})).await.location(), "/admin/");
    assert!(b.logged_in().await);
    let actions: Vec<String> = audit(&rig).await.into_iter().map(|r| r.0).collect();
    assert_eq!(actions, ["login_failed", "login_failed", "login"]);
}

#[tokio::test]
async fn a_provider_error_and_a_failing_token_endpoint_send_the_browser_back() {
    let rig = rig().await;
    let mut b = Browser::new(&rig);
    assert_eq!(b.get("/admin/auth/callback?error=access_denied").await.location(), "/admin/?sso_error=denied");
    rig.idp.state.lock().unwrap().fail_token = true;
    assert_eq!(b.sign_in(&rig.idp, json!({"email": "ann@example.com"})).await.location(), "/admin/?sso_error=unavailable");
    assert!(!b.logged_in().await);
    let reasons: Vec<String> = audit(&rig).await.iter().map(|r| r.2["reason"].as_str().unwrap().to_string()).collect();
    assert_eq!(reasons, ["provider:access_denied", "exchange_failed"]);
}

#[tokio::test]
async fn a_discovery_document_that_names_another_issuer_is_refused() {
    let rig = rig().await;
    rig.idp.state.lock().unwrap().discovery_issuer = Some("https://evil.example.com".into());
    let mut b = Browser::new(&rig);
    let reply = b.get(START).await;
    assert_eq!((reply.status, reply.location().as_str()), (302, "/admin/?sso_error=unavailable"));
    assert!(!b.jar.contains_key(FLOW_COOKIE));
}

#[tokio::test]
async fn an_unreachable_provider_sends_the_browser_back_at_the_start() {
    let rig = rig().await;
    let dead = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = dead.local_addr().unwrap().port();
    drop(dead);
    let site_rig = rig_with(json!({"discovery_url": format!("http://127.0.0.1:{port}/.well-known/openid-configuration")})).await;
    let mut b = Browser::new(&site_rig);
    assert_eq!(b.get(START).await.location(), "/admin/?sso_error=unavailable");
    drop(rig);
}

#[tokio::test]
async fn the_login_page_names_the_sso_start() {
    let rig = rig().await;
    let mut b = Browser::new(&rig);
    assert_eq!(b.get("/admin/api/login").await.body["sso"], START);
}
