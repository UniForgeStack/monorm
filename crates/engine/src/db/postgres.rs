//! Postgres driver: tokio-postgres connections in a deadpool pool.
//!
//! * Parameters are sent in Postgres's binary format with their types stated in the
//!   `Parse` message (from the sea-query [`Value`] variant, which the planner picked from
//!   the column type), so the server never has to guess them.
//! * Statements are prepared once per connection and cached (deadpool's per-connection
//!   cache). A per-connection [`StatementLru`] beside it records the last use of each
//!   statement; past [`STATEMENT_CACHE_MAX`] statements, the least recently used one is
//!   dropped from the cache and closed on the server (e.g. many different `IN (...)`
//!   lengths push out one-shot statements, never the hot ones). The order lives in the
//!   pooled [`PgConn`], so it is lock-free across connections and freed with the connection.
//! * Transactions pin one pooled connection and run `BEGIN` / `SAVEPOINT` on it. A
//!   transaction dropped without commit or rollback closes its connection instead of
//!   returning it to the pool mid-transaction.
//! * With `pgbouncer=true` in the URL (a transaction pooler in front of the server) no statement
//!   has a name: queries go through the unnamed statement (`query_typed`), a failed lookup of a
//!   named statement becomes a clear error, and the types of the schema are read once at connect.
//! * TLS follows libpq's `sslmode`: `disable`; `prefer` (the default) and `require`
//!   encrypt without verifying the certificate; `verify-ca` / `verify-full` verify it
//!   against the webpki roots, hostname included.

use std::borrow::Cow;
use std::cell::Cell as StdCell;
use std::collections::HashMap;
use std::error::Error as StdError;
use std::ops::Deref;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use bytes::BytesMut;
use chrono::{DateTime, NaiveDateTime, Utc};
use deadpool::managed::{self, Metrics, RecycleError, RecycleResult};
use deadpool_postgres::{ClientWrapper, Manager, ManagerConfig, RecyclingMethod};
use sea_query::{ArrayType, Value};
use tokio::sync::Mutex;
use tokio_postgres::types::{to_sql_checked, FromSql, IsNull, Kind, ToSql, Type};
use tokio_postgres::binary_copy::BinaryCopyInWriter;
use tokio_postgres::{NoTls, Row, SimpleQueryMessage, Statement};

use super::deadline::Fate;
use super::{numeric, BoxFuture, Cell, DbError, DbResult, Driver, ErrorKind, Executor, RawRows, RowSet, SessionLock, TimeoutOutcome, Transaction};
use monorm_core::ir::{ColType, ValueType};

const STATEMENT_CACHE_MAX: usize = 512;

fn pg_err(e: tokio_postgres::Error) -> DbError {
    match e.as_db_error() {
        Some(db) => {
            let code = db.code().code();
            let kind = if code.starts_with("23") {
                ErrorKind::Integrity
            } else if code == "55P03" {
                ErrorKind::LockNotAvailable
            } else if code == "25P02" {
                ErrorKind::TransactionAborted
            } else {
                ErrorKind::Other
            };
            let mut message = format!("{} ({code}): {}", db.severity(), db.message());
            if let Some(detail) = db.detail() {
                message.push_str("\nDETAIL: ");
                message.push_str(detail);
            }
            DbError {
                kind,
                message,
                sqlstate: Some(code.to_owned()),
                constraint: db.constraint().map(str::to_owned),
                detail: db.detail().map(str::to_owned),
                cause: None,
                outcome: None,
            }
        }
        None => DbError::other(e.to_string()),
    }
}

fn closed() -> DbError {
    DbError::other("transaction is already committed or rolled back")
}

// -- parameters ---------------------------------------------------------------------------

fn pg_type(v: &Value) -> DbResult<Type> {
    Ok(match v {
        Value::Bool(_) => Type::BOOL,
        Value::SmallInt(_) | Value::TinyInt(_) | Value::TinyUnsigned(_) => Type::INT2,
        Value::Int(_) | Value::SmallUnsigned(_) => Type::INT4,
        Value::BigInt(_) | Value::Unsigned(_) | Value::BigUnsigned(_) => Type::INT8,
        Value::Float(_) => Type::FLOAT4,
        Value::Double(_) => Type::FLOAT8,
        Value::String(_) => Type::TEXT,
        Value::Bytes(_) => Type::BYTEA,
        Value::Json(_) => Type::JSONB,
        Value::ChronoDate(_) => Type::DATE,
        Value::ChronoDateTimeWithTimeZone(_) | Value::ChronoDateTimeUtc(_) => Type::TIMESTAMPTZ,
        Value::ChronoDateTime(_) => Type::TIMESTAMP,
        Value::Uuid(_) => Type::UUID,
        Value::BigDecimal(_) => Type::NUMERIC,
        Value::Array(t, _) => match t {
            ArrayType::Bool => Type::BOOL_ARRAY,
            ArrayType::Int => Type::INT4_ARRAY,
            ArrayType::BigInt => Type::INT8_ARRAY,
            ArrayType::Double => Type::FLOAT8_ARRAY,
            ArrayType::String => Type::TEXT_ARRAY,
            ArrayType::Json => Type::JSONB_ARRAY,
            ArrayType::ChronoDate => Type::DATE_ARRAY,
            ArrayType::ChronoDateTimeWithTimeZone => Type::TIMESTAMPTZ_ARRAY,
            ArrayType::Uuid => Type::UUID_ARRAY,
            ArrayType::BigDecimal => Type::NUMERIC_ARRAY,
            other => return Err(DbError::other(format!("unsupported array of {other:?}"))),
        },
        other => return Err(DbError::other(format!("unsupported parameter value {other:?}"))),
    })
}

/// A sea-query value as a binary Postgres parameter of the type `pg_type` gives it.
#[derive(Debug)]
struct Param<'a>(&'a Value);

impl ToSql for Param<'_> {
    fn to_sql(&self, ty: &Type, out: &mut BytesMut) -> Result<IsNull, Box<dyn StdError + Sync + Send>> {
        match self.0 {
            Value::Bool(v) => v.to_sql(ty, out),
            Value::TinyInt(v) => v.map(i16::from).to_sql(ty, out),
            Value::TinyUnsigned(v) => v.map(i16::from).to_sql(ty, out),
            Value::SmallInt(v) => v.to_sql(ty, out),
            Value::SmallUnsigned(v) => v.map(i32::from).to_sql(ty, out),
            Value::Int(v) => v.to_sql(ty, out),
            Value::Unsigned(v) => v.map(i64::from).to_sql(ty, out),
            Value::BigInt(v) => v.to_sql(ty, out),
            Value::BigUnsigned(v) => v.map(i64::try_from).transpose()?.to_sql(ty, out),
            Value::Float(v) => v.to_sql(ty, out),
            Value::Double(v) => v.to_sql(ty, out),
            Value::String(v) => v.as_deref().to_sql(ty, out),
            Value::Bytes(v) => v.as_deref().to_sql(ty, out),
            Value::Json(v) => v.as_deref().to_sql(ty, out),
            Value::ChronoDate(v) => v.to_sql(ty, out),
            Value::ChronoDateTimeWithTimeZone(v) => v.to_sql(ty, out),
            Value::ChronoDateTimeUtc(v) => v.to_sql(ty, out),
            Value::ChronoDateTime(v) => v.to_sql(ty, out),
            Value::Uuid(v) => v.to_sql(ty, out),
            Value::BigDecimal(None) | Value::Array(_, None) => Ok(IsNull::Yes),
            Value::BigDecimal(Some(d)) => {
                numeric::encode(&d.to_plain_string(), out)?;
                Ok(IsNull::No)
            }
            Value::Array(_, Some(items)) => {
                let params: Vec<Param<'_>> = items.iter().map(Param).collect();
                params.as_slice().to_sql(ty, out)
            }
            other => Err(format!("unsupported parameter value {other:?}").into()),
        }
    }

    fn accepts(_: &Type) -> bool {
        true // the statement was prepared with the types `pg_type` gives
    }

    to_sql_checked!();
}

/// The declared type and the bytes Postgres receives for `v`; equal keys cannot be told apart
/// by the server (`Value: Eq` is looser: it ignores a numeric scale and the sign of a float zero).
pub(super) fn wire_key(v: &Value) -> Option<(Type, Option<Vec<u8>>)> {
    let ty = pg_type(v).ok()?;
    let mut out = BytesMut::new();
    let null = Param(v).to_sql(&ty, &mut out).ok()?;
    Some((ty, matches!(null, IsNull::No).then(|| out.to_vec())))
}

async fn prepare(c: &PgConn, sql: &str, args: &[Value]) -> DbResult<Statement> {
    let types = args.iter().map(pg_type).collect::<DbResult<Vec<_>>>()?;
    let stmt = c.prepare_typed_cached(sql, &types).await.map_err(pg_err)?;
    // Dropping the removed `Statement` closes it on the server (tokio-postgres sends `Close`).
    for key in c.lru().touch(sql, &types) {
        drop(c.statement_cache.remove(&key.sql, &key.types));
    }
    Ok(stmt)
}

/// Drops the cached statement of a call that failed with `0A000` (a changed table made the
/// plan stale), so the next call prepares it again.
fn forget_stale(c: &PgConn, sql: &str, args: &[Value], e: tokio_postgres::Error) -> DbError {
    if e.code().is_some_and(|code| code.code() == "0A000") {
        if let Ok(types) = args.iter().map(pg_type).collect::<DbResult<Vec<_>>>() {
            c.statement_cache.remove(sql, &types);
            c.lru().forget(sql, &types);
        }
    }
    pg_err(e)
}

