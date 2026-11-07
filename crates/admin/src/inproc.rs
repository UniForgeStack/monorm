//! The served router called in memory: the guard, login, CSRF and the headers run as on a socket,
//! with no port. A test client uses it; the headers and the client address are the caller's.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ConnectInfo;
use axum::http::Request;
use axum::Router;
use monorm_engine::db::Executor;
use tower::ServiceExt;

use crate::auth::AuthConfig;
use crate::service::{service, Logger, Options};
use crate::site::Site;

/// What one request got back. `headers` keeps repeated names (`set-cookie`) in order.
#[derive(Debug)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

#[derive(Clone)]
pub struct Routed {
    router: Router,
}

impl Routed {
    pub fn new(site: Arc<Site>, conn: Arc<dyn Executor>, auth: AuthConfig, timeout: Option<Duration>) -> Routed {
        let mut options = Options::new(timeout);
        options.auth = Some(auth);
        options.log = Logger::new(|_| {});
        Routed { router: service(site, conn, options) }
    }

    /// `ip` is the address that the rate limit sees. The request gets no `Host` header unless `headers` has one.
    pub async fn request(&self, method: &str, path: &str, headers: &[(String, String)], body: Option<String>, ip: &str) -> Result<Reply, String> {
        let addr: SocketAddr = format!("{}:4000", if ip.contains(':') { format!("[{ip}]") } else { ip.to_string() })
            .parse()
            .map_err(|_| format!("{ip:?} is not an IP address"))?;
        let mut builder = Request::builder().method(method).uri(path);
        for (name, value) in headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        let body = match body {
            Some(text) => axum::body::Body::from(text),
            None => axum::body::Body::empty(),
        };
        let mut request = builder.body(body).map_err(|e| e.to_string())?;
        request.extensions_mut().insert(ConnectInfo(addr));
        let Ok(response) = self.router.clone().oneshot(request).await;
        let (parts, body) = response.into_parts();
        let bytes = axum::body::to_bytes(body, usize::MAX).await.map_err(|e| e.to_string())?;
        let headers = parts.headers.iter().map(|(k, v)| (k.as_str().to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned())).collect();
        Ok(Reply { status: parts.status.as_u16(), headers, body: String::from_utf8_lossy(&bytes).into_owned() })
    }
}
