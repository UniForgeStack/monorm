//! OIDC single sign-on: the authorization-code flow with state, nonce and PKCE, and a verified ID token.
//!
//! `<base>/auth/start` redirects the browser to the provider. The state, the nonce and the PKCE verifier wait in a
//! signed cookie that only `<base>/auth/` sees. `<base>/auth/callback` checks the state, trades the code for an ID token
//! at the token endpoint, verifies the token (signature, issuer, audience, expiry, nonce, `hd`) and asks the host
//! (`auth.sso_login`) to find or create the user and set the role membership from the roles claim.
//! The audit row, the user and the role membership commit in one transaction.
//!
//! The provider is read from its discovery document, over `reqwest` (rustls, system-free root store, no redirect, a time
//! and a size limit). `jsonwebtoken` checks the signature.
//! Only RS256 and ES256 tokens are accepted, as the key set of the provider lists them.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use monorm_engine::db::Executor;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::audit::{self, Entry};
use crate::auth::{client_ip, database_error, host_error, password_hash, session_user, user_key, with_cookies, AuthConfig};
use crate::bridge::CallContext;
use crate::scope::Scoped;
use crate::security::{self, cookie_value, random, Session, Stamp};
use crate::service::{ErrorNote, RequestId};
use crate::site::Site;

/// The host function that finds or creates the user of a verified ID token and sets the roles:
/// `{issuer, subject, username, email, roles}` gives `{user, created}`; `user` is null when the host refuses the login.
pub const SSO_LOGIN: &str = "auth.sso_login";

/// A login flow ends this many seconds after its start.
const FLOW_SECS: i64 = 10 * 60;
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RESPONSE: usize = 1 << 20;
const DISCOVERY_TTL: Duration = Duration::from_secs(60 * 60);
const JWKS_TTL: Duration = Duration::from_secs(10 * 60);
/// A key set is fetched again for an unknown `kid` at most this often.
const JWKS_REFRESH: Duration = Duration::from_secs(30);
const LEEWAY: i64 = 60;
const MAX_NAME: usize = 150;
const MAX_ROLES: usize = 200;

/// The `[admin.oidc]` settings. The client secret is never a setting: `MONORM_ADMIN_OIDC_CLIENT_SECRET` holds it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OidcSettings {
    /// The issuer URL. The `iss` claim of every token must equal it.
    pub issuer: String,
    pub client_id: String,
    #[serde(default, skip_serializing)]
    pub client_secret: Option<String>,
    /// The claim that lists the roles of the user (a list of names, or one name). A dotted path reads a nested claim.
    #[serde(default = "default_roles_claim")]
    pub roles_claim: String,
    /// Scopes to ask for besides `openid profile email`, such as the roles scope of the provider.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// The only `hd` (hosted domain) claim that may log in. None: any.
    #[serde(default)]
    pub hd: Option<String>,
    /// The claim that names the user: the username of the user row.
    #[serde(default = "default_username_claim")]
    pub username_claim: String,
    /// Where the discovery document is. None: `<issuer>/.well-known/openid-configuration`.
    #[serde(default)]
    pub discovery_url: Option<String>,
    /// The origin the provider sends the browser back to (`https://admin.example.com`). None: the `Host` of the request.
    #[serde(default)]
    pub public_url: Option<String>,
    /// A visitor with no session goes to the provider at once, with no sign-in form. The form stays at `<base>/-/login` and `<base>/?password=1`.
    #[serde(default)]
    pub auto_start: bool,
}

fn default_roles_claim() -> String {
    "roles".into()
}

fn default_username_claim() -> String {
    "email".into()
}

impl OidcSettings {
    /// The settings of a JSON object, checked. A wrong key or value names itself.
    pub fn from_value(value: &Value) -> Result<OidcSettings, String> {
        let settings: OidcSettings = serde_json::from_value(value.clone()).map_err(|e| format!("oidc: {e}"))?;
        settings.validate()?;
        Ok(settings)
    }

    pub fn validate(&self) -> Result<(), String> {
        parse_url(&self.issuer).map_err(|e| format!("oidc.issuer: {e}"))?;
        if let Some(url) = &self.discovery_url {
            parse_url(url).map_err(|e| format!("oidc.discovery_url: {e}"))?;
        }
        if let Some(origin) = &self.public_url {
            let url = parse_url(origin).map_err(|e| format!("oidc.public_url: {e}"))?;
            if url.path() != "/" || url.query().is_some() {
                return Err("oidc.public_url must be an origin with no path".into());
            }
        }
        for (name, value) in [("client_id", &self.client_id), ("roles_claim", &self.roles_claim), ("username_claim", &self.username_claim)] {
            if value.trim().is_empty() {
                return Err(format!("oidc.{name} must not be empty"));
            }
        }
        if self.scopes.iter().any(|s| s.is_empty() || s.contains(char::is_whitespace)) {
            return Err("oidc.scopes must hold scope names with no spaces".into());
        }
        if self.hd.as_deref().is_some_and(|h| h.trim().is_empty()) {
            return Err("oidc.hd must not be empty".into());
        }
        Ok(())
    }

