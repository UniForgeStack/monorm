//! The session cookie, the CSRF token, the Origin check and the login rate limit. No I/O.
//!
//! The session is stateless: a cookie that holds the user and two times, signed with
//! HMAC-SHA256 under `MONORM_ADMIN_SECRET`. Two processes with the same secret accept the
//! same cookie. The CSRF token is a random value signed for one session id.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine;
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Sha256;
use subtle::ConstantTimeEq;

pub const SESSION_COOKIE: &str = "__Host-monorm-session";
pub const CSRF_COOKIE: &str = "__Host-monorm-csrf";
pub const CSRF_HEADER: &str = "x-csrf-token";
/// A shorter secret is refused.
pub const MIN_SECRET: usize = 32;

/// Failures of one username from one IP in a window.
const USER_FAILURES: usize = 5;
/// Failures of one IP, for any username, in a window.
const IP_FAILURES: usize = 20;
/// Failures of one username from every IP together: a guess spread over many IPs.
const NAME_FAILURES: usize = 50;
/// One `login_blocked` audit row per blocking key in this many seconds.
const BLOCK_LOG_SECS: i64 = 60;
const WINDOW_SECS: i64 = 15 * 60;
const MAX_KEYS: usize = 10_000;

#[derive(Clone, Debug, PartialEq)]
pub struct SecurityOptions {
    /// A session ends this long after its last request.
    pub idle_secs: i64,
    /// A session ends this long after the login, whatever its use.
    pub absolute_secs: i64,
    /// Origins (`https://admin.example.com`) a write may come from, besides the `Host` of the request.
    pub origins: Vec<String>,
    /// Peers (exact addresses) whose `X-Forwarded-For` header the login limits believe. Empty: the socket address is the client.
    pub trusted_proxies: Vec<IpAddr>,
}

impl Default for SecurityOptions {
    fn default() -> Self {
        SecurityOptions { idle_secs: 30 * 60, absolute_secs: 12 * 60 * 60, origins: Vec::new(), trusted_proxies: Vec::new() }
    }
}

/// What the signed cookie holds.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub sid: String,
    pub user: Value,
    /// The login time, seconds since the epoch.
    pub iat: i64,
    /// The time of the last request that renewed the cookie.
    pub seen: i64,
    /// `Security::pwd_tag` of the password hash at the login: a changed password ends the session at the next renewal.
    #[serde(default)]
    pub pwd: String,
}

pub struct Security {
    secret: Vec<u8>,
    pub options: SecurityOptions,
    /// Seconds a test adds to the real time.
    skew: AtomicI64,
    name_limit: Limiter,
    user_limit: Limiter,
    ip_limit: Limiter,
    block_log: Limiter,
    calls: AtomicU64,
    /// Session ids that logged out, with the time their cookie ends anyway. One process only.
    revoked: Mutex<HashMap<String, i64>>,
}

impl std::fmt::Debug for Security {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Security").field("options", &self.options).finish_non_exhaustive()
    }
}

impl Security {
    pub fn new(secret: &[u8], options: SecurityOptions) -> Result<Arc<Security>, String> {
        if secret.len() < MIN_SECRET {
            return Err(format!("MONORM_ADMIN_SECRET must have at least {MIN_SECRET} bytes (it has {})", secret.len()));
        }
        if options.idle_secs <= 0 || options.absolute_secs <= 0 {
            return Err("the session timeouts must be above 0".into());
        }
        Ok(Arc::new(Security {
            secret: secret.to_vec(),
            options,
            skew: AtomicI64::new(0),
            name_limit: Limiter::new(NAME_FAILURES, WINDOW_SECS),
            user_limit: Limiter::new(USER_FAILURES, WINDOW_SECS),
            ip_limit: Limiter::new(IP_FAILURES, WINDOW_SECS),
            block_log: Limiter::new(1, BLOCK_LOG_SECS),
            calls: AtomicU64::new(1),
            revoked: Mutex::new(HashMap::new()),
        }))
    }

