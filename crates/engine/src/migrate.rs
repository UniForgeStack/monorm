//! Applying and reverting the migrations of a directory (`monorm_core::migrate::files`) on
//! a database. The bindings and the `monorm` CLI all run this one implementation.
//!
//! Applied migrations are recorded with the SHA-256 of their `up.sql` in the
//! `monorm_migrations` table; a migration whose file changed after it was applied stops
//! [`upgrade`]. Each migration runs in its own transaction together with its row, under
//! an advisory lock, so a failing migration leaves the database at the previous one and
//! concurrent migrators apply each migration once.
//!
//! A migration whose script starts with [`NO_TX_HEADER`] holds one statement that cannot
//! run in a transaction (`CREATE INDEX CONCURRENTLY`). [`apply_no_transaction`] runs it
//! on PostgreSQL under a session lock, then records it in a short transaction of its own.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use monorm_core::dialect::Dialect;
use monorm_core::migrate::files::{self, MigrationFolder};
use monorm_core::migrate::pg::ident;
use monorm_core::migrate::Destructive;

use crate::db::{Driver, Executor, SessionLock, Transaction};
use crate::error::{Error, Result};

pub const TABLE: &str = "monorm_migrations";
/// Arbitrary constant: the advisory lock serializing concurrent migrators.
pub const LOCK_ID: i64 = 0x6F72_6D6D;

/// One migration folder.
#[derive(Debug, Clone)]
pub struct Migration {
    pub name: String,
    pub path: PathBuf,
}

impl Migration {
    fn read(&self, file: &str) -> Result<String> {
        let p = self.path.join(file);
        std::fs::read_to_string(&p).map_err(|e| Error::Migration(format!("{}: {e}", p.display())))
    }

    pub fn up_sql(&self) -> Result<String> {
        self.read("up.sql")
    }

    pub fn down_sql(&self) -> Result<String> {
        self.read("down.sql")
    }

    /// The no-transaction header of `up.sql`, if it has one.
    pub fn no_transaction(&self) -> Result<Option<NoTx>> {
        NoTx::parse(&self.up_sql()?).map_err(|e| Error::Migration(format!("{}/up.sql: {e}", self.name)))
    }

    /// The (name, checksum) of each migration this one replaces: empty unless it is a squash folder.
    pub fn replaces(&self) -> Result<Vec<(String, String)>> {
        files::parse_replaces(&self.up_sql()?).map_err(|e| Error::Migration(format!("{}/up.sql: {e}", self.name)))
    }

    /// Hex SHA-256 of `up.sql`.
    pub fn checksum(&self) -> Result<String> {
        Ok(checksum(&self.up_sql()?))
    }
}

pub use files::{checksum, DATA_FILES, NO_TX_HEADER, REPLACES_HEADER};

/// The index a no-transaction script builds or drops.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoTxIndex {
    Create(String),
    Drop(String),
}

/// A parsed [`NO_TX_HEADER`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoTx {
    pub index: Option<NoTxIndex>,
}

impl NoTx {
    /// The header of `sql`: `None` for a script that runs in a transaction.
    pub fn parse(sql: &str) -> std::result::Result<Option<NoTx>, String> {
        let first = sql.lines().next().unwrap_or("").trim_end();
        let Some(rest) = first.strip_prefix(NO_TX_HEADER) else { return Ok(None) };
        if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
            return Ok(None);
        }
        let rest = rest.trim();
        let (word, name) = rest.split_once(char::is_whitespace).map_or((rest, ""), |(w, n)| (w, n.trim()));
        let index = match (word, name) {
            ("", _) => None,
            ("create-index", n) if !n.is_empty() => Some(NoTxIndex::Create(n.to_owned())),
            ("drop-index", n) if !n.is_empty() => Some(NoTxIndex::Drop(n.to_owned())),
            _ => return Err(format!("unknown no-transaction header `{}`; expected `{NO_TX_HEADER} [create-index <name> | drop-index <name>]`", first.trim())),
        };
        Ok(Some(NoTx { index }))
    }
}

impl From<MigrationFolder> for Migration {
    fn from(f: MigrationFolder) -> Self {
        Migration { name: f.name, path: f.path }
    }
}

