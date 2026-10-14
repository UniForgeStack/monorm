//! Database drivers.
//!
//! The planner produces sea-query statements; [`build`] renders them for the
//! connection's [`Dialect`] into SQL text plus [`Value`]s, and a driver runs them.
//! Everything above this module is driver-neutral: a driver implements [`Driver`],
//! [`Executor`], [`Transaction`] and [`RowSet`] for one database, and [`connect`] picks
//! it from the URL scheme. A build compiles PostgreSQL (tokio-postgres), SQLite
//! (rusqlite), or both.

#[cfg(feature = "postgres")]
mod numeric;
#[cfg(feature = "postgres")]
mod postgres;
#[cfg(feature = "sqlite")]
mod sqlite;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use chrono::{DateTime, NaiveDate, Utc};
use sea_query::{QueryStatementWriter, Value};
#[cfg(feature = "postgres")]
use sea_query::PostgresQueryBuilder;
#[cfg(feature = "sqlite")]
use sea_query::SqliteQueryBuilder;

use orm_core::dialect::Dialect;
use orm_core::ir::ValueType;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
pub type DbResult<T> = Result<T, DbError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// Unique, foreign key, check, exclusion or not-null violation.
    Integrity,
    /// A `NOWAIT` lock (or a lock timeout) found the rows locked.
    LockNotAvailable,
    /// A statement ran in a transaction (or savepoint) that an earlier error aborted.
    TransactionAborted,
    Other,
}

#[derive(Debug, Clone)]
pub struct DbError {
    pub kind: ErrorKind,
    pub message: String,
    /// The five-character SQLSTATE (`23505`); SQLite constraint failures get the
    /// Postgres code too.
    pub sqlstate: Option<String>,
    /// The violated constraint's name (Postgres only).
    pub constraint: Option<String>,
    /// The database's DETAIL line (Postgres only).
    pub detail: Option<String>,
    /// For `TransactionAborted`: the first error of the transaction.
    pub cause: Option<Arc<DbError>>,
}

impl DbError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        DbError { kind, message: message.into(), sqlstate: None, constraint: None, detail: None, cause: None }
    }

    pub fn other(message: impl Into<String>) -> Self {
        DbError::new(ErrorKind::Other, message)
    }
}