    pub fn now(&self) -> i64 {
        chrono::Utc::now().timestamp() + self.skew.load(Ordering::SeqCst)
    }

    /// Moves this server's clock forward: a test ends a session without a wait.
    pub fn advance(&self, secs: i64) {
        self.skew.fetch_add(secs, Ordering::SeqCst);
    }

    /// Failures of a username from all IPs.
    pub fn name_limit(&self) -> &Limiter {
        &self.name_limit
    }

    /// Failures of a username from one IP (the key holds both), and password changes of one user.
    pub fn user_limit(&self) -> &Limiter {
        &self.user_limit
    }

    pub fn ip_limit(&self) -> &Limiter {
        &self.ip_limit
    }

    /// The id of the next host call, unique in this process.
    pub fn call_id(&self) -> u64 {
        self.calls.fetch_add(1, Ordering::Relaxed)
    }

    /// Gates the audit row of a blocked login: true once per key and minute.
    pub fn log_block(&self, key: &str) -> bool {
        self.block_log.reserve(key, self.now()).is_none()
    }

    fn mac(&self, domain: &str, data: &str) -> Hmac<Sha256> {
        let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(&self.secret).expect("HMAC takes a key of any length");
        mac.update(domain.as_bytes());
        mac.update(b"|");
        mac.update(data.as_bytes());
        mac
    }

    pub fn new_session(&self, user: Value, pwd: String) -> Session {
        let now = self.now();
        Session { sid: random(), user, iat: now, seen: now, pwd }
    }

    /// The same login under a new session id, for a password change: the old cookie stays revoked.
    pub fn rotate(&self, old: &Session, pwd: String) -> Session {
        Session { sid: random(), user: old.user.clone(), iat: old.iat, seen: self.now(), pwd }
    }

    /// The cookie keeps this keyed hash, never the password hash itself (the Django way: a changed hash ends the session).
    pub fn pwd_tag(&self, hash: Option<&str>) -> String {
        B64.encode(self.mac("pwd", hash.unwrap_or("")).finalize().into_bytes())
    }

    pub fn pwd_matches(&self, session: &Session, hash: Option<&str>) -> bool {
        bool::from(session.pwd.as_bytes().ct_eq(self.pwd_tag(hash).as_bytes()))
    }

    pub fn sign_session(&self, session: &Session) -> String {
        let payload = B64.encode(serde_json::to_vec(session).expect("a session is JSON"));
        let tag = B64.encode(self.mac("session", &payload).finalize().into_bytes());
        format!("s1.{payload}.{tag}")
    }

    /// The session of a cookie value: the signature is right and neither timeout has passed.
    pub fn read_session(&self, token: &str) -> Option<Session> {
        let rest = token.strip_prefix("s1.")?;
        let (payload, tag) = rest.split_once('.')?;
        self.mac("session", payload).verify_slice(&B64.decode(tag).ok()?).ok()?;
        let session: Session = serde_json::from_slice(&B64.decode(payload).ok()?).ok()?;
        let now = self.now();
        let live = session.iat <= now + 60 && now - session.iat < self.options.absolute_secs && now - session.seen < self.options.idle_secs;
        let revoked = self.revoked.lock().unwrap_or_else(|e| e.into_inner()).contains_key(&session.sid);
        (live && !revoked).then_some(session)
    }

    /// A logout: this process refuses the session from now on.
    pub fn revoke(&self, session: &Session) {
        let now = self.now();
        let mut revoked = self.revoked.lock().unwrap_or_else(|e| e.into_inner());
        revoked.retain(|_, until| *until > now);
        revoked.insert(session.sid.clone(), session.iat + self.options.absolute_secs);
    }

    /// The idle time is renewed when the cookie is this old.
    pub fn renew_after(&self) -> i64 {
        (self.options.idle_secs / 4).clamp(1, 60)
    }