/// The error of a statement. Behind a pooler a missing or doubled statement name (`26000`,
/// `42P05`) means a named statement reached another server: the connection is marked broken so
/// the pool replaces it.
fn run_err(c: &PgConn, sql: &str, args: &[Value], e: tokio_postgres::Error) -> DbError {
    if c.pgbouncer { pooler_err(Some(c), e) } else { forget_stale(c, sql, args, e) }
}

fn pooler_err(c: Option<&PgConn>, e: tokio_postgres::Error) -> DbError {
    if !pooler_code(&e) {
        return pg_err(e);
    }
    let code = e.code().map(|c| c.code().to_owned()).unwrap_or_default();
    if let Some(c) = c {
        c.broken.store(true, Ordering::Release);
    }
    let mut err = pg_err(e);
    let name = err.message.split('"').nth(1).map(str::to_owned).unwrap_or_default();
    err.message = format!(
        "prepared statement \"{name}\" is not known in pgbouncer mode ({code}): a type that the schema does not list needs a statement of its own; add it to the schema or use a URL without pgbouncer=true"
    );
    err
}

/// The parameters of a statement with their declared types, for the unnamed statement.
fn typed<'a>(params: &'a [Param<'a>], args: &[Value]) -> DbResult<Vec<(&'a (dyn ToSql + Sync), Type)>> {
    args.iter().zip(params).map(|(v, p)| Ok((p as &(dyn ToSql + Sync), pg_type(v)?))).collect()
}

async fn query(c: &PgConn, sql: &str, args: &[Value]) -> DbResult<Box<dyn RowSet>> {
    let params: Vec<Param<'_>> = args.iter().map(Param).collect();
    if c.pgbouncer {
        let rows = c.query_typed(sql, &typed(&params, args)?).await.map_err(|e| run_err(c, sql, args, e))?;
        return Ok(Box::new(PgRows(rows)));
    }
    let stmt = prepare(c, sql, args).await?;
    let refs: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p as &(dyn ToSql + Sync)).collect();
    let rows = c.query(&stmt, &refs).await.map_err(|e| run_err(c, sql, args, e))?;
    Ok(Box::new(PgRows(rows)))
}

async fn fetch(c: &PgConn, sql: &str, args: &[Value]) -> DbResult<Box<dyn RawRows>> {
    let params: Vec<Param<'_>> = args.iter().map(Param).collect();
    if c.pgbouncer {
        // The unnamed statement has no description to read first: the columns come from the
        // first row, so a result with no rows has no columns.
        let rows = c.query_typed(sql, &typed(&params, args)?).await.map_err(|e| run_err(c, sql, args, e))?;
        let cols = rows.first().map(Row::columns).unwrap_or_default();
        let types = cols.iter().map(|c| raw_type(c.name(), c.type_())).collect::<DbResult<Vec<_>>>()?;
        let columns = cols.iter().map(|c| c.name().to_owned()).collect();
        return Ok(Box::new(PgRawRows { columns, types, rows }));
    }
    let stmt = prepare(c, sql, args).await?;
    let refs: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p as &(dyn ToSql + Sync)).collect();
    let types = stmt.columns().iter().map(|c| raw_type(c.name(), c.type_())).collect::<DbResult<Vec<_>>>()?;
    let columns = stmt.columns().iter().map(|c| c.name().to_owned()).collect();
    let rows = c.query(&stmt, &refs).await.map_err(|e| run_err(c, sql, args, e))?;
    Ok(Box::new(PgRawRows { columns, types, rows }))
}

/// How a raw column decodes. `int2`, `float4`, and `timestamp` have no `ValueType` of their own.
#[derive(Clone, Copy)]
enum RawType {
    Value(ValueType),
    SmallInt,
    Real,
    Timestamp,
    SmallIntArray,
    RealArray,
    TimestampArray,
}

fn raw_type(name: &str, ty: &Type) -> DbResult<RawType> {
    let (scalar, array) = match ty.kind() {
        Kind::Array(inner) => (inner, true),
        _ => (ty, false),
    };
    let col = match *scalar {
        Type::INT2 => return Ok(if array { RawType::SmallIntArray } else { RawType::SmallInt }),
        Type::FLOAT4 => return Ok(if array { RawType::RealArray } else { RawType::Real }),
        Type::TIMESTAMP => return Ok(if array { RawType::TimestampArray } else { RawType::Timestamp }),
        Type::INT8 => ColType::BigInt,
        Type::INT4 => ColType::Int,
        Type::FLOAT8 => ColType::Float,
        Type::BOOL => ColType::Bool,
        Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME => ColType::Text,
        Type::TIMESTAMPTZ => ColType::DateTime,
        Type::DATE => ColType::Date,
        Type::UUID => ColType::Uuid,
        Type::JSON | Type::JSONB => ColType::Json,
        Type::NUMERIC => ColType::Decimal,
        _ if matches!(scalar.kind(), Kind::Enum(_)) => ColType::Text,
        _ => {
            return Err(DbError::other(format!(
                "column {name:?} has type {ty}, which fetch() does not read; cast it in the SQL (for example ::text)"
            )))
        }
    };
    Ok(RawType::Value(ValueType { ty: col, array, enum_idx: None }))
}

struct PgRawRows {
    columns: Vec<String>,
    types: Vec<RawType>,
    rows: Vec<Row>,
}

impl RawRows for PgRawRows {
    fn columns(&self) -> &[String] {
        &self.columns
    }
    fn len(&self) -> usize {
        self.rows.len()
    }
    fn cell(&self, row: usize, col: usize) -> DbResult<Cell<'_>> {
        let r = &self.rows[row];
        match self.types[col] {
            RawType::Value(ty) => cell(r, col, ty),
            RawType::SmallInt => opt(r, col, |v: i16| Cell::Int(v.into())),
            RawType::Real => opt(r, col, |v: f32| Cell::Float(v.into())),
            RawType::SmallIntArray => list(r, col, |v: i16| Cell::Int(v.into())),
            RawType::RealArray => list(r, col, |v: f32| Cell::Float(v.into())),
            RawType::Timestamp => opt(r, col, timestamp_cell),
            RawType::TimestampArray => list(r, col, timestamp_cell),
        }
    }
}

async fn execute(c: &PgConn, sql: &str, args: &[Value]) -> DbResult<u64> {
    let params: Vec<Param<'_>> = args.iter().map(Param).collect();
    if c.pgbouncer {
        return c.execute_typed(sql, &typed(&params, args)?).await.map_err(|e| run_err(c, sql, args, e));
    }
    let stmt = prepare(c, sql, args).await?;
    let refs: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p as &(dyn ToSql + Sync)).collect();
    c.execute(&stmt, &refs).await.map_err(|e| run_err(c, sql, args, e))
}

/// Simple-query protocol: several statements, no parameters, text results.
async fn simple(c: &PgConn, sql: &str) -> DbResult<Vec<SimpleQueryMessage>> {
    c.simple_query(sql).await.map_err(pg_err)
}

async fn batch(c: &PgConn, sql: &str) -> DbResult<u64> {
    Ok(simple(c, sql)
        .await?
        .iter()
        .map(|m| match m {
            SimpleQueryMessage::CommandComplete(n) => *n,
            _ => 0,
        })
        .sum())
}

async fn query_text(c: &PgConn, sql: &str) -> DbResult<Vec<Vec<Option<String>>>> {
    Ok(simple(c, sql)
        .await?
        .iter()
        .filter_map(|m| match m {
            SimpleQueryMessage::Row(r) => Some((0..r.len()).map(|i| r.get(i).map(str::to_owned)).collect()),
            _ => None,
        })
        .collect())
}

fn quote(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

/// `v` as the server's column type `ty` wants it: binary COPY has no parameter types of
/// its own, so an `Int` field on a `smallint` column must be sent as two bytes.
fn fit<'a>(v: &'a Value, ty: &Type) -> DbResult<std::borrow::Cow<'a, Value>> {
    use std::borrow::Cow;
    let int = match v {
        Value::TinyInt(i) => i.map(i64::from),
        Value::SmallInt(i) => i.map(i64::from),
        Value::Int(i) => i.map(i64::from),
        Value::BigInt(i) => *i,
        Value::Float(f) if *ty == Type::FLOAT8 => return Ok(Cow::Owned(Value::Double(f.map(f64::from)))),
        #[allow(clippy::cast_possible_truncation)]
        Value::Double(f) if *ty == Type::FLOAT4 => return Ok(Cow::Owned(Value::Float(f.map(|f| f as f32)))),
        _ => return Ok(Cow::Borrowed(v)),
    };
    let range = |e| DbError::other(format!("{e}: value out of range for a {} column", ty.name()));
    Ok(Cow::Owned(match *ty {
        Type::INT2 => Value::SmallInt(int.map(i16::try_from).transpose().map_err(range)?),
        Type::INT4 => Value::Int(int.map(i32::try_from).transpose().map_err(range)?),
        Type::INT8 => Value::BigInt(int),
        _ => return Ok(Cow::Borrowed(v)),
    }))
}