impl std::fmt::Display for DbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// One decoded cell, borrowing from its row set. Bindings turn it into their own value.
#[derive(Debug, Clone, PartialEq)]
pub enum Cell<'a> {
    Null,
    Bool(bool),
    Int(i32),
    BigInt(i64),
    Float(f64),
    /// Text, or the label of a database enum.
    Text(&'a str),
    /// timestamptz, in UTC as on the wire.
    DateTime(DateTime<Utc>),
    Date(NaiveDate),
    Uuid(uuid::Uuid),
    Json(serde_json::Value),
    /// A `numeric` as decimal text: `-12.50`, `NaN`.
    Decimal(String),
    Array(Vec<Cell<'a>>),
}

/// The rows one statement returned, decoded by the schema's column types.
pub trait RowSet: Send + Sync {
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// One cell (enum values as stored: the binding maps them).
    fn cell(&self, row: usize, col: usize, ty: ValueType) -> DbResult<Cell<'_>>;
    /// One cell as a bind parameter (prefetch keys are read back this way).
    fn value(&self, row: usize, col: usize, ty: ValueType) -> DbResult<Value>;
    fn get_i64(&self, row: usize, col: usize) -> DbResult<i64>;
    fn get_bool(&self, row: usize, col: usize) -> DbResult<bool>;
}

/// The rows of raw SQL: column names, and cells decoded by the column types the database
/// reports (Postgres) or by each value's storage class (SQLite).
pub trait RawRows: Send + Sync {
    fn columns(&self) -> &[String];
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn cell(&self, row: usize, col: usize) -> DbResult<Cell<'_>>;
}

/// Something statements run on: the pool (each call takes a connection) or an open
/// transaction (every call uses its connection).
pub trait Executor: Send + Sync {
    fn dialect(&self) -> Dialect { Dialect::Postgres }
    /// A migration may rebuild SQLite tables with FK checks deferred until commit.
    fn begin_migration(&self) -> BoxFuture<'_, DbResult<Arc<dyn Transaction>>> { self.begin() }
    fn query(&self, sql: String, args: Vec<Value>) -> BoxFuture<'_, DbResult<Box<dyn RowSet>>>;
    /// Raw SQL with parameters, the columns typed by the database.
    fn fetch(&self, sql: String, args: Vec<Value>) -> BoxFuture<'_, DbResult<Box<dyn RawRows>>>;
    /// Rows affected.
    fn execute(&self, sql: String, args: Vec<Value>) -> BoxFuture<'_, DbResult<u64>>;
    /// One or more statements without parameters (DDL scripts, raw SQL).
    fn batch(&self, sql: String) -> BoxFuture<'_, DbResult<u64>>;
    /// A query whose columns are all read as text (tooling such as the migration runner).
    fn query_text(&self, sql: String) -> BoxFuture<'_, DbResult<Vec<Vec<Option<String>>>>>;
    /// A single advisory-lock result, without materializing text rows in the binding.
    fn advisory_lock(&self, key: i64, exclusive: bool, nowait: bool) -> BoxFuture<'_, DbResult<bool>> {
        Box::pin(async move {
            let rows = self.query_text(crate::advisory::sql(key, exclusive, nowait)).await?;
            Ok(!nowait || rows.first().and_then(|r| r.first()).and_then(|v| v.as_deref()) == Some("true"))
        })
    }
    /// A transaction, or a savepoint when called on a transaction.
    fn begin(&self) -> BoxFuture<'_, DbResult<Arc<dyn Transaction>>>;
    /// Bulk load: `COPY table (columns) FROM STDIN`, one value per column in each row.
    /// Gives the number of rows written.
    fn copy_in(&self, table: String, columns: Vec<String>, rows: Vec<Vec<Value>>) -> BoxFuture<'_, DbResult<u64>> {
        let _ = (table, columns, rows);
        Box::pin(async { Err(DbError::other("COPY is not supported by this database")) })
    }
}

pub trait Transaction: Executor {
    fn commit(&self) -> BoxFuture<'_, DbResult<()>>;
    fn rollback(&self) -> BoxFuture<'_, DbResult<()>>;
    /// Marks the transaction failed as if a statement had failed with `e`
    /// (a no-op for a transaction that does not track failures).
    fn mark_failed(&self, _e: DbError) {}
}

/// A transaction that remembers its first failed statement, also when the caller catches
/// the error. A later statement and `commit` then give `TransactionAborted`, so Postgres
/// (which aborts the transaction) and SQLite (which keeps the other writes) behave the
/// same. A savepoint is guarded on its own: rolling it back clears its mark and leaves
/// the enclosing transaction usable.
struct Guarded {
    inner: Arc<dyn Transaction>,
    failed: std::sync::Mutex<Option<Arc<DbError>>>,
}

/// `tx` with the failed mark; give the bindings this, never a bare driver transaction.
pub fn guard(tx: Arc<dyn Transaction>) -> Arc<dyn Transaction> {
    Arc::new(Guarded { inner: tx, failed: std::sync::Mutex::new(None) })
}

impl Guarded {
    fn aborted(&self) -> Option<DbError> {
        let first = self.failed.lock().unwrap().clone()?;
        let line = first.message.lines().next().unwrap_or_default();
        let mut e = DbError::new(
            ErrorKind::TransactionAborted,
            format!("the transaction is aborted by an earlier error ({line}); roll it back, or catch errors only around a savepoint (a nested transaction block)"),
        );
        e.cause = Some(first);
        Some(e)
    }