    /// Seconds a session issued at `iat` has left.
    pub fn remaining(&self, session: &Session) -> i64 {
        (session.iat + self.options.absolute_secs - self.now()).max(0)
    }

    /// A CSRF token for `sid` (empty for a visitor with no session).
    pub fn csrf_token(&self, sid: &str) -> String {
        let nonce = random();
        let tag = B64.encode(self.mac("csrf", &format!("{sid}|{nonce}")).finalize().into_bytes());
        format!("{nonce}.{tag}")
    }

    /// The double-submit check: the header equals the cookie, and the token is one this server signed for `sid`.
    pub fn csrf_ok(&self, sid: &str, cookie: Option<&str>, header: Option<&str>) -> bool {
        let (Some(cookie), Some(header)) = (cookie, header) else { return false };
        if !bool::from(cookie.as_bytes().ct_eq(header.as_bytes())) {
            return false;
        }
        let Some((nonce, tag)) = header.split_once('.') else { return false };
        let Ok(tag) = B64.decode(tag) else { return false };
        self.mac("csrf", &format!("{sid}|{nonce}")).verify_slice(&tag).is_ok()
    }

    /// The write comes from this server's own origin: `Origin`, else `Referer`, names the `Host` or a configured origin.
    pub fn origin_ok(&self, origin: Option<&str>, referer: Option<&str>, host: Option<&str>) -> bool {
        let claimed = match (origin, referer) {
            (Some(o), _) => o.to_owned(),
            (None, Some(r)) => match origin_of(r) {
                Some(o) => o,
                None => return false,
            },
            (None, None) => return false,
        };
        let Some((scheme, authority)) = claimed.split_once("://") else { return false };
        if scheme != "http" && scheme != "https" || authority.is_empty() || authority.contains('/') {
            return false;
        }
        self.options.origins.iter().any(|o| o.trim_end_matches('/').eq_ignore_ascii_case(&claimed)) || host.is_some_and(|h| h.eq_ignore_ascii_case(authority))
    }
}

/// `https://a.b:8` from `https://a.b:8/x?y`.
fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    Some(format!("{scheme}://{authority}"))
}

fn random() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

/// The value of cookie `name` in a `Cookie` header.
pub fn cookie_value<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    header.split(';').filter_map(|p| p.trim().split_once('=')).find(|(k, _)| *k == name).map(|(_, v)| v)
}

pub fn session_cookie(value: &str, max_age: i64) -> String {
    format!("{SESSION_COOKIE}={value}; Path=/; Max-Age={max_age}; HttpOnly; Secure; SameSite=Lax")
}

/// The CSRF cookie is readable by script: the client copies it into the header.
pub fn csrf_cookie(value: &str, max_age: i64) -> String {
    format!("{CSRF_COOKIE}={value}; Path=/; Max-Age={max_age}; Secure; SameSite=Lax")
}

pub fn ended_cookies() -> [String; 2] {
    [session_cookie("", 0), csrf_cookie("", 0)]
}

/// Attempts per key in a time window, in memory (one process).
pub struct Limiter {
    max: usize,
    window: i64,
    max_keys: usize,
    hits: Mutex<HashMap<String, Vec<i64>>>,
}

impl Limiter {
    pub fn new(max: usize, window: i64) -> Self {
        Limiter::with_max_keys(max, window, MAX_KEYS)
    }

    /// `max_keys` is a hard cap on the keys held: a flood of new keys evicts the oldest, it never grows the map.
    pub fn with_max_keys(max: usize, window: i64, max_keys: usize) -> Self {
        Limiter { max, window, max_keys: max_keys.max(1), hits: Mutex::new(HashMap::new()) }
    }

