//! Transaction options and the deadline of a root transaction.
//!
//! The engine owns the deadline. It starts when the outer `transaction` is entered, before
//! the pool or the SQLite gate is waited on, and it covers setup, idle time, every statement
//! and the commit. When it passes, the engine marks the root expired, lets the driver cancel
//! what runs and roll back, and every later use of the transaction raises a timeout error.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::watch;
use tokio::time::Instant;

use super::{DbError, DbResult, ErrorKind, Transaction};

/// The separate time a driver gets for cancel, drain and rollback after the deadline.
pub const CLEANUP_GRACE: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Isolation {
    ReadCommitted,
    RepeatableRead,
    Serializable,
}

impl Isolation {
    pub fn parse(name: &str) -> DbResult<Self> {
        match name {
            "read_committed" => Ok(Isolation::ReadCommitted),
            "repeatable_read" => Ok(Isolation::RepeatableRead),
            "serializable" => Ok(Isolation::Serializable),
            other => Err(DbError::new(
                ErrorKind::Query,
                format!("unknown isolation level {other:?}; use \"read_committed\", \"repeatable_read\" or \"serializable\""),
            )),
        }
    }

    /// The words that follow `BEGIN ISOLATION LEVEL`.
    pub fn sql(self) -> &'static str {
        match self {
            Isolation::ReadCommitted => "READ COMMITTED",
            Isolation::RepeatableRead => "REPEATABLE READ",
            Isolation::Serializable => "SERIALIZABLE",
        }
    }
}

/// The options of an outer transaction, validated.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TransactionOptions {
    pub isolation: Option<Isolation>,
    pub timeout: Option<Duration>,
}

impl TransactionOptions {
    /// Rejects an unknown level, and a timeout that is not finite, positive and
    /// representable as a deadline. A positive duration shorter than one nanosecond
    /// becomes one nanosecond; it is never turned into "no timeout".
    pub fn parse(isolation: Option<&str>, timeout: Option<f64>) -> DbResult<Self> {
        let isolation = isolation.map(Isolation::parse).transpose()?;
        let timeout = match timeout {
            None => None,
            Some(secs) => {
                let bad = || DbError::new(ErrorKind::Query, format!("transaction timeout must be a finite number of seconds greater than zero, got {secs}"));
                if !secs.is_finite() || secs <= 0.0 {
                    return Err(bad());
                }
                let d = Duration::try_from_secs_f64(secs).map_err(|_| bad())?.max(Duration::from_nanos(1));
                if std::time::Instant::now().checked_add(d).is_none() || Instant::now().checked_add(d).is_none() {
                    return Err(bad());
                }
                Some(d)
            }
        };
        Ok(TransactionOptions { isolation, timeout })
    }

    pub fn is_empty(&self) -> bool {
        self.isolation.is_none() && self.timeout.is_none()
    }
}

/// What a timeout error reports about the transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeoutOutcome {
    /// The rollback was confirmed, or no transaction was ever opened.
    RolledBack,
    /// A COMMIT may have reached the server, or the cleanup could not be confirmed.
    Unknown,
}

impl TimeoutOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            TimeoutOutcome::RolledBack => "rolled_back",
            TimeoutOutcome::Unknown => "unknown",
        }
    }
}

/// What `Executor::begin_with` gets for an outer transaction.
#[derive(Clone)]
pub struct TxStart {
    pub isolation: Option<Isolation>,
    /// Set by the driver once it holds a connection (or the SQLite gate). Before that, a
    /// deadline needs no cleanup; after it, the cleanup decides what the error reports.
    pub acquired: Arc<AtomicBool>,
    /// The root deadline, for a driver that must stop a wait of its own (the SQLite busy handler).
    pub deadline: Option<std::time::Instant>,
}