/// Binary `COPY ... FROM STDIN`. The column types come from the server, so enums,
/// `json` and arrays encode as the columns expect.
///
/// Behind a pooler the statements it prepares are dropped before it returns: tokio-postgres
/// sends their `Close` ahead of the next message, so the caller's `COMMIT` ends a transaction
/// that holds no statement.
async fn copy_in(c: &PgConn, table: &str, columns: &[String], rows: &[Vec<Value>]) -> DbResult<u64> {
    let err = |e| if c.pgbouncer { pooler_err(Some(c), e) } else { pg_err(e) };
    let cols = columns.iter().map(|c| quote(c)).collect::<Vec<_>>().join(", ");
    let table = quote(table);
    let probe = c.prepare(&format!("SELECT {cols} FROM {table} LIMIT 0")).await.map_err(err)?;
    let types: Vec<Type> = probe.columns().iter().map(|c| c.type_().clone()).collect();
    let copy_sql = format!("COPY {table} ({cols}) FROM STDIN (FORMAT binary)");
    // `copy_in(&str)` prepares and drops its statement inside the call; here the drop is ours.
    let copy = if c.pgbouncer { Some(c.prepare(&copy_sql).await.map_err(err)?) } else { None };
    let sink = match &copy {
        Some(stmt) => c.copy_in(stmt).await,
        None => c.copy_in(&copy_sql).await,
    }
    .map_err(err)?;
    let writer = BinaryCopyInWriter::new(sink, &types);
    let mut writer = std::pin::pin!(writer);
    for row in rows {
        let row = row.iter().zip(&types).map(|(v, ty)| fit(v, ty)).collect::<DbResult<Vec<_>>>()?;
        let params: Vec<Param<'_>> = row.iter().map(|v| Param(v)).collect();
        let refs: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p as &(dyn ToSql + Sync)).collect();
        writer.as_mut().write(&refs).await.map_err(err)?;
    }
    writer.finish().await.map_err(err)
}

/// `copy_in` as a statement of its own: behind a pooler it runs in one transaction, so the
/// probe, the `COPY` and the `Close` of both reach one server.
async fn copy_in_alone(c: Object, table: &str, columns: &[String], rows: &[Vec<Value>]) -> DbResult<u64> {
    if !c.pgbouncer {
        return copy_in(&c, table, columns, rows).await;
    }
    // Dropped mid-way, the session detaches the connection instead of pooling an open transaction.
    let mut session = Session { client: Some(c), finished: false, next_savepoint: 0, savepoints: vec![] };
    let client = || session.client.as_ref().expect("the session holds the client");
    client().batch_execute("BEGIN").await.map_err(pg_err)?;
    let result = match copy_in(client(), table, columns, rows).await {
        Ok(n) => client().batch_execute("COMMIT").await.map(|()| n).map_err(pg_err),
        Err(e) => Err(e),
    };
    if result.is_err() && client().batch_execute("ROLLBACK").await.is_err() {
        return result; // unfinished: the drop of the session closes the connection
    }
    session.finished = true;
    result
}

// -- rows --------------------------------------------------------------------------------

struct PgRows(Vec<Row>);

fn get<'a, T: FromSql<'a>>(row: &'a Row, idx: usize) -> DbResult<Option<T>> {
    row.try_get::<_, Option<T>>(idx).map_err(|e| DbError::other(format!("column {idx}: {e}")))
}

/// Text, or the label of a database enum (sent as its text).
struct Text<'a>(&'a str);

impl<'a> FromSql<'a> for Text<'a> {
    fn from_sql(_: &Type, raw: &'a [u8]) -> Result<Self, Box<dyn StdError + Sync + Send>> {
        Ok(Text(std::str::from_utf8(raw)?))
    }

    fn accepts(ty: &Type) -> bool {
        <&str as FromSql>::accepts(ty) || matches!(ty.kind(), Kind::Enum(_))
    }
}

/// A `numeric` as decimal text.
struct Numeric(String);

impl<'a> FromSql<'a> for Numeric {
    fn from_sql(_: &Type, raw: &'a [u8]) -> Result<Self, Box<dyn StdError + Sync + Send>> {
        Ok(Numeric(numeric::decode(raw)?))
    }

    fn accepts(ty: &Type) -> bool {
        *ty == Type::NUMERIC
    }
}

fn opt<'a, T: FromSql<'a>>(row: &'a Row, idx: usize, f: impl Fn(T) -> Cell<'a>) -> DbResult<Cell<'a>> {
    Ok(get::<T>(row, idx)?.map(f).unwrap_or(Cell::Null))
}

/// An array cell, each element converted by `f`.
fn list<'a, T: FromSql<'a>>(row: &'a Row, idx: usize, f: impl Fn(T) -> Cell<'a>) -> DbResult<Cell<'a>> {
    let Some(items) = get::<Vec<Option<T>>>(row, idx)? else { return Ok(Cell::Null) };
    Ok(Cell::Array(items.into_iter().map(|v| v.map(&f).unwrap_or(Cell::Null)).collect()))
}

/// A `timestamp` has no zone: read it as UTC, like other DateTime fields.
fn timestamp_cell<'a>(v: NaiveDateTime) -> Cell<'a> {
    Cell::DateTime(v.and_utc())
}

fn cell(row: &Row, idx: usize, ty: ValueType) -> DbResult<Cell<'_>> {
    if ty.array {
        return match ty.ty {
            ColType::BigInt => list(row, idx, Cell::BigInt),
            ColType::Int => list(row, idx, Cell::Int),
            ColType::Float => list(row, idx, Cell::Float),
            ColType::Bool => list(row, idx, Cell::Bool),
            ColType::String | ColType::Text => list(row, idx, |t: Text<'_>| Cell::Text(t.0)),
            ColType::DateTime => list(row, idx, Cell::DateTime),
            ColType::Date => list(row, idx, Cell::Date),
            ColType::Uuid => list(row, idx, Cell::Uuid),
            ColType::Json => list(row, idx, Cell::Json),
            ColType::Decimal => list(row, idx, |n: Numeric| Cell::Decimal(n.0)),
        };
    }
    match ty.ty {
        ColType::BigInt => opt(row, idx, Cell::BigInt),
        ColType::Int => opt(row, idx, Cell::Int),
        ColType::Float => opt(row, idx, Cell::Float),
        ColType::Bool => opt(row, idx, Cell::Bool),
        ColType::String | ColType::Text => opt(row, idx, |t: Text<'_>| Cell::Text(t.0)),
        // timestamptz is UTC on the wire.
        ColType::DateTime => opt(row, idx, Cell::DateTime),
        ColType::Date => opt(row, idx, Cell::Date),
        ColType::Uuid => opt(row, idx, Cell::Uuid),
        ColType::Json => opt(row, idx, Cell::Json),
        ColType::Decimal => opt(row, idx, |n: Numeric| Cell::Decimal(n.0)),
    }
}

impl RowSet for PgRows {
    fn len(&self) -> usize {
        self.0.len()
    }

    fn cell(&self, row: usize, col: usize, ty: ValueType) -> DbResult<Cell<'_>> {
        let r = &self.0[row];
        // The IR type has no int2 or float4; read the wire type, as fetch() does.
        match raw_type("", r.columns()[col].type_()) {
            Ok(RawType::SmallInt) => opt(r, col, |v: i16| Cell::Int(v.into())),
            Ok(RawType::Real) => opt(r, col, |v: f32| Cell::Float(v.into())),
            Ok(RawType::SmallIntArray) => list(r, col, |v: i16| Cell::Int(v.into())),
            Ok(RawType::RealArray) => list(r, col, |v: f32| Cell::Float(v.into())),
            Ok(RawType::Timestamp) => opt(r, col, timestamp_cell),
            Ok(RawType::TimestampArray) => list(r, col, timestamp_cell),
            _ => cell(r, col, ty),
        }
    }

    fn value(&self, row: usize, col: usize, ty: ValueType) -> DbResult<Value> {
        let r = &self.0[row];
        if ty.array {
            return Err(DbError::other("an array can't be a relation key"));
        }
        let wire = r.columns()[col].type_();
        if *wire == Type::INT2 {
            return Ok(Value::Int(get::<i16>(r, col)?.map(i32::from)));
        }
        if *wire == Type::FLOAT4 {
            return Ok(Value::Double(get::<f32>(r, col)?.map(f64::from)));
        }
        if *wire == Type::TIMESTAMP {
            return Ok(Value::ChronoDateTimeWithTimeZone(get::<NaiveDateTime>(r, col)?.map(|d| d.and_utc().fixed_offset())));
        }
        Ok(match ty.ty {
            ColType::BigInt => Value::BigInt(get(r, col)?),
            ColType::Int => Value::Int(get(r, col)?),
            ColType::Float => Value::Double(get(r, col)?),
            ColType::Bool => Value::Bool(get(r, col)?),
            ColType::String | ColType::Text => Value::String(get::<Text>(r, col)?.map(|t| t.0.to_owned())),
            ColType::DateTime => {
                Value::ChronoDateTimeWithTimeZone(get::<DateTime<Utc>>(r, col)?.map(|d| d.fixed_offset()))
            }
            ColType::Date => Value::ChronoDate(get(r, col)?),
            ColType::Uuid => Value::Uuid(get(r, col)?),
            ColType::Json => Value::Json(get::<serde_json::Value>(r, col)?.map(Box::new)),
            ColType::Decimal => Value::BigDecimal(
                get::<Numeric>(r, col)?
                    .map(|n| n.0.parse().map(Box::new))
                    .transpose()
                    .map_err(|e| DbError::other(format!("column {col}: {e}")))?,
            ),
        })
    }

    fn get_i64(&self, row: usize, col: usize) -> DbResult<i64> {
        get::<i64>(&self.0[row], col)?.ok_or_else(|| DbError::other("unexpected NULL"))
    }

    fn get_bool(&self, row: usize, col: usize) -> DbResult<bool> {
        get::<bool>(&self.0[row], col)?.ok_or_else(|| DbError::other("unexpected NULL"))
    }
}

// -- statement cache order ---------------------------------------------------------------

/// A cached statement: its SQL text and parameter types, as deadpool's cache keys it.
#[derive(PartialEq, Eq, Hash, Clone)]
struct StmtKey<'a> {
    sql: Cow<'a, str>,
    types: Cow<'a, [Type]>,
}

impl StmtKey<'static> {
    fn owned(sql: &str, types: &[Type]) -> Self {
        StmtKey { sql: Cow::Owned(sql.to_owned()), types: Cow::Owned(types.to_owned()) }
    }
}