/// The migrations of `dir`, in order.
pub fn list(dir: &Path) -> Result<Vec<Migration>> {
    Ok(files::list(dir).map_err(Error::Migration)?.into_iter().map(Migration::from).collect())
}

/// A migration by folder name or number (`"3"`, `"0003"`, `"0003_add_tags"`).
pub fn find(dir: &Path, name: &str) -> Result<Migration> {
    files::find(dir, name).map(Migration::from).map_err(Error::Migration)
}

/// Folds migrations 1..=`to` into one folder (see `files::squash`); no database is read.
pub fn squash(dir: &Path, to: &str, name: Option<&str>) -> Result<Migration> {
    files::squash(dir, to, name).map(Migration::from).map_err(Error::Migration)
}

#[derive(Debug, Clone)]
pub struct Status {
    pub migration: Migration,
    pub applied: bool,
    /// When it was applied, as Postgres prints a `timestamptz`.
    pub applied_at: Option<String>,
}

/// What [`downgrade`] reverts.
#[derive(Debug, Clone)]
pub enum Down {
    /// The last `n` applied migrations.
    Steps(usize),
    /// Every migration after this one; `"zero"` reverts all.
    To(String),
}

fn lit(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

const ALLOW_HINT: &str = "pass --allow-destructive (Python: allow_destructive=True, TypeScript: { allowDestructive: true })";

/// One cell of a one-row query as a flag: `1` is true. A missing row is false.
async fn flag(conn: &dyn Executor, sql: String) -> Result<bool> {
    let rows = conn.query_text(sql).await?;
    Ok(rows.into_iter().next().and_then(|r| r.into_iter().next().flatten()).is_some_and(|v| v == "1"))
}

/// Whether `table` exists. A missing table counts as empty, so a replay on a fresh
/// database needs no flag.
async fn exists(conn: &dyn Executor, table: &str) -> Result<bool> {
    let sql = match conn.dialect() {
        Dialect::Postgres => format!("SELECT CAST(to_regclass({}) IS NOT NULL AS integer)", lit(&ident(table))),
        Dialect::Sqlite => format!("SELECT CAST(EXISTS (SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = {}) AS integer)", lit(table)),
    };
    flag(conn, sql).await
}

/// Refuses `sql` when a step it marks as destructive touches a table with rows.
/// `what` starts the error text: the migration, and for a rollback its direction.
/// `in_tx` is true for a migration transaction: on PostgreSQL the check then locks the
/// table first, as the step itself will, so a write that commits later cannot slip past.
/// SQLite holds the write lock for the whole transaction already.
async fn gate(conn: &dyn Executor, what: &str, sql: &str, in_tx: bool) -> Result<()> {
    let mut seen: Vec<String> = vec![];
    for d in sql.lines().filter_map(Destructive::parse_marker) {
        if seen.contains(&d.table) {
            continue;
        }
        if exists(conn, &d.table).await? {
            if in_tx && conn.dialect() == Dialect::Postgres {
                conn.batch(format!("LOCK TABLE {} IN ACCESS EXCLUSIVE MODE", ident(&d.table))).await?;
            }
            if flag(conn, format!("SELECT CAST(EXISTS (SELECT 1 FROM {}) AS integer)", ident(&d.table))).await? {
                return Err(Error::Migration(format!("{what} {}, and {} has rows; {ALLOW_HINT}", d.describe(), d.table)));
            }
        }
        seen.push(d.table);
    }
    Ok(())
}

async fn ensure_table(conn: &dyn Executor) -> Result<()> {
    let (timestamp, flag) = match conn.dialect() {
        Dialect::Postgres => ("timestamptz NOT NULL DEFAULT now()", "boolean NOT NULL DEFAULT false"),
        Dialect::Sqlite => ("text NOT NULL DEFAULT CURRENT_TIMESTAMP", "integer NOT NULL DEFAULT 0"),
    };
    conn.batch(format!(
        "CREATE TABLE IF NOT EXISTS {TABLE} (name text PRIMARY KEY, checksum text NOT NULL, applied_at {timestamp}, baseline {flag})"
    ))
    .await?;
    // A table made by an older version lacks `baseline`; add it once, under the migration lock.
    let has = match conn.dialect() {
        Dialect::Postgres => format!(
            "SELECT 1 FROM pg_attribute WHERE attrelid = to_regclass({}) AND attname = 'baseline' AND NOT attisdropped",
            lit(TABLE)
        ),
        Dialect::Sqlite => format!("SELECT 1 FROM pragma_table_info({}) WHERE name = 'baseline'", lit(TABLE)),
    };
    if conn.query_text(has).await?.is_empty() {
        let add = format!("ALTER TABLE {TABLE} ADD COLUMN {}baseline {flag}", if conn.dialect() == Dialect::Postgres { "IF NOT EXISTS " } else { "" });
        locked(conn, None, |_| Some(["SELECT 1".to_owned(), add])).await?;
    }
    Ok(())
}

/// Applied migrations that `baseline` recorded without running them.
async fn baselined(conn: &dyn Executor) -> Result<Vec<String>> {
    let rows = conn.query_text(format!("SELECT name FROM {TABLE} WHERE baseline ORDER BY name")).await?;
    Ok(rows.into_iter().filter_map(|r| r.into_iter().next().flatten()).collect())
}

/// name → (checksum, applied_at)
async fn applied(conn: &dyn Executor) -> Result<BTreeMap<String, (String, String)>> {
    let rows = conn.query_text(format!("SELECT name, checksum, CAST(applied_at AS text) FROM {TABLE} ORDER BY name")).await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            let mut it = r.into_iter();
            match (it.next().flatten(), it.next().flatten(), it.next().flatten()) {
                (Some(n), Some(c), Some(a)) => Some((n, (c, a))),
                _ => None,
            }
        })
        .collect())
}

