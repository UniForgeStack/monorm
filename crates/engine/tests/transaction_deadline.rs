//! The transaction deadline against the real drivers: statement stop, idle expiry, busy waits,
//! connection reuse. The fake-driver tests of `db/deadline.rs` cover the state machine.
use std::time::{Duration, Instant};

use monorm_engine::db::{self, begin_root, DbError, ErrorKind, Executor, Isolation, TimeoutOutcome, TransactionOptions};

fn runtime() -> tokio::runtime::Runtime { tokio::runtime::Runtime::new().unwrap() }

fn options(isolation: Option<Isolation>, timeout_ms: u64) -> TransactionOptions {
    TransactionOptions { isolation, timeout: Some(Duration::from_millis(timeout_ms)) }
}

fn assert_timeout(error: &DbError, outcome: TimeoutOutcome) {
    assert_eq!(error.kind, ErrorKind::TransactionTimeout, "{error:?}");
    assert_eq!(error.outcome, Some(outcome), "{error:?}");
}

#[cfg(feature = "sqlite")]
mod sqlite {
    use super::*;

    const FOREVER: &str = "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c) SELECT count(*) FROM c";

    async fn count(db: &dyn Executor) -> i64 {
        db.query("SELECT count(*) FROM probe".into(), vec![]).await.unwrap().get_i64(0, 0).unwrap()
    }

    #[test]
    fn running_statement_is_interrupted_and_rolled_back() {
        runtime().block_on(async {
            let db = db::connect("sqlite://:memory:", 1).await.unwrap();
            db.batch("CREATE TABLE probe (id INTEGER PRIMARY KEY)".into()).await.unwrap();
            let tx = begin_root(&*db, options(None, 200)).await.unwrap();
            tx.batch("INSERT INTO probe VALUES (1)".into()).await.unwrap();
            let started = Instant::now();
            let error = tx.query(FOREVER.into(), vec![]).await.err().unwrap();
            assert_timeout(&error, TimeoutOutcome::RolledBack);
            assert!(started.elapsed() < Duration::from_secs(3), "{:?}", started.elapsed());
            assert_eq!(count(&*db).await, 0, "the insert rolled back");
        });
    }

    #[test]
    fn idle_expiry_rolls_back_and_the_worker_is_reusable() {
        runtime().block_on(async {
            let db = db::connect("sqlite://:memory:", 1).await.unwrap();
            db.batch("CREATE TABLE probe (id INTEGER PRIMARY KEY)".into()).await.unwrap();
            let tx = begin_root(&*db, options(Some(Isolation::Serializable), 100)).await.unwrap();
            tx.batch("INSERT INTO probe VALUES (1)".into()).await.unwrap();
            tokio::time::sleep(Duration::from_millis(400)).await;
            assert_timeout(&tx.commit().await.err().unwrap(), TimeoutOutcome::RolledBack);
            assert_eq!(count(&*db).await, 0);
            let tx = begin_root(&*db, options(None, 5_000)).await.unwrap();
            tx.batch("INSERT INTO probe VALUES (2)".into()).await.unwrap();
            tx.commit().await.unwrap();
            assert_eq!(count(&*db).await, 1);
        });
    }

    #[test]
    fn every_isolation_level_runs() {
        runtime().block_on(async {
            let db = db::connect("sqlite://:memory:", 1).await.unwrap();
            for level in [Isolation::ReadCommitted, Isolation::RepeatableRead, Isolation::Serializable] {
                let tx = begin_root(&*db, TransactionOptions { isolation: Some(level), timeout: None }).await.unwrap();
                tx.query("SELECT 1".into(), vec![]).await.unwrap();
                tx.commit().await.unwrap();
            }
        });
    }