/// The last use of each statement in one connection's cache. A hit allocates nothing; the
/// least recently used statement is found by a scan, only on a miss past the bound (one
/// scan of at most `max + 1` entries next to a `Parse` round trip).
struct StatementLru {
    max: usize,
    tick: u64,
    used: HashMap<StmtKey<'static>, StdCell<u64>>,
}

impl StatementLru {
    fn new(max: usize) -> Self {
        StatementLru { max, tick: 0, used: HashMap::new() }
    }

    /// Marks `sql` as just used. Returns the statements to drop from the cache so it holds
    /// at most `max` (never the one just used).
    fn touch(&mut self, sql: &str, types: &[Type]) -> Vec<StmtKey<'static>> {
        self.tick += 1;
        let probe = StmtKey { sql: Cow::Borrowed(sql), types: Cow::Borrowed(types) };
        if let Some(last) = self.used.get(&probe) {
            last.set(self.tick);
            return Vec::new();
        }
        self.used.insert(StmtKey::owned(sql, types), StdCell::new(self.tick));
        let mut evicted = Vec::new();
        while self.used.len() > self.max.max(1) {
            let oldest = self.used.iter().min_by_key(|(_, last)| last.get()).map(|(k, _)| k.clone());
            let Some(oldest) = oldest else { break };
            self.used.remove(&oldest);
            evicted.push(oldest);
        }
        evicted
    }

    fn forget(&mut self, sql: &str, types: &[Type]) {
        self.used.remove(&StmtKey::owned(sql, types));
    }
}

/// A pooled connection with the order of its statement cache.
pub(super) struct PgConn {
    client: ClientWrapper,
    /// `None` behind a pooler: no statement has a name, so there is no cache to bound.
    lru: Option<StdMutex<StatementLru>>,
    pgbouncer: bool,
    /// Set when a named statement failed behind a pooler: the pool must not hand this connection out again.
    broken: AtomicBool,
}

impl PgConn {
    fn lru(&self) -> std::sync::MutexGuard<'_, StatementLru> {
        // The order is a hint for eviction: a panic while it was held leaves it usable.
        self.lru.as_ref().expect("a connection without a pooler has a statement cache").lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Deref for PgConn {
    type Target = ClientWrapper;
    fn deref(&self) -> &ClientWrapper {
        &self.client
    }
}

struct PgManager {
    inner: Manager,
    statement_cache_max: usize,
    pgbouncer: bool,
    /// The SQL types that `create` reads once behind a pooler (see `warm_up`).
    types: Vec<String>,
}

impl managed::Manager for PgManager {
    type Type = PgConn;
    type Error = tokio_postgres::Error;

    async fn create(&self) -> Result<PgConn, Self::Error> {
        let client = managed::Manager::create(&self.inner).await?;
        if self.pgbouncer {
            warm_up(&client, &self.types).await?;
        }
        Ok(PgConn {
            client,
            lru: (!self.pgbouncer).then(|| StdMutex::new(StatementLru::new(self.statement_cache_max))),
            pgbouncer: self.pgbouncer,
            broken: AtomicBool::new(false),
        })
    }

    async fn recycle(&self, conn: &mut PgConn, metrics: &Metrics) -> RecycleResult<Self::Error> {
        if conn.broken.load(Ordering::Acquire) {
            return Err(RecycleError::message("a statement name was lost behind the pooler"));
        }
        managed::Manager::recycle(&self.inner, &mut conn.client, metrics).await
    }

    fn detach(&self, conn: &mut PgConn) {
        managed::Manager::detach(&self.inner, &mut conn.client)
    }
}

/// Reads the type of every name in `types` through the unnamed statement, in one transaction on
/// one server. tokio-postgres looks up a type it does not know with a named statement of its own
/// (`s{N}`) and caches the answer; after this, a result of a listed type needs no lookup.
/// The named lookup statements are then closed with `DEALLOCATE ALL`: a pooler would not send a
/// later `Bind` to the server that holds them. A type that does not exist yet (before `migrate`)
/// is skipped; each cast has a savepoint of its own so the transaction stays open.
async fn warm_up(client: &tokio_postgres::Client, types: &[String]) -> Result<(), tokio_postgres::Error> {
    if types.is_empty() {
        return Ok(());
    }
    async fn read(client: &tokio_postgres::Client, list: &[&String]) -> Result<(), tokio_postgres::Error> {
        let casts = list.iter().map(|t| format!("NULL::{t}")).collect::<Vec<_>>().join(", ");
        client.query_typed(&format!("SELECT {casts}"), &[]).await.map(drop)
    }
    let missing = |e: &tokio_postgres::Error| e.as_db_error().is_some() && !pooler_code(e);
    client.batch_execute("BEGIN").await?;
    let result: Result<(), tokio_postgres::Error> = async {
        client.batch_execute("SAVEPOINT warm").await?;
        let all: Vec<&String> = types.iter().collect();
        if let Err(e) = read(client, &all).await {
            if !missing(&e) {
                return Err(e);
            }
            client.batch_execute("ROLLBACK TO warm").await?;
            for t in &all {
                client.batch_execute("SAVEPOINT warm_one").await?;
                if let Err(e) = read(client, &[t]).await {
                    if !missing(&e) {
                        return Err(e);
                    }
                    client.batch_execute("ROLLBACK TO warm_one").await?;
                }
                client.batch_execute("RELEASE warm_one").await?;
            }
        }
        client.batch_execute("DEALLOCATE ALL").await
    }
    .await;
    // One message, so the pooler keeps one server: the aborted transaction ends first, then `DEALLOCATE ALL` can run.
    match result {
        Ok(()) => client.batch_execute("COMMIT").await,
        Err(e) => {
            let _ = client.batch_execute("ROLLBACK; DEALLOCATE ALL").await;
            Err(e)
        }
    }
}

/// True for the SQLSTATE of a named statement that the connected server does not hold (`26000`) or already holds (`42P05`).
fn pooler_code(e: &tokio_postgres::Error) -> bool {
    e.code().is_some_and(|c| matches!(c.code(), "26000" | "42P05"))
}

type Pool = managed::Pool<PgManager>;
type Object = managed::Object<PgManager>;

// -- pool --------------------------------------------------------------------------------

pub struct PgDriver {
    pool: Pool,
    /// The connect settings and connector of the pool's connections: for the dedicated
    /// connection of a session lock, and for a cancel request.
    config: tokio_postgres::Config,
    tls: Option<tls::MakeRustlsConnect>,
    /// The URL says a transaction pooler is in front of the server.
    pgbouncer: bool,
}

impl PgDriver {
    #[cfg(test)]
    pub async fn connect(url: &str, max_connections: usize) -> DbResult<Self> {
        Self::connect_with_types(url, max_connections, vec![]).await
    }

    /// `connect` with the SQL types of the schema. With `pgbouncer=true` in the URL, each new
    /// connection reads them once, so no later result needs a statement of its own.
    pub async fn connect_with_types(url: &str, max_connections: usize, types: Vec<String>) -> DbResult<Self> {
        Self::connect_with_cache(url, max_connections, STATEMENT_CACHE_MAX, types).await
    }

    /// `connect` with another statement cache bound per connection (for tests).
    async fn connect_with_cache(url: &str, max_connections: usize, statement_cache_max: usize, types: Vec<String>) -> DbResult<Self> {
        let (mut config, tls, pgbouncer) = tls::parse(url)?;
        // DateTime fields are UTC instants, including when stored in a timestamp
        // without time zone. Keep Postgres's timestamptz -> timestamp cast in UTC
        // on every pooled and dedicated connection, regardless of server defaults.
        let options = format!("{} -c TimeZone=UTC", config.get_options().unwrap_or(""));
        config.options(&options);
        let mgr_config = ManagerConfig { recycling_method: RecyclingMethod::Fast };
        let inner = match tls.clone() {
            Some(tls) => Manager::from_config(config.clone(), tls, mgr_config),
            None => Manager::from_config(config.clone(), NoTls, mgr_config),
        };
        let manager = PgManager { inner, statement_cache_max, pgbouncer, types };
        let pool = Pool::builder(manager)
            .max_size(max_connections.max(1))
            .build()
            .map_err(|e| DbError::other(e.to_string()))?;
        let probe = get_client(&pool).await?; // fail at connect() on a bad URL or password
        if pgbouncer {
            // The pooler may drop the `TimeZone` startup parameter that every other pooled and dedicated connection relies on.
            let zone = query_text(&probe, "SHOW TimeZone").await?;
            let zone = zone.first().and_then(|r| r.first()).cloned().flatten().unwrap_or_default();
            if zone != "UTC" {
                return Err(DbError::other(format!(
                    "the connection through the pooler has TimeZone {zone:?}, not \"UTC\": the pooler dropped the startup parameter; \
                     remove `options` from ignore_startup_parameters in its configuration (PgBouncer 1.20 or later) or set the server time zone to UTC"
                )));
            }
        }
        drop(probe);
        Ok(PgDriver { pool, config, tls, pgbouncer })
    }

    /// Takes a connection and sends `BEGIN`. The session owns the connection before the first
    /// await that can be cancelled, so a dropped future never returns it to the pool mid-transaction.
    async fn start_transaction(
        &self, isolation: Option<super::Isolation>, acquired: Option<Arc<AtomicBool>>,
    ) -> DbResult<Arc<dyn Transaction>> {
        let client = get_client(&self.pool).await?;
        if let Some(acquired) = acquired { acquired.store(true, Ordering::SeqCst); }
        let expiry = Arc::new(Expiry { fate: Fate::new(), cancel: client.cancel_token(), tls: self.tls.clone() });
        let session = Session { client: Some(client), finished: false, next_savepoint: 0, savepoints: vec![] };
        let sql = match isolation {
            Some(level) => format!("BEGIN ISOLATION LEVEL {}", level.sql()),
            None => "BEGIN".to_owned(),
        };
        // A failed or cancelled BEGIN leaves the session unfinished: its drop detaches the connection.
        session.client.as_ref().expect("the session holds the client").batch_execute(&sql).await.map_err(pg_err)?;
        let tx: Arc<dyn Transaction> = Arc::new(PgTx {
            session: Arc::new(Mutex::new(session)),
            savepoint: None,
            done: AtomicBool::new(false),
            expiry,
        });
        Ok(tx)
    }