/// Every migration of `dir` and whether it is applied. Migrations recorded in the
/// database but missing from the directory are an error.
pub async fn status(conn: &dyn Executor, dir: &Path) -> Result<Vec<Status>> {
    for m in list(dir)? {
        let snapshot = m.path.join("snapshot.json");
        if snapshot.exists() {
            let text = std::fs::read_to_string(&snapshot).map_err(|e| Error::Migration(e.to_string()))?;
            let target = monorm_core::migrate::parse_snapshot(&text).map_err(Error::Migration)?.dialect;
            if target != conn.dialect() {
                return Err(Error::Migration(format!("{} targets {}, connection uses {}", m.name, target.name(), conn.dialect().name())));
            }
        }
    }
    ensure_table(conn).await?;
    let mut done = applied(conn).await?;
    let mut out: Vec<Status> = vec![];
    for m in list(dir)? {
        let replaces = m.replaces()?;
        let mut at = None;
        if replaces.is_empty() {
            at = done.remove(&m.name).map(|(_, at)| at);
        } else {
            match squash_state(&m, &replaces, &done) {
                Squash::Own => at = done.remove(&m.name).map(|(_, at)| at),
                Squash::Fresh => {}
                Squash::Replaced => at = replaces.iter().filter_map(|(r, _)| done.remove(r)).map(|(_, at)| at).max(),
                Squash::Partial(n) => {
                    return Err(Error::Migration(format!(
                        "the database applied {n} of the {} migrations that {} replaces; migrate it with a release before the squash",
                        replaces.len(),
                        m.name
                    )))
                }
            }
        }
        out.push(Status { applied: at.is_some(), applied_at: at, migration: m });
    }
    if !done.is_empty() {
        let names: Vec<&str> = done.keys().map(String::as_str).collect();
        return Err(Error::Migration(format!(
            "the database has migrations missing from the directory: {}",
            names.join(", ")
        )));
    }
    Ok(out)
}

/// How the database stands to a squash folder (one with `replaces` lines).
#[derive(Debug, PartialEq, Eq)]
enum Squash {
    /// The squash folder itself is recorded (it ran, or the database adopted it).
    Own,
    /// All the replaced migrations are recorded and the squash folder is not: adoption is due.
    Replaced,
    Fresh,
    /// Only this many of the replaced migrations are recorded.
    Partial(usize),
}

/// A squash named like one of the migrations it replaces shares a row with it: the checksum of the replaced
/// migration (from its `replaces` line) tells the two apart.
fn squash_state(m: &Migration, replaces: &[(String, String)], done: &BTreeMap<String, (String, String)>) -> Squash {
    if let Some((sum, _)) = done.get(&m.name) {
        // A row that is not the replaced migration of this name is the squash row, even after its `up.sql` changed.
        if replaces.iter().find(|(r, _)| *r == m.name).is_none_or(|(_, replaced)| sum != replaced) {
            return Squash::Own;
        }
    }
    match replaces.iter().filter(|(r, _)| done.contains_key(r)).count() {
        0 => Squash::Fresh,
        n if n == replaces.len() => Squash::Replaced,
        n => Squash::Partial(n),
    }
}

