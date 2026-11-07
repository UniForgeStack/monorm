//! The served form: timeout and rollback, request ids, health, graceful stop, the table check.
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::routing::get;
use axum::Router;
use monorm_admin::service::{with_request_log, with_timeout, Logger};
use monorm_admin::{serve, Scoped, ServeConfig, Server, Site};
use monorm_core::{dsl, migrate, schema::Schema};
use monorm_engine::db::{self, Executor, Transaction};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const APP: &str = include_str!("../../../apps/admin/schema.prisma");

fn schema(extra: &str) -> Arc<Schema> {
    let source = format!("datasource db {{\n  provider = \"sqlite\"\n}}\n\nmodel Note {{\n  id Int @id\n}}\n{extra}");
    Arc::new(dsl::check(dsl::compile(&source, None).unwrap()).unwrap().1)
}

async fn sqlite(schema: &Schema) -> Arc<dyn db::Driver> {
    let conn = db::connect("sqlite://:memory:", 1).await.unwrap();
    for step in migrate::plan(schema, &Default::default()).unwrap().up {
        conn.batch(step.sql).await.unwrap();
    }
    conn
}

fn capture() -> (Logger, Arc<Mutex<Vec<Value>>>) {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = lines.clone();
    (Logger::new(move |line| sink.lock().unwrap().push(serde_json::from_str(&line).unwrap())), lines)
}

async fn http(addr: std::net::SocketAddr, request: &str) -> String {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut out = String::new();
    stream.read_to_string(&mut out).await.unwrap();
    out
}

async fn listener() -> tokio::net::TcpListener {
    tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap()
}

type Held = Arc<Mutex<Vec<Arc<dyn Transaction>>>>;

#[derive(Clone)]
struct Hold {
    conn: Arc<dyn Executor>,
    held: Held,
}

/// A route that begins a transaction, hands a reference to a "host" that keeps it, and runs past the timeout.
async fn slow(State(s): State<Hold>) -> &'static str {
    let scope = Scoped::begin(s.conn.as_ref()).await.unwrap();
    s.held.lock().unwrap().push(scope.transaction());
    tokio::time::sleep(Duration::from_secs(30)).await;
    scope.commit().await.unwrap();
    "late"
}

async fn quick(State(s): State<Hold>) -> &'static str {
    let scope = Scoped::begin(s.conn.as_ref()).await.unwrap();
    scope.executor().query_text("SELECT 1".into()).await.unwrap();
    scope.commit().await.unwrap();
    "ok"
}

fn hold_router(conn: Arc<dyn Executor>, held: Held, limit: Duration) -> Router {
    let routes = Router::new().route("/slow", get(slow)).route("/quick", get(quick)).with_state(Hold { conn, held });
    with_timeout(routes, limit)
}

async fn get_status(router: Router, uri: &str) -> u16 {
    use tower::ServiceExt;
    let request = axum::http::Request::builder().uri(uri).body(axum::body::Body::empty()).unwrap();
    router.oneshot(request).await.unwrap().status().as_u16()
}

#[tokio::test]
async fn timeout_rolls_back_a_transaction_the_host_still_holds() {
    // One SQLite connection: while the cancelled request's transaction stays open, the next request waits for it.
    let conn: Arc<dyn Executor> = sqlite(&schema("")).await;
    let held: Held = Arc::default();
    let router = hold_router(conn, held.clone(), Duration::from_millis(100));
    assert_eq!(get_status(router.clone(), "/slow").await, 504);
    assert_eq!(held.lock().unwrap().len(), 1, "the host still holds the transaction");
    let next = tokio::time::timeout(Duration::from_secs(5), get_status(router, "/quick")).await;
    let status = next.expect("the next request hangs");
    assert_eq!(status, 200, "504: the next request waited for the connection of a transaction that was not rolled back");
}

#[tokio::test]
async fn an_expired_request_answers_504_and_its_log_line_has_the_request_id() {
    let conn: Arc<dyn Executor> = sqlite(&schema("")).await;
    let (log, lines) = capture();
    let router = with_request_log(hold_router(conn, Arc::default(), Duration::from_millis(50)), log);
    use tower::ServiceExt;
    let request = axum::http::Request::builder().uri("/slow").header("x-request-id", "abc-1").body(axum::body::Body::empty()).unwrap();
    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), 504);
    assert_eq!(response.headers()["x-request-id"], "abc-1");
    let body = http_body_util::BodyExt::collect(response.into_body()).await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["error"]["kind"], "Timeout");
    let lines = lines.lock().unwrap();
    assert_eq!(lines.len(), 1);
    assert_eq!((lines[0]["request_id"].as_str(), lines[0]["status"].as_u64(), lines[0]["level"].as_str()), (Some("abc-1"), Some(504), Some("error")));
}