    fn note<T>(&self, out: DbResult<T>) -> DbResult<T> {
        if let Err(e) = &out {
            self.mark_failed(e.clone());
        }
        out
    }

    async fn guarded<T>(&self, run: impl Future<Output = DbResult<T>>) -> DbResult<T> {
        if let Some(e) = self.aborted() {
            return Err(e);
        }
        self.note(run.await)
    }
}

impl Executor for Guarded {
    fn dialect(&self) -> Dialect {
        self.inner.dialect()
    }
    fn query(&self, sql: String, args: Vec<Value>) -> BoxFuture<'_, DbResult<Box<dyn RowSet>>> {
        Box::pin(self.guarded(self.inner.query(sql, args)))
    }
    fn fetch(&self, sql: String, args: Vec<Value>) -> BoxFuture<'_, DbResult<Box<dyn RawRows>>> {
        Box::pin(self.guarded(self.inner.fetch(sql, args)))
    }
    fn execute(&self, sql: String, args: Vec<Value>) -> BoxFuture<'_, DbResult<u64>> {
        Box::pin(self.guarded(self.inner.execute(sql, args)))
    }
    fn batch(&self, sql: String) -> BoxFuture<'_, DbResult<u64>> {
        Box::pin(self.guarded(self.inner.batch(sql)))
    }
    fn query_text(&self, sql: String) -> BoxFuture<'_, DbResult<Vec<Vec<Option<String>>>>> {
        Box::pin(self.guarded(self.inner.query_text(sql)))
    }
    fn advisory_lock(&self, key: i64, exclusive: bool, nowait: bool) -> BoxFuture<'_, DbResult<bool>> {
        Box::pin(self.guarded(self.inner.advisory_lock(key, exclusive, nowait)))
    }
    fn copy_in(&self, table: String, columns: Vec<String>, rows: Vec<Vec<Value>>) -> BoxFuture<'_, DbResult<u64>> {
        Box::pin(self.guarded(self.inner.copy_in(table, columns, rows)))
    }
    fn begin(&self) -> BoxFuture<'_, DbResult<Arc<dyn Transaction>>> {
        Box::pin(async move {
            let tx = self.guarded(self.inner.begin()).await?;
            Ok(guard(tx))
        })
    }
}

impl Transaction for Guarded {
    fn commit(&self) -> BoxFuture<'_, DbResult<()>> {
        Box::pin(async move {
            if let Some(e) = self.aborted() {
                let _ = self.inner.rollback().await;
                return Err(e);
            }
            self.inner.commit().await
        })
    }
    fn rollback(&self) -> BoxFuture<'_, DbResult<()>> {
        self.inner.rollback()
    }
    fn mark_failed(&self, e: DbError) {
        let mut failed = self.failed.lock().unwrap();
        if failed.is_none() && e.kind != ErrorKind::TransactionAborted {
            *failed = Some(Arc::new(e));
        }
    }
}

pub trait Driver: Executor {
    fn close(&self) -> BoxFuture<'_, ()>;
    /// A session advisory lock on a connection of its own (not from the pool), outside any transaction.
    /// `None` when it is not taken: held with `nowait`, or still held after `timeout_ms`.
    fn session_lock(
        &self, _key: i64, _exclusive: bool, _nowait: bool, _timeout_ms: Option<u64>,
    ) -> BoxFuture<'_, DbResult<Option<Arc<dyn SessionLock>>>> {
        Box::pin(async { Err(DbError::other(format!("{} does not support advisory locks", self.dialect().name()))) })
    }
}

/// A held session advisory lock. Dropped without `release`, it closes its connection,
/// so the server releases the lock.
pub trait SessionLock: Send + Sync {
    /// Unlocks, and gives the connection back to the pool.
    fn release(&self) -> BoxFuture<'_, DbResult<()>>;
}