/// Applied migrations must be a prefix of the directory and unchanged since. The recorded rows of the
/// migrations that a squash folder replaces must still have the checksums of its `replaces` lines.
fn verify(statuses: &[Status], applied: &BTreeMap<String, (String, String)>) -> Result<()> {
    let mut pending: Option<&str> = None;
    for s in statuses {
        if !s.applied {
            pending = pending.or(Some(&s.migration.name));
        } else if let Some(p) = pending {
            return Err(Error::Migration(format!(
                "{} is applied but the earlier {p} is not; renumber the unapplied migration after the applied ones",
                s.migration.name
            )));
        } else {
            let m = &s.migration;
            let replaces = m.replaces()?;
            let sum = m.checksum()?;
            if squash_state(m, &replaces, applied) == Squash::Replaced {
                for (name, expected) in &replaces {
                    if applied[name].0 != *expected {
                        return Err(Error::Migration(format!("{name}/up.sql changed after it was applied (the checksum differs from the replaces line of {})", m.name)));
                    }
                }
            } else if applied[&m.name].0 != sum {
                return Err(Error::Migration(format!("{}/up.sql changed after it was applied", m.name)));
            }
        }
    }
    Ok(())
}

/// For each squash folder whose replaced migrations are all recorded and which is not yet: records the
/// folder and forgets the replaced rows, in one transaction under the migration lock, with no SQL of the
/// folder run. The new row is baselined when any replaced row was. Gives the folders it adopted.
pub async fn adopt(conn: &dyn Executor, dir: &Path) -> Result<Vec<Migration>> {
    let mut done = vec![];
    for m in list(dir)? {
        let replaces = m.replaces()?;
        if replaces.is_empty() {
            continue;
        }
        ensure_table(conn).await?;
        let sum = m.checksum()?;
        let names = replaces.iter().map(|(n, _)| lit(n)).collect::<Vec<_>>().join(", ");
        let any = match conn.dialect() {
            Dialect::Postgres => "COALESCE(bool_or(baseline), false)",
            Dialect::Sqlite => "COALESCE(MAX(baseline), 0)",
        };
        let record = format!(
            "INSERT INTO {TABLE} (name, checksum, baseline) SELECT {}, {}, {any} FROM {TABLE} WHERE name IN ({names}) \
             ON CONFLICT (name) DO UPDATE SET checksum = excluded.checksum",
            lit(&m.name),
            lit(&sum)
        );
        let forget = format!("DELETE FROM {TABLE} WHERE name IN ({names}) AND name <> {}", lit(&m.name));
        let ran = locked(conn, None, |now| (squash_state(&m, &replaces, now) == Squash::Replaced).then_some([record, forget])).await?;
        if ran {
            done.push(m);
        }
    }
    Ok(done)
}

/// [`adopt`] after the checks of [`status`] and [`verify`], so a partly applied database or a changed checksum fails before anything is written.
async fn adopt_if_ready(conn: &dyn Executor, dir: &Path) -> Result<()> {
    if list(dir)?.iter().any(|m| m.replaces().is_ok_and(|r| !r.is_empty())) {
        let statuses = status(conn, dir).await?;
        verify(&statuses, &applied(conn).await?)?;
        adopt(conn, dir).await?;
    }
    Ok(())
}

/// In one transaction holding the migration lock: runs the scripts `scripts` gives for
/// the migrations applied by then (none: nothing to do), and commits. With `gated`
/// (error text start, SQL), the destructive markers of that SQL are checked first, in the
/// transaction.
async fn locked(
    conn: &dyn Executor,
    gated: Option<(&str, &str)>,
    scripts: impl FnOnce(&BTreeMap<String, (String, String)>) -> Option<[String; 2]>,
) -> Result<bool> {
    let tx = conn.begin_migration().await?;
    let run = async {
        if conn.dialect() == Dialect::Postgres {
            take_xact_lock(&*tx).await?;
        }
        let Some(scripts) = scripts(&applied(&*tx).await?) else { return Ok(false) };
        if let Some((what, sql)) = gated {
            gate(&*tx, what, sql, true).await?;
        }
        for sql in scripts {
            tx.batch(sql).await?;
        }
        Ok::<_, Error>(true)
    };
    match run.await {
        Ok(ran) => {
            tx.commit().await?;
            Ok(ran)
        }
        Err(e) => {
            let _ = Transaction::rollback(&*tx).await;
            Err(e)
        }
    }
}