    /// Counts one attempt before its verdict, so parallel attempts cannot all pass the check.
    /// Gives the seconds to wait, and counts nothing, when `key` has used its failures.
    /// A success calls `release`.
    pub fn reserve(&self, key: &str, now: i64) -> Option<i64> {
        let mut hits = self.hits.lock().unwrap_or_else(|e| e.into_inner());
        if hits.len() >= self.max_keys && !hits.contains_key(key) {
            hits.retain(|_, list| list.last().is_some_and(|t| now - *t < self.window));
            if hits.len() >= self.max_keys {
                let keep = self.max_keys - (self.max_keys / 10).max(1);
                let mut newest: Vec<(i64, String)> = hits.iter().map(|(k, list)| (list.last().copied().unwrap_or(i64::MIN), k.clone())).collect();
                newest.sort();
                for (_, k) in newest.into_iter().take(hits.len() - keep) {
                    hits.remove(&k);
                }
            }
        }
        let list = hits.entry(key.to_owned()).or_default();
        list.retain(|t| now - *t < self.window);
        if list.len() >= self.max {
            return Some((self.window - (now - list[0])).max(1));
        }
        list.push(now);
        None
    }

    /// Takes back the newest attempt of `key`.
    pub fn release(&self, key: &str) {
        let mut hits = self.hits.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(list) = hits.get_mut(key) {
            list.pop();
            if list.is_empty() {
                hits.remove(key);
            }
        }
    }

    pub fn clear(&self, key: &str) {
        self.hits.lock().unwrap_or_else(|e| e.into_inner()).remove(key);
    }