    fn scope(&self) -> String {
        let mut scopes: Vec<&str> = vec!["openid", "profile", "email"];
        for s in &self.scopes {
            if !scopes.contains(&s.as_str()) {
                scopes.push(s);
            }
        }
        scopes.join(" ")
    }
}

// ---------------------------------------------------------------- URL and HTTP

fn is_loopback(host: &str) -> bool {
    host == "localhost" || host == "127.0.0.1" || host == "::1"
}

/// `https://host[:port]/path`, or `http://` for a loopback host only (a fake provider in a test).
fn parse_url(text: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(text).map_err(|_| format!("{text:?} is not a URL (use https://...)"))?;
    if text.bytes().any(|b| b <= b' ' || b == 0x7f) || !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err(format!("{text:?} is not a plain URL"));
    }
    let loopback = is_loopback(url.host_str().unwrap_or("").trim_matches(['[', ']']));
    match url.scheme() {
        "https" if url.host_str().is_some() => Ok(url),
        "http" if loopback => Ok(url),
        "http" => Err(format!("{text:?} must use https (http is allowed for localhost only)")),
        _ => Err(format!("{text:?} must start with https://")),
    }
}

/// One request: no redirect, no proxy from the environment, a time limit and a size limit. Gives the status and the body.
async fn http(url: &str, form: Option<String>) -> Result<(u16, Vec<u8>), String> {
    let url = parse_url(url)?;
    let host = url.host_str().unwrap_or_default().to_string();
    let client = reqwest::Client::builder().timeout(HTTP_TIMEOUT).redirect(reqwest::redirect::Policy::none()).no_proxy().user_agent("monorm-admin").build().map_err(|e| e.to_string())?;
    let request = match form {
        Some(body) => client.post(url).header(header::CONTENT_TYPE, "application/x-www-form-urlencoded").body(body),
        None => client.get(url),
    };
    let fail = |e: reqwest::Error| format!("{host}: {}", e.without_url());
    let mut response = request.header(header::ACCEPT, "application/json").send().await.map_err(fail)?;
    let status = response.status().as_u16();
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(fail)? {
        if body.len() + chunk.len() > MAX_RESPONSE {
            return Err("the answer is too large".into());
        }
        body.extend_from_slice(&chunk);
    }
    Ok((status, body))
}

async fn get_json(url: &str) -> Result<Value, String> {
    let (status, body) = http(url, None).await?;
    if status != 200 {
        return Err(format!("{url} answered {status}"));
    }
    serde_json::from_slice(&body).map_err(|_| format!("{url} did not answer JSON"))
}

// ---------------------------------------------------------------- ID token

/// The reason a token or a login is refused: a fixed word, for the audit row and the log.
pub type Reason = &'static str;

fn segment(text: &str) -> Option<Vec<u8>> {
    B64.decode(text).ok()
}

/// The usable keys of the key set for the header of a token.
fn keys_for(jwks: &Value, kid: Option<&str>, alg: &str) -> Vec<DecodingKey> {
    let Some(list) = jwks["keys"].as_array() else { return Vec::new() };
    let mut found = Vec::new();
    for key in list {
        if key["use"].as_str().is_some_and(|u| u != "sig") || key["alg"].as_str().is_some_and(|a| a != alg) {
            continue;
        }
        if kid.is_some() && key["kid"].as_str() != kid {
            continue;
        }
        let parts = |a: &str, b: &str| key[a].as_str().zip(key[b].as_str());
        match (alg, key["kty"].as_str()) {
            ("RS256", Some("RSA")) => found.extend(parts("n", "e").and_then(|(n, e)| DecodingKey::from_rsa_components(n, e).ok())),
            ("ES256", Some("EC")) if key["crv"].as_str() == Some("P-256") => found.extend(parts("x", "y").and_then(|(x, y)| DecodingKey::from_ec_components(x, y).ok())),
            _ => {}
        }
    }
    found
}