    /// A connection outside the pool, closed when dropped.
    async fn dedicated(&self) -> DbResult<tokio_postgres::Client> {
        match self.tls.clone() {
            Some(tls) => {
                let (client, conn) = self.config.connect(tls).await.map_err(pg_err)?;
                tokio::spawn(async move { let _ = conn.await; });
                Ok(client)
            }
            None => {
                let (client, conn) = self.config.connect(NoTls).await.map_err(pg_err)?;
                tokio::spawn(async move { let _ = conn.await; });
                Ok(client)
            }
        }
    }
}

async fn get_client(pool: &Pool) -> DbResult<Object> {
    pool.get().await.map_err(|e| match e {
        deadpool_postgres::PoolError::Backend(e) if pool.manager().pgbouncer => pooler_err(None, e),
        deadpool_postgres::PoolError::Backend(e) => pg_err(e),
        other => DbError::other(other.to_string()),
    })
}

impl Executor for PgDriver {
    fn pooler_transactions(&self) -> bool {
        self.pgbouncer
    }
    fn query(&self, sql: String, args: Vec<Value>) -> BoxFuture<'_, DbResult<Box<dyn RowSet>>> {
        Box::pin(async move { let c = get_client(&self.pool).await?; query(&c, &sql, &args).await })
    }

    fn fetch(&self, sql: String, args: Vec<Value>) -> BoxFuture<'_, DbResult<Box<dyn RawRows>>> {
        Box::pin(async move { let c = get_client(&self.pool).await?; fetch(&c, &sql, &args).await })
    }

    fn execute(&self, sql: String, args: Vec<Value>) -> BoxFuture<'_, DbResult<u64>> {
        Box::pin(async move { let c = get_client(&self.pool).await?; execute(&c, &sql, &args).await })
    }

    fn batch(&self, sql: String) -> BoxFuture<'_, DbResult<u64>> {
        Box::pin(async move { let c = get_client(&self.pool).await?; batch(&c, &sql).await })
    }

    fn query_text(&self, sql: String) -> BoxFuture<'_, DbResult<Vec<Vec<Option<String>>>>> {
        Box::pin(async move { let c = get_client(&self.pool).await?; query_text(&c, &sql).await })
    }

    fn copy_in(&self, table: String, columns: Vec<String>, rows: Vec<Vec<Value>>) -> BoxFuture<'_, DbResult<u64>> {
        Box::pin(async move { let c = get_client(&self.pool).await?; copy_in_alone(c, &table, &columns, &rows).await })
    }

    fn begin(&self) -> BoxFuture<'_, DbResult<Arc<dyn Transaction>>> {
        Box::pin(self.start_transaction(None, None))
    }

    fn begin_with(&self, start: super::TxStart) -> BoxFuture<'_, DbResult<Arc<dyn Transaction>>> {
        Box::pin(self.start_transaction(start.isolation, Some(start.acquired)))
    }
}

impl Driver for PgDriver {
    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move { self.pool.close() })
    }

    fn session_lock(
        &self, key: i64, exclusive: bool, nowait: bool, timeout_ms: Option<u64>,
    ) -> BoxFuture<'_, DbResult<Option<Arc<dyn SessionLock>>>> {
        Box::pin(async move {
            if self.pgbouncer {
                // The lock belongs to one server session, and a pooler hands the connection to others.
                return Err(DbError::other(
                    "a session lock needs a direct connection, and this URL has pgbouncer=true: use a URL without it (for migrate, set MONORM_DATABASE_URL)",
                ));
            }
            // `lock_timeout = 0` means no timeout in Postgres.
            let nowait = nowait || timeout_ms == Some(0);
            // A connection of its own: a lock wait never holds a pool slot.
            let client = self.dedicated().await?;
            // `lock_timeout` applies to the lock statement only.
            let timeout = match timeout_ms { Some(ms) if !nowait => format!("SET LOCAL lock_timeout = {ms}; "), _ => String::new() };
            let sql = format!("BEGIN; {timeout}{}; COMMIT", crate::advisory::session_sql(key, exclusive, nowait));
            let mut cancel = CancelOnDrop { token: Some(client.cancel_token()), tls: self.tls.clone() };
            let result = client.simple_query(&sql).await.map_err(pg_err);
            cancel.token = None;
            let taken = match result {
                Ok(messages) => !nowait || messages.iter().any(|m| matches!(m,
                    SimpleQueryMessage::Row(r) if r.get(0) == Some("true"))),
                Err(e) if e.kind == ErrorKind::LockNotAvailable => false,
                Err(e) => return Err(e),
            };
            if !taken {
                return Ok(None);
            }
            let lock: Arc<dyn SessionLock> = Arc::new(PgSessionLock {
                client: Mutex::new(Some(client)),
                unlock: crate::advisory::unlock_sql(key, exclusive),
            });
            Ok(Some(lock))
        })
    }
}

/// Cancels a lock wait on the server when the waiting future is dropped (a cancelled
/// task): the server does not see a closed socket while its backend waits for a lock.
struct CancelOnDrop {
    token: Option<tokio_postgres::CancelToken>,
    tls: Option<tls::MakeRustlsConnect>,
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        let (Some(token), Ok(runtime)) = (self.token.take(), tokio::runtime::Handle::try_current()) else { return };
        let tls = self.tls.take();
        runtime.spawn(async move {
            let _ = match tls {
                Some(tls) => token.cancel_query(tls).await,
                None => token.cancel_query(NoTls).await,
            };
        });
    }
}

/// The dedicated connection of a session advisory lock. Release unlocks, then closes it;
/// a failed unlock still closes it, and the server drops the lock.
struct PgSessionLock {
    client: Mutex<Option<tokio_postgres::Client>>,
    unlock: String,
}

impl SessionLock for PgSessionLock {
    fn release(&self) -> BoxFuture<'_, DbResult<()>> {
        Box::pin(async move {
            if let Some(client) = self.client.lock().await.take() {
                let _ = client.batch_execute(&self.unlock).await;
            }
            Ok(())
        })
    }
}

// -- transactions ------------------------------------------------------------------------

/// The connection a transaction (and its savepoints) runs on.
struct Session {
    client: Option<Object>,
    finished: bool,
    next_savepoint: u64,
    savepoints: Vec<String>,
}

impl Drop for Session {
    fn drop(&mut self) {
        // Never hand a connection that is still inside a transaction back to the pool:
        // detach it, so it closes and the server rolls back.
        if !self.finished {
            if let Some(obj) = self.client.take() {
                drop(Object::take(obj));
            }
        }
    }
}

/// What the root transaction and its savepoints share for the deadline.
struct Expiry {
    /// Decides between the root COMMIT and the expiry; no statement starts after expiry won.
    fate: Fate,
    cancel: tokio_postgres::CancelToken,
    tls: Option<tls::MakeRustlsConnect>,
}

impl Expiry {
    async fn send_cancel(&self) {
        let _ = match self.tls.clone() {
            Some(tls) => self.cancel.cancel_query(tls).await,
            None => self.cancel.cancel_query(NoTls).await,
        };
    }
}

struct PgTx {
    session: Arc<Mutex<Session>>,
    /// `None` for the outermost transaction.
    savepoint: Option<String>,
    done: AtomicBool,
    expiry: Arc<Expiry>,
}

impl PgTx {
    async fn begin_savepoint(&self) -> DbResult<Self> {
        if self.done.load(Ordering::Acquire) {
            return Err(closed());
        }
        let mut session = self.session.lock().await;
        if self.done.load(Ordering::Acquire) || self.expiry.fate.is_expired() { return Err(closed()); }
        super::check_savepoint(&session.savepoints, &self.savepoint)?;
        let name = format!("monorm_sp_{}", session.next_savepoint);
        session.next_savepoint += 1;
        let client = session.client.as_ref().ok_or_else(closed)?;
        client.batch_execute(&format!("SAVEPOINT {name}")).await.map_err(pg_err)?;
        session.savepoints.push(name.clone());
        Ok(Self {
            session: self.session.clone(),
            savepoint: Some(name),
            done: AtomicBool::new(false),
            expiry: self.expiry.clone(),
        })
    }

    /// Runs `f` on the transaction's connection, unless it already ended.
    async fn with<T>(
        &self,
        f: impl for<'c> FnOnce(&'c PgConn) -> BoxFuture<'c, DbResult<T>>,
    ) -> DbResult<T> {
        if self.done.load(Ordering::Acquire) {
            return Err(closed());
        }
        let session = self.session.lock().await;
        if self.done.load(Ordering::Acquire) || self.expiry.fate.is_expired() { return Err(closed()); }
        super::check_savepoint(&session.savepoints, &self.savepoint)?;
        let client = session.client.as_ref().ok_or_else(closed)?;
        f(client).await
    }

    async fn finish(&self, sql: String, commit: bool) -> DbResult<()> {
        let mut session = self.session.lock().await;
        if self.done.load(Ordering::Acquire) || self.expiry.fate.is_expired() { return Err(closed()); }
        super::check_savepoint(&session.savepoints, &self.savepoint)?;
        // The swap goes right before the COMMIT: when expiry won, nothing is sent.
        if commit && self.savepoint.is_none() && !self.expiry.fate.commit_sent() { return Err(closed()); }
        self.done.store(true, Ordering::Release);
        let client = session.client.as_ref().ok_or_else(closed)?;
        let result = client.batch_execute(&sql).await.map_err(pg_err);
        if result.is_err() && self.savepoint.is_none() {
            // A COMMIT that never ran (the probe below failed) leaves the aborted
            // transaction open; this is a harmless warning after a COMMIT that ran.
            if client.batch_execute("ROLLBACK").await.is_err() {
                // A failed cleanup cannot prove this connection is safe to reuse.
                if let Some(obj) = session.client.take() {
                    drop(Object::take(obj));
                }
            }
        }
        if self.savepoint.is_some() && result.is_ok() {
            session.savepoints.pop();
        }
        if self.savepoint.is_none() {
            // Failed cleanup detached the connection above; otherwise the ended
            // transaction's connection can return to the pool.
            session.finished = true;
            session.client = None;
        }
        result
    }
}