fn real_site(schema: &Arc<Schema>) -> Arc<Site> {
    Arc::new(Site::build(schema.clone(), &json!({"resources": [{"model": "Note"}]}).to_string()).unwrap())
}

#[tokio::test]
async fn health_answers_without_a_user_and_a_request_id_is_made_and_echoed() {
    let schema = schema(APP);
    let driver = sqlite(&schema).await;
    let (log, lines) = capture();
    let server = serve(real_site(&schema), driver, ServeConfig { host: "127.0.0.1".into(), port: 0, timeout: Duration::from_secs(5), db_settings: vec![], auth: None }, log).await.unwrap();
    let addr = server.addr();
    let health = http(addr, "GET /healthz HTTP/1.1\r\nhost: x\r\nconnection: close\r\n\r\n").await;
    assert!(health.starts_with("HTTP/1.1 200"), "{health}");
    let ready = http(addr, "GET /readyz HTTP/1.1\r\nhost: x\r\nconnection: close\r\n\r\n").await;
    assert!(ready.starts_with("HTTP/1.1 200"), "{ready}");
    let api = http(addr, "GET /admin/api/resources HTTP/1.1\r\nhost: x\r\nx-request-id: r-7\r\nconnection: close\r\n\r\n").await;
    assert!(api.starts_with("HTTP/1.1 401"), "{api}");
    assert!(api.to_ascii_lowercase().contains("x-request-id: r-7"), "{api}");
    let made = http(addr, "GET /healthz HTTP/1.1\r\nhost: x\r\nx-request-id: bad id\r\nconnection: close\r\n\r\n").await;
    let id = made.lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("x-request-id: ").map(str::to_string)).expect("a request id header");
    assert_eq!(id.len(), 36, "a new UUID replaces an id that is not a plain token: {id}");
    server.stop().await.unwrap();
    let lines = lines.lock().unwrap();
    let ids: Vec<&str> = lines.iter().filter(|l| l["msg"] == "request").map(|l| l["request_id"].as_str().unwrap()).collect();
    assert_eq!(ids.len(), 4);
    assert!(ids.contains(&"r-7") && ids.contains(&id.as_str()));
    assert!(lines.iter().any(|l| l["msg"] == "listening" && l["url"].as_str().unwrap().starts_with("http://127.0.0.1:")));
}