/// The claims of `token` when its signature is one of `jwks` and the claims fit `settings` and `nonce`.
/// `unknown_key` means no key of the set fits the header: the key set may be old.
pub fn verify_id_token(token: &str, jwks: &Value, settings: &OidcSettings, nonce: &str, now: i64) -> Result<Value, Reason> {
    let mut parts = token.split('.');
    let (Some(head), Some(_), Some(_), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else { return Err("bad_token") };
    let header: Value = serde_json::from_slice(&segment(head).ok_or("bad_token")?).map_err(|_| "bad_token")?;
    let alg = header["alg"].as_str().unwrap_or("");
    let algorithm = match alg {
        "RS256" => Algorithm::RS256,
        "ES256" => Algorithm::ES256,
        _ => return Err("bad_algorithm"),
    };
    let keys = keys_for(jwks, header["kid"].as_str(), alg);
    if keys.is_empty() {
        return Err("unknown_key");
    }
    // The library checks the signature only; the claims are checked below, with the reasons of the audit log.
    let mut validation = Validation::new(algorithm);
    validation.required_spec_claims.clear();
    validation.validate_exp = false;
    validation.validate_aud = false;
    let claims = keys.iter().find_map(|key| jsonwebtoken::decode::<Value>(token, key, &validation).ok()).ok_or("bad_signature")?.claims;
    check_claims(&claims, settings, nonce, now)?;
    Ok(claims)
}

fn check_claims(claims: &Value, settings: &OidcSettings, nonce: &str, now: i64) -> Result<(), Reason> {
    if claims["iss"].as_str() != Some(settings.issuer.as_str()) {
        return Err("bad_issuer");
    }
    let audiences: Vec<&str> = match &claims["aud"] {
        Value::String(a) => vec![a.as_str()],
        Value::Array(list) => list.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    if !audiences.contains(&settings.client_id.as_str()) {
        return Err("bad_audience");
    }
    if audiences.len() > 1 && claims["azp"].as_str() != Some(settings.client_id.as_str()) {
        return Err("bad_audience");
    }
    match claims["exp"].as_i64() {
        Some(exp) if exp + LEEWAY > now => {}
        _ => return Err("expired"),
    }
    if claims["iat"].as_i64().is_some_and(|iat| iat > now + LEEWAY) {
        return Err("bad_time");
    }
    if claims["nbf"].as_i64().is_some_and(|nbf| nbf > now + LEEWAY) {
        return Err("bad_time");
    }
    if !claims["nonce"].as_str().is_some_and(|n| bool::from(n.as_bytes().ct_eq(nonce.as_bytes()))) {
        return Err("bad_nonce");
    }
    if claims["sub"].as_str().is_none_or(str::is_empty) {
        return Err("no_subject");
    }
    if let Some(hd) = &settings.hd {
        if !claims["hd"].as_str().is_some_and(|c| c.eq_ignore_ascii_case(hd)) {
            return Err("wrong_domain");
        }
    }
    Ok(())
}

/// What the host gets about a verified login.
#[derive(Debug, PartialEq)]
pub struct Identity {
    pub subject: String,
    pub username: String,
    pub email: String,
    pub roles: Vec<String>,
}

fn claim<'a>(claims: &'a Value, path: &str) -> Option<&'a Value> {
    if let Some(v) = claims.get(path) {
        return Some(v);
    }
    path.split('.').try_fold(claims, |v, part| v.get(part))
}

pub fn identity(claims: &Value, settings: &OidcSettings) -> Result<Identity, Reason> {
    let username = claim(claims, &settings.username_claim).and_then(Value::as_str).map(str::trim).filter(|u| !u.is_empty() && u.len() <= MAX_NAME).ok_or("no_username")?.to_string();
    let email = claims["email"].as_str().unwrap_or_default().to_string();
    // An email the provider did not verify must not name an account.
    if settings.username_claim == "email" && claims["email_verified"].as_bool() == Some(false) {
        return Err("email_unverified");
    }
    let mut roles: Vec<String> = match claim(claims, &settings.roles_claim) {
        Some(Value::Array(list)) => list.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        Some(Value::String(one)) => vec![one.clone()],
        _ => Vec::new(),
    };
    roles.retain(|r| !r.is_empty() && r.len() <= MAX_NAME);
    roles.sort();
    roles.dedup();
    roles.truncate(MAX_ROLES);
    Ok(Identity { subject: claims["sub"].as_str().unwrap_or_default().to_string(), username, email, roles })
}

// ---------------------------------------------------------------- the provider

struct Discovery {
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
}

struct Cached<T> {
    value: T,
    /// A monotonic stamp, so a jump of the wall clock cannot extend or shorten the cache; a test ages it with `Security::advance`.
    at: Stamp,
}

/// The routes and the state of one site's SSO.
pub struct Oidc {
    settings: OidcSettings,
    auth: AuthConfig,
    site: Arc<Site>,
    conn: Arc<dyn Executor>,
    discovery: tokio::sync::Mutex<Option<Cached<Arc<Discovery>>>>,
    jwks: tokio::sync::Mutex<Option<Cached<Value>>>,
}

/// The two SSO routes, or none when the site has no `oidc` settings.
pub fn routes(auth: &AuthConfig, site: Arc<Site>, conn: Arc<dyn Executor>) -> Option<Router> {
    let settings = auth.security.options.oidc.clone()?;
    let base = site.base_path.clone();
    let oidc = Arc::new(Oidc { settings, auth: auth.clone(), site, conn, discovery: Default::default(), jwks: Default::default() });
    Some(Router::new().route(&format!("{base}/auth/start"), get(start)).route(&format!("{base}/auth/callback"), get(callback)).with_state(oidc))
}

impl Oidc {
    fn base(&self) -> &str {
        &self.site.base_path
    }