impl TxStart {
    pub fn new(isolation: Option<Isolation>) -> Self {
        TxStart { isolation, acquired: Arc::new(AtomicBool::new(false)), deadline: None }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Active,
    Committing,
    Done,
    Expiring { committing: bool },
    Expired(TimeoutOutcome),
}

fn timeout_error(timeout: Duration, outcome: TimeoutOutcome, source: Option<&DbError>) -> DbError {
    let what = match outcome {
        TimeoutOutcome::RolledBack => "the transaction was rolled back",
        TimeoutOutcome::Unknown => "whether the transaction committed is unknown",
    };
    let mut e = DbError::new(ErrorKind::TransactionTimeout, format!("the transaction exceeded its timeout of {timeout:?}; {what}"));
    e.outcome = Some(outcome);
    e.sqlstate = source.and_then(|s| s.sqlstate.clone());
    e
}

/// The state that one root transaction and its savepoints share.
pub(super) struct Root {
    inner: Arc<dyn Transaction>,
    phase: watch::Sender<Phase>,
    timeout: Duration,
    watchdog: Mutex<Option<tokio::task::AbortHandle>>,
}

impl Drop for Root {
    fn drop(&mut self) {
        if let Some(h) = self.watchdog.get_mut().unwrap().take() {
            h.abort();
        }
    }
}

impl Root {
    /// Wraps `inner` and starts the watchdog that expires it at `deadline`.
    pub(super) fn start(inner: Arc<dyn Transaction>, timeout: Duration, deadline: Instant) -> Arc<Root> {
        let root = Arc::new(Root { inner, phase: watch::channel(Phase::Active).0, timeout, watchdog: Mutex::new(None) });
        let weak = Arc::downgrade(&root);
        let task = tokio::spawn(async move {
            tokio::time::sleep_until(deadline).await;
            if let Some(root) = weak.upgrade() {
                root.expire().await;
            }
        });
        *root.watchdog.lock().unwrap() = Some(task.abort_handle());
        root
    }

    pub(super) fn timeout_error(&self, outcome: TimeoutOutcome, source: Option<&DbError>) -> DbError {
        timeout_error(self.timeout, outcome, source)
    }

    fn change(&self, f: impl FnOnce(&mut Phase) -> bool) -> bool {
        self.phase.send_if_modified(f)
    }

    pub(super) fn is_expiring_or_expired(&self) -> bool {
        matches!(*self.phase.borrow(), Phase::Expiring { .. } | Phase::Expired(_))
    }

    /// Waits until the cleanup after the deadline has ended, then gives its outcome.
    pub(super) async fn settled(&self) -> TimeoutOutcome {
        let mut rx = self.phase.subscribe();
        loop {
            if let Phase::Expired(o) = *rx.borrow_and_update() {
                return o;
            }
            if rx.changed().await.is_err() {
                return TimeoutOutcome::Unknown;
            }
        }
    }

    /// Resolves when the deadline has passed (the cleanup may still run).
    async fn wait_expiring(&self) {
        let mut rx = self.phase.subscribe();
        loop {
            if matches!(*rx.borrow_and_update(), Phase::Expiring { .. } | Phase::Expired(_)) {
                return;
            }
            if rx.changed().await.is_err() {
                return;
            }
        }
    }

    /// An error when the deadline passed; waits out a cleanup that runs.
    pub(super) async fn admit(&self) -> DbResult<()> {
        if self.is_expiring_or_expired() {
            return Err(self.timeout_error(self.settled().await, None));
        }
        Ok(())
    }

    /// Runs `run`; when the deadline passes first, waits for the cleanup and gives the timeout error.
    /// A statement that the driver could not cancel stops holding the caller after the cleanup budget.
    pub(super) async fn within<T>(&self, run: impl std::future::Future<Output = DbResult<T>>) -> DbResult<T> {
        let mut run = std::pin::pin!(run);
        let out = tokio::select! {
            biased;
            out = &mut run => out,
            outcome = self.settled_after_expiry() => return Err(self.timeout_error(outcome, None)),
        };
        match out {
            Err(e) if self.is_expiring_or_expired() => Err(self.timeout_error(self.settled().await, Some(&e))),
            out => out,
        }
    }

    async fn settled_after_expiry(&self) -> TimeoutOutcome {
        self.wait_expiring().await;
        self.settled().await
    }

