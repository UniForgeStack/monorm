//! The settings transport on PostgreSQL: a scoped autocommit statement costs the same client
//! flights as an unscoped one (the pipelining gate of the scope runtime), the transaction
//! remembers what it set, and a failed or dropped statement leaves no setting behind.
//! A TCP proxy delays every chunk by `DELAY` in each direction, so one round trip is about
//! `2 * DELAY`, and it counts the flights the client sends.
#![cfg(feature = "postgres")]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use monorm_engine::db::{self, Executor, SettingsTx, WithSettings};
use sea_query::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const DELAY: Duration = Duration::from_millis(40);

fn url() -> Option<String> { std::env::var("ORM_RUST_TEST_POSTGRES_URL").ok() }

fn set(pairs: &[(&str, &str)]) -> Vec<(String, String)> { pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect() }

async fn pump(mut from: tokio::net::tcp::OwnedReadHalf, mut to: tokio::net::tcp::OwnedWriteHalf, flights: Option<Arc<AtomicUsize>>) {
    // A chunk leaves `DELAY` after it arrived; chunks do not queue behind each other's delay.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(Instant, Vec<u8>)>();
    tokio::spawn(async move {
        while let Some((at, bytes)) = rx.recv().await {
            tokio::time::sleep_until((at + DELAY).into()).await;
            if to.write_all(&bytes).await.is_err() { break; }
        }
    });
    let mut buf = vec![0u8; 65536];
    while let Ok(n) = from.read(&mut buf).await {
        if n == 0 { break; }
        if let Some(f) = &flights { f.fetch_add(1, Ordering::SeqCst); }
        if tx.send((Instant::now(), buf[..n].to_vec())).is_err() { break; }
    }
}

/// The URL of a delaying proxy in front of `url`, and the counter of client flights.
async fn proxy(url: &str) -> (String, Arc<AtomicUsize>) {
    let cfg: tokio_postgres::Config = url.parse().unwrap();
    let host = match &cfg.get_hosts()[0] { tokio_postgres::config::Host::Tcp(h) => h.clone(), _ => "localhost".into() };
    let upstream = format!("{host}:{}", cfg.get_ports()[0]);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let flights = Arc::new(AtomicUsize::new(0));
    let counter = flights.clone();
    tokio::spawn(async move {
        loop {
            let (down, _) = listener.accept().await.unwrap();
            let up = TcpStream::connect(&upstream).await.unwrap();
            let (dr, dw) = down.into_split();
            let (ur, uw) = up.into_split();
            tokio::spawn(pump(dr, uw, Some(counter.clone())));
            tokio::spawn(pump(ur, dw, None));
        }
    });
    let user = cfg.get_user().unwrap();
    let auth = cfg.get_password().map(|p| format!("{user}:{}", String::from_utf8_lossy(p))).unwrap_or_else(|| user.to_owned());
    (format!("postgres://{auth}@127.0.0.1:{port}/{}", cfg.get_dbname().unwrap()), flights)
}

async fn cell(db: &dyn Executor, sql: &str) -> Option<String> {
    db.query_text(sql.to_owned()).await.unwrap().remove(0).remove(0)
}

const SHOP: &str = "SELECT nullif(current_setting('scope.shop', true), '')";