    async fn discovery(&self) -> Result<Arc<Discovery>, String> {
        let mut slot = self.discovery.lock().await;
        if let Some(c) = slot.as_ref().filter(|c| self.auth.security.age(c.at) < DISCOVERY_TTL) {
            return Ok(c.value.clone());
        }
        let url = self.settings.discovery_url.clone().unwrap_or_else(|| format!("{}/.well-known/openid-configuration", self.settings.issuer.trim_end_matches('/')));
        let doc = get_json(&url).await?;
        if doc["issuer"].as_str() != Some(self.settings.issuer.as_str()) {
            return Err(format!("the provider names the issuer {:?}, and the setting is {:?}", doc["issuer"].as_str().unwrap_or(""), self.settings.issuer));
        }
        let endpoint = |name: &str| -> Result<String, String> {
            let url = doc[name].as_str().ok_or_else(|| format!("the discovery document has no {name}"))?;
            parse_url(url).map_err(|e| format!("{name}: {e}"))?;
            Ok(url.to_string())
        };
        let found = Arc::new(Discovery { authorization_endpoint: endpoint("authorization_endpoint")?, token_endpoint: endpoint("token_endpoint")?, jwks_uri: endpoint("jwks_uri")? });
        *slot = Some(Cached { value: found.clone(), at: self.auth.security.stamp() });
        Ok(found)
    }

    /// The key set, cached. `force` fetches it again when the cache is older than `JWKS_REFRESH`.
    async fn jwks(&self, d: &Discovery, force: bool) -> Result<Value, String> {
        let mut slot = self.jwks.lock().await;
        if let Some(c) = slot.as_ref() {
            let age = self.auth.security.age(c.at);
            if (!force && age < JWKS_TTL) || (force && age < JWKS_REFRESH) {
                return Ok(c.value.clone());
            }
        }
        let keys = get_json(&d.jwks_uri).await?;
        *slot = Some(Cached { value: keys.clone(), at: self.auth.security.stamp() });
        Ok(keys)
    }

    /// The origin the browser returns to: the setting, or the `Host` of the request (`http` for a loopback host, else `https`).
    fn redirect_uri(&self, req: &Request) -> Option<String> {
        let origin = match &self.settings.public_url {
            Some(url) => url.trim_end_matches('/').to_string(),
            None => {
                let host = req.headers().get(header::HOST)?.to_str().ok()?;
                if host.is_empty() || !host.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']')) {
                    return None;
                }
                let name = host.trim_start_matches('[').split([']', ':']).next().unwrap_or("");
                format!("{}://{host}", if is_loopback(name) { "http" } else { "https" })
            }
        };
        Some(format!("{origin}{}/auth/callback", self.base()))
    }

    /// The browser goes back to the login page with a word that the page words for the user.
    fn back(&self, word: &str, cookies: &[String]) -> Response {
        redirect(&format!("{}/?sso_error={word}", self.base()), cookies)
    }

    fn ended_flow(&self) -> String {
        security::flow_cookie(self.base(), "", 0)
    }

    async fn exchange(&self, d: &Discovery, code: &str, verifier: &str, redirect_uri: &str) -> Result<String, String> {
        let body = {
            let mut form = form_urlencoded::Serializer::new(String::new());
            form.append_pair("grant_type", "authorization_code").append_pair("code", code).append_pair("redirect_uri", redirect_uri).append_pair("client_id", &self.settings.client_id).append_pair("code_verifier", verifier);
            if let Some(secret) = &self.settings.client_secret {
                form.append_pair("client_secret", secret);
            }
            form.finish()
        };
        let (status, body) = http(&d.token_endpoint, Some(body)).await?;
        let answer: Value = serde_json::from_slice(&body).map_err(|_| format!("the token endpoint answered {status} with no JSON"))?;
        if status != 200 {
            return Err(format!("the token endpoint answered {status}: {}", answer["error"].as_str().unwrap_or("no error code")));
        }
        answer["id_token"].as_str().map(str::to_string).ok_or_else(|| "the token answer has no id_token".to_string())
    }

    /// The claims of a token that passed every check. A key set that does not know the `kid` is fetched once more.
    async fn verified(&self, d: &Discovery, token: &str, nonce: &str) -> Result<Value, Reason> {
        let now = self.auth.security.now();
        let jwks = self.jwks(d, false).await.map_err(|_| "provider_unreachable")?;
        match verify_id_token(token, &jwks, &self.settings, nonce, now) {
            Err("unknown_key") | Err("bad_signature") => {
                let fresh = self.jwks(d, true).await.map_err(|_| "provider_unreachable")?;
                verify_id_token(token, &fresh, &self.settings, nonce, now)
            }
            other => other,
        }
    }

    /// One failed login in the audit log. Best effort: the redirect does not wait for a database that fails.
    async fn record_failure(&self, reason: &str, label: &str, ip: &str, request_id: &str) {
        let Ok(scope) = Scoped::begin(self.conn.as_ref()).await else { return };
        let changes = Some(json!({"method": "sso", "reason": reason, "ip": ip, "request_id": request_id}));
        let entry = Entry { user_key: "", action: "login_failed", resource: "user", record_key: "", record_label: label, changes };
        match audit::record(&self.site, scope.executor(), entry).await {
            Ok(()) => {
                let _ = scope.commit().await;
            }
            Err(_) => scope.rollback().await,
        }
    }
}

