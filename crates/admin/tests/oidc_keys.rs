//! OIDC login through the served router against a fake identity provider on a loopback port:
//! RS256 and ES256 tokens, a forged signature, and a key set that rotates.
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::ConnectInfo;
use axum::http::Request;
use axum::Router;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD as B64};
use base64::Engine;
use monorm_admin::oidc::OidcSettings;
use monorm_admin::service::{service, Options};
use monorm_admin::{AuthConfig, BoxFuture, BridgeError, CallContext, HostBridge, Security, SecurityOptions, Site};
use monorm_core::{dsl, migrate, schema::Schema};
use monorm_engine::db;
use ring::rand::SystemRandom;
use ring::signature::{EcdsaKeyPair, KeyPair, RsaKeyPair, ECDSA_P256_SHA256_FIXED_SIGNING, RSA_PKCS1_SHA256};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tower::ServiceExt;

const APP: &str = include_str!("../../../apps/admin/schema.prisma");
const SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";
/// An RSA-2048 key (PKCS#8, base64) made for these tests only.
const RSA_KEY_A: &str = "MIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQCzmW3eEFBi+lW3KQ7ZPYwRcUQTuZkVL7PQzTSZIaFnBVGU0ZNw6pWmdYtgRdfvDmbsF+ioQrBrz5FwJuABEj4nNOJf56GCwy48gLIVYkITEDHiGF+/rqYv6FfktyXfIUHtrYNwbQQUE6iMqbQ2SRpUO6Wn9pI2Gf0eUlcF8bM4YnUd5AnnGT26dFiW523q54TAyAo7P2IB5paSPxifSR61bzO5nNdGOQ1JtEUS5OPdqMADMOV9Q67x7E2AEBvXXDPq/HnLw2jL9UtYneGYTF+cP3JxilZ31ZhbcaJ02ut6KCYmVwWiHu3MxjPIUwOwHGDuky6Kfxa5FHTEfVir9G6VAgMBAAECggEAEcfijmCpGhN/Vvcz/CKmx7dMrnM/36QYYQIKmg/aoztt50Dpyv0T5yYDWtBaCisv2gydQRimehX+Z88W0E8BDkVNwCxLhjRSqIoel2q3RfHi2RKMBYssnYOa6A7l1fW8nuOrJqn0Wb8p1GXyFzRyQd1Mj5jOwu0cbTBpDd1EXr7dzHTofAnmsfQGCTVNldekkfGIF9PJlSirLtSG08ADwOPca3hdUHeCvZR79+FfdkVVkEzzCJ1V7ZOMcPFzsIHMqHLoCM8Y4nxHLMo6A9qKYgP5CU7h/ci4DjvOc5aruafhcXjciM091mAVDVlXT78k1171zt6jJ1F04T3zKch6BQKBgQD2S06LCvs/0FpiDKCFE3RKwVTSIB8DWG4xvTf5zNwOGlei0Zcs8CKtNHRTVISm5ow7hfjAVRCMWsaB29PRdJhkuN3tzI5msOhPdC85UoZtt4HnUYi6VGLLwl+Tkg/iRFoZ2b6BkBExacA5vO1fNO/htKLeNM/K2YvqSezkBc6kXwKBgQC6rUiqPJIKcNOWj+6acBt5fByxJB0lAa7GuknaYdbxgWv5rHqvf5n201L0DqDCtPqeYuU8cJX9MK0dH7jaS2awInJHTZYvloN0qad0W/OZQTJec0zGGxvEUGzloFGr/Ys4Q9MbYdrCs57tt6g3m2QUYxKAXzBoI19k7di8NJUxiwKBgQCwY86klgDK+xVAE3sC06P9xehRdN/BLZv90147cRRIpWUOHrz+Whz7nw2QgH9gaM0V9CTq2gE4A+sXJbDV2YMq35tRq6ZwtVIrVTJdA1VzTnGikSozCCyZcWOx9HGbwwgka2WctOkbUnlw4HXAMdF6/DuImJoY1ST3bmpSBDayUQKBgAS7MZVG9biYBXTSbwyw/LFGiP+gV7DBaymUPRzwz97NFex9B5r9E+zijD6tRDvLFHMBNKgkflitbCXJ0QiPUqnyJY1TFRRdrzU8u4bCcdkXguxL5c07kKeYFjp3prpCMc4aQfZ2s3Gt1cBWoEfQg2fU3vor73EkPF21fYG/n2xjAoGBAMTNJliLnUar5qGJ3KGA04stfdWP12p9QXBR0OWVNjKsSWS8yQlndqPeJsYNnXP4gM6LRiHp7z8OueOXtx0h/1McnxVTPZhye9gZ+SfxnUHGvQzCvVbp+kcxxBQ5VnZcUHJOQI8cZo0/gzkWpGK3r50fOS+Y8oEU3OWUcuWSZ6jk";
/// A second RSA-2048 key (PKCS#8, base64) for these tests only: the key that a provider rotates to.
const RSA_KEY_B: &str = "MIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQCXaPBvdVgxuKh/FkJertYl3Rtye5JAtyD2hZ+Zdyw7ff3a2cmPxxQtTX4c4nPiII7cEoVv3GFgYG3ACxIkWLZNbiqmDBgoBHiw0yiaKwCw7iHcmLet+LaF+WPY2DbZi2nO2j5FvY9O/Xp4ALA1WnHKlmxWiv2+znwzgybSb/vgIcQAeyA6ndVG4mHleu0zG/oeOXvoBc8w5r3xIShD+wbo1FWApGTjSHNqfDqyFb9elN/6uGcpbVwccgmyZRDKHS9XfXD1gNq5eplkUAm1oHi8qxCLxnZDc+fuD+VmSK0Kiqkfp0v/CCn0mk5JV1zl+qJF5wsgTjM5ai6k325jef3hAgMBAAECggEAEJ+tcldcaQ0gtt9fRx0EFyEpH88wdhmFLXvrU4KxoRCKvCoYRshxTy4IcCxu07+GX+RKsUbtjBLaSfENO19gGK1FUnjWeCJRi/yLIy4gg8yJfDpqRFgf1iLVExxeHVGAuvzcRHT1FJOhDCbrMdht0PPmDjRxeq8aFN/L9ePksC1pnb+qIfThQfIdhfJIOi/qK+Rhqci3rhews2D1hklFRsMcwRWjeyu3Qrq0PD+s8rIF5ChVDRCv01+sWjsuYQjNHuooRb4JH2GZXcK100ZrWuCu+7u2hOy+/e/YP9xjLM+Zmr587CxphU/869Ofx74ulgBq9i/yo//SR6AVP0Ha5wKBgQDNazwVQBmCwy0TEyHvWKegzhq+yPHUXomLSo9FLLihKGSb2TJIkds8K+hMavvtNUJbiMfk3TUrueu9bjubJ8zlJxhlYutiFTuWw+hCskOZi0feVYTkNEb4o6cYVlR0qil57Gu+9AM5C1FGI7lXVBDxNDDWUBNtqOE21Zmx3CcwywKBgQC8sTNOcpLds3VJaWHcsihXinvKXkS4/eXpsimcrHZNoPjJvvv4464PCyI1Jn86euc6vT/t0UWfnd5PL+GkTe86YoOQdY8pN5Vrf0MO3O4YhBYxZYgzhjEMA2JWR07jkdvJ2DQb+JiL4VDHi1UjCHopUMqBNoTZz2uNk4Y+VwFSgwKBgQC1xEjTKPgp7BjLgyeJCfKyaQ3jeh365HhjB+PEuzDYccys10EWU/IhyVxxmk039eO06VCwJElzWr3NzmfyAALfVnvQ4voTvWQQIWQ4+FK24DcE9rlz0Qpn4y/tah6raRLwbxJcUsEzTd9p6ByNuLaRKLR6Pzsh0skOY23vozYOhwKBgHIw1rjZ05GBojAcK1IxKwLXVtUAOZyuYq4L2Ozs3edazFTjVH+p4dNOzJPXyVUtvbOphf+5DuZq1nebpXUWiPkPV0lcK5gpAC7+5CkhgKGrhfa82DE+O35mqeNG4jIMOjOruNVYm5DDeWxZINvDrJJGR1btFHbv9MnIBozBgJZbAoGBAMexxZQpdjbKtpCkHv/j9NH+RyPfAvm1uwpwD7PMCjcAP//MB3QLG70tjeKME1cqL9s+R2u5A7EvxqNxnXAvAMs5RFhQk0KU0Wqt/nXX1QknAe184F/9Y7opxqAOYjwgRYPOn+a0N9Re8Bit7qk3jaDGUwkp8vOZHEilg6rXkGP3";