#[tokio::test]
async fn scoped_autocommit_statement_costs_the_flights_of_an_unscoped_one() {
    let Some(url) = url() else { return };
    let (proxied, flights) = proxy(&url).await;
    let plain = db::connect(&proxied, 1).await.unwrap();
    let scoped = WithSettings::new(plain.clone(), set(&[("scope.shop", "7"), ("monorm.bypass", "{}")]));
    let warm = "SELECT current_setting('scope.shop', true) AS warm";
    // Warm the statement caches: the first run of each text pays its prepare.
    for _ in 0..2 {
        plain.query_text(warm.into()).await.unwrap();
        scoped.query(warm.into(), vec![]).await.unwrap();
    }
    let measure = |sql: &'static str, scoped_run: bool| {
        let (plain, scoped, flights) = (plain.clone(), &scoped, flights.clone());
        async move {
            let before = flights.load(Ordering::SeqCst);
            let started = Instant::now();
            if scoped_run { scoped.query(sql.into(), vec![]).await.unwrap(); } else { plain.query(sql.into(), vec![]).await.unwrap(); }
            (flights.load(Ordering::SeqCst) - before, started.elapsed())
        }
    };
    let (plain_warm, plain_time) = measure(warm, false).await;
    let (scoped_warm, scoped_time) = measure(warm, true).await;
    assert_eq!(scoped_warm, plain_warm, "warm: scoped {scoped_warm} flights, plain {plain_warm}");
    let rtt = 2.0 * DELAY.as_secs_f64();
    assert!(scoped_time.as_secs_f64() < 1.6 * rtt, "warm scoped statement took {scoped_time:?}");

    let (plain_cold, plain_cold_time) = measure("SELECT 41 AS cold_plain", false).await;
    let (scoped_cold, scoped_cold_time) = measure("SELECT 42 AS cold_scoped", true).await;
    assert_eq!(scoped_cold, plain_cold, "cold: scoped {scoped_cold} flights, plain {plain_cold}");
    eprintln!(
        "TRANSPORT one-way delay {DELAY:?}: warm plain {plain_warm} flights {:.1} RTT, scoped {scoped_warm} flights {:.1} RTT; cold plain {plain_cold} flights {:.1} RTT, scoped {scoped_cold} flights {:.1} RTT",
        plain_time.as_secs_f64() / rtt, scoped_time.as_secs_f64() / rtt, plain_cold_time.as_secs_f64() / rtt, scoped_cold_time.as_secs_f64() / rtt,
    );
    scoped_warm_result(&scoped).await;
}

async fn scoped_warm_result(scoped: &WithSettings) {
    let rows = scoped.query(format!("{SHOP} AS shop"), vec![]).await.unwrap();
    assert_eq!(rows.len(), 1);
}

#[tokio::test]
async fn autocommit_settings_do_not_stay_on_the_connection() {
    let Some(url) = url() else { return };
    let plain = db::connect(&url, 1).await.unwrap();
    let scoped = WithSettings::new(plain.clone(), set(&[("scope.shop", "7")]));
    assert_eq!(cell(&scoped, SHOP).await, Some("7".into()));
    assert_eq!(cell(&*plain, SHOP).await, None, "the one pooled connection kept the setting");
    let row = scoped.query("SELECT $1::text".into(), vec![Value::String(Some("x".into()))]).await.unwrap();
    assert_eq!(row.len(), 1);
    assert_eq!(cell(&*plain, SHOP).await, None);
    plain.close().await;
}

#[tokio::test]
async fn a_transaction_sets_only_what_changed_and_restores_it() {
    let Some(url) = url() else { return };
    let (proxied, flights) = proxy(&url).await;
    let plain = db::connect(&proxied, 1).await.unwrap();
    let tx = plain.begin().await.unwrap();
    tx.settle(set(&[("scope.shop", "1")])).await.unwrap();
    assert_eq!(cell(&*tx, SHOP).await, Some("1".into()));

    // The same set again sends nothing.
    let before = flights.load(Ordering::SeqCst);
    tx.settle(set(&[("scope.shop", "1")])).await.unwrap();
    assert_eq!(flights.load(Ordering::SeqCst), before, "an unchanged set sent a flight");

    // A block with another value: its statement sees it, and the next statement sees the old one again.
    let block = SettingsTx::wrap(tx.clone(), set(&[("scope.shop", "2")]));
    assert_eq!(cell(&*block, SHOP).await, Some("2".into()));
    block.query(format!("{SHOP} AS shop"), vec![]).await.unwrap();
    let before = flights.load(Ordering::SeqCst);
    block.query(format!("{SHOP} AS shop"), vec![]).await.unwrap();
    assert_eq!(flights.load(Ordering::SeqCst), before + 1, "a statement of the block with its values in place costs one flight");
    assert_eq!(cell(&*tx, SHOP).await, Some("2".into()), "the outer handle sends no settings of its own");
    tx.settle(set(&[("scope.shop", "1")])).await.unwrap();
    assert_eq!(cell(&*tx, SHOP).await, Some("1".into()));

    // A name that is no longer wanted resets to the empty string.
    tx.settle(set(&[])).await.unwrap();
    assert_eq!(cell(&*tx, SHOP).await, None);
    tx.rollback().await.unwrap();
    plain.close().await;
}