impl Executor for PgTx {
    fn advisory_lock(&self, key: i64, exclusive: bool, nowait: bool) -> BoxFuture<'_, DbResult<bool>> {
        Box::pin(async move {
            let sql = crate::advisory::sql(key, exclusive, nowait);
            self.with(|c| Box::pin(async move {
                let messages = simple(c, &sql).await?;
                Ok(!nowait || messages.iter().any(|m| matches!(m,
                    SimpleQueryMessage::Row(r) if r.get(0) == Some("true"))))
            })).await
        })
    }
    fn query(&self, sql: String, args: Vec<Value>) -> BoxFuture<'_, DbResult<Box<dyn RowSet>>> {
        Box::pin(async move { self.with(|c| Box::pin(async move { query(c, &sql, &args).await })).await })
    }

    fn fetch(&self, sql: String, args: Vec<Value>) -> BoxFuture<'_, DbResult<Box<dyn RawRows>>> {
        Box::pin(async move { self.with(|c| Box::pin(async move { fetch(c, &sql, &args).await })).await })
    }

    fn execute(&self, sql: String, args: Vec<Value>) -> BoxFuture<'_, DbResult<u64>> {
        Box::pin(async move { self.with(|c| Box::pin(async move { execute(c, &sql, &args).await })).await })
    }

    fn batch(&self, sql: String) -> BoxFuture<'_, DbResult<u64>> {
        Box::pin(async move { self.with(|c| Box::pin(async move { batch(c, &sql).await })).await })
    }

    fn query_text(&self, sql: String) -> BoxFuture<'_, DbResult<Vec<Vec<Option<String>>>>> {
        Box::pin(async move { self.with(|c| Box::pin(async move { query_text(c, &sql).await })).await })
    }

    fn copy_in(&self, table: String, columns: Vec<String>, rows: Vec<Vec<Value>>) -> BoxFuture<'_, DbResult<u64>> {
        Box::pin(async move {
            self.with(|c| Box::pin(async move { copy_in(c, &table, &columns, &rows).await })).await
        })
    }

    fn begin(&self) -> BoxFuture<'_, DbResult<Arc<dyn Transaction>>> {
        Box::pin(async move {
            let tx: Arc<dyn Transaction> = Arc::new(self.begin_savepoint().await?);
            Ok(tx)
        })
    }
}

impl Transaction for PgTx {
    fn commit(&self) -> BoxFuture<'_, DbResult<()>> {
        Box::pin(async move {
            let sql = match &self.savepoint {
                Some(sp) => format!("RELEASE SAVEPOINT {sp}"),
                // tokio-postgres hides the command tag, so `SELECT 1` stands in for the
                // check for a `ROLLBACK` tag: in an aborted transaction it fails (25P02)
                // and the COMMIT does not run, instead of ending as a silent ROLLBACK.
                None => "SELECT 1; COMMIT".to_owned(),
            };
            self.finish(sql, true).await.map_err(|mut e| {
                if self.savepoint.is_none() && e.kind == ErrorKind::TransactionAborted {
                    e.message = "the database aborted the transaction before COMMIT".to_owned();
                }
                e
            })
        })
    }

    fn rollback(&self) -> BoxFuture<'_, DbResult<()>> {
        Box::pin(async move {
            let sql = match &self.savepoint {
                Some(sp) => format!("ROLLBACK TO SAVEPOINT {sp}; RELEASE SAVEPOINT {sp}"),
                None => "ROLLBACK".to_owned(),
            };
            self.finish(sql, false).await
        })
    }

    /// When expiry wins the `Fate` swap, a statement that runs gets a cancel request. When a
    /// COMMIT won, no cancel goes out and the connection is closed. The connection is reused
    /// only after a confirmed `ROLLBACK` that no cancel preceded: a cancel request could hit a
    /// later user of the connection.
    fn expire(&self, grace: std::time::Duration) -> BoxFuture<'_, TimeoutOutcome> {
        Box::pin(async move {
            let won = self.expiry.fate.expire();
            let cleanup = async {
                let mut cancelled = false;
                let mut session = match self.session.try_lock() {
                    Ok(session) => session,
                    Err(_) => {
                        if won {
                            self.expiry.send_cancel().await;
                            cancelled = true;
                        }
                        self.session.lock().await
                    }
                };
                let mut retire = cancelled || !won;
                let mut rolled_back = false;
                if won {
                    if let Some(client) = session.client.as_ref() {
                        rolled_back = client.batch_execute("ROLLBACK").await.is_ok();
                    }
                    retire |= !rolled_back;
                }
                self.done.store(true, Ordering::Release);
                session.finished = true;
                if let Some(obj) = session.client.take() {
                    if retire { drop(Object::take(obj)); }
                }
                rolled_back
            };
            // A cleanup that does not end in time leaves the session unfinished: its drop closes the connection.
            match tokio::time::timeout(grace, cleanup).await {
                Ok(true) if won => TimeoutOutcome::RolledBack,
                _ => TimeoutOutcome::Unknown,
            }
        })
    }
}

// -- TLS ---------------------------------------------------------------------------------

mod tls {
    use std::sync::Arc;

    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
    pub use tokio_postgres_rustls::MakeRustlsConnect;

    use super::{DbError, DbResult};

    /// The tokio-postgres config for `url`, the TLS connector its `sslmode` asks for (`None`
    /// for `disable`), and whether `pgbouncer=true` is set. tokio-postgres does not know that key.
    pub fn parse(url: &str) -> DbResult<(tokio_postgres::Config, Option<MakeRustlsConnect>, bool)> {
        let (base, query) = url.split_once('?').unwrap_or((url, ""));
        let mut mode = String::from("prefer");
        let mut pairs = vec![];
        let mut pgbouncer = false;
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            if let Some(v) = pair.strip_prefix("pgbouncer=") {
                pgbouncer = match v {
                    "true" => true,
                    "false" => false,
                    other => return Err(DbError::other(format!("pgbouncer must be true or false, not {other:?}"))),
                };
                continue;
            }
            match pair.strip_prefix("sslmode=") {
                Some(m) => {
                    mode = m.to_owned();
                    // tokio-postgres knows disable / prefer / require; the stricter modes
                    // are enforced by the connector built below.
                    let m = match m {
                        "verify-ca" | "verify-full" => "require",
                        "allow" => "prefer",
                        m => m,
                    };
                    pairs.push(format!("sslmode={m}"));
                }
                None => pairs.push(pair.to_owned()),
            }
        }
        if !matches!(mode.as_str(), "disable" | "allow" | "prefer" | "require" | "verify-ca" | "verify-full") {
            return Err(DbError::other(format!("unknown sslmode {mode:?}")));
        }
        let url = if pairs.is_empty() { base.to_owned() } else { format!("{base}?{}", pairs.join("&")) };
        let config: tokio_postgres::Config =
            url.parse().map_err(|e: tokio_postgres::Error| DbError::other(format!("invalid database URL: {e}")))?;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let builder = ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .map_err(|e| DbError::other(e.to_string()))?;
        let tls = match mode.as_str() {
            "disable" => return Ok((config, None, pgbouncer)),
            "allow" | "prefer" | "require" => {
                builder.dangerous().with_custom_certificate_verifier(Arc::new(NoVerify(provider))).with_no_client_auth()
            }
            "verify-ca" | "verify-full" => {
                let roots = RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
                builder.with_root_certificates(roots).with_no_client_auth()
            }
            _ => unreachable!("checked above"),
        };
        Ok((config, Some(MakeRustlsConnect::new(tls)), pgbouncer))
    }

    /// libpq's `prefer` / `require`: encrypt, but accept any certificate. Handshake
    /// signatures are still checked.
    #[derive(Debug)]
    struct NoVerify(Arc<CryptoProvider>);

    impl ServerCertVerifier for NoVerify {
        fn verify_server_cert(
            &self,
            _: &CertificateDer<'_>,
            _: &[CertificateDer<'_>],
            _: &ServerName<'_>,
            _: &[u8],
            _: UnixTime,
        ) -> Result<ServerCertVerified, rustls::Error> {
            Ok(ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
        }

        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            self.0.signature_verification_algorithms.supported_schemes()
        }
    }
}