/// A driver whose every transaction first runs `set_config(name, value, true)` (the
/// same as `SET LOCAL`) for each setting, so row-level security policies see them. A
/// statement outside a transaction runs in a transaction of its own. Savepoints inherit
/// the settings of their transaction.
pub struct WithSettings {
    inner: Arc<dyn Driver>,
    settings: Vec<(String, String)>,
}

impl WithSettings {
    pub fn new(inner: Arc<dyn Driver>, settings: Vec<(String, String)>) -> Self {
        WithSettings { inner, settings }
    }
}

async fn finish<T>(tx: Arc<dyn Transaction>, out: DbResult<T>) -> DbResult<T> {
    match out {
        Ok(v) => tx.commit().await.map(|_| v),
        Err(e) => {
            let _ = tx.rollback().await;
            Err(e)
        }
    }
}

impl Executor for WithSettings {
    fn dialect(&self) -> Dialect {
        self.inner.dialect()
    }
    fn query(&self, sql: String, args: Vec<Value>) -> BoxFuture<'_, DbResult<Box<dyn RowSet>>> {
        Box::pin(async move {
            let tx = self.begin().await?;
            let out = tx.query(sql, args).await;
            finish(tx, out).await
        })
    }
    fn fetch(&self, sql: String, args: Vec<Value>) -> BoxFuture<'_, DbResult<Box<dyn RawRows>>> {
        Box::pin(async move {
            let tx = self.begin().await?;
            let out = tx.fetch(sql, args).await;
            finish(tx, out).await
        })
    }
    fn execute(&self, sql: String, args: Vec<Value>) -> BoxFuture<'_, DbResult<u64>> {
        Box::pin(async move {
            let tx = self.begin().await?;
            let out = tx.execute(sql, args).await;
            finish(tx, out).await
        })
    }
    fn batch(&self, sql: String) -> BoxFuture<'_, DbResult<u64>> {
        Box::pin(async move {
            let tx = self.begin().await?;
            let out = tx.batch(sql).await;
            finish(tx, out).await
        })
    }
    fn query_text(&self, sql: String) -> BoxFuture<'_, DbResult<Vec<Vec<Option<String>>>>> {
        Box::pin(async move {
            let tx = self.begin().await?;
            let out = tx.query_text(sql).await;
            finish(tx, out).await
        })
    }
    fn copy_in(&self, table: String, columns: Vec<String>, rows: Vec<Vec<Value>>) -> BoxFuture<'_, DbResult<u64>> {
        Box::pin(async move {
            let tx = self.begin().await?;
            let out = tx.copy_in(table, columns, rows).await;
            finish(tx, out).await
        })
    }
    fn begin(&self) -> BoxFuture<'_, DbResult<Arc<dyn Transaction>>> {
        Box::pin(async move {
            let tx = self.inner.begin().await?;
            for (name, value) in &self.settings {
                let args = vec![Value::from(name.clone()), Value::from(value.clone())];
                if let Err(e) = tx.execute("SELECT set_config($1, $2, true)".into(), args).await {
                    let _ = tx.rollback().await;
                    return Err(e);
                }
            }
            Ok(tx)
        })
    }
}

impl Driver for WithSettings {
    /// The pool belongs to the wrapped driver.
    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
    fn session_lock(
        &self, key: i64, exclusive: bool, nowait: bool, timeout_ms: Option<u64>,
    ) -> BoxFuture<'_, DbResult<Option<Arc<dyn SessionLock>>>> {
        self.inner.session_lock(key, exclusive, nowait, timeout_ms)
    }
}

/// Opens the selected driver for `postgres://`, `postgresql://` or `sqlite://`.
pub async fn connect(url: &str, max_connections: usize) -> DbResult<Arc<dyn Driver>> {
    let _ = max_connections;
    let scheme = url.split_once("://").map(|(s, _)| s).unwrap_or("");
    match scheme {
        #[cfg(feature = "sqlite")]
        "sqlite" => Ok(Arc::new(sqlite::SqliteDriver::connect(url).await?)),
        #[cfg(feature = "postgres")]
        "postgres" | "postgresql" => Ok(Arc::new(postgres::PgDriver::connect(url, max_connections).await?)),
        #[allow(unreachable_patterns)]
        "postgres" | "postgresql" | "sqlite" => Err(DbError::other(format!("database backend for {scheme:?} is not compiled into this native profile"))),
        other => Err(DbError::other(format!("unsupported database URL scheme {other:?}"))),
    }
}