/// The longest `next` path that the flow cookie keeps.
const MAX_NEXT: usize = 1024;

/// A `next` path that stays under the base path, or None. The path goes into a `Location` header, so it is a path of this origin and nothing else:
/// it names `base` or goes below `base/`, has no dot segment, space, backslash or control character, is at most `MAX_NEXT` bytes, and does not re-enter `base/auth/`.
fn safe_next(base: &str, raw: &str) -> Option<String> {
    if raw.len() > MAX_NEXT || raw.chars().any(|c| c.is_control() || c == '\\' || c == ' ') {
        return None;
    }
    let path = raw.split(['?', '#']).next().unwrap_or("");
    let rest = path.strip_prefix(base)?;
    if path.starts_with("//") || !(rest.is_empty() || rest.starts_with('/')) || rest == "/auth" || rest.starts_with("/auth/") {
        return None;
    }
    // The dot segments of the WHATWG URL rule: a browser reads each of these as `.` or `..`.
    if path.split('/').any(|s| [".", "%2e", "..", ".%2e", "%2e.", "%2e%2e"].iter().any(|d| s.eq_ignore_ascii_case(d))) {
        return None;
    }
    Some(raw.to_string())
}

fn redirect(to: &str, cookies: &[String]) -> Response {
    let mut response = (StatusCode::FOUND, "").into_response();
    if let Ok(v) = HeaderValue::from_str(to) {
        response.headers_mut().insert(header::LOCATION, v);
    }
    with_cookies(response, cookies)
}

fn noted(mut response: Response, note: String) -> Response {
    response.extensions_mut().insert(ErrorNote(note));
    response
}

/// The word of the login page for a reason.
fn word(reason: &str) -> &'static str {
    match reason {
        "inactive" => "inactive",
        "wrong_domain" => "domain",
        "provider_unreachable" | "exchange_failed" => "unavailable",
        r if r.starts_with("provider:") => "denied",
        _ => "failed",
    }
}

async fn start(State(o): State<Arc<Oidc>>, req: Request) -> Response {
    let Some(redirect_uri) = o.redirect_uri(&req) else {
        return o.back("failed", &[]);
    };
    let d = match o.discovery().await {
        Ok(d) => d,
        Err(e) => return noted(o.back("unavailable", &[]), format!("oidc discovery: {e}")),
    };
    let (state, nonce, verifier) = (random(), random(), random());
    let next = req.uri().query().and_then(|q| form_urlencoded::parse(q.as_bytes()).find(|(k, _)| k == "next")).and_then(|(_, v)| safe_next(o.base(), &v));
    let challenge = B64.encode(Sha256::digest(verifier.as_bytes()));
    let sec = &o.auth.security;
    let mut flow = json!({"state": state, "nonce": nonce, "verifier": verifier, "redirect_uri": redirect_uri, "exp": sec.now() + FLOW_SECS});
    if let Some(next) = next {
        flow["next"] = json!(next);
    }
    let mut query = form_urlencoded::Serializer::new(String::new());
    query
        .append_pair("response_type", "code")
        .append_pair("client_id", &o.settings.client_id)
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("scope", &o.settings.scope())
        .append_pair("state", &state)
        .append_pair("nonce", &nonce)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256");
    let sep = if d.authorization_endpoint.contains('?') { '&' } else { '?' };
    let to = format!("{}{sep}{}", d.authorization_endpoint, query.finish());
    redirect(&to, &[security::flow_cookie(o.base(), &sec.sign_flow(&flow), FLOW_SECS)])
}

async fn callback(State(o): State<Arc<Oidc>>, req: Request) -> Response {
    let sec = &o.auth.security;
    let ip = client_ip(&req, &sec.options.trusted_proxies);
    let request_id = req.extensions().get::<RequestId>().map(|r| r.0.clone()).unwrap_or_default();
    let ended = [o.ended_flow()];
    let limit = format!("sso:{ip}");
    if sec.ip_limit().reserve(&limit, sec.now()).is_some() {
        return o.back("too_many", &ended);
    }
    let params: Vec<(String, String)> = form_urlencoded::parse(req.uri().query().unwrap_or("").as_bytes()).into_owned().collect();
    let param = |name: &str| params.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str());
    let flow = req.headers().get(header::COOKIE).and_then(|c| c.to_str().ok()).and_then(|c| cookie_value(c, security::FLOW_COOKIE)).and_then(|t| sec.read_flow(t));
    // Checked again here on purpose: the cookie is signed, and this keeps the redirect safe if a later change to the flow cookie lets an unchecked value in.
    let next = flow.as_ref().and_then(|f| f["next"].as_str()).and_then(|n| safe_next(o.base(), n));
    let result = finish(&o, &params_view(&param), flow, &ip, &request_id).await;
    match result {
        Ok(session) => {
            sec.ip_limit().release(&limit);
            let csrf = sec.csrf_token(&session.sid);
            let max_age = sec.remaining(&session);
            let cookies = [security::session_cookie(o.base(), &sec.sign_session(&session), max_age), security::csrf_cookie(o.base(), &csrf, max_age), o.ended_flow()];
            redirect(&next.unwrap_or_else(|| format!("{}/", o.base())), &cookies)
        }
        Err(Failure::Server(response)) => {
            sec.ip_limit().release(&limit);
            with_cookies(response, &ended)
        }
        Err(Failure::Audited(w)) => {
            sec.ip_limit().release(&limit);
            o.back(w, &ended)
        }
        Err(Failure::Refused { reason, label, note }) => {
            o.record_failure(&reason, &label, &ip, &request_id).await;
            let response = o.back(word(&reason), &ended);
            noted(response, note.unwrap_or_else(|| format!("sso login refused: {reason}")))
        }
    }
}