impl Migration {
    /// The data step (`data.py` / `data.ts`) of this migration, if it has one.
    pub fn data_file(&self) -> Option<&'static str> {
        DATA_FILES.into_iter().find(|f| self.path.join(f).is_file())
    }
}

/// The migrations [`upgrade`] would apply, in order: pending ones up to and including
/// `target` (default: all), after the same checks. Unless `allow_destructive`, a pending
/// migration that marks a step destructive on a table with rows is refused. This check
/// holds no lock: [`begin_apply`] checks again in its transaction. A no-transaction
/// migration (see [`apply_no_transaction`]) with a data step, or on SQLite, is refused.
pub async fn pending(conn: &dyn Executor, dir: &Path, target: Option<&str>, allow_destructive: bool) -> Result<Vec<Migration>> {
    adopt_if_ready(conn, dir).await?;
    let statuses = status(conn, dir).await?;
    verify(&statuses, &applied(conn).await?)?;
    let stop = target.map(|t| find(dir, t)).transpose()?.map(|m| m.name);
    let todo: Vec<Migration> = statuses
        .into_iter()
        .filter(|s| !s.applied && stop.as_ref().is_none_or(|stop| s.migration.name <= *stop))
        .map(|s| s.migration)
        .collect();
    for m in &todo {
        if m.no_transaction()?.is_none() {
            continue;
        }
        if conn.dialect() != Dialect::Postgres {
            return Err(no_tx_refusal(&m.name, conn.dialect()));
        }
        if conn.pooler_transactions() {
            return Err(pooler_refusal(&m.name));
        }
        if let Some(file) = m.data_file() {
            return Err(Error::Migration(format!("{} is a no-transaction migration and has a data step ({file}); a data step needs a transaction", m.name)));
        }
    }
    if !allow_destructive {
        for m in &todo {
            gate(conn, &m.name, &m.up_sql()?, false).await?;
        }
    }
    Ok(todo)
}

/// Starts applying `m`: a migration transaction holding the migration lock, with
/// `up.sql` run in it. `None` when another migrator applied `m` meanwhile. The caller
/// runs the data step in the transaction, then [`finish_apply`] (or rolls back).
/// Unless `allow_destructive`, a destructive step on a table with rows is refused here,
/// after the lock, so a row inserted since [`pending`] still counts.
pub async fn begin_apply(conn: &dyn Executor, m: &Migration, allow_destructive: bool) -> Result<Option<Arc<dyn Transaction>>> {
    let up = m.up_sql()?;
    let tx = conn.begin_migration().await?;
    let run = async {
        if conn.dialect() == Dialect::Postgres {
            take_xact_lock(&*tx).await?;
        }
        if applied(&*tx).await?.contains_key(&m.name) {
            return Ok(false);
        }
        if !allow_destructive {
            gate(&*tx, &m.name, &up, true).await?;
        }
        tx.batch(up).await?;
        Ok::<_, Error>(true)
    };
    match run.await {
        Ok(true) => Ok(Some(tx)),
        Ok(false) => {
            let _ = Transaction::rollback(&*tx).await;
            Ok(None)
        }
        Err(e) => {
            let _ = Transaction::rollback(&*tx).await;
            Err(e)
        }
    }
}

/// Records `m` in the transaction of [`begin_apply`] and commits; rolls back on error.
pub async fn finish_apply(tx: &dyn Transaction, m: &Migration) -> Result<()> {
    let record = format!("INSERT INTO {TABLE} (name, checksum) VALUES ({}, {})", lit(&m.name), lit(&m.checksum()?));
    match tx.batch(record).await {
        Ok(_) => Ok(tx.commit().await?),
        Err(e) => {
            let _ = tx.rollback().await;
            Err(e.into())
        }
    }
}