/// SQL text and parameters of a statement in `dialect`.
pub fn build<S: QueryStatementWriter>(dialect: Dialect, stmt: &S) -> (String, Vec<Value>) {
    let (sql, values): (String, sea_query::Values) = match dialect {
        #[cfg(feature = "postgres")]
        Dialect::Postgres => stmt.build(PostgresQueryBuilder),
        #[cfg(feature = "sqlite")]
        Dialect::Sqlite => stmt.build(SqliteQueryBuilder),
        #[allow(unreachable_patterns)]
        _ => panic!("database dialect is not compiled into this native profile"),
    };
    match dialect {
        Dialect::Postgres => merge_equal_params(sql, values.0),
        _ => (sql, values.0),
    }
}

/// Postgres matches a GROUP BY, HAVING, ORDER BY or DISTINCT expression to the select list by
/// its text, and `$1` differs from `$2`. Equal values (same type, same value) share one slot,
/// so a repeated `CASE` has equal text. Types are sent with the values, so sharing is safe.
fn merge_equal_params(sql: String, values: Vec<Value>) -> (String, Vec<Value>) {
    let grouped = ["GROUP BY", "HAVING", "ORDER BY", "DISTINCT"].iter().any(|k| sql.contains(k));
    if !grouped || values.len() < 2 {
        return (sql, values);
    }
    let mut first = std::collections::HashMap::<&Value, usize>::new();
    let mut slot = Vec::with_capacity(values.len());
    let mut kept = Vec::new();
    for (i, v) in values.iter().enumerate() {
        let n = *first.entry(v).or_insert_with(|| { kept.push(i); kept.len() });
        slot.push(n);
    }
    if kept.len() == values.len() {
        return (sql, values);
    }
    let mut out = String::with_capacity(sql.len());
    let mut quote = None;
    let mut chars = sql.chars().peekable();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => { quote = None; out.push(c); }
            (None, '\'' | '"') => { quote = Some(c); out.push(c); }
            (None, '$') if chars.peek().is_some_and(|d| d.is_ascii_digit()) => {
                let mut n = 0usize;
                while let Some(d) = chars.peek().and_then(|d| d.to_digit(10)) {
                    n = n * 10 + d as usize;
                    chars.next();
                }
                out.push_str(&format!("${}", slot[n - 1]));
            }
            (_, c) => out.push(c),
        }
    }
    let values = kept.into_iter().map(|i| values[i].clone()).collect();
    (out, values)
}

/// `stmt` with parameters inlined, for debugging.
pub fn to_string<S: QueryStatementWriter>(dialect: Dialect, stmt: &S) -> String {
    match dialect {
        #[cfg(feature = "postgres")]
        Dialect::Postgres => stmt.to_string(PostgresQueryBuilder),
        #[cfg(feature = "sqlite")]
        Dialect::Sqlite => stmt.to_string(SqliteQueryBuilder),
        #[allow(unreachable_patterns)]
        _ => panic!("database dialect is not compiled into this native profile"),
    }
}

/// Reject unsupported schema dialects before planning or opening connections.
pub fn require_dialect(dialect: Dialect) -> DbResult<()> {
    let enabled = match dialect {
        Dialect::Postgres => cfg!(feature = "postgres"),
        Dialect::Sqlite => cfg!(feature = "sqlite"),
    };
    if enabled { Ok(()) } else { Err(DbError::other(format!("{} is not compiled into this native profile", dialect.name()))) }
}