/// The callback parameters that matter.
struct Params<'a> {
    state: Option<&'a str>,
    code: Option<&'a str>,
    error: Option<&'a str>,
}

fn params_view<'a>(param: &impl Fn(&str) -> Option<&'a str>) -> Params<'a> {
    Params { state: param("state"), code: param("code"), error: param("error") }
}

enum Failure {
    /// A failed login, written to the audit log.
    Refused { reason: String, label: String, note: Option<String> },
    /// A failed login that the audit row of this request already holds: only the word for the page is left.
    Audited(&'static str),
    /// The database or the host failed: a 500, no audit row (the transaction rolled back).
    Server(Response),
}

fn refused(reason: &str) -> Failure {
    Failure::Refused { reason: reason.to_string(), label: String::new(), note: None }
}

async fn finish(o: &Oidc, p: &Params<'_>, flow: Option<Value>, ip: &str, request_id: &str) -> Result<Session, Failure> {
    let sec = &o.auth.security;
    if let Some(error) = p.error {
        let code: String = error.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').take(40).collect();
        return Err(refused(&format!("provider:{code}")));
    }
    let flow = flow.ok_or_else(|| refused("no_flow"))?;
    let state_ok = match (p.state, flow["state"].as_str()) {
        (Some(a), Some(b)) => bool::from(a.as_bytes().ct_eq(b.as_bytes())),
        _ => false,
    };
    if !state_ok {
        return Err(refused("bad_state"));
    }
    let code = p.code.filter(|c| !c.is_empty()).ok_or_else(|| refused("no_code"))?;
    let (nonce, verifier, redirect_uri) = (flow["nonce"].as_str().unwrap_or_default(), flow["verifier"].as_str().unwrap_or_default(), flow["redirect_uri"].as_str().unwrap_or_default());
    let d = o.discovery().await.map_err(|e| Failure::Refused { reason: "provider_unreachable".into(), label: String::new(), note: Some(format!("oidc discovery: {e}")) })?;
    let token = o.exchange(&d, code, verifier, redirect_uri).await.map_err(|e| Failure::Refused { reason: "exchange_failed".into(), label: String::new(), note: Some(format!("oidc token exchange: {e}")) })?;
    let claims = o.verified(&d, &token, nonce).await.map_err(refused)?;
    let who = identity(&claims, &o.settings).map_err(refused)?;
    let scope = Scoped::begin(o.conn.as_ref()).await.map_err(|e| Failure::Server(database_error(&e.message)))?;
    let ctx = CallContext { request_id: sec.call_id(), scope: json!({}), tx: Some(scope.transaction()) };
    let payload = json!({"issuer": o.settings.issuer, "subject": who.subject, "username": who.username, "email": who.email, "roles": who.roles});
    let out = match o.auth.bridge.call(SSO_LOGIN, payload, ctx).await {
        Ok(v) => v,
        Err(e) => {
            scope.rollback().await;
            return Err(Failure::Server(host_error(&e)));
        }
    };
    let found = session_user(&out["user"]);
    let hash = password_hash(&out["user"]).map(str::to_owned);
    let mut changes = json!({"method": "sso", "ip": ip, "request_id": request_id, "roles": who.roles});
    if out["created"].as_bool() == Some(true) {
        changes["created"] = json!(true);
    }
    let entry = match &found {
        Some(user) => Entry { user_key: &user_key(user), action: "login", resource: "user", record_key: &user_key(user), record_label: &who.username, changes: Some(changes) },
        None => {
            changes["reason"] = json!(if out["user"].is_null() { "refused" } else { "inactive" });
            Entry { user_key: "", action: "login_failed", resource: "user", record_key: "", record_label: &who.username, changes: Some(changes) }
        }
    };
    if let Err(e) = audit::record(&o.site, scope.executor(), entry).await {
        scope.rollback().await;
        return Err(Failure::Server(database_error(&e)));
    }
    scope.commit().await.map_err(|e| Failure::Server(database_error(&e.message)))?;
    match found {
        Some(user) => Ok(sec.new_session(user, sec.pwd_tag(hash.as_deref()))),
        // The failed-login row is already written above, so no second row: the word of the page only.
        None => Err(Failure::Audited(if out["user"].is_null() { "failed" } else { "inactive" })),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_next_path_stays_under_the_base_path() {
        let ok = ["/admin", "/admin/", "/admin/Note", "/admin/Note?page=2&q=a%20b", "/admin/Note/12/edit", "/admin/Note/a.b"];
        for next in ok {
            assert_eq!(super::safe_next("/admin", next).as_deref(), Some(next), "{next}");
        }
        let bad = [
            "", "/", "/administrator", "/other/admin", "//evil.com", "//evil.com/admin", "https://evil.com", "https://evil.com/admin", "/admin/../evil", "/admin/%2e%2E/evil", "/admin/.%2e/evil", "/admin/%2E./evil", "/admin/./x", "/admin\\evil", "/admin/x\r\nSet-Cookie: a=b", "/admin/a b",
            "/admin/auth/start", "/admin/auth/callback?x=1", "/admin/auth", "admin/Note",
        ];
        for next in bad {
            assert_eq!(super::safe_next("/admin", next), None, "{next:?}");
        }
        assert_eq!(super::safe_next("/admin", &format!("/admin/{}", "a".repeat(2000))), None);
        assert_eq!(super::safe_next("", "//evil.com"), None);
        assert_eq!(super::safe_next("", "/Note").as_deref(), Some("/Note"));
    }

    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};

    fn settings() -> OidcSettings {
        OidcSettings::from_value(&json!({"issuer": "https://idp.example.com", "client_id": "admin", "client_secret": "s", "roles_claim": "roles"})).unwrap()
    }

    struct Signer {
        pair: EcdsaKeyPair,
    }

    impl Signer {
        fn new() -> Signer {
            let rng = SystemRandom::new();
            let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
            Signer { pair: EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng).unwrap() }
        }

        fn jwks(&self) -> Value {
            let point = self.pair.public_key().as_ref();
            json!({"keys": [{"kty": "EC", "crv": "P-256", "kid": "k1", "use": "sig", "alg": "ES256", "x": B64.encode(&point[1..33]), "y": B64.encode(&point[33..])}]})
        }

        fn token(&self, header: Value, claims: Value) -> String {
            let head = B64.encode(serde_json::to_vec(&header).unwrap());
            let body = B64.encode(serde_json::to_vec(&claims).unwrap());
            let sig = self.pair.sign(&SystemRandom::new(), format!("{head}.{body}").as_bytes()).unwrap();
            format!("{head}.{body}.{}", B64.encode(sig.as_ref()))
        }
    }

