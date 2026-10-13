//! Per-statement events for query hooks and the N+1 finder.
//!
//! A binding passes a [`Trace`] only when something listens; [`wrap`] then puts a
//! [`Traced`] executor around the pool or transaction, which records each statement it
//! sends (prefetch queries and `update_many` batches included). The binding drains the
//! trace after the call, in the caller's task, also when the call failed. A statement
//! that is still in flight then (the caller cancelled the call) gives an event with the
//! error "cancelled".

use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use sea_query::Value;

use crate::db::{BoxFuture, DbResult, Executor, RawRows, RowSet, Transaction};
use orm_core::dialect::Dialect;

/// One statement the database ran.
#[derive(Debug, Clone)]
pub struct QueryEvent {
    /// The SQL with its parameter placeholders: the statement shape.
    pub sql: String,
    pub start: SystemTime,
    pub duration: Duration,
    /// Rows returned, or rows affected by a statement without rows.
    pub rows: u64,
    /// The error message when the statement failed.
    pub error: Option<String>,
}

/// The events of the calls that share it.
#[derive(Debug, Default, Clone)]
pub struct Trace(Arc<Mutex<Inner>>);

#[derive(Debug, Default)]
struct Inner {
    next: u64,
    /// The statements that have started and not ended, oldest first.
    open: Vec<Open>,
    done: Vec<QueryEvent>,
}

#[derive(Debug)]
struct Open {
    id: u64,
    sql: String,
    start: SystemTime,
    clock: Instant,
}

impl Open {
    fn end(self, rows: u64, error: Option<String>) -> QueryEvent {
        QueryEvent { sql: self.sql, start: self.start, duration: self.clock.elapsed(), rows, error }
    }
}

impl Trace {
    pub fn new() -> Self {
        Self::default()
    }

    /// The events so far, oldest first, then one "cancelled" event for each statement
    /// still in flight; the trace is empty afterwards.
    ///
    /// A dropped statement cannot report itself: the runtime stops the task of a
    /// cancelled call after the caller has drained the trace.
    pub fn take(&self) -> Vec<QueryEvent> {
        let mut inner = self.lock();
        let mut events = std::mem::take(&mut inner.done);
        events.extend(inner.open.drain(..).map(|o| o.end(0, Some("cancelled".into()))));
        events
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn start(&self, sql: String) -> u64 {
        let mut inner = self.lock();
        let id = inner.next;
        inner.next += 1;
        inner.open.push(Open { id, sql, start: SystemTime::now(), clock: Instant::now() });
        id
    }

    /// Ends statement `id`; nothing happens when `take` already reported it as cancelled.
    fn finish(&self, id: u64, rows: u64, error: Option<String>) {
        let mut inner = self.lock();
        if let Some(at) = inner.open.iter().position(|o| o.id == id) {
            let event = inner.open.remove(at).end(rows, error);
            inner.done.push(event);
        }
    }
}

/// `conn`, recording into `trace` when there is one.
pub fn wrap(conn: Arc<dyn Executor>, trace: Option<&Trace>) -> Arc<dyn Executor> {
    match trace {
        Some(t) => Arc::new(Traced { conn, tx: None, trace: t.clone() }),
        None => conn,
    }
}

/// Runs `run` and records it as statement `sql` into `trace`, when there is one: for
/// work outside an executor, such as session advisory locks.
pub async fn timed<T>(trace: Option<&Trace>, sql: String, run: impl Future<Output = DbResult<T>>, rows: impl Fn(&T) -> u64) -> DbResult<T> {
    let Some(trace) = trace else { return run.await };
    let id = trace.start(sql);
    let out = run.await;
    let (rows, error) = match &out {
        Ok(v) => (rows(v), None),
        Err(e) => (0, Some(e.message.clone())),
    };
    trace.finish(id, rows, error);
    out
}

/// An executor that records each statement; transactions it begins record too.
pub struct Traced {
    conn: Arc<dyn Executor>,
    tx: Option<Arc<dyn Transaction>>,
    trace: Trace,
}

impl Traced {
    fn timed<'a, T: 'a>(&'a self, sql: String, run: impl Future<Output = DbResult<T>> + 'a, rows: impl Fn(&T) -> u64 + 'a) -> impl Future<Output = DbResult<T>> + 'a {
        timed(Some(&self.trace), sql, run, rows)
    }

    fn traced_tx(&self, tx: Arc<dyn Transaction>) -> Arc<dyn Transaction> {
        Arc::new(Traced { conn: tx.clone(), tx: Some(tx), trace: self.trace.clone() })
    }
}

impl Executor for Traced {
    fn dialect(&self) -> Dialect {
        self.conn.dialect()
    }
    fn begin_migration(&self) -> BoxFuture<'_, DbResult<Arc<dyn Transaction>>> {
        Box::pin(async move { Ok(self.traced_tx(self.conn.begin_migration().await?)) })
    }
    fn query(&self, sql: String, args: Vec<Value>) -> BoxFuture<'_, DbResult<Box<dyn RowSet>>> {
        Box::pin(self.timed(sql.clone(), self.conn.query(sql, args), |r| r.len() as u64))
    }
    fn fetch(&self, sql: String, args: Vec<Value>) -> BoxFuture<'_, DbResult<Box<dyn RawRows>>> {
        Box::pin(self.timed(sql.clone(), self.conn.fetch(sql, args), |r| r.len() as u64))
    }
    fn execute(&self, sql: String, args: Vec<Value>) -> BoxFuture<'_, DbResult<u64>> {
        Box::pin(self.timed(sql.clone(), self.conn.execute(sql, args), |n| *n))
    }
    fn batch(&self, sql: String) -> BoxFuture<'_, DbResult<u64>> {
        Box::pin(self.timed(sql.clone(), self.conn.batch(sql), |n| *n))
    }
    fn query_text(&self, sql: String) -> BoxFuture<'_, DbResult<Vec<Vec<Option<String>>>>> {
        Box::pin(self.timed(sql.clone(), self.conn.query_text(sql), |r| r.len() as u64))
    }
    fn copy_in(&self, table: String, columns: Vec<String>, rows: Vec<Vec<Value>>) -> BoxFuture<'_, DbResult<u64>> {
        let sql = format!("COPY {table} ({}) FROM STDIN (FORMAT binary)", columns.join(", "));
        Box::pin(self.timed(sql, self.conn.copy_in(table, columns, rows), |n| *n))
    }
    fn advisory_lock(&self, key: i64, exclusive: bool, nowait: bool) -> BoxFuture<'_, DbResult<bool>> {
        let sql = crate::advisory::sql(key, exclusive, nowait);
        Box::pin(self.timed(sql, self.conn.advisory_lock(key, exclusive, nowait), |_| 1))
    }
    fn begin(&self) -> BoxFuture<'_, DbResult<Arc<dyn Transaction>>> {
        Box::pin(async move { Ok(self.traced_tx(self.conn.begin().await?)) })
    }
}