/// The signing key of the provider and the matching key-set entry.
enum Key {
    Rsa { kid: &'static str, pair: RsaKeyPair },
    Ec { kid: &'static str, pair: EcdsaKeyPair },
}

impl Key {
    fn rsa(kid: &'static str, pkcs8: &str) -> Key {
        Key::Rsa { kid, pair: RsaKeyPair::from_pkcs8(&STANDARD.decode(pkcs8).unwrap()).unwrap() }
    }

    fn ec(kid: &'static str) -> Key {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        Key::Ec { kid, pair: EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng).unwrap() }
    }

    fn alg(&self) -> &'static str {
        match self {
            Key::Rsa { .. } => "RS256",
            Key::Ec { .. } => "ES256",
        }
    }

    fn kid(&self) -> &'static str {
        match self {
            Key::Rsa { kid, .. } | Key::Ec { kid, .. } => kid,
        }
    }

    fn jwk(&self) -> Value {
        match self {
            Key::Rsa { kid, pair } => {
                let parts = ring::rsa::PublicKeyComponents::<Vec<u8>>::from(pair.public());
                json!({"kty": "RSA", "kid": kid, "use": "sig", "alg": "RS256", "n": B64.encode(&parts.n), "e": B64.encode(&parts.e)})
            }
            Key::Ec { kid, pair } => {
                let point = pair.public_key().as_ref();
                json!({"kty": "EC", "crv": "P-256", "kid": kid, "use": "sig", "alg": "ES256", "x": B64.encode(&point[1..33]), "y": B64.encode(&point[33..])})
            }
        }
    }