    /// Takes the root from `Active` to `Committing`; an error when the deadline passed.
    pub(super) async fn begin_commit(&self) -> DbResult<()> {
        loop {
            self.admit().await?;
            if self.change(|p| if *p == Phase::Active { *p = Phase::Committing; true } else { false }) {
                return Ok(());
            }
            // The deadline won between the check and the change: loop, so `admit` gives the error.
            if !self.is_expiring_or_expired() {
                return Ok(());
            }
        }
    }

    /// Runs the root's `COMMIT`. After the deadline no `COMMIT` is sent; a wait for a sent
    /// one is cancelled, and the outcome is unknown.
    pub(super) async fn commit(&self) -> DbResult<()> {
        // The deadline can pass after `begin_commit`: the COMMIT is then not started at all.
        if self.is_expiring_or_expired() {
            return Err(self.timeout_error(self.settled().await, None));
        }
        let mut commit = self.inner.commit();
        let out = tokio::select! {
            biased;
            out = &mut commit => Some(out),
            _ = self.wait_expiring() => None,
        };
        let finished = self.change(|p| if *p == Phase::Committing { *p = Phase::Done; true } else { false });
        match out {
            Some(out) if finished => {
                self.stop_watchdog();
                out
            }
            Some(out) if !self.is_expiring_or_expired() => out,
            Some(out) => Err(self.timeout_error(self.settled().await, out.err().as_ref())),
            None => {
                // Dropping the wait frees what the commit holds (a connection lock) for the cleanup.
                drop(commit);
                Err(self.timeout_error(self.settled().await, None))
            }
        }
    }

    /// Rolls the root back. After the deadline the cleanup has done it, so this is a no-op.
    pub(super) async fn rollback(&self) -> DbResult<()> {
        loop {
            match *self.phase.borrow() {
                Phase::Expiring { .. } | Phase::Expired(_) => break,
                _ => {}
            }
            if self.change(|p| if *p == Phase::Active { *p = Phase::Done; true } else { false }) || matches!(*self.phase.borrow(), Phase::Committing | Phase::Done) {
                self.stop_watchdog();
                return self.inner.rollback().await;
            }
        }
        self.settled().await;
        Ok(())
    }

    fn stop_watchdog(&self) {
        if let Some(h) = self.watchdog.lock().unwrap().take() {
            h.abort();
        }
    }

    async fn expire(&self) {
        let mut committing = false;
        let moved = self.change(|p| match *p {
            Phase::Active => { *p = Phase::Expiring { committing: false }; true }
            Phase::Committing => { *p = Phase::Expiring { committing: true }; committing = true; true }
            _ => false,
        });
        if !moved {
            return;
        }
        let ran = tokio::time::timeout(CLEANUP_GRACE + Duration::from_secs(1), self.inner.expire(committing, CLEANUP_GRACE)).await;
        let outcome = match ran {
            Ok(o) if !committing => o,
            _ => TimeoutOutcome::Unknown,
        };
        self.phase.send_replace(Phase::Expired(outcome));
    }
}

/// Starts a root transaction with `options`: the deadline begins now, before any wait for a
/// connection. Without a timeout, the transaction has no watchdog.
pub async fn begin_root(exec: &dyn super::Executor, options: TransactionOptions) -> DbResult<Arc<dyn Transaction>> {
    let mut start = TxStart::new(options.isolation);
    let Some(timeout) = options.timeout else {
        let inner = if options.isolation.is_some() { exec.begin_with(start).await? } else { exec.begin().await? };
        return Ok(super::guard(inner));
    };
    let deadline = Instant::now().checked_add(timeout).ok_or_else(|| DbError::new(ErrorKind::Query, "transaction timeout is too large"))?;
    start.deadline = Some(deadline.into_std());
    let acquired = start.acquired.clone();
    let error = |outcome, source: Option<&DbError>| timeout_error(timeout, outcome, source);
    let mut begin = std::pin::pin!(exec.begin_with(start));
    let inner = tokio::select! {
        biased;
        out = &mut begin => out?,
        _ = tokio::time::sleep_until(deadline) => {
            if !acquired.load(std::sync::atomic::Ordering::SeqCst) {
                // No connection or gate was taken: there is nothing to clean up.
                return Err(error(TimeoutOutcome::RolledBack, None));
            }
            // The driver holds a connection. Let the begin end (it cleans up after itself on an
            // error), then roll back; a begin that does not end within the budget is dropped.
            return Err(match tokio::time::timeout(CLEANUP_GRACE, &mut begin).await {
                Ok(Ok(tx)) => error(tx.expire(false, CLEANUP_GRACE).await, None),
                Ok(Err(e)) => error(TimeoutOutcome::RolledBack, Some(&e)),
                Err(_) => error(TimeoutOutcome::Unknown, None),
            });
        }
    };
    let root = Root::start(inner.clone(), timeout, deadline);
    Ok(super::guard_root(inner, root))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{BoxFuture, Executor, RawRows, RowSet};
    use sea_query::Value;
    use tokio::sync::Notify;

    #[derive(Clone, Default)]
    struct Script {
        /// Time between the start of `begin_with` and `acquired`.
        acquire: Duration,
        /// Time between `acquired` and the end of `begin_with`.
        begin: Duration,
        /// Time a `batch("slow")` runs, unless `cancel` stops it.
        slow: Duration,
        commit: Duration,
        /// `expire` never ends.
        hang_expire: bool,
        /// `expire` cannot confirm the rollback: the connection is quarantined and takes no more work.
        rollback_fails: bool,
        quarantined: Arc<std::sync::atomic::AtomicBool>,
        log: Arc<Mutex<Vec<String>>>,
        cancel: Arc<Notify>,
    }

    impl Script {
        fn log(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }
    }

    struct FakeDb(Script);
    struct FakeTx(Script);

    fn unused<T>() -> BoxFuture<'static, DbResult<T>> {
        Box::pin(async { Err(DbError::other("unused")) })
    }