    /// The number of keys held.
    pub fn len(&self) -> usize {
        self.hits.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sec() -> Arc<Security> {
        Security::new(&[7u8; 32], SecurityOptions::default()).unwrap()
    }

    #[test]
    fn a_short_secret_is_refused() {
        assert!(Security::new(b"short", SecurityOptions::default()).unwrap_err().contains("at least 32 bytes"));
    }

    #[test]
    fn a_session_round_trips_and_a_changed_byte_or_secret_fails() {
        let a = sec();
        let token = a.sign_session(&a.new_session(json!({"key": "1"}), String::new()));
        assert_eq!(a.read_session(&token).unwrap().user["key"], "1");
        let other = Security::new(&[8u8; 32], SecurityOptions::default()).unwrap();
        assert!(other.read_session(&token).is_none());
        let same = Security::new(&[7u8; 32], SecurityOptions::default()).unwrap();
        assert!(same.read_session(&token).is_some());
        let mut bad = token.clone();
        bad.insert(5, 'x');
        assert!(a.read_session(&bad).is_none());
        assert!(a.read_session("garbage").is_none());
    }

    #[test]
    fn idle_and_absolute_timeouts_end_a_session() {
        let a = Security::new(&[7u8; 32], SecurityOptions { idle_secs: 100, absolute_secs: 1000, ..Default::default() }).unwrap();
        let s = a.new_session(json!({"key": "1"}), String::new());
        let token = a.sign_session(&s);
        a.advance(99);
        assert!(a.read_session(&token).is_some());
        a.advance(2);
        assert!(a.read_session(&token).is_none());
        let renewed = Session { seen: a.now(), ..s };
        let token = a.sign_session(&renewed);
        a.advance(950);
        assert!(a.read_session(&token).is_none(), "past the absolute time");
    }

    #[test]
    fn a_revoked_session_is_refused() {
        let a = sec();
        let s = a.new_session(json!({"key": "1"}), String::new());
        let token = a.sign_session(&s);
        a.revoke(&s);
        assert!(a.read_session(&token).is_none());
    }

    #[test]
    fn csrf_needs_cookie_and_header_equal_and_signed_for_the_session() {
        let a = sec();
        let t = a.csrf_token("sid1");
        assert!(a.csrf_ok("sid1", Some(&t), Some(&t)));
        assert!(!a.csrf_ok("sid2", Some(&t), Some(&t)), "another session");
        assert!(!a.csrf_ok("sid1", Some(&t), Some(&a.csrf_token("sid1"))), "cookie and header differ");
        assert!(!a.csrf_ok("sid1", None, Some(&t)));
        assert!(!a.csrf_ok("sid1", Some("x.y"), Some("x.y")), "not signed");
    }

    #[test]
    fn origin_or_referer_must_name_the_host_or_a_configured_origin() {
        let a = Security::new(&[7u8; 32], SecurityOptions { origins: vec!["https://admin.example.com/".into()], ..Default::default() }).unwrap();
        let host = Some("localhost:8000");
        assert!(a.origin_ok(Some("http://localhost:8000"), None, host));
        assert!(a.origin_ok(None, Some("http://localhost:8000/admin/x?y=1"), host));
        assert!(a.origin_ok(Some("https://admin.example.com"), None, host));
        assert!(!a.origin_ok(Some("http://evil.test"), None, host));
        assert!(!a.origin_ok(Some("null"), None, host));
        assert!(!a.origin_ok(None, None, host));
        assert!(!a.origin_ok(Some("http://evil.test"), Some("http://localhost:8000/"), host), "Origin wins over Referer");
        assert!(!a.origin_ok(None, Some("http://localhost:8000.evil.test/"), host));
    }

    #[test]
    fn the_limiter_blocks_after_max_attempts_and_frees_after_the_window() {
        let l = Limiter::new(2, 100);
        assert!(l.reserve("k", 0).is_none());
        assert!(l.reserve("k", 10).is_none());
        assert_eq!(l.reserve("k", 20), Some(80), "the third attempt waits and is not counted");
        assert!(l.reserve("other", 20).is_none());
        assert!(l.reserve("k", 101).is_none(), "the first attempt left the window");
        l.release("k");
        assert!(l.reserve("k", 102).is_none(), "a released attempt does not count");
        l.clear("k");
        assert!(l.reserve("k", 103).is_none());
        assert!(l.reserve("k", 104).is_none());
        assert!(l.reserve("k", 105).is_some());
    }

    #[test]
    fn a_full_limiter_evicts_the_oldest_keys_and_never_grows_past_its_cap() {
        let l = Limiter::with_max_keys(2, 1000, 3);
        assert!(l.reserve("a", 0).is_none());
        assert!(l.reserve("a", 1).is_none());
        assert!(l.reserve("a", 2).is_some(), "a has used its attempts");
        assert!(l.reserve("b", 3).is_none());
        assert!(l.reserve("c", 4).is_none());
        assert!(l.reserve("d", 5).is_none(), "the cap is 3 keys: the oldest key (a) goes");
        assert_eq!(l.len(), 3);
        assert!(l.reserve("a", 6).is_none(), "an evicted key starts from zero");
        for i in 0..50 {
            l.reserve(&format!("flood{i}"), 10 + i);
            assert!(l.len() <= 3, "{} keys", l.len());
        }
    }

    #[test]
    fn the_password_tag_follows_the_hash_and_the_secret() {
        let a = sec();
        let s = a.new_session(json!({"key": "1"}), a.pwd_tag(Some("hash1")));
        assert!(a.pwd_matches(&s, Some("hash1")));
        assert!(!a.pwd_matches(&s, Some("hash2")));
        assert!(!a.pwd_matches(&s, None));
        assert!(!s.pwd.contains("hash1"), "the cookie never holds the hash");
        let other = Security::new(&[8u8; 32], SecurityOptions::default()).unwrap();
        assert_ne!(other.pwd_tag(Some("hash1")), s.pwd);
    }

    #[test]
    fn cookies_are_read_by_name() {
        assert_eq!(cookie_value("a=1; __Host-monorm-csrf=x.y; b=2", CSRF_COOKIE), Some("x.y"));
        assert_eq!(cookie_value("a=1", CSRF_COOKIE), None);
        let c = session_cookie("v", 5);
        assert!(c.contains("HttpOnly") && c.contains("Secure") && c.contains("SameSite=Lax"));
        assert!(!csrf_cookie("v", 5).contains("HttpOnly"));
    }
}