    fn sign(&self, message: &[u8]) -> Vec<u8> {
        match self {
            Key::Rsa { pair, .. } => {
                let mut sig = vec![0; pair.public().modulus_len()];
                pair.sign(&RSA_PKCS1_SHA256, &SystemRandom::new(), message, &mut sig).unwrap();
                sig
            }
            Key::Ec { pair, .. } => pair.sign(&SystemRandom::new(), message).unwrap().as_ref().to_vec(),
        }
    }

    fn token(&self, claims: &Value) -> String {
        let head = B64.encode(json!({"alg": self.alg(), "kid": self.kid()}).to_string());
        let body = B64.encode(claims.to_string());
        let sig = self.sign(format!("{head}.{body}").as_bytes());
        format!("{head}.{body}.{}", B64.encode(sig))
    }
}

/// What the fake provider serves right now.
struct Idp {
    issuer: String,
    jwks: Mutex<Value>,
    id_token: Mutex<String>,
    jwks_hits: AtomicUsize,
}

impl Idp {
    fn publish(&self, key: &Key) {
        *self.jwks.lock().unwrap() = json!({"keys": [key.jwk()]});
    }

    fn answer(&self, path: &str) -> Value {
        match path {
            "/.well-known/openid-configuration" => json!({"issuer": self.issuer, "authorization_endpoint": format!("{}/authorize", self.issuer), "token_endpoint": format!("{}/token", self.issuer), "jwks_uri": format!("{}/jwks", self.issuer)}),
            "/jwks" => {
                self.jwks_hits.fetch_add(1, Ordering::SeqCst);
                self.jwks.lock().unwrap().clone()
            }
            "/token" => json!({"id_token": self.id_token.lock().unwrap().clone()}),
            _ => json!({}),
        }
    }
}

/// One request per connection, an HTTP/1.1 answer with `Connection: close`.
async fn serve(listener: TcpListener, idp: Arc<Idp>) {
    loop {
        let Ok((mut sock, _)) = listener.accept().await else { return };
        let idp = idp.clone();
        tokio::spawn(async move {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            let (head_end, body_len) = loop {
                let n = sock.read(&mut chunk).await.unwrap_or(0);
                if n == 0 {
                    return;
                }
                buf.extend_from_slice(&chunk[..n]);
                if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&buf[..at]).to_lowercase();
                    let len = head.lines().find_map(|l| l.strip_prefix("content-length:")).and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(0);
                    break (at + 4, len);
                }
            };
            while buf.len() < head_end + body_len {
                let n = sock.read(&mut chunk).await.unwrap_or(0);
                if n == 0 {
                    return;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            let line = String::from_utf8_lossy(&buf).lines().next().unwrap_or("").to_string();
            let path = line.split(' ').nth(1).unwrap_or("/").to_string();
            let body = idp.answer(&path).to_string();
            let reply = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            let _ = sock.write_all(reply.as_bytes()).await;
            let _ = sock.shutdown().await;
        });
    }
}

