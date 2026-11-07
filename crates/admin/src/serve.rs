//! The socket: `monorm admin` binds it, nothing else does. Handlers never read it.

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use monorm_engine::db::{Driver, Executor, WithSettings};
use serde_json::json;
use tokio::sync::{watch, Notify};

use crate::service::{self, Logger, Options};
use crate::site::Site;
use crate::tables;

pub struct ServeConfig {
    pub host: String,
    /// 0 picks a free port; `Server::addr` says which.
    pub port: u16,
    /// One limit for every request.
    pub timeout: Duration,
    /// Each pair runs as `set_config(name, value, true)` at the start of every admin transaction.
    pub db_settings: Vec<(String, String)>,
}

/// A running server. The host stops it on SIGTERM; the library installs no signal handler.
pub struct Server {
    addr: SocketAddr,
    stopping: Arc<std::sync::atomic::AtomicBool>,
    shutdown: Arc<Notify>,
    done: watch::Receiver<Option<Result<(), String>>>,
}

impl Server {
    /// Serves `app` on `listener`. `stopping` is the flag that readiness reads.
    pub fn start(listener: tokio::net::TcpListener, app: axum::Router, stopping: Arc<std::sync::atomic::AtomicBool>) -> Server {
        let addr = listener.local_addr().expect("a bound listener has an address");
        let shutdown = Arc::new(Notify::new());
        let (done_tx, done) = watch::channel(None);
        let signal = shutdown.clone();
        tokio::spawn(async move {
            let out = axum::serve(listener, app).with_graceful_shutdown(async move { signal.notified().await }).await.map_err(|e| e.to_string());
            let _ = done_tx.send(Some(out));
        });
        Server { addr, stopping, shutdown, done }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Readiness answers 503 from now on, new connections stop, and open requests finish.
    pub async fn stop(&self) -> Result<(), String> {
        self.stopping.store(true, Ordering::SeqCst);
        self.shutdown.notify_one();
        self.wait().await
    }

    /// Waits until the server has ended.
    pub async fn wait(&self) -> Result<(), String> {
        let mut done = self.done.clone();
        let ended = done.wait_for(|v| v.is_some()).await.map_err(|e| e.to_string())?;
        ended.clone().unwrap_or(Ok(()))
    }
}

/// The driver every admin transaction goes through: with `db_settings`, each transaction starts with
/// `set_config`. SQLite has no `set_config`, so it ignores the key and says so.
pub fn with_settings(driver: Arc<dyn Driver>, settings: &[(String, String)], log: &Logger) -> Arc<dyn Executor> {
    if settings.is_empty() {
        return driver;
    }
    if driver.dialect() != monorm_core::dialect::Dialect::Postgres {
        log.line("warn", "db_settings is ignored: this database has no set_config", json!({"dialect": driver.dialect().name()}));
        return driver;
    }
    Arc::new(WithSettings::new(driver, settings.to_vec()))
}

/// Checks the admin tables, wraps the driver for `db_settings`, binds the socket and serves.
pub async fn serve(site: Arc<Site>, driver: Arc<dyn Driver>, config: ServeConfig, log: Logger) -> Result<Server, String> {
    tables::check(&site.schema, driver.as_ref()).await?;
    let conn = with_settings(driver, &config.db_settings, &log);
    let listener = tokio::net::TcpListener::bind((config.host.as_str(), config.port))
        .await
        .map_err(|e| format!("cannot listen on {}:{}: {e}", config.host, config.port))?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    let mut options = Options::new(Some(config.timeout));
    options.log = log.clone();
    let stopping = options.draining.clone();
    let app = service::service(site, conn, options);
    let server = Server::start(listener, app, stopping);
    log.line("info", "listening", json!({"addr": addr.to_string(), "url": format!("http://{addr}"), "timeout_ms": config.timeout.as_millis() as u64}));
    Ok(server)
}