#[cfg(test)]
mod failure_tests {
    use super::*;

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Runtime::new().unwrap()
    }

    fn test_url() -> String {
        std::env::var("ORM_RUST_TEST_POSTGRES_URL")
            .or_else(|_| std::env::var("ORM_TEST_DATABASE_URL"))
            .unwrap_or_else(|_| "postgres://postgres:postgres@localhost/orm_test".into())
    }

    async fn driver() -> PgDriver {
        PgDriver::connect(&test_url(), 1).await.unwrap()
    }

    /// `url` with `pgbouncer=true`.
    fn flagged(url: &str) -> String {
        format!("{url}{}pgbouncer=true", if url.contains('?') { '&' } else { '?' })
    }

    #[test]
    fn concurrent_sibling_savepoints_raise_before_sql() {
        runtime().block_on(async {
            let db = driver().await;
            let obj = get_client(&db.pool).await.unwrap();
            obj.batch_execute("BEGIN; CREATE TEMP TABLE sibling_savepoints (id INTEGER PRIMARY KEY) ON COMMIT PRESERVE ROWS").await.unwrap();
            let expiry = Arc::new(Expiry { fate: Fate::new(), cancel: obj.cancel_token(), tls: None });
            let outer = Arc::new(PgTx {
                session: Arc::new(Mutex::new(Session { client: Some(obj), finished: false, next_savepoint: 0, savepoints: vec![] })),
                savepoint: None,
                done: AtomicBool::new(false),
                expiry,
            });
            let (started, ready) = tokio::sync::oneshot::channel();
            let (rolled_back, finished) = tokio::sync::oneshot::channel();
            let successful_outer = outer.clone();
            let successful = tokio::spawn(async move {
                let tx = successful_outer.begin_savepoint().await.unwrap();
                tx.batch("INSERT INTO sibling_savepoints VALUES (1)".into()).await.unwrap();
                started.send(()).unwrap();
                // PostgreSQL savepoints must unwind in stack order, even in child tasks.
                finished.await.unwrap();
                tx.commit().await.unwrap();
            });
            let failing_outer = outer.clone();
            let failing = tokio::spawn(async move {
                ready.await.unwrap();
                let e = failing_outer.begin_savepoint().await.err().expect("overlapping sibling must fail");
                assert_eq!(e.kind, ErrorKind::Query);
                assert!(e.message.starts_with("concurrent nested transactions"));
                rolled_back.send(()).unwrap();
            });
            successful.await.unwrap();
            failing.await.unwrap();
            assert_eq!(outer.query_text("SELECT id FROM sibling_savepoints ORDER BY id".into()).await.unwrap(), vec![vec![Some("1".into())]]);
            outer.commit().await.unwrap();
            assert_eq!(db.query_text("SELECT id FROM sibling_savepoints ORDER BY id".into()).await.unwrap(), vec![vec![Some("1".into())]]);
            db.batch("DROP TABLE sibling_savepoints".into()).await.unwrap();
        });
    }

    #[test]
    fn bare_transaction_commit_detects_server_abort() {
        runtime().block_on(async {
            let db = driver().await;
            db.batch("CREATE TABLE IF NOT EXISTS tx_probe (id INTEGER PRIMARY KEY); TRUNCATE tx_probe".into()).await.unwrap();
            // PgDriver::begin returns a bare PgTx, with no Guarded failed mark.
            let tx = db.begin().await.unwrap();
            tx.batch("INSERT INTO tx_probe VALUES (1)".into()).await.unwrap();
            let first = tx.batch("INSERT INTO tx_probe VALUES (1)".into()).await.unwrap_err();
            assert_eq!(first.kind, ErrorKind::Integrity);
            let e = tx.commit().await.unwrap_err();
            assert_eq!(e.kind, ErrorKind::TransactionAborted);
            assert_eq!(e.message, "the database aborted the transaction before COMMIT");
            assert!(e.cause.is_none());
            assert_eq!(db.query_text("SELECT count(*) FROM tx_probe".into()).await.unwrap()[0][0].as_deref(), Some("0"));
            db.batch("DROP TABLE tx_probe".into()).await.unwrap();
        });
    }

    #[test]
    fn failed_rollback_detaches_connection_from_pool() {
        runtime().block_on(async {
            let db = driver().await;
            let obj = get_client(&db.pool).await.unwrap();
            obj.batch_execute("BEGIN").await.unwrap();
            let pid: i32 = obj.query_one("SELECT pg_backend_pid()", &[]).await.unwrap().get(0);
            let expiry = Arc::new(Expiry { fate: Fate::new(), cancel: obj.cancel_token(), tls: None });
            let tx = PgTx {
                session: Arc::new(Mutex::new(Session { client: Some(obj), finished: false, next_savepoint: 0, savepoints: vec![] })),
                savepoint: None,
                done: AtomicBool::new(false),
                expiry,
            };
            let killer = driver().await;
            killer.batch(format!("SELECT pg_terminate_backend({pid})")).await.unwrap();
            assert!(tx.commit().await.is_err());
            // Even a closed connection must be detached before pool recycling.
            assert_eq!(db.pool.status().available, 0);
            assert_eq!(db.pool.status().size, 0);
            db.batch("SELECT 1".into()).await.unwrap();
        });
    }

    fn key(sql: &str) -> (String, Vec<Type>) {
        (sql.to_owned(), vec![Type::INT8])
    }

    fn touch(lru: &mut StatementLru, sql: &str) -> Vec<String> {
        let (sql, types) = key(sql);
        lru.touch(&sql, &types).into_iter().map(|k| k.sql.into_owned()).collect()
    }

    #[test]
    fn lru_evicts_the_least_recently_used_statement() {
        let mut lru = StatementLru::new(3);
        assert!(touch(&mut lru, "a").is_empty());
        assert!(touch(&mut lru, "b").is_empty());
        assert!(touch(&mut lru, "c").is_empty());
        assert!(touch(&mut lru, "a").is_empty()); // a is now newer than b
        assert_eq!(touch(&mut lru, "d"), ["b"]);
        assert_eq!(touch(&mut lru, "e"), ["c"]);
        assert_eq!(lru.used.len(), 3);
        // The same SQL with other parameter types is another statement.
        assert_eq!(lru.touch("a", &[Type::TEXT]).len(), 1);
    }

    #[test]
    fn lru_forget_removes_a_statement_from_the_order() {
        let mut lru = StatementLru::new(2);
        touch(&mut lru, "a");
        touch(&mut lru, "b");
        let (sql, types) = key("a");
        lru.forget(&sql, &types);
        assert!(touch(&mut lru, "c").is_empty()); // room again: a left the order
        assert_eq!(lru.used.len(), 2);
    }

    #[test]
    fn eviction_keeps_hot_statements_prepared() {
        runtime().block_on(async {
            let url = test_url();
            let db = PgDriver::connect_with_cache(&url, 1, 3, vec![]).await.unwrap(); // one connection: one cache
            let run = |n: &str| db.query(format!("SELECT {n}::int8 AS monorm_lru_{n}"), vec![]);
            let prepared = || async {
                let rows = db
                    .query_text("SELECT name, statement FROM pg_prepared_statements WHERE statement LIKE '%monorm_lru_%'".into())
                    .await
                    .unwrap();
                rows.into_iter().map(|r| (r[0].clone().unwrap(), r[1].clone().unwrap())).collect::<Vec<_>>()
            };
            run("0").await.unwrap(); // the hot statement
            let hot_name = prepared().await[0].0.clone();
            for n in 1..=6 {
                run(&n.to_string()).await.unwrap(); // a one-shot statement
                run("0").await.unwrap();
            }
            let after = prepared().await;
            assert_eq!(after.len(), 3, "the cache holds the bound: {after:?}");
            assert!(after.iter().any(|(name, _)| *name == hot_name), "the hot statement stayed prepared: {after:?}");
            assert!(!after.iter().any(|(_, sql)| sql.contains("monorm_lru_1")), "an old one-shot statement was closed: {after:?}");
        });
    }
    // -- pgbouncer=true ---------------------------------------------------------------------

    #[test]
    fn the_pgbouncer_flag_is_read_and_removed_from_the_url() {
        let parse = |q: &str| tls::parse(&format!("postgres://u@h/db{q}")).map(|(_, _, flag)| flag);
        assert!(!parse("").unwrap());
        assert!(parse("?pgbouncer=true").unwrap());
        assert!(parse("?sslmode=disable&pgbouncer=true").unwrap());
        assert!(!parse("?pgbouncer=false").unwrap());
        let e = parse("?pgbouncer=yes").unwrap_err();
        assert!(e.message.contains("pgbouncer must be true or false"), "{}", e.message);
    }

    #[cfg(feature = "sqlite")]
    #[test]
    fn a_sqlite_url_refuses_the_pgbouncer_flag() {
        runtime().block_on(async {
            for url in ["sqlite://:memory:?pgbouncer=true", "sqlite://:memory:?pgbouncer=false"] {
                let e = crate::db::connect(url, 1).await.err().expect("a sqlite URL has no pgbouncer flag");
                assert!(e.message.contains("PostgreSQL URL flag"), "{}", e.message);
            }
        });
    }

    #[test]
    fn a_session_lock_is_refused_before_any_sql() {
        runtime().block_on(async {
            let db = PgDriver::connect(&flagged(&test_url()), 1).await.unwrap();
            let e = db.session_lock(1, true, false, None).await.err().expect("refused");
            assert!(e.message.contains("pgbouncer=true") && e.message.contains("MONORM_DATABASE_URL"), "{}", e.message);
        });
    }

    #[test]
    fn a_no_transaction_migration_names_its_step_when_the_pooler_refuses_the_lock() {
        runtime().block_on(async {
            let path = std::env::temp_dir().join(format!("monorm_pgb_{}", std::process::id()));
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("up.sql"), "-- monorm: no-transaction\nSELECT 1").unwrap();
            let m = crate::migrate::Migration { name: "0001_concurrent".into(), path: path.clone() };
            let db = PgDriver::connect(&flagged(&test_url()), 1).await.unwrap();
            let e = crate::migrate::apply_no_transaction(&db, &m, true).await.unwrap_err().to_string();
            std::fs::remove_dir_all(&path).unwrap();
            assert!(e.contains("0001_concurrent runs outside a transaction and needs a session lock"), "{e}");
            assert!(e.contains("pgbouncer=true") && e.contains("MONORM_DATABASE_URL"), "{e}");
        });
    }

    /// The URL of a PgBouncer in transaction mode (CI sets it); the tests below skip without it.
    fn pooler_url() -> Option<String> {
        std::env::var("MONORM_PGBOUNCER_URL").ok()
    }

    /// Statements that differ in text, so the client keeps preparing new ones.
    fn workload(i: i64) -> (String, Vec<Value>) {
        (format!("SELECT $1::int8 + {i} AS n"), vec![bigint(i)])
    }

    #[test]
    fn behind_pooler_the_flag_survives_server_switches() {
        let Some(url) = pooler_url() else { return };
        runtime().block_on(async {
            let direct = driver().await;
            direct
                .batch(
                    "DROP TABLE IF EXISTS monorm_pool_t; DROP TYPE IF EXISTS monorm_pool_role; \
                     CREATE TYPE monorm_pool_role AS ENUM ('a', 'b'); \
                     CREATE TABLE monorm_pool_t (id int8 PRIMARY KEY, role monorm_pool_role NOT NULL)"
                        .into(),
                )
                .await
                .unwrap();
            // Two clients at the same time: the only way to see a name that another client holds (`42P05`).
            let work = |client: i64| {
                let url = flagged(&url);
                async move {
                    let db = PgDriver::connect_with_types(&url, 2, vec!["\"monorm_pool_role\"".into()]).await.unwrap();
                    let zone = db.query_text("SHOW TimeZone".into()).await.unwrap();
                    assert_eq!(zone[0][0].as_deref(), Some("UTC"));
                    for i in 0..100 {
                        let id = client * 1000 + i;
                        let (sql, args) = workload(i);
                        assert_eq!(db.query(sql, args).await.unwrap().len(), 1);
                        let insert = "INSERT INTO monorm_pool_t (id, role) VALUES ($1, CAST($2 AS monorm_pool_role))";
                        assert_eq!(db.execute(insert.into(), vec![bigint(id), "a".into()]).await.unwrap(), 1);
                        let copied = db.copy_in("monorm_pool_t".into(), vec!["id".into(), "role".into()], vec![vec![bigint(id + 500), "b".into()]]).await;
                        assert_eq!(copied.unwrap(), 1);
                        let tx = db.begin().await.unwrap();
                        tx.query("SELECT role FROM monorm_pool_t WHERE id = $1".into(), vec![bigint(id)]).await.unwrap();
                        tx.commit().await.unwrap();
                    }
                    let e = db.session_lock(1, true, false, None).await.err().expect("refused");
                    assert!(e.message.contains("pgbouncer=true"), "{}", e.message);
                    db.close().await;
                }
            };
            tokio::join!(work(1), work(2));
            let count = direct.query_text("SELECT count(*) FROM monorm_pool_t".into()).await.unwrap();
            assert_eq!(count[0][0].as_deref(), Some("400"));
            direct.batch("DROP TABLE monorm_pool_t; DROP TYPE monorm_pool_role".into()).await.unwrap();
        });
    }

    #[test]
    fn behind_pooler_without_the_flag_a_statement_name_is_lost() {
        let Some(url) = pooler_url() else { return };
        runtime().block_on(async {
            let db = PgDriver::connect(&url, 4).await.unwrap();
            let mut lost = None;
            for round in 0..20 {
                for i in 0..20 {
                    let (sql, args) = workload(i);
                    if let Err(e) = db.query(sql, args).await {
                        lost = Some((round, e));
                    }
                }
                if lost.is_some() {
                    break;
                }
            }
            let (_, e) = lost.expect("the cache must lose a statement name when the pooler switches servers");
            assert!(matches!(e.sqlstate.as_deref(), Some("26000" | "42P05")), "{e:?}");
        });
    }

    #[test]
    fn a_lost_statement_name_becomes_a_clear_error_and_retires_the_connection() {
        runtime().block_on(async {
            let db = PgDriver::connect(&flagged(&test_url()), 1).await.unwrap();
            let pid = || async { db.query_text("SELECT pg_backend_pid()".into()).await.unwrap()[0][0].clone().unwrap() };
            let before = pid().await;
            let c = get_client(&db.pool).await.unwrap();
            let e = c.simple_query("EXECUTE monorm_s7").await.unwrap_err();
            let e = pooler_err(Some(&c), e);
            drop(c);
            assert_eq!(e.sqlstate.as_deref(), Some("26000"));
            assert!(e.message.contains("\"monorm_s7\"") && e.message.contains("pgbouncer mode") && !e.message.contains("typeinfo"), "{}", e.message);
            assert_ne!(pid().await, before, "the pool made a new connection");
        });
    }

    fn bigint(n: i64) -> Value {
        n.into()
    }

    #[test]
    fn pgbouncer_mode_leaves_no_prepared_statement() {
        runtime().block_on(async {
            let direct = driver().await;
            direct
                .batch(
                    "DROP TABLE IF EXISTS monorm_pgb; DROP TYPE IF EXISTS monorm_pgb_role; DROP TYPE IF EXISTS monorm_pgb_late; \
                     CREATE TYPE monorm_pgb_role AS ENUM ('a', 'b'); \
                     CREATE TABLE monorm_pgb (id int8 PRIMARY KEY, role monorm_pgb_role NOT NULL, name text)"
                        .into(),
                )
                .await
                .unwrap();
            // The enum is listed, so its first read needs no statement of its own.
            let db = PgDriver::connect_with_types(&flagged(&test_url()), 1, vec!["\"monorm_pgb_role\"".into(), "\"monorm_pgb_role\"[]".into(), "monorm_pgb_missing".into()])
                .await
                .unwrap();
            let none = || async {
                let rows = db.query_text("SELECT name, statement FROM pg_prepared_statements".into()).await.unwrap();
                assert!(rows.is_empty(), "a statement is still prepared: {rows:?}");
            };
            none().await; // the warm-up closed the lookups it made
            let n = |sql: &str, args: Vec<Value>| db.query(sql.to_owned(), args);

            n("SELECT $1::int8 AS n", vec![bigint(1)]).await.unwrap();
            none().await;
            // The same text again and again is what `.prepare()` does in a binding.
            for i in 0..3 {
                n("SELECT $1::int8 AS n", vec![bigint(i)]).await.unwrap();
                db.execute("UPDATE monorm_pgb SET name = $1 WHERE id = $2".into(), vec!["q".into(), bigint(i)]).await.unwrap();
            }
            none().await;
            let insert = "INSERT INTO monorm_pgb (id, role, name) VALUES ($1, CAST($2 AS monorm_pgb_role), $3)";
            assert_eq!(db.execute(insert.into(), vec![bigint(1), "a".into(), "x".into()]).await.unwrap(), 1);
            none().await;
            let raw = db.fetch("SELECT id, role, name FROM monorm_pgb ORDER BY id".into(), vec![]).await.unwrap();
            assert_eq!((raw.columns().to_vec(), raw.len()), (vec!["id".to_owned(), "role".into(), "name".into()], 1));
            assert!(matches!(raw.cell(0, 1).unwrap(), Cell::Text("a")));
            none().await;
            let raw = db.fetch("SELECT id, role FROM monorm_pgb WHERE id < $1".into(), vec![bigint(0)]).await.unwrap();
            assert_eq!((raw.len(), raw.columns().len()), (0, 0)); // no row, so no column list
            none().await;
            let rows = vec![vec![bigint(2), "b".into(), "y".into()], vec![bigint(3), "a".into(), Value::String(None)]];
            assert_eq!(db.copy_in("monorm_pgb".into(), vec!["id".into(), "role".into(), "name".into()], rows).await.unwrap(), 2);
            none().await;
            let tx = db.begin().await.unwrap();
            tx.execute(insert.into(), vec![bigint(4), "b".into(), "z".into()]).await.unwrap();
            tx.copy_in("monorm_pgb".into(), vec!["id".into(), "role".into()], vec![vec![bigint(5), "a".into()]]).await.unwrap();
            assert_eq!(tx.query("SELECT id FROM monorm_pgb".into(), vec![]).await.unwrap().len(), 5);
            tx.commit().await.unwrap();
            none().await;
            let count = db.query_text("SELECT count(*) FROM monorm_pgb".into()).await.unwrap();
            assert_eq!(count[0][0].as_deref(), Some("5"));

            // A failed copy ends its transaction: nothing stays open or prepared.
            let dup = vec![vec![bigint(1), "a".into(), "again".into()]];
            let e = db.copy_in("monorm_pgb".into(), vec!["id".into(), "role".into(), "name".into()], dup).await.unwrap_err();
            assert_eq!(e.kind, ErrorKind::Integrity);
            none().await;
            let status = db.query_text("SELECT count(*) FROM pg_stat_activity WHERE state LIKE 'idle in transaction%' AND datname = current_database()".into()).await.unwrap();
            assert_eq!(status[0][0].as_deref(), Some("0"));

            // The warm-up cached the client's lookup statement and then deallocated it, as a pooler
            // would. A type the schema did not list needs a lookup, so it fails with a clear error
            // and the pool replaces the connection.
            direct.batch("CREATE TYPE monorm_pgb_late AS ENUM ('q')".into()).await.unwrap();
            let before = db.query_text("SELECT pg_backend_pid()".into()).await.unwrap();
            let e = db.query("SELECT 'q'::monorm_pgb_late".into(), vec![]).await.err().expect("the lookup statement is gone");
            assert!(matches!(e.sqlstate.as_deref(), Some("26000" | "42P05")), "{e:?}");
            assert!(e.message.contains("pgbouncer mode") && !e.message.contains("typeinfo"), "{}", e.message);
            assert_ne!(db.query_text("SELECT pg_backend_pid()".into()).await.unwrap(), before);
            direct.batch("DROP TABLE monorm_pgb; DROP TYPE monorm_pgb_role; DROP TYPE monorm_pgb_late".into()).await.unwrap();
        });
    }
}