/// The host: `auth.sso_login` makes a user of the verified identity.
struct Host;

impl HostBridge for Host {
    fn call(&self, name: &str, payload: Value, _ctx: CallContext) -> BoxFuture<Result<Value, BridgeError>> {
        let result = match name {
            "auth.sso_login" => Ok(json!({"user": {"key": 7, "username": payload["username"], "is_active": true, "is_superuser": false}, "created": true})),
            other => Err(BridgeError::Bridge(format!("no host function {other}"))),
        };
        Box::pin(async move { result })
    }
}

struct Rig {
    router: Router,
    security: Arc<Security>,
    idp: Arc<Idp>,
    log: Arc<Mutex<Vec<Value>>>,
}

async fn rig(first: &Key) -> Rig {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    let idp = Arc::new(Idp { issuer: issuer.clone(), jwks: Mutex::new(json!({})), id_token: Mutex::new(String::new()), jwks_hits: AtomicUsize::new(0) });
    idp.publish(first);
    tokio::spawn(serve(listener, idp.clone()));

    let driver = db::connect("sqlite://:memory:", 1).await.unwrap();
    let source = format!("datasource db {{\n  provider = \"sqlite\"\n}}\n\nmodel Note {{\n  id Int @id\n}}\n{APP}");
    let schema: Arc<Schema> = Arc::new(dsl::check(dsl::compile(&source, None).unwrap()).unwrap().1);
    for step in migrate::plan(&schema, &Default::default()).unwrap().up {
        driver.batch(step.sql).await.unwrap();
    }
    let site = Arc::new(Site::build(schema, &json!({"resources": [{"model": "Note"}]}).to_string()).unwrap());
    let settings = OidcSettings::from_value(&json!({"issuer": issuer, "client_id": "admin", "client_secret": "s"})).unwrap();
    let security = Security::new(SECRET, SecurityOptions { oidc: Some(settings), ..Default::default() }).unwrap();
    let mut opts = Options::new(None);
    opts.auth = Some(AuthConfig { security: security.clone(), bridge: Arc::new(Host) });
    let log = Arc::new(Mutex::new(Vec::new()));
    let sink = log.clone();
    opts.log = monorm_admin::service::Logger::new(move |line| sink.lock().unwrap().push(serde_json::from_str(&line).unwrap()));
    Rig { router: service(site, driver, opts), security, idp, log }
}

impl Rig {
    async fn get(&self, uri: &str, cookie: &str) -> (u16, axum::http::HeaderMap) {
        let mut req = Request::builder().method("GET").uri(uri).header("host", "localhost");
        if !cookie.is_empty() {
            req = req.header("cookie", cookie);
        }
        let mut req = req.body(axum::body::Body::empty()).unwrap();
        req.extensions_mut().insert(ConnectInfo("10.0.0.1:4000".parse::<SocketAddr>().unwrap()));
        let response = self.router.clone().oneshot(req).await.unwrap();
        (response.status().as_u16(), response.headers().clone())
    }

    fn claims(&self, nonce: &str) -> Value {
        let now = self.security.now();
        json!({"iss": self.idp.issuer, "aud": "admin", "sub": "u1", "exp": now + 300, "iat": now, "nonce": nonce, "email": "ann@example.com", "preferred_username": "ann"})
    }

    /// The cause that the last callback request logged: `None` when the login passed.
    fn refusal(&self) -> Option<String> {
        let log = self.log.lock().unwrap();
        let line = log.iter().rev().find(|l| l["path"] == "/admin/auth/callback").expect("a callback request was logged");
        line["error"].as_str().map(str::to_string)
    }

    /// A browser login. `token` makes the ID token that the provider hands out from the nonce of the flow.
    /// The answer is where the callback sends the browser.
    async fn login(&self, token: impl Fn(&str) -> String) -> String {
        let (status, headers) = self.get("/admin/auth/start", "").await;
        assert_eq!(status, 302, "start redirects to the provider");
        let to = headers["location"].to_str().unwrap().to_string();
        let query: Vec<(String, String)> = form_urlencoded::parse(to.split_once('?').unwrap().1.as_bytes()).into_owned().collect();
        let find = |name: &str| query.iter().find(|(k, _)| k == name).unwrap().1.clone();
        let flow: Vec<String> = headers.get_all("set-cookie").iter().map(|c| c.to_str().unwrap().split(';').next().unwrap().to_string()).collect();
        *self.idp.id_token.lock().unwrap() = token(&find("nonce"));
        let (status, headers) = self.get(&format!("/admin/auth/callback?code=c&state={}", find("state")), &flow.join("; ")).await;
        assert_eq!(status, 302, "the callback redirects");
        let location = headers["location"].to_str().unwrap().to_string();
        let signed_in = headers.get_all("set-cookie").iter().any(|c| c.to_str().unwrap().starts_with(&format!("{}=", monorm_admin::security::SESSION_COOKIE)));
        assert_eq!(signed_in, !location.contains("sso_error"), "a session cookie comes with a good login only: {location}");
        location
    }
}