#[tokio::test]
async fn a_rolled_back_savepoint_forgets_what_it_set_and_a_released_one_keeps_it() {
    let Some(url) = url() else { return };
    let plain = db::connect(&url, 1).await.unwrap();
    let tx = plain.begin().await.unwrap();
    tx.settle(set(&[("scope.shop", "1")])).await.unwrap();

    let sp = tx.begin().await.unwrap();
    sp.settle(set(&[("scope.shop", "2")])).await.unwrap();
    assert_eq!(cell(&*sp, SHOP).await, Some("2".into()));
    sp.rollback().await.unwrap();
    assert_eq!(cell(&*tx, SHOP).await, Some("1".into()), "PostgreSQL reverted the value");
    // The tracker agrees: asking for "1" again sends nothing, and the value is still "1".
    tx.settle(set(&[("scope.shop", "1")])).await.unwrap();
    assert_eq!(cell(&*tx, SHOP).await, Some("1".into()));

    let sp = tx.begin().await.unwrap();
    sp.settle(set(&[("scope.shop", "3")])).await.unwrap();
    sp.commit().await.unwrap();
    assert_eq!(cell(&*tx, SHOP).await, Some("3".into()), "RELEASE keeps the value");
    tx.settle(set(&[("scope.shop", "3")])).await.unwrap();
    assert_eq!(cell(&*tx, SHOP).await, Some("3".into()));
    tx.rollback().await.unwrap();
    plain.close().await;
}

#[tokio::test]
async fn a_failed_statement_surfaces_its_own_error_and_leaves_no_setting() {
    let Some(url) = url() else { return };
    let plain = db::connect(&url, 1).await.unwrap();
    let scoped = WithSettings::new(plain.clone(), set(&[("scope.shop", "7")]));
    let error = scoped.query("SELECT 1 / 0".into(), vec![]).await.err().expect("division by zero");
    assert!(error.message.contains("division by zero"), "{error:?}");
    assert_eq!(cell(&*plain, SHOP).await, None, "the failed statement left its setting on the connection");
    plain.close().await;
}

#[tokio::test]
async fn a_commit_error_surfaces_and_the_connection_is_clean() {
    let Some(url) = url() else { return };
    let plain = db::connect(&url, 1).await.unwrap();
    plain.batch("DROP TABLE IF EXISTS st_child; DROP TABLE IF EXISTS st_parent; CREATE TABLE st_parent (id int PRIMARY KEY); CREATE TABLE st_child (id int PRIMARY KEY, parent int REFERENCES st_parent(id) DEFERRABLE INITIALLY DEFERRED)".into()).await.unwrap();
    let scoped = WithSettings::new(plain.clone(), set(&[("scope.shop", "7")]));
    let error = scoped.execute("INSERT INTO st_child VALUES (1, 99)".into(), vec![]).await.err().expect("the deferred check fails at COMMIT");
    assert!(error.message.contains("st_child"), "{error:?}");
    assert_eq!(cell(&*plain, SHOP).await, None);
    assert_eq!(cell(&*plain, "SELECT count(*)::text FROM st_child").await, Some("0".into()));
    plain.batch("DROP TABLE st_child; DROP TABLE st_parent".into()).await.unwrap();
    plain.close().await;
}

#[tokio::test]
async fn a_statement_dropped_in_flight_leaves_no_open_transaction_in_the_pool() {
    let Some(url) = url() else { return };
    let plain = db::connect(&url, 1).await.unwrap();
    let scoped = WithSettings::new(plain.clone(), set(&[("scope.shop", "7")]));
    let timed_out = tokio::time::timeout(Duration::from_millis(150), scoped.query("SELECT pg_sleep(2)".into(), vec![])).await;
    assert!(timed_out.is_err(), "the statement ended before the drop");
    // The one pooled connection is replaced, not returned inside the dropped transaction.
    assert_eq!(cell(&*plain, SHOP).await, None);
    assert_eq!(cell(&*plain, "SELECT (xact_start = statement_timestamp())::text FROM pg_stat_activity WHERE pid = pg_backend_pid()").await, Some("true".into()));
    plain.close().await;
}