/// The refusal of a no-transaction script on a database that has no such mode.
fn no_tx_refusal(what: &str, dialect: Dialect) -> Error {
    Error::Migration(format!("{what} is a no-transaction migration; {} has no such mode", dialect.name()))
}

/// The refusal of a no-transaction script behind a pooler: its session lock needs a direct connection.
fn pooler_refusal(what: &str) -> Error {
    Error::Migration(format!(
        "{what} runs outside a transaction and needs a session lock, which needs {}",
        crate::db::POOLER_LOCK_REFUSAL
    ))
}

/// An index name of a header as SQL: a quoted name stays as written.
fn index_ref(name: &str) -> String {
    if name.starts_with('"') { name.to_owned() } else { ident(name) }
}

/// Runs `sql` (one statement, outside a transaction) and then `record`, under the session
/// lock of the migration lock. The run happens only when `m` is recorded as applied exactly
/// when `expect_applied` says so; otherwise it gives `false` and runs nothing.
struct NoTxRun<'a> {
    /// Starts the error text of the destructive check.
    what: &'a str,
    sql: String,
    index: Option<NoTxIndex>,
    record: String,
    expect_applied: bool,
}

/// Takes the transaction lock of the migration lock by polling, never by a blocked
/// `pg_advisory_xact_lock`: a blocked statement holds a snapshot, and a no-transaction
/// `CREATE INDEX CONCURRENTLY` of the session-lock holder waits for it (`deadlock detected`).
async fn take_xact_lock(tx: &dyn Executor) -> Result<()> {
    loop {
        let rows = tx.query_text(format!("SELECT CAST(pg_try_advisory_xact_lock({LOCK_ID}) AS text)")).await?;
        if rows.first().and_then(|r| r.first()).and_then(|v| v.as_deref()) == Some("true") {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// Takes the migration lock by polling, never by a blocked `pg_advisory_lock`: a blocked
/// statement holds a snapshot, and the `CREATE INDEX CONCURRENTLY` of the holder waits for it.
async fn take_migration_lock(driver: &dyn Driver) -> Result<Arc<dyn SessionLock>> {
    loop {
        if let Some(lock) = driver.session_lock(LOCK_ID, true, true, None).await? {
            return Ok(lock);
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

async fn run_no_transaction(driver: &dyn Driver, m: &Migration, step: NoTxRun<'_>, allow_destructive: bool) -> Result<bool> {
    let NoTxRun { what, sql, index, record, expect_applied } = step;
    if driver.dialect() != Dialect::Postgres {
        return Err(no_tx_refusal(&m.name, driver.dialect()));
    }
    if driver.pooler_transactions() {
        return Err(pooler_refusal(&m.name));
    }
    // Before the lock: it may take the transaction lock, which the session lock would block.
    ensure_table(driver).await?;
    let lock = take_migration_lock(driver).await?;
    let run = async {
        if applied(driver).await?.contains_key(&m.name) != expect_applied {
            return Ok(false);
        }
        if !allow_destructive {
            gate(driver, what, &sql, false).await?;
        }
        if let Some(NoTxIndex::Create(name)) = &index {
            // A failed concurrent build leaves an INVALID index, and IF NOT EXISTS would keep it.
            let target = index_ref(name);
            let valid = driver.query_text(format!("SELECT CAST(indisvalid AS integer) FROM pg_index WHERE indexrelid = to_regclass({})", lit(&target))).await?;
            if valid.into_iter().next().and_then(|r| r.into_iter().next().flatten()).is_some_and(|v| v == "0") {
                driver.batch(format!("DROP INDEX CONCURRENTLY {target}")).await?;
            }
        }
        driver.batch(sql).await?;
        // A statement outside a transaction commits by itself. The transaction lock would wait
        // for this process's own session lock.
        driver.batch(record).await?;
        Ok::<_, Error>(true)
    };
    let out = run.await;
    let released = lock.release().await;
    let ran = out?;
    released?;
    Ok(ran)
}

/// Applies `m`, a migration whose `up.sql` starts with [`NO_TX_HEADER`], on PostgreSQL:
/// under a session lock, the statement runs outside any transaction and the record row goes
/// in a transaction of its own. `false` when another migrator applied `m` meanwhile. A failed
/// `CREATE INDEX CONCURRENTLY` leaves an INVALID index; a rerun drops it before the build.
/// Unless `allow_destructive`, a destructive step on a table with rows is refused.
pub async fn apply_no_transaction(driver: &dyn Driver, m: &Migration, allow_destructive: bool) -> Result<bool> {
    let up = m.up_sql()?;
    let header = NoTx::parse(&up).map_err(|e| Error::Migration(format!("{}/up.sql: {e}", m.name)))?;
    let Some(header) = header else {
        return Err(Error::Migration(format!("{} has no `{NO_TX_HEADER}` header", m.name)));
    };
    let record = format!("INSERT INTO {TABLE} (name, checksum) VALUES ({}, {})", lit(&m.name), lit(&checksum(&up)));
    let step = NoTxRun { what: &m.name, sql: up, index: header.index, record, expect_applied: false };
    run_no_transaction(driver, m, step, allow_destructive).await
}

/// Applies pending migrations up to and including `target` (default: all); gives the
/// ones it applied. A migration with a data step (`data.py`, `data.ts`) is refused
/// before anything runs: the Python or TypeScript migrator applies it.
pub async fn upgrade(conn: &dyn Driver, dir: &Path, target: Option<&str>, allow_destructive: bool) -> Result<Vec<Migration>> {
    let todo = pending(conn, dir, target, allow_destructive).await?;
    if let Some((m, file)) = todo.iter().find_map(|m| m.data_file().map(|f| (m, f))) {
        let tool = if file == "data.py" { "python -m monorm migrate (or Migrator.upgrade())" } else { "npx monorm migrate (or Migrator.upgrade())" };
        return Err(Error::Migration(format!("{} has a data step ({file}); apply it with {tool}", m.name)));
    }
    let mut done = vec![];
    for m in todo {
        if m.no_transaction()?.is_some() {
            if apply_no_transaction(conn, &m, allow_destructive).await? {
                done.push(m);
            }
        } else if let Some(tx) = begin_apply(conn, &m, allow_destructive).await? {
            finish_apply(&*tx, &m).await?;
            done.push(m);
        }
    }
    Ok(done)
}

/// The names of the applied migrations, in order.
pub async fn applied_names(conn: &dyn Executor) -> Result<Vec<String>> {
    ensure_table(conn).await?;
    Ok(applied(conn).await?.into_keys().collect())
}

/// Records the first migration of `dir` as applied without running it, for a database
/// that already has its schema (`monorm pull`). Refused once any migration is applied.
pub async fn baseline(conn: &dyn Executor, dir: &Path) -> Result<Migration> {
    let first = list(dir)?.into_iter().next().ok_or_else(|| Error::Migration(format!("no migration in {}", dir.display())))?;
    ensure_table(conn).await?;
    let record = format!(
        "INSERT INTO {TABLE} (name, checksum, baseline) VALUES ({}, {}, {})",
        lit(&first.name),
        lit(&first.checksum()?),
        if conn.dialect() == Dialect::Postgres { "true" } else { "1" }
    );
    let mut applied = vec![];
    let ran = locked(conn, None, |now| {
        applied = now.keys().cloned().collect();
        now.is_empty().then(|| ["SELECT 1".to_owned(), record])
    })
    .await?;
    if !ran {
        return Err(Error::Migration(format!("the database already has applied migrations ({}); baseline only marks the first", applied.join(", "))));
    }
    Ok(first)
}

/// Reverts applied migrations, newest first; gives the ones it reverted. A baselined
/// migration was never run here, so its `down.sql` would drop live tables: refused
/// unless `force`, before anything is reverted. Unless `allow_destructive`, a `down.sql`
/// that marks a destructive step on a table with rows is refused too: checked first for
/// every migration to revert, then again in the transaction of each.
pub async fn downgrade(conn: &dyn Driver, dir: &Path, down: Down, force: bool, allow_destructive: bool) -> Result<Vec<Migration>> {
    adopt_if_ready(conn, dir).await?;
    let applied: Vec<Migration> = status(conn, dir).await?.into_iter().filter(|s| s.applied).map(|s| s.migration).collect();
    let revert: Vec<Migration> = match down {
        Down::To(t) => {
            let keep = if t == "zero" { String::new() } else { find(dir, &t)?.name };
            applied.into_iter().filter(|m| m.name > keep).collect()
        }
        Down::Steps(n) => {
            let skip = applied.len().saturating_sub(n);
            applied.into_iter().skip(skip).collect()
        }
    };
    if !force {
        let marked = baselined(conn).await?;
        if let Some(m) = revert.iter().find(|m| marked.contains(&m.name)) {
            return Err(Error::Migration(format!(
                "{} was baselined: the database had its schema before, so reverting it would drop live tables; pass --force to revert it anyway",
                m.name
            )));
        }
    }
    let what = |m: &Migration| format!("rolling back {}", m.name);
    if !allow_destructive {
        for m in &revert {
            gate(conn, &what(m), &m.down_sql()?, false).await?;
        }
    }
    let headers = revert
        .iter()
        .map(|m| NoTx::parse(&m.down_sql()?).map_err(|e| Error::Migration(format!("{}/down.sql: {e}", m.name))))
        .collect::<Result<Vec<_>>>()?;
    if conn.dialect() != Dialect::Postgres {
        if let Some((m, _)) = revert.iter().zip(&headers).find(|(_, h)| h.is_some()) {
            return Err(no_tx_refusal(&format!("{}/down.sql", m.name), conn.dialect()));
        }
    }
    let mut done = vec![];
    for (m, header) in revert.into_iter().zip(headers).rev() {
        let scripts = [m.down_sql()?, format!("DELETE FROM {TABLE} WHERE name = {}", lit(&m.name))];
        let label = what(&m);
        if let Some(header) = header {
            let [sql, record] = scripts;
            let step = NoTxRun { what: &label, sql, index: header.index, record, expect_applied: true };
            if run_no_transaction(conn, &m, step, allow_destructive).await? {
                done.push(m);
            }
            continue;
        }
        let gated = (!allow_destructive).then(|| (label.as_str(), scripts[0].as_str()));
        let known = |now: &BTreeMap<String, (String, String)>| now.contains_key(&m.name).then(|| scripts.clone());
        if locked(conn, gated, known).await? {
            done.push(m);
        }
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(sql: &str) -> Option<NoTx> {
        NoTx::parse(sql).unwrap()
    }

    #[test]
    fn header_marks_a_no_transaction_script() {
        assert_eq!(parse("-- monorm: no-transaction\nCREATE INDEX CONCURRENTLY i ON t (a);"), Some(NoTx { index: None }));
        assert_eq!(parse("-- monorm: no-transaction\r\nSELECT 1"), Some(NoTx { index: None }));
    }

    #[test]
    fn header_names_the_index() {
        assert_eq!(parse("-- monorm: no-transaction create-index post_idx\nSELECT 1").unwrap().index, Some(NoTxIndex::Create("post_idx".into())));
        assert_eq!(parse("-- monorm: no-transaction drop-index \"Post Idx\"\nSELECT 1").unwrap().index, Some(NoTxIndex::Drop("\"Post Idx\"".into())));
    }

    #[test]
    fn other_first_lines_run_in_a_transaction() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("CREATE TABLE t (a int);\n-- monorm: no-transaction"), None);
        assert_eq!(parse("-- monorm: no-transactions\nSELECT 1"), None);
        assert_eq!(parse("-- monorm: destructive drop-table t\nDROP TABLE t;"), None);
    }

    #[test]
    fn unknown_header_words_are_an_error() {
        assert!(NoTx::parse("-- monorm: no-transaction create-index").is_err());
        assert!(NoTx::parse("-- monorm: no-transaction vacuum t").is_err());
    }

    #[test]
    fn an_edited_squash_row_with_the_name_of_a_replaced_migration_is_still_the_squash_row() {
        let m = Migration { name: "0001_init".into(), path: PathBuf::new() };
        let replaces = vec![("0001_init".to_owned(), "old".to_owned()), ("0002_b".to_owned(), "b".to_owned())];
        let row = |sum: &str| BTreeMap::from([("0001_init".to_owned(), (sum.to_owned(), String::new()))]);
        assert_eq!(squash_state(&m, &replaces, &row("edited-squash")), Squash::Own);
        // the row of the replaced migration itself is one of the replaced rows
        assert_eq!(squash_state(&m, &replaces, &row("old")), Squash::Partial(1));
    }
}