    #[test]
    fn busy_wait_stops_at_the_deadline() {
        runtime().block_on(async {
            let dir = std::env::temp_dir().join(format!("monorm-deadline-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("busy.db");
            let db = db::connect(&format!("sqlite://{}", path.display()), 1).await.unwrap();
            db.batch("CREATE TABLE probe (id INTEGER PRIMARY KEY)".into()).await.unwrap();
            let other = rusqlite::Connection::open(&path).unwrap();
            other.execute_batch("BEGIN IMMEDIATE").unwrap();
            let started = Instant::now();
            let error = begin_root(&*db, options(None, 300)).await.err().unwrap();
            assert_timeout(&error, TimeoutOutcome::RolledBack);
            assert!(started.elapsed() < Duration::from_secs(3), "the 5 s busy timeout was cut at the deadline: {:?}", started.elapsed());
            other.execute_batch("ROLLBACK").unwrap();
            let tx = begin_root(&*db, options(None, 5_000)).await.unwrap();
            tx.commit().await.unwrap();
            let _ = std::fs::remove_dir_all(&dir);
        });
    }

    #[test]
    fn a_commit_that_waits_for_a_reader_past_the_deadline_is_a_timeout() {
        runtime().block_on(async {
            let dir = std::env::temp_dir().join(format!("monorm-deadline-commit-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("busy.db");
            let db = db::connect(&format!("sqlite://{}", path.display()), 1).await.unwrap();
            db.batch("CREATE TABLE probe (id INTEGER PRIMARY KEY)".into()).await.unwrap();
            let tx = begin_root(&*db, options(None, 300)).await.unwrap();
            tx.batch("INSERT INTO probe VALUES (1)".into()).await.unwrap();
            // A reader on a second connection keeps its shared lock, so the COMMIT cannot get the exclusive lock.
            let reader = rusqlite::Connection::open(&path).unwrap();
            reader.execute_batch("BEGIN").unwrap();
            reader.query_row("SELECT count(*) FROM probe", [], |r| r.get::<_, i64>(0)).unwrap();
            let started = Instant::now();
            let error = tx.commit().await.err().unwrap();
            // The COMMIT was sent, so the outcome is unknown; the lock error is not what the caller sees.
            assert_timeout(&error, TimeoutOutcome::Unknown);
            // The caller does not wait for the busy wait (5 s) or the cleanup grace: it returns near the deadline.
            assert!(started.elapsed() < Duration::from_secs(1), "{:?}", started.elapsed());
            reader.execute_batch("ROLLBACK").unwrap();
            // The abandoned COMMIT may still finish once the reader is gone: the outcome was unknown, so either result is fine.
            assert!(count(&*db).await <= 1);
            let _ = std::fs::remove_dir_all(&dir);
        });
    }

    #[test]
    fn the_busy_timeout_is_restored_after_a_deadline_job() {
        runtime().block_on(async {
            let db = db::connect("sqlite://:memory:", 1).await.unwrap();
            let tx = begin_root(&*db, options(None, 60_000)).await.unwrap();
            tx.commit().await.unwrap();
            let rows = db.query_text("PRAGMA busy_timeout".into()).await.unwrap();
            assert_eq!(rows[0][0].as_deref(), Some("5000"));
        });
    }

    #[test]
    fn cancel_does_not_hit_the_next_lease() {
        runtime().block_on(async {
            let db = db::connect("sqlite://:memory:", 1).await.unwrap();
            for round in 0..20 {
                let tx = begin_root(&*db, options(None, 30)).await.unwrap();
                let error = tx.query(FOREVER.into(), vec![]).await.err().unwrap();
                assert_timeout(&error, TimeoutOutcome::RolledBack);
                // The next lease starts at once and runs a statement that an old interrupt would break.
                let next = begin_root(&*db, options(None, 10_000)).await.unwrap();
                let rows = next.query("WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < 200000) SELECT count(*) FROM c".into(), vec![]).await
                    .unwrap_or_else(|e| panic!("round {round}: {e:?}"));
                assert_eq!(rows.get_i64(0, 0).unwrap(), 200_000);
                next.commit().await.unwrap();
            }
        });
    }
}

mod postgres {
    use super::*;

    fn url() -> Option<String> { std::env::var("ORM_RUST_TEST_POSTGRES_URL").ok() }

    async fn pid(db: &dyn Executor) -> String {
        db.query_text("SELECT pg_backend_pid()::text".into()).await.unwrap()[0][0].clone().unwrap()
    }

    #[test]
    fn isolation_levels_apply_and_do_not_leak() {
        let Some(url) = url() else { return };
        runtime().block_on(async {
            let db = db::connect(&url, 1).await.unwrap();
            for (level, shown) in [(Isolation::ReadCommitted, "read committed"), (Isolation::RepeatableRead, "repeatable read"), (Isolation::Serializable, "serializable")] {
                let tx = begin_root(&*db, TransactionOptions { isolation: Some(level), timeout: None }).await.unwrap();
                assert_eq!(tx.query_text("SHOW transaction_isolation".into()).await.unwrap()[0][0].as_deref(), Some(shown));
                tx.commit().await.unwrap();
                let plain = begin_root(&*db, TransactionOptions::default()).await.unwrap();
                assert_eq!(plain.query_text("SHOW transaction_isolation".into()).await.unwrap()[0][0].as_deref(), Some("read committed"), "after {shown}");
                plain.commit().await.unwrap();
            }
        });
    }

    #[test]
    fn running_statement_is_cancelled_and_the_connection_is_retired() {
        let Some(url) = url() else { return };
        runtime().block_on(async {
            let db = db::connect(&url, 1).await.unwrap();
            let before = pid(&*db).await;
            let tx = begin_root(&*db, options(None, 300)).await.unwrap();
            let started = Instant::now();
            let error = tx.batch("SELECT pg_sleep(30)".into()).await.err().unwrap();
            assert_timeout(&error, TimeoutOutcome::RolledBack);
            assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
            // A cancel was sent, so the connection is gone: the next lease cannot receive a late cancel.
            let after = pid(&*db).await;
            assert_ne!(before, after);
        });
    }

    #[test]
    fn idle_expiry_rolls_back_without_a_cancel_and_reuses_the_connection() {
        let Some(url) = url() else { return };
        runtime().block_on(async {
            let db = db::connect(&url, 1).await.unwrap();
            db.batch("CREATE TABLE IF NOT EXISTS tx_deadline_probe (id INT PRIMARY KEY)".into()).await.unwrap();
            db.batch("DELETE FROM tx_deadline_probe".into()).await.unwrap();
            let before = pid(&*db).await;
            let tx = begin_root(&*db, options(None, 150)).await.unwrap();
            tx.batch("INSERT INTO tx_deadline_probe VALUES (1)".into()).await.unwrap();
            tokio::time::sleep(Duration::from_millis(600)).await;
            assert_timeout(&tx.commit().await.err().unwrap(), TimeoutOutcome::RolledBack);
            let rows = db.query_text("SELECT count(*)::text, pg_backend_pid()::text FROM tx_deadline_probe".into()).await.unwrap();
            assert_eq!(rows[0][0].as_deref(), Some("0"));
            assert_eq!(rows[0][1].as_deref(), Some(before.as_str()), "no cancel was sent, so the connection is reused");
        });
    }

    #[test]
    fn waiting_for_a_connection_counts_against_the_deadline() {
        let Some(url) = url() else { return };
        runtime().block_on(async {
            let db = db::connect(&url, 1).await.unwrap();
            let holder = begin_root(&*db, TransactionOptions::default()).await.unwrap();
            let started = Instant::now();
            let error = begin_root(&*db, options(None, 300)).await.err().unwrap();
            assert_timeout(&error, TimeoutOutcome::RolledBack);
            assert!(started.elapsed() < Duration::from_secs(3));
            holder.commit().await.unwrap();
        });
    }
}