    const NOW: i64 = 1_800_000_000;

    fn good() -> Value {
        json!({"iss": "https://idp.example.com", "aud": "admin", "sub": "u1", "exp": NOW + 300, "iat": NOW, "nonce": "n1", "email": "ann@example.com", "roles": ["editor", "auditor"]})
    }

    fn header() -> Value {
        json!({"alg": "ES256", "kid": "k1"})
    }

    #[test]
    fn a_good_token_passes_and_gives_the_identity() {
        let s = Signer::new();
        let claims = verify_id_token(&s.token(header(), good()), &s.jwks(), &settings(), "n1", NOW).unwrap();
        let who = identity(&claims, &settings()).unwrap();
        assert_eq!(who, Identity { subject: "u1".into(), username: "ann@example.com".into(), email: "ann@example.com".into(), roles: vec!["auditor".into(), "editor".into()] });
    }

    #[test]
    fn each_wrong_claim_is_refused_with_its_own_reason() {
        let s = Signer::new();
        let check = |claims: Value, nonce: &str| verify_id_token(&s.token(header(), claims), &s.jwks(), &settings(), nonce, NOW).unwrap_err();
        let with = |name: &str, value: Value| {
            let mut c = good();
            c[name] = value;
            c
        };
        assert_eq!(check(with("iss", json!("https://evil.example.com")), "n1"), "bad_issuer");
        assert_eq!(check(with("aud", json!("other")), "n1"), "bad_audience");
        assert_eq!(check(with("aud", json!(["admin", "other"])), "n1"), "bad_audience");
        assert_eq!(check(with("exp", json!(NOW - 61)), "n1"), "expired");
        assert_eq!(check(with("iat", json!(NOW + 120)), "n1"), "bad_time");
        assert_eq!(check(good(), "n2"), "bad_nonce");
        assert_eq!(check(with("sub", json!("")), "n1"), "no_subject");
        let several = json!({"iss": "https://idp.example.com", "aud": ["admin", "other"], "azp": "admin", "sub": "u1", "exp": NOW + 300, "nonce": "n1"});
        assert!(verify_id_token(&s.token(header(), several), &s.jwks(), &settings(), "n1", NOW).is_ok());
    }