    macro_rules! unused_statements {
        () => {
            fn query(&self, _: String, _: Vec<Value>) -> BoxFuture<'_, DbResult<Box<dyn RowSet>>> { unused() }
            fn fetch(&self, _: String, _: Vec<Value>) -> BoxFuture<'_, DbResult<Box<dyn RawRows>>> { unused() }
            fn execute(&self, _: String, _: Vec<Value>) -> BoxFuture<'_, DbResult<u64>> { unused() }
            fn query_text(&self, _: String) -> BoxFuture<'_, DbResult<Vec<Vec<Option<String>>>>> { unused() }
        };
    }

    impl Executor for FakeDb {
        unused_statements!();
        fn batch(&self, _: String) -> BoxFuture<'_, DbResult<u64>> { unused() }
        fn begin(&self) -> BoxFuture<'_, DbResult<Arc<dyn Transaction>>> {
            self.begin_with(TxStart::new(None))
        }
        fn begin_with(&self, start: TxStart) -> BoxFuture<'_, DbResult<Arc<dyn Transaction>>> {
            Box::pin(async move {
                if self.0.quarantined.load(std::sync::atomic::Ordering::SeqCst) {
                    return Err(DbError::other("connection is quarantined"));
                }
                tokio::time::sleep(self.0.acquire).await;
                start.acquired.store(true, std::sync::atomic::Ordering::SeqCst);
                self.0.log.lock().unwrap().push(format!("begin {:?}", start.isolation));
                tokio::time::sleep(self.0.begin).await;
                let tx: Arc<dyn Transaction> = Arc::new(FakeTx(self.0.clone()));
                Ok(tx)
            })
        }
    }

    impl Executor for FakeTx {
        unused_statements!();
        fn batch(&self, sql: String) -> BoxFuture<'_, DbResult<u64>> {
            Box::pin(async move {
                self.0.log.lock().unwrap().push(format!("batch {sql}"));
                if sql == "slow" {
                    tokio::select! {
                        _ = tokio::time::sleep(self.0.slow) => {}
                        // A cancelled statement fails with the server's SQLSTATE.
                        _ = self.0.cancel.notified() => {
                            let mut e = DbError::other("canceling statement due to user request");
                            e.sqlstate = Some("57014".into());
                            return Err(e);
                        }
                    }
                }
                Ok(0)
            })
        }
        fn begin(&self) -> BoxFuture<'_, DbResult<Arc<dyn Transaction>>> {
            Box::pin(async move {
                self.0.log.lock().unwrap().push("savepoint".into());
                let tx: Arc<dyn Transaction> = Arc::new(FakeTx(self.0.clone()));
                Ok(tx)
            })
        }
    }

    impl Transaction for FakeTx {
        fn commit(&self) -> BoxFuture<'_, DbResult<()>> {
            Box::pin(async move {
                self.0.log.lock().unwrap().push("commit".into());
                tokio::time::sleep(self.0.commit).await;
                Ok(())
            })
        }
        fn rollback(&self) -> BoxFuture<'_, DbResult<()>> {
            Box::pin(async move {
                self.0.log.lock().unwrap().push("rollback".into());
                Ok(())
            })
        }
        fn expire(&self, committing: bool, _: Duration) -> BoxFuture<'_, TimeoutOutcome> {
            Box::pin(async move {
                self.0.log.lock().unwrap().push(format!("expire committing={committing}"));
                self.0.cancel.notify_waiters();
                if self.0.hang_expire {
                    std::future::pending::<()>().await;
                }
                if self.0.rollback_fails {
                    self.0.quarantined.store(true, std::sync::atomic::Ordering::SeqCst);
                    return TimeoutOutcome::Unknown;
                }
                if committing { TimeoutOutcome::Unknown } else { TimeoutOutcome::RolledBack }
            })
        }
    }

    fn secs(s: f64) -> TransactionOptions {
        TransactionOptions::parse(None, Some(s)).unwrap()
    }

    async fn begin(script: &Script, options: TransactionOptions) -> DbResult<Arc<dyn Transaction>> {
        begin_root(&FakeDb(script.clone()), options).await
    }

    fn timeout_outcome(e: &DbError) -> TimeoutOutcome {
        assert_eq!(e.kind, ErrorKind::TransactionTimeout, "{e:?}");
        e.outcome.expect("a timeout error has an outcome")
    }

    #[test]
    fn options_validate_before_sql() {
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1e300, f64::MAX] {
            let e = TransactionOptions::parse(None, Some(bad)).unwrap_err();
            assert_eq!(e.kind, ErrorKind::Query, "{bad}");
        }
        for bad in ["", "READ COMMITTED", "read uncommitted", "snapshot"] {
            assert_eq!(TransactionOptions::parse(Some(bad), None).unwrap_err().kind, ErrorKind::Query, "{bad}");
        }
        let tiny = TransactionOptions::parse(None, Some(1e-12)).unwrap();
        assert_eq!(tiny.timeout, Some(Duration::from_nanos(1)));
        let all = TransactionOptions::parse(Some("repeatable_read"), Some(0.5)).unwrap();
        assert_eq!(all.isolation, Some(Isolation::RepeatableRead));
        assert_eq!(all.timeout, Some(Duration::from_millis(500)));
        assert!(TransactionOptions::parse(None, None).unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn isolation_reaches_the_driver() {
        let script = Script::default();
        let opts = TransactionOptions::parse(Some("serializable"), None).unwrap();
        begin(&script, opts).await.unwrap().commit().await.unwrap();
        assert_eq!(script.log()[0], "begin Some(Serializable)");
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_waiting_for_connection() {
        let script = Script { acquire: Duration::from_secs(10), ..Default::default() };
        let e = begin(&script, secs(1.0)).await.err().unwrap();
        assert_eq!(timeout_outcome(&e), TimeoutOutcome::RolledBack);
        assert!(script.log().is_empty(), "no connection, no SQL: {:?}", script.log());
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_during_begin_rolls_back_the_acquired_transaction() {
        let script = Script { begin: Duration::from_secs(2), ..Default::default() };
        let e = begin(&script, secs(1.0)).await.err().unwrap();
        assert_eq!(timeout_outcome(&e), TimeoutOutcome::RolledBack);
        assert_eq!(script.log(), ["begin None", "expire committing=false"]);
    }

    #[tokio::test(start_paused = true)]
    async fn begin_that_never_ends_reports_unknown() {
        let script = Script { begin: Duration::from_secs(3600), ..Default::default() };
        let e = begin(&script, secs(1.0)).await.err().unwrap();
        assert_eq!(timeout_outcome(&e), TimeoutOutcome::Unknown);
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_running_statement() {
        let script = Script { slow: Duration::from_secs(3600), ..Default::default() };
        let tx = begin(&script, secs(1.0)).await.unwrap();
        let e = tx.batch("slow".into()).await.unwrap_err();
        assert_eq!(timeout_outcome(&e), TimeoutOutcome::RolledBack);
        // The cancel error of the driver is not the primary error; its SQLSTATE is kept.
        assert_eq!(e.sqlstate.as_deref(), Some("57014"));
        assert_eq!(script.log(), ["begin None", "batch slow", "expire committing=false"]);
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_is_cumulative() {
        let script = Script::default();
        let tx = begin(&script, secs(1.0)).await.unwrap();
        for _ in 0..3 {
            tx.batch("fast".into()).await.unwrap();
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
        let e = tx.batch("fast".into()).await.unwrap_err();
        assert_eq!(timeout_outcome(&e), TimeoutOutcome::RolledBack);
        assert_eq!(e.sqlstate, None);
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_while_body_idle_sends_no_more_sql() {
        let script = Script::default();
        let tx = begin(&script, secs(1.0)).await.unwrap();
        tokio::time::sleep(Duration::from_secs(2)).await;
        let e = tx.batch("late".into()).await.unwrap_err();
        assert_eq!(timeout_outcome(&e), TimeoutOutcome::RolledBack);
        assert_eq!(script.log(), ["begin None", "expire committing=false"]);
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_invalidates_savepoints_and_joined_scopes() {
        let script = Script::default();
        let tx = begin(&script, secs(1.0)).await.unwrap();
        let child = tx.begin().await.unwrap();
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(timeout_outcome(&child.batch("x".into()).await.unwrap_err()), TimeoutOutcome::RolledBack);
        assert_eq!(timeout_outcome(&child.begin().await.err().unwrap()), TimeoutOutcome::RolledBack);
        assert_eq!(timeout_outcome(&child.commit().await.unwrap_err()), TimeoutOutcome::RolledBack);
        // A child's rollback has nothing left to undo.
        child.rollback().await.unwrap();
        assert_eq!(timeout_outcome(&tx.commit().await.unwrap_err()), TimeoutOutcome::RolledBack);
        tx.rollback().await.unwrap();
        assert!(!script.log().iter().any(|l| l == "rollback" || l == "commit"), "{:?}", script.log());
    }

    #[tokio::test(start_paused = true)]
    async fn caught_timeout_prevents_commit() {
        let script = Script { slow: Duration::from_secs(3600), ..Default::default() };
        let tx = begin(&script, secs(1.0)).await.unwrap();
        let _ = tx.batch("slow".into()).await.unwrap_err();
        let e = tx.commit().await.unwrap_err();
        assert_eq!(timeout_outcome(&e), TimeoutOutcome::RolledBack);
        assert!(!script.log().iter().any(|l| l == "commit"));
    }

    #[tokio::test(start_paused = true)]
    async fn commit_before_the_deadline_stops_the_watchdog() {
        let script = Script::default();
        let tx = begin(&script, secs(1.0)).await.unwrap();
        tx.commit().await.unwrap();
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert_eq!(script.log(), ["begin None", "commit"]);
    }

    #[tokio::test(start_paused = true)]
    async fn commit_that_passes_the_deadline_is_unknown() {
        let script = Script { commit: Duration::from_secs(3600), ..Default::default() };
        let tx = begin(&script, secs(1.0)).await.unwrap();
        let e = tx.commit().await.unwrap_err();
        assert_eq!(timeout_outcome(&e), TimeoutOutcome::Unknown);
        assert_eq!(script.log(), ["begin None", "commit", "expire committing=true"]);
    }

    #[tokio::test(start_paused = true)]
    async fn rollback_before_the_deadline_stops_the_watchdog() {
        let script = Script::default();
        let tx = begin(&script, secs(1.0)).await.unwrap();
        tx.rollback().await.unwrap();
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert_eq!(script.log(), ["begin None", "rollback"]);
    }

    #[tokio::test(start_paused = true)]
    async fn cleanup_that_never_ends_reports_unknown() {
        let script = Script { slow: Duration::from_secs(3600), hang_expire: true, ..Default::default() };
        let tx = begin(&script, secs(1.0)).await.unwrap();
        let started = tokio::time::Instant::now();
        let e = tx.batch("slow".into()).await.unwrap_err();
        assert_eq!(timeout_outcome(&e), TimeoutOutcome::Unknown);
        // The caller waits for the deadline and the cleanup budget only.
        assert!(started.elapsed() <= Duration::from_secs(1) + CLEANUP_GRACE + Duration::from_secs(2));
    }

    #[tokio::test(start_paused = true)]
    async fn no_timeout_means_no_watchdog() {
        let script = Script::default();
        let opts = TransactionOptions::parse(Some("read_committed"), None).unwrap();
        let tx = begin(&script, opts).await.unwrap();
        tokio::time::sleep(Duration::from_secs(86_400)).await;
        tx.batch("fast".into()).await.unwrap();
        tx.commit().await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_and_commit_race() {
        // The COMMIT ends just before, at and just after the deadline. Whatever wins, the result
        // and the log agree: a confirmed commit never meets an expiry, and no rollback follows a COMMIT.
        for commit_ms in [900, 999, 1000, 1001, 1100] {
            let script = Script { commit: Duration::from_millis(commit_ms), ..Default::default() };
            let tx = begin(&script, secs(1.0)).await.unwrap();
            let result = tx.commit().await;
            tokio::time::sleep(Duration::from_secs(30)).await;
            let log = script.log();
            assert!(!log.contains(&"rollback".to_string()), "{commit_ms} ms: {log:?}");
            match result {
                Ok(()) => assert!(!log.iter().any(|l| l.starts_with("expire")), "{commit_ms} ms: {log:?}"),
                Err(e) => {
                    assert_eq!(timeout_outcome(&e), TimeoutOutcome::Unknown, "{commit_ms} ms");
                    assert!(log.contains(&"expire committing=true".to_string()), "{commit_ms} ms: {log:?}");
                }
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_deadline_between_begin_commit_and_commit_sends_no_commit() {
        let script = Script::default();
        let tx: Arc<dyn Transaction> = Arc::new(FakeTx(script.clone()));
        let root = Root::start(tx, Duration::from_secs(1), Instant::now() + Duration::from_secs(1));
        root.begin_commit().await.unwrap();
        tokio::time::sleep(Duration::from_secs(2)).await;
        let e = root.commit().await.unwrap_err();
        assert_eq!(timeout_outcome(&e), TimeoutOutcome::Unknown);
        assert_eq!(script.log(), ["expire committing=true"]);
    }

    #[tokio::test(start_paused = true)]
    async fn rollback_failure_quarantines() {
        let script = Script { slow: Duration::from_secs(3600), rollback_fails: true, ..Default::default() };
        let tx = begin(&script, secs(1.0)).await.unwrap();
        let e = tx.batch("slow".into()).await.unwrap_err();
        assert_eq!(timeout_outcome(&e), TimeoutOutcome::Unknown);
        // The quarantined connection takes no more work, so a later caller cannot meet a transaction nobody ended.
        assert!(begin(&script, TransactionOptions::default()).await.is_err());
        assert_eq!(script.log(), ["begin None", "batch slow", "expire committing=false"]);
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_does_not_hit_the_next_lease() {
        let script = Script { slow: Duration::from_secs(5), ..Default::default() };
        let first = begin(&script, secs(1.0)).await.unwrap();
        first.batch("slow".into()).await.unwrap_err();
        let second = begin(&script, secs(60.0)).await.unwrap();
        // Nothing cancels the second transaction: its statement runs to its end.
        second.batch("slow".into()).await.unwrap();
        second.commit().await.unwrap();
        tokio::time::sleep(Duration::from_secs(120)).await;
        let expires = script.log().iter().filter(|l| l.starts_with("expire")).count();
        assert_eq!(expires, 1, "{:?}", script.log());
    }
}