impl Transaction for Traced {
    fn commit(&self) -> BoxFuture<'_, DbResult<()>> {
        match &self.tx {
            Some(tx) => tx.commit(),
            None => Box::pin(async { Err(crate::db::DbError::other("not a transaction")) }),
        }
    }
    fn rollback(&self) -> BoxFuture<'_, DbResult<()>> {
        match &self.tx {
            Some(tx) => tx.rollback(),
            None => Box::pin(async { Err(crate::db::DbError::other("not a transaction")) }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn take_drains() {
        let t = Trace::new();
        let id = t.start("SELECT 1".into());
        t.finish(id, 1, None);
        assert_eq!(t.take().len(), 1);
        assert!(t.take().is_empty());
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap()
    }

    #[test]
    fn a_dropped_statement_is_reported_as_cancelled() {
        let t = Trace::new();
        runtime().block_on(async {
            let never = timed(Some(&t), "SELECT pg_sleep(60)".into(), std::future::pending::<DbResult<()>>(), |_| 0);
            assert!(tokio::time::timeout(Duration::from_millis(20), never).await.is_err());
        });
        let events = t.take();
        assert_eq!(events.len(), 1);
        assert_eq!((events[0].sql.as_str(), events[0].rows, events[0].error.as_deref()), ("SELECT pg_sleep(60)", 0, Some("cancelled")));
        assert!(events[0].duration >= Duration::from_millis(20));
        assert!(t.take().is_empty());
    }

    #[test]
    fn a_statement_that_ends_after_take_gives_no_second_event() {
        let t = Trace::new();
        runtime().block_on(async {
            let (done, wait) = tokio::sync::oneshot::channel::<()>();
            let own = t.clone();
            let task = tokio::spawn(async move {
                timed(Some(&own), "SELECT 1".into(), async { wait.await.ok(); Ok(1u64) }, |n| *n).await
            });
            tokio::task::yield_now().await;
            let events = t.take();
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].error.as_deref(), Some("cancelled"));
            done.send(()).unwrap();
            task.await.unwrap().unwrap();
        });
        assert!(t.take().is_empty());
    }

    #[test]
    fn a_finished_statement_keeps_its_rows_and_error() {
        let t = Trace::new();
        runtime().block_on(async {
            timed(Some(&t), "SELECT 1".into(), async { Ok(3u64) }, |n| *n).await.unwrap();
            timed(Some(&t), "SELECT 2".into(), async { Err::<u64, _>(crate::db::DbError::other("boom")) }, |n| *n).await.unwrap_err();
        });
        let events = t.take();
        assert_eq!(events.iter().map(|e| (e.sql.as_str(), e.rows, e.error.as_deref())).collect::<Vec<_>>(), [("SELECT 1", 3, None), ("SELECT 2", 0, Some("boom"))]);
    }
}