    #[test]
    fn the_algorithm_and_the_signature_are_checked() {
        let s = Signer::new();
        let other = Signer::new();
        assert_eq!(verify_id_token(&other.token(header(), good()), &s.jwks(), &settings(), "n1", NOW).unwrap_err(), "bad_signature");
        assert_eq!(verify_id_token(&s.token(json!({"alg": "none", "kid": "k1"}), good()), &s.jwks(), &settings(), "n1", NOW).unwrap_err(), "bad_algorithm");
        assert_eq!(verify_id_token(&s.token(json!({"alg": "HS256", "kid": "k1"}), good()), &s.jwks(), &settings(), "n1", NOW).unwrap_err(), "bad_algorithm");
        assert_eq!(verify_id_token(&s.token(json!({"alg": "ES256", "kid": "zz"}), good()), &s.jwks(), &settings(), "n1", NOW).unwrap_err(), "unknown_key");
        assert_eq!(verify_id_token("a.b", &s.jwks(), &settings(), "n1", NOW).unwrap_err(), "bad_token");
        let tampered = {
            let t = s.token(header(), good());
            let parts: Vec<&str> = t.split('.').collect();
            let mut claims = good();
            claims["roles"] = json!(["admin"]);
            format!("{}.{}.{}", parts[0], B64.encode(serde_json::to_vec(&claims).unwrap()), parts[2])
        };
        assert_eq!(verify_id_token(&tampered, &s.jwks(), &settings(), "n1", NOW).unwrap_err(), "bad_signature");
    }

    #[test]
    fn the_hosted_domain_is_checked_on_the_server() {
        let s = Signer::new();
        let mut set = settings();
        set.hd = Some("example.com".into());
        let mut claims = good();
        assert_eq!(verify_id_token(&s.token(header(), claims.clone()), &s.jwks(), &set, "n1", NOW).unwrap_err(), "wrong_domain");
        claims["hd"] = json!("other.com");
        assert_eq!(verify_id_token(&s.token(header(), claims.clone()), &s.jwks(), &set, "n1", NOW).unwrap_err(), "wrong_domain");
        claims["hd"] = json!("Example.com");
        assert!(verify_id_token(&s.token(header(), claims), &s.jwks(), &set, "n1", NOW).is_ok());
    }

    #[test]
    fn identity_reads_roles_from_a_list_a_name_or_a_nested_claim() {
        let mut set = settings();
        let mut claims = good();
        claims["roles"] = json!("solo");
        assert_eq!(identity(&claims, &set).unwrap().roles, ["solo"]);
        claims["roles"] = json!([1, "a", "a", ""]);
        assert_eq!(identity(&claims, &set).unwrap().roles, ["a"]);
        claims.as_object_mut().unwrap().remove("roles");
        assert!(identity(&claims, &set).unwrap().roles.is_empty());
        set.roles_claim = "realm_access.roles".into();
        claims["realm_access"] = json!({"roles": ["x"]});
        assert_eq!(identity(&claims, &set).unwrap().roles, ["x"]);
        claims["email_verified"] = json!(false);
        assert_eq!(identity(&claims, &set).unwrap_err(), "email_unverified");
        claims["email"] = json!("");
        claims["email_verified"] = json!(true);
        assert_eq!(identity(&claims, &set).unwrap_err(), "no_username");
    }

    #[test]
    fn settings_are_checked() {
        for (value, needle) in [
            (json!({"issuer": "http://idp.example.com", "client_id": "a"}), "https"),
            (json!({"issuer": "idp.example.com", "client_id": "a"}), "not a URL"),
            (json!({"issuer": "https://idp.example.com", "client_id": " "}), "client_id"),
            (json!({"issuer": "https://idp.example.com", "client_id": "a", "scopes": ["a b"]}), "scopes"),
            (json!({"issuer": "https://idp.example.com", "client_id": "a", "colour": 1}), "colour"),
            (json!({"issuer": "https://idp.example.com", "client_id": "a", "public_url": "https://a.example.com/x"}), "no path"),
        ] {
            let e = OidcSettings::from_value(&value).unwrap_err();
            assert!(e.contains(needle), "{e}");
        }
        assert!(OidcSettings::from_value(&json!({"issuer": "http://127.0.0.1:9000/", "client_id": "a"})).is_ok());
        assert_eq!(OidcSettings::from_value(&json!({"issuer": "https://i.example.com", "client_id": "a", "scopes": ["roles", "email"]})).unwrap().scope(), "openid profile email roles");
    }

    #[test]
    fn urls_parse() {
        let u = parse_url("https://idp.example.com:8443/a/b?x=1").unwrap();
        assert_eq!((u.scheme(), u.host_str(), u.port(), u.path(), u.query()), ("https", Some("idp.example.com"), Some(8443), "/a/b", Some("x=1")));
        assert!(parse_url("http://[::1]:9/").is_ok());
        assert!(parse_url("https://u@idp.example.com/").is_err());
        assert!(parse_url("https://idp.example.com/#x").is_err());
        assert!(parse_url("ftp://idp.example.com/").is_err());
    }
}