#[tokio::test]
async fn an_rs256_token_signs_the_user_in() {
    let key = Key::rsa("r1", RSA_KEY_A);
    let rig = rig(&key).await;
    let location = rig.login(|nonce| key.token(&rig.claims(nonce))).await;
    assert_eq!(location, "/admin/");
    assert_eq!(rig.refusal(), None);
}

#[tokio::test]
async fn an_rs256_token_with_a_changed_payload_is_refused() {
    let key = Key::rsa("r1", RSA_KEY_A);
    let rig = rig(&key).await;
    let location = rig
        .login(|nonce| {
            let token = key.token(&rig.claims(nonce));
            let mut parts: Vec<&str> = token.split('.').collect();
            let forged = B64.encode(rig.claims(nonce).as_object().map(|c| json!({"sub": "admin", "iss": c["iss"], "aud": c["aud"], "exp": c["exp"], "nonce": nonce})).unwrap().to_string());
            parts[1] = &forged;
            parts.join(".")
        })
        .await;
    assert_eq!(location, "/admin/?sso_error=failed");
    assert_eq!(rig.refusal().as_deref(), Some("sso login refused: bad_signature"));
}

/// The provider rotates from `old` to `new`. Within 30 s the key set is not fetched again; after that it is.
async fn rotated_key_login(old: Key, new: Key, unknown: &str) {
    let rig = rig(&old).await;
    assert_eq!(rig.login(|nonce| old.token(&rig.claims(nonce))).await, "/admin/");
    assert_eq!(rig.idp.jwks_hits.load(Ordering::SeqCst), 1);

    rig.idp.publish(&new);
    assert_eq!(rig.login(|nonce| new.token(&rig.claims(nonce))).await, "/admin/?sso_error=failed");
    assert_eq!(rig.refusal().as_deref(), Some(unknown));
    assert_eq!(rig.idp.jwks_hits.load(Ordering::SeqCst), 1, "the key set is younger than 30 s");

    rig.security.advance(31);
    assert_eq!(rig.login(|nonce| new.token(&rig.claims(nonce))).await, "/admin/");
    assert_eq!(rig.refusal(), None);
    assert_eq!(rig.idp.jwks_hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn an_rs256_key_that_rotates_by_kid_is_fetched_again_once_the_throttle_ends() {
    // The same algorithm: the `kid` of the token alone finds no key in the cached set.
    rotated_key_login(Key::rsa("r1", RSA_KEY_A), Key::rsa("r2", RSA_KEY_B), "sso login refused: unknown_key").await;
}

#[tokio::test]
async fn a_rotation_from_rs256_to_es256_is_fetched_again_once_the_throttle_ends() {
    // Another algorithm: the cached RSA key does not fit an ES256 token.
    rotated_key_login(Key::rsa("r1", RSA_KEY_A), Key::ec("e2"), "sso login refused: unknown_key").await;
}

#[tokio::test]
async fn a_token_signed_by_a_second_key_under_the_old_kid_is_refused_even_after_the_throttle() {
    // The `kid` matches but the key does not: a forged signature, not a rotation.
    let old = Key::rsa("r1", RSA_KEY_A);
    let rig = rig(&old).await;
    assert_eq!(rig.login(|nonce| old.token(&rig.claims(nonce))).await, "/admin/");
    let impostor = Key::rsa("r1", RSA_KEY_B);
    rig.security.advance(31);
    assert_eq!(rig.login(|nonce| impostor.token(&rig.claims(nonce))).await, "/admin/?sso_error=failed");
    assert_eq!(rig.refusal().as_deref(), Some("sso login refused: bad_signature"));
    assert_eq!(rig.idp.jwks_hits.load(Ordering::SeqCst), 2, "the unknown signature made one more fetch");
}