#[tokio::test]
async fn stop_lets_an_open_request_finish_then_the_port_closes_and_readiness_says_draining() {
    let draining = Arc::new(AtomicBool::new(false));
    let app = Router::new().route("/wait", get(|| async {
        tokio::time::sleep(Duration::from_millis(400)).await;
        "done"
    }));
    let listener = listener().await;
    let server = Arc::new(Server::start(listener, app, draining.clone()));
    let addr = server.addr();
    let open = tokio::spawn(async move { http(addr, "GET /wait HTTP/1.1\r\nhost: x\r\nconnection: close\r\n\r\n").await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    server.stop().await.unwrap();
    assert!(draining.load(std::sync::atomic::Ordering::SeqCst));
    let response = open.await.unwrap();
    assert!(response.starts_with("HTTP/1.1 200") && response.ends_with("done"), "{response}");
    assert!(tokio::net::TcpStream::connect(addr).await.is_err(), "the port is closed");
}

#[tokio::test]
async fn readiness_answers_503_once_draining() {
    use monorm_admin::service::{service, Options};
    use tower::ServiceExt;
    let schema = schema(APP);
    let driver: Arc<dyn Executor> = sqlite(&schema).await;
    let options = Options::new(None);
    let draining = options.draining.clone();
    let router = service(real_site(&schema), driver, options);
    let ask = |router: Router| async move {
        let request = axum::http::Request::builder().uri("/readyz").body(axum::body::Body::empty()).unwrap();
        router.oneshot(request).await.unwrap().status().as_u16()
    };
    assert_eq!(ask(router.clone()).await, 200);
    draining.store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(ask(router).await, 503);
}

#[tokio::test]
async fn startup_fails_when_the_schema_has_no_admin_model_or_the_database_has_no_table() {
    let (log, _) = capture();
    let config = || ServeConfig { host: "127.0.0.1".into(), port: 0, timeout: Duration::from_secs(1), db_settings: vec![], auth: None };

    let plain = schema("");
    let err = serve(real_site(&plain), sqlite(&plain).await, config(), log.clone()).await.err().expect("no admin model");
    assert!(err.contains("AdminLogEntry") && err.contains("import"), "{err}");

    let with_app = schema(APP);
    let bare = sqlite(&plain).await;
    let err = serve(real_site(&with_app), bare, config(), log.clone()).await.err().expect("no tables");
    assert!(err.contains("adminlogentry"), "{err}");
    assert!(err.contains("makemigrations") && err.contains("migrate"), "{err}");

    let ok = serve(real_site(&with_app), sqlite(&with_app).await, config(), log).await.unwrap();
    ok.stop().await.unwrap();
}

#[tokio::test]
async fn db_settings_on_sqlite_is_ignored_with_a_startup_message() {
    let with_app = schema(APP);
    let (log, lines) = capture();
    let config = ServeConfig { host: "127.0.0.1".into(), port: 0, timeout: Duration::from_secs(1), db_settings: vec![("app.tenant".into(), "a".into())], auth: None };
    let server = serve(real_site(&with_app), sqlite(&with_app).await, config, log).await.unwrap();
    server.stop().await.unwrap();
    assert!(lines.lock().unwrap().iter().any(|l| l["level"] == "warn" && l["msg"].as_str().unwrap().contains("db_settings")));
}

/// A row-level security policy reads the setting that `db_settings` puts in every admin transaction.
/// A superuser bypasses RLS, so the lists run through a second driver that logs in as a plain role.
#[tokio::test]
async fn db_settings_reach_row_level_security_policies_on_postgres() {
    let Ok(url) = std::env::var("ORM_RUST_TEST_POSTGRES_URL") else {
        eprintln!("skipped: ORM_RUST_TEST_POSTGRES_URL is not set");
        return;
    };
    let source = "datasource db {\n  provider = \"postgresql\"\n}\n\nmodel RlsNote {\n  id     Int    @id\n  tenant String\n}\n";
    let schema = Arc::new(dsl::check(dsl::compile(source, None).unwrap()).unwrap().1);
    let owner = db::connect(&url, 2).await.unwrap();
    owner
        .batch(
            "DROP TABLE IF EXISTS rlsnote;
             DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'monorm_rls_reader') THEN CREATE ROLE monorm_rls_reader LOGIN; END IF; END $$;
             ALTER ROLE monorm_rls_reader PASSWORD 'rls' NOSUPERUSER NOBYPASSRLS"
                .into(),
        )
        .await
        .unwrap();
    for step in migrate::plan(&schema, &Default::default()).unwrap().up {
        owner.batch(step.sql).await.unwrap();
    }
    owner
        .batch(
            "INSERT INTO rlsnote (id, tenant) VALUES (1, 'a'), (2, 'b'), (3, 'a');
             ALTER TABLE rlsnote ENABLE ROW LEVEL SECURITY;
             CREATE POLICY by_tenant ON rlsnote USING (tenant = current_setting('app.tenant', true));
             GRANT SELECT ON rlsnote TO monorm_rls_reader"
                .into(),
        )
        .await
        .unwrap();
    let (scheme, rest) = url.split_once("://").unwrap();
    let host = rest.rsplit_once('@').map(|(_, h)| h).unwrap_or(rest);
    let reader = db::connect(&format!("{scheme}://monorm_rls_reader:rls@{host}"), 2).await.unwrap();

    let site = Arc::new(Site::build(schema.clone(), "").unwrap());
    let user = Some(json!({"is_superuser": true}));
    let ids = |body: &str| -> Vec<i64> {
        let body: Value = serde_json::from_str(body).unwrap();
        body["rows"].as_array().unwrap_or(&vec![]).iter().map(|r| r[0].as_i64().unwrap()).collect()
    };
    let (log, lines) = capture();

    let plain: Arc<dyn Executor> = reader.clone();
    let (status, body) = monorm_admin::call(site.clone(), plain, "GET", "/admin/api/resources/RlsNote", user.clone()).await;
    assert_eq!((status, ids(&body)), (200, vec![]), "no setting: the policy hides every row");

    let wrapped = monorm_admin::serve::with_settings(reader, &[("app.tenant".into(), "a".into())], &log);
    let (status, body) = monorm_admin::call(site, wrapped, "GET", "/admin/api/resources/RlsNote", user).await;
    assert_eq!((status, ids(&body)), (200, vec![1, 3]), "{body}");
    assert!(lines.lock().unwrap().is_empty(), "no startup message on PostgreSQL");

    owner.batch("DROP TABLE rlsnote".into()).await.unwrap();
}
