//! Applying and reverting the migrations of a directory (`monorm_core::migrate::files`) on
//! a database. The bindings and the `monorm` CLI all run this one implementation.
//!
//! A migration is a list of operations. [`monorm_core::migrate::render`] turns a range of them into
//! transaction groups. Each atomic group runs in one transaction under an advisory lock, with
//! the progress of its migration recorded in the same transaction. An operation with
//! `"atomic": false` runs alone outside a transaction, under a session lock, and marks itself
//! started before it runs.
//!
//! The `monorm_migrations` table holds one row for each migration that has started. `ops_done` is
//! the number of operations applied from the start of the list, `ops_total` the length of the
//! list and `started` the operation that runs outside a transaction. A migration is applied when
//! `ops_done` equals `ops_total`. A migration that stopped halfway is refused by a plain run, which
//! prints the `--ops` slice that continues it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use monorm_core::dialect::Dialect;
use monorm_core::migrate::diff::Op;
use monorm_core::migrate::files::{self, MigrationEntry};
use monorm_core::migrate::ops::{MigrationFile, OpKind};
use monorm_core::migrate::pg::ident;
use monorm_core::migrate::render::{self, Body, CodeCall, Direction, Group, Item, Rendered};

use crate::db::{Driver, Executor, SessionLock, Transaction};
use crate::error::{Error, Result};

pub const TABLE: &str = "monorm_migrations";
/// Arbitrary constant: the advisory lock serializing concurrent migrators.
pub const LOCK_ID: i64 = 0x6F72_6D6D;

/// One migration file.
#[derive(Debug, Clone)]
pub struct Migration {
    pub name: String,
    /// The `.json` file.
    pub path: PathBuf,
}

impl Migration {
    fn entry(&self) -> MigrationEntry {
        MigrationEntry { name: self.name.clone(), path: self.path.clone() }
    }

    /// The directory of the file, and of the scripts it names.
    pub fn dir(&self) -> &Path {
        self.path.parent().unwrap_or(Path::new("."))
    }

    pub fn file(&self) -> Result<MigrationFile> {
        files::read(&self.entry()).map_err(Error::Migration)
    }

    /// The names of the migrations this one replaces: empty unless it is a squash file.
    pub fn replaces(&self) -> Result<Vec<String>> {
        Ok(self.file()?.replaces)
    }

    /// Hex SHA-256 of the file and the `.sql` files it names.
    pub fn checksum(&self) -> Result<String> {
        files::file_checksum(&self.entry()).map_err(Error::Migration)
    }
}

impl From<MigrationEntry> for Migration {
    fn from(f: MigrationEntry) -> Self {
        Migration { name: f.name, path: f.path }
    }
}

/// The migrations of `dir`, in order.
pub fn list(dir: &Path) -> Result<Vec<Migration>> {
    Ok(files::list(dir).map_err(Error::Migration)?.into_iter().map(Migration::from).collect())
}

/// The place of `name` in the order of `statuses`: names compare by order key, not as text.
fn position(statuses: &[Status], name: &str) -> Result<usize> {
    statuses.iter().position(|s| s.migration.name == name).ok_or_else(|| Error::Migration(format!("no migration named {name}")))
}

/// A migration by name or number (`"3"`, `"0003"`, `"0003_add_tags"`).
pub fn find(dir: &Path, name: &str) -> Result<Migration> {
    files::find(dir, name).map(Migration::from).map_err(Error::Migration)
}

#[derive(Debug, Clone)]
pub struct Status {
    pub migration: Migration,
    pub applied: bool,
    /// When it was applied, as Postgres prints a `timestamptz`.
    pub applied_at: Option<String>,
    /// The operations done and the operations in all, for a migration that stopped halfway.
    pub partial: Option<(usize, usize)>,
}

/// What [`plan_down`] reverts.
#[derive(Debug, Clone)]
pub enum Down {
    /// The last `n` applied migrations.
    Steps(usize),
    /// Every migration after this one; `"zero"` reverts all.
    To(String),
}

/// The operations of one migration to run: numbers from 1, both ends included.
pub type Slice = (usize, Option<usize>);

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

/// Refuses a group when an operation in it can delete the data of a table with rows.
/// `what` starts the error text: the migration, and for a rollback its direction.
/// `in_tx` is true for a migration transaction: on PostgreSQL the check then locks the
/// table first, as the step itself will, so a write that commits later cannot slip past.
/// SQLite holds the write lock for the whole transaction already.
async fn gate(conn: &dyn Executor, what: &str, group: &Group, in_tx: bool) -> Result<()> {
    let mut seen: Vec<String> = vec![];
    for step in group_steps(group) {
        for d in &step.destructive {
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
            seen.push(d.table.clone());
        }
    }
    Ok(())
}

fn group_steps(group: &Group) -> impl Iterator<Item = &monorm_core::migrate::Step> {
    group.items.iter().flat_map(|i| match &i.body {
        Body::Sql(s) => s.as_slice(),
        Body::Code(_) => &[],
    })
}

async fn has_column(conn: &dyn Executor, column: &str) -> Result<bool> {
    let sql = match conn.dialect() {
        Dialect::Postgres => format!("SELECT 1 FROM pg_attribute WHERE attrelid = to_regclass({}) AND attname = {} AND NOT attisdropped", lit(TABLE), lit(column)),
        Dialect::Sqlite => format!("SELECT 1 FROM pragma_table_info({}) WHERE name = {}", lit(TABLE), lit(column)),
    };
    Ok(!conn.query_text(sql).await?.is_empty())
}

async fn ensure_table(conn: &dyn Executor) -> Result<()> {
    let (timestamp, flag) = match conn.dialect() {
        Dialect::Postgres => ("timestamptz NOT NULL DEFAULT now()", "boolean NOT NULL DEFAULT false"),
        Dialect::Sqlite => ("text NOT NULL DEFAULT CURRENT_TIMESTAMP", "integer NOT NULL DEFAULT 0"),
    };
    conn.batch(format!(
        "CREATE TABLE IF NOT EXISTS {TABLE} (name text PRIMARY KEY, checksum text NOT NULL, applied_at {timestamp}, baseline {flag}, \
         ops_done integer NOT NULL DEFAULT 0, ops_total integer NOT NULL DEFAULT 0, started integer)"
    ))
    .await?;
    // A table made by an older version lacks columns; add them once, under the migration lock.
    // The lock helper reads the progress columns, so this does its own transaction.
    let mut missing = vec![];
    for (column, ddl) in [("baseline", flag), ("ops_done", "integer NOT NULL DEFAULT 0"), ("ops_total", "integer NOT NULL DEFAULT 0"), ("started", "integer")] {
        if !has_column(conn, column).await? {
            missing.push(format!("ALTER TABLE {TABLE} ADD COLUMN {}{column} {ddl}", if conn.dialect() == Dialect::Postgres { "IF NOT EXISTS " } else { "" }));
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    let tx = conn.begin_migration().await?;
    let run = async {
        if conn.dialect() == Dialect::Postgres {
            take_xact_lock(&*tx).await?;
        }
        for sql in missing {
            tx.batch(sql).await?;
        }
        Ok::<_, Error>(())
    };
    match run.await {
        Ok(()) => Ok(tx.commit().await?),
        Err(e) => {
            let _ = Transaction::rollback(&*tx).await;
            Err(e)
        }
    }
}

/// Applied migrations that `baseline` recorded without running them.
async fn baselined(conn: &dyn Executor) -> Result<Vec<String>> {
    let rows = conn.query_text(format!("SELECT name FROM {TABLE} WHERE baseline ORDER BY name")).await?;
    Ok(rows.into_iter().filter_map(|r| r.into_iter().next().flatten()).collect())
}

/// The row of one migration.
#[derive(Debug, Clone)]
struct Row {
    checksum: String,
    applied_at: String,
    done: usize,
    total: usize,
    started: Option<usize>,
}

impl Row {
    fn complete(&self) -> bool {
        self.done >= self.total
    }
}

fn count(cell: Option<String>) -> usize {
    cell.and_then(|v| v.parse().ok()).unwrap_or(0)
}

async fn applied(conn: &dyn Executor) -> Result<BTreeMap<String, Row>> {
    let rows = conn.query_text(format!("SELECT name, checksum, CAST(applied_at AS text), ops_done, ops_total, started FROM {TABLE} ORDER BY name")).await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            let mut it = r.into_iter();
            match (it.next().flatten(), it.next().flatten(), it.next().flatten()) {
                (Some(n), Some(c), Some(a)) => {
                    let (done, total, started) = (count(it.next().flatten()), count(it.next().flatten()), it.next().flatten().and_then(|v| v.parse().ok()));
                    Some((n, Row { checksum: c, applied_at: a, done, total, started }))
                }
                _ => None,
            }
        })
        .collect())
}

/// Every migration of `dir` and whether it is applied. Migrations recorded in the
/// database but missing from the directory are an error.
pub async fn status(conn: &dyn Executor, dir: &Path) -> Result<Vec<Status>> {
    let all = list(dir)?;
    ensure_table(conn).await?;
    let mut done = applied(conn).await?;
    let mut out: Vec<Status> = vec![];
    for m in all {
        let replaces = m.replaces()?;
        let mut row = None;
        if replaces.is_empty() {
            row = done.remove(&m.name);
        } else {
            match squash_state(&m, &replaces, &done) {
                Squash::Own => row = done.remove(&m.name),
                Squash::Fresh => {}
                Squash::Replaced => {
                    let rows: Vec<Row> = replaces.iter().filter_map(|r| done.remove(r)).collect();
                    row = rows.into_iter().max_by(|a, b| a.applied_at.cmp(&b.applied_at));
                    if let Some(r) = &mut row {
                        r.done = r.total;
                    }
                }
                Squash::Partial(n) => {
                    return Err(Error::Migration(format!(
                        "the database applied {n} of the {} migrations that {} replaces; migrate it with a release before the squash",
                        replaces.len(),
                        m.name
                    )))
                }
                Squash::Unfinished(r) => {
                    return Err(Error::Migration(format!("{r}, which {} replaces, is only partly applied; finish it first", m.name)))
                }
            }
        }
        let partial = row.as_ref().filter(|r| !r.complete()).map(|r| (r.done, r.total));
        out.push(Status { applied: row.as_ref().is_some_and(Row::complete), applied_at: row.map(|r| r.applied_at), partial, migration: m });
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

/// How the database stands to a squash file (one with `replaces`).
#[derive(Debug, PartialEq, Eq)]
enum Squash {
    /// The squash file itself is recorded (it ran, or the database adopted it).
    Own,
    /// All the replaced migrations are recorded and the squash file is not: adoption is due.
    Replaced,
    Fresh,
    /// Only this many of the replaced migrations are recorded.
    Partial(usize),
    /// A replaced migration is recorded but not finished.
    Unfinished(String),
}

fn squash_state(m: &Migration, replaces: &[String], done: &BTreeMap<String, Row>) -> Squash {
    if done.contains_key(&m.name) {
        return Squash::Own;
    }
    if let Some(r) = replaces.iter().find(|r| done.get(*r).is_some_and(|row| !row.complete())) {
        return Squash::Unfinished(r.clone());
    }
    match replaces.iter().filter(|r| done.contains_key(*r)).count() {
        0 => Squash::Fresh,
        n if n == replaces.len() => Squash::Replaced,
        n => Squash::Partial(n),
    }
}

/// Applied migrations must be a prefix of the directory and unchanged since. A migration that
/// stopped halfway must be the last one with a row.
fn verify(statuses: &[Status], applied: &BTreeMap<String, Row>) -> Result<()> {
    let mut pending: Option<&str> = None;
    for s in statuses {
        let m = &s.migration;
        let row = applied.get(&m.name);
        if row.is_none() && !s.applied {
            pending = pending.or(Some(&m.name));
        } else if let Some(p) = pending {
            return Err(Error::Migration(format!(
                "{} is applied but the earlier {p} is not; renumber the unapplied migration after the applied ones",
                m.name
            )));
        } else if let Some(row) = row {
            if row.checksum != m.checksum()? {
                return Err(Error::Migration(format!("{} changed after it was applied", m.name)));
            }
        }
    }
    Ok(())
}

/// For each squash file whose replaced migrations are all recorded and which is not yet: records the
/// file and forgets the replaced rows, in one transaction under the migration lock, with no SQL of the
/// file run. The new row is baselined when any replaced row was. Gives the files it adopted.
pub async fn adopt(conn: &dyn Executor, dir: &Path) -> Result<Vec<Migration>> {
    let mut done = vec![];
    for m in list(dir)? {
        let replaces = m.replaces()?;
        if replaces.is_empty() {
            continue;
        }
        ensure_table(conn).await?;
        let sum = m.checksum()?;
        let total = m.file()?.operations.len();
        let names = replaces.iter().map(|n| lit(n)).collect::<Vec<_>>().join(", ");
        let any = match conn.dialect() {
            Dialect::Postgres => "COALESCE(bool_or(baseline), false)",
            Dialect::Sqlite => "COALESCE(MAX(baseline), 0)",
        };
        let record = format!(
            "INSERT INTO {TABLE} (name, checksum, ops_done, ops_total, baseline) SELECT {}, {}, {total}, {total}, {any} FROM {TABLE} WHERE name IN ({names})",
            lit(&m.name),
            lit(&sum)
        );
        let forget = format!("DELETE FROM {TABLE} WHERE name IN ({names}) AND name <> {}", lit(&m.name));
        let ran = locked(conn, |now| (squash_state(&m, &replaces, now) == Squash::Replaced).then(|| vec![record, forget])).await?;
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

/// In one transaction holding the migration lock: runs the statements `scripts` gives for
/// the rows present by then (none: nothing to do), and commits.
async fn locked(conn: &dyn Executor, scripts: impl FnOnce(&BTreeMap<String, Row>) -> Option<Vec<String>>) -> Result<bool> {
    let tx = conn.begin_migration().await?;
    let run = async {
        if conn.dialect() == Dialect::Postgres {
            take_xact_lock(&*tx).await?;
        }
        let Some(scripts) = scripts(&applied(&*tx).await?) else { return Ok(false) };
        for sql in scripts {
            tx.batch(sql).await?;
        }
        Ok::<_, Error>(true)
    };
    match run.await {
        Ok(ran) => {
            tx.commit().await?;
            crate::db::forget_cascades(conn);
            Ok(ran)
        }
        Err(e) => {
            let _ = Transaction::rollback(&*tx).await;
            Err(e)
        }
    }
}

/// What a host that runs code is: the language of its `run_code` entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Host {
    Python,
    TypeScript,
}

impl Host {
    pub fn name(self) -> &'static str {
        match self {
            Host::Python => "python",
            Host::TypeScript => "typescript",
        }
    }

    /// The `file:function` entry of `call` for this host.
    pub fn entry(self, call: &CodeCall) -> Option<&str> {
        match self {
            Host::Python => call.python.as_deref(),
            Host::TypeScript => call.typescript.as_deref(),
        }
    }
}

/// The work of one migration: the operations `range` in `direction`, rendered into groups.
#[derive(Debug, Clone)]
pub struct Plan {
    pub migration: Migration,
    pub direction: Direction,
    /// The operations to run, from 1, both ends included.
    pub range: (usize, usize),
    /// The number of operations in the file.
    pub total: usize,
    pub rendered: Rendered,
    pub checksum: String,
    /// The error text starts with this: the migration, and for a rollback its direction.
    pub what: String,
}

impl Plan {
    /// For each group, the progress (`ops_done`) before it and after it.
    pub fn bounds(&self) -> Vec<(usize, usize)> {
        let (a, b) = self.range;
        let groups = &self.rendered.groups;
        let low = |g: &Group| g.items.iter().map(|i| i.first).min().unwrap_or(a);
        let mut out = vec![];
        for (k, g) in groups.iter().enumerate() {
            match self.direction {
                Direction::Up => {
                    let before = if k == 0 { a - 1 } else { out.last().map_or(a - 1, |p: &(usize, usize)| p.1) };
                    let after = groups.get(k + 1).map_or(b, |n| low(n) - 1);
                    out.push((before, after));
                }
                Direction::Down => {
                    let before = if k == 0 { b } else { out.last().map_or(b, |p: &(usize, usize)| p.1) };
                    let after = if k + 1 == groups.len() { a - 1 } else { low(g) - 1 };
                    out.push((before, after));
                }
            }
        }
        out
    }

    /// The code calls of the plan, in run order.
    pub fn code_calls(&self) -> impl Iterator<Item = (&Item, &CodeCall)> {
        self.rendered.items().filter_map(|i| match &i.body {
            Body::Code(c) => Some((i, c)),
            Body::Sql(_) => None,
        })
    }
}

/// The error text of the `--ops` slice that continues a migration that stopped halfway.
fn continue_hint(m: &Migration, done: usize, total: usize, direction: Direction) -> String {
    match direction {
        Direction::Up => format!("migrate {} --ops {}-{total}", m.name, done + 1),
        Direction::Down => format!("rollback {} --ops 1-{done}", m.name),
    }
}

/// Reads `--ops` text: `A-B` (both ends included) or `A-` (to the end); numbers start at 1.
pub fn parse_slice(text: &str) -> Result<Slice> {
    let bad = || Error::Migration(format!("--ops takes A-B or A- (operation numbers from 1), not {text:?}"));
    let (a, b) = text.split_once('-').ok_or_else(bad)?;
    let a: usize = a.trim().parse().map_err(|_| bad())?;
    let b = match b.trim() {
        "" => None,
        b => Some(b.parse::<usize>().map_err(|_| bad())?),
    };
    if a < 1 || b.is_some_and(|b| b < a) {
        return Err(bad());
    }
    Ok((a, b))
}

fn slice_range(slice: Slice, limit: usize) -> Result<(usize, usize)> {
    let (a, b) = (slice.0, slice.1.unwrap_or(limit));
    if a < 1 || a > b {
        return Err(Error::Migration(format!("--ops {a}-{b} is not a range of operations (numbers start at 1)")));
    }
    Ok((a, b))
}

fn render_plan(m: &Migration, dialect: Dialect, direction: Direction, range: (usize, usize), what: String) -> Result<Plan> {
    let dir = m.dir().to_path_buf();
    let file = m.file()?;
    let total = file.operations.len();
    let start = files::state_before(&dir, &m.name, dialect).map_err(Error::Migration)?;
    let rendered = render::render(&m.name, &dir, &file, &start, direction, Some(range)).map_err(Error::Migration)?;
    Ok(Plan { migration: m.clone(), direction, range, total, rendered, checksum: m.checksum()?, what })
}

/// The refusals that hold before anything runs: a step outside a transaction needs PostgreSQL and a
/// direct connection, and code needs a host with an entry for its language.
fn check_plan(conn: &dyn Executor, plan: &Plan, host: Option<Host>) -> Result<()> {
    let name = &plan.migration.name;
    for g in plan.rendered.groups.iter().filter(|g| !g.atomic) {
        let first = g.items.first().map_or(0, |i| i.first);
        if conn.dialect() != Dialect::Postgres {
            return Err(Error::Migration(format!("{name}: operation {first} runs outside a transaction; {} has no such mode", conn.dialect().name())));
        }
        if conn.pooler_transactions() {
            return Err(Error::Migration(format!(
                "{name}: operation {first} runs outside a transaction and needs a session lock, which needs {}",
                crate::db::POOLER_LOCK_REFUSAL
            )));
        }
    }
    for (item, call) in plan.code_calls() {
        match host {
            None => {
                return Err(Error::Migration(format!(
                    "{name}: operation {} runs code (run_code); apply it with the Python or TypeScript Migrator (python -m monorm migrate, npx monorm migrate), not with this program",
                    item.first
                )))
            }
            Some(h) if h.entry(call).is_none() => {
                return Err(Error::Migration(format!("{name}: operation {} has no run_code entry for {}; add a \"{}\" entry or run it from a host that has one", item.first, h.name(), h.name())))
            }
            _ => {}
        }
    }
    Ok(())
}

/// The destructive check of every group of `plan`, with no lock. [`begin_group`] checks again in its
/// transaction.
async fn precheck(conn: &dyn Executor, plan: &Plan) -> Result<()> {
    for g in &plan.rendered.groups {
        gate(conn, &plan.what, g, false).await?;
    }
    Ok(())
}

/// A partial row may belong to a run that still holds the migration lock. Waits for that run, so a
/// plan never refuses a migration that another migrator is about to finish.
async fn wait_for_running(conn: &dyn Executor) -> Result<()> {
    if conn.dialect() == Dialect::Postgres && applied(conn).await?.values().any(|r| !r.complete()) {
        locked(conn, |_| None).await?;
    }
    Ok(())
}

/// The plans [`upgrade`] runs: the pending migrations up to and including `target` (default: all), after
/// the same checks. With `slice` (`--ops A-B`), `target` names one migration and the plan is the slice of
/// its operations that follows the ones done. Unless `allow_destructive`, a destructive operation on a table
/// with rows is refused. This check holds no lock: [`begin_group`] checks again in its transaction.
pub async fn plan_up(conn: &dyn Executor, dir: &Path, target: Option<&str>, slice: Option<Slice>, host: Option<Host>, allow_destructive: bool) -> Result<Vec<Plan>> {
    adopt_if_ready(conn, dir).await?;
    ensure_table(conn).await?;
    wait_for_running(conn).await?;
    let statuses = status(conn, dir).await?;
    let rows = applied(conn).await?;
    verify(&statuses, &rows)?;
    let mut plans = vec![];
    if let Some(slice) = slice {
        let name = target.ok_or_else(|| Error::Migration("--ops needs one named migration".into()))?;
        let m = find(dir, name)?;
        let row = rows.get(&m.name);
        let total = m.file()?.operations.len();
        let done = row.map_or(0, |r| r.done.min(r.total));
        if row.is_some_and(Row::complete) {
            return Err(Error::Migration(format!("{} is already applied", m.name)));
        }
        let at = position(&statuses, &m.name)?;
        if let Some(p) = statuses[..at].iter().find(|s| !s.applied && !rows.contains_key(&s.migration.name)) {
            return Err(Error::Migration(format!("the earlier {} is not applied", p.migration.name)));
        }
        let (a, b) = slice_range(slice, total)?;
        if b > total {
            return Err(Error::Migration(format!("{} has {total} operations; --ops {a}-{b} is out of range", m.name)));
        }
        if a != done + 1 {
            return Err(Error::Migration(format!("{} has {done} of {total} operations applied; the next slice starts at {} (--ops {}-)", m.name, done + 1, done + 1)));
        }
        plans.push(render_plan(&m, conn.dialect(), Direction::Up, (a, b), m.name.clone())?);
    } else {
        let stop = target.map(|t| find(dir, t).and_then(|m| position(&statuses, &m.name))).transpose()?;
        for s in statuses.iter().enumerate().filter(|(i, s)| !s.applied && stop.is_none_or(|stop| *i <= stop)).map(|(_, s)| s) {
            let m = &s.migration;
            if let Some((done, total)) = s.partial {
                return Err(Error::Migration(format!(
                    "{} stopped after {done} of {total} operations; continue it with `{}`",
                    m.name,
                    continue_hint(m, done, total, Direction::Up)
                )));
            }
            let total = m.file()?.operations.len();
            plans.push(render_plan(m, conn.dialect(), Direction::Up, (1, total.max(1)), m.name.clone())?);
        }
    }
    for p in &plans {
        check_plan(conn, p, host)?;
    }
    if !allow_destructive {
        for p in &plans {
            precheck(conn, p).await?;
        }
    }
    Ok(plans)
}

/// The plans [`downgrade`] runs, newest migration first, after the checks. With `slice`, `down` is
/// `Down::To` no migration: `name` is the one newest migration, and the plan is the operations of
/// the slice, which must end at the operations done.
pub async fn plan_down(conn: &dyn Executor, dir: &Path, down: Down, slice: Option<(&str, Slice)>, host: Option<Host>, force: bool, allow_destructive: bool) -> Result<Vec<Plan>> {
    adopt_if_ready(conn, dir).await?;
    ensure_table(conn).await?;
    wait_for_running(conn).await?;
    let statuses = status(conn, dir).await?;
    let rows = applied(conn).await?;
    verify(&statuses, &rows)?;
    let started: Vec<(usize, &Status)> = statuses.iter().enumerate().filter(|(_, s)| s.applied || s.partial.is_some()).collect();
    let mut plans = vec![];
    let revert: Vec<&Status> = if let Some((name, _)) = slice {
        let m = find(dir, name)?;
        match started.last() {
            Some((_, last)) if last.migration.name == m.name => vec![*last],
            Some((_, last)) => return Err(Error::Migration(format!("{} is not the newest applied migration ({} is); roll back the newer ones first", m.name, last.migration.name))),
            None => return Err(Error::Migration(format!("{} is not applied", m.name))),
        }
    } else {
        match down {
            Down::To(t) => {
                let keep = if t == "zero" { None } else { Some(position(&statuses, &find(dir, &t)?.name)?) };
                started.into_iter().filter(|(i, _)| keep.is_none_or(|k| *i > k)).map(|(_, s)| s).collect()
            }
            Down::Steps(n) => {
                let skip = started.len().saturating_sub(n);
                started.into_iter().skip(skip).map(|(_, s)| s).collect()
            }
        }
    };
    if slice.is_none() {
        if let Some(s) = revert.iter().find(|s| s.partial.is_some()) {
            let (done, total) = s.partial.unwrap_or((0, 0));
            return Err(Error::Migration(format!(
                "{} stopped after {done} of {total} operations; roll back what is done with `{}`",
                s.migration.name,
                continue_hint(&s.migration, done, total, Direction::Down)
            )));
        }
    }
    if !force {
        let marked = baselined(conn).await?;
        if let Some(s) = revert.iter().find(|s| marked.contains(&s.migration.name)) {
            return Err(Error::Migration(format!(
                "{} was baselined: the database had its schema before, so reverting it would drop live tables; pass --force to revert it anyway",
                s.migration.name
            )));
        }
    }
    for s in revert.iter().rev() {
        let m = &s.migration;
        let row = rows.get(&m.name);
        let total = m.file()?.operations.len();
        let done = row.map_or(total, |r| r.done.min(r.total));
        let range = match slice {
            Some((_, sl)) => {
                let (a, b) = slice_range(sl, done)?;
                if b != done {
                    return Err(Error::Migration(format!("{} has {done} of {total} operations applied; a rollback slice must end at {done} (--ops {a}-{done})", m.name)));
                }
                (a, b)
            }
            None => (1, done.max(1)),
        };
        plans.push(render_plan(m, conn.dialect(), Direction::Down, range, format!("rolling back {}", m.name))?);
    }
    for p in &plans {
        check_plan(conn, p, host)?;
    }
    if !allow_destructive {
        for p in &plans {
            precheck(conn, p).await?;
        }
    }
    Ok(plans)
}

/// The SQL that records the progress of `plan` after a group: the row is added by the first group of a
/// migration, deleted when a rollback reaches the start, and updated otherwise.
fn progress_sql(plan: &Plan, exists: bool, done: usize, started: Option<usize>) -> String {
    let name = lit(&plan.migration.name);
    let started = started.map_or("NULL".to_owned(), |n| n.to_string());
    match plan.direction {
        Direction::Down if done == 0 => format!("DELETE FROM {TABLE} WHERE name = {name}"),
        _ if !exists => format!(
            "INSERT INTO {TABLE} (name, checksum, ops_done, ops_total, started) VALUES ({name}, {}, {done}, {}, {started})",
            lit(&plan.checksum),
            plan.total
        ),
        _ => format!("UPDATE {TABLE} SET ops_done = {done}, started = {started} WHERE name = {name}"),
    }
}

/// Whether the row of `plan` stands where the group expects: the progress before it.
fn expected(plan: &Plan, rows: &BTreeMap<String, Row>, before: usize) -> bool {
    let row = rows.get(&plan.migration.name);
    match plan.direction {
        Direction::Up => row.map_or(0, |r| r.done) == before && row.is_none_or(|r| r.started.is_none() || r.started == Some(before + 1)),
        Direction::Down => row.is_some_and(|r| r.done == before),
    }
}

/// Starts group `k` of `plan`, an atomic one: a migration transaction holding the migration lock. `None`
/// when another migrator ran the group meanwhile. The caller runs the items with [`run_item`] (and the
/// code itself), then [`finish_group`] (or rolls back). Unless `allow_destructive`, a destructive
/// operation on a table with rows is refused here, after the lock, so a row inserted since [`plan_up`]
/// still counts.
pub async fn begin_group(conn: &dyn Executor, plan: &Plan, k: usize, allow_destructive: bool) -> Result<Option<Arc<dyn Transaction>>> {
    let (before, _) = plan.bounds()[k];
    let tx = conn.begin_migration().await?;
    let run = async {
        if conn.dialect() == Dialect::Postgres {
            take_xact_lock(&*tx).await?;
        }
        if !expected(plan, &applied(&*tx).await?, before) {
            return Ok(false);
        }
        if !allow_destructive {
            gate(&*tx, &plan.what, &plan.rendered.groups[k], true).await?;
        }
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

/// Runs the SQL steps of one item in the transaction of [`begin_group`].
pub async fn run_item(tx: &dyn Transaction, item: &Item) -> Result<()> {
    if let Body::Sql(steps) = &item.body {
        for s in steps {
            tx.batch(s.sql.clone()).await?;
        }
    }
    Ok(())
}

/// Records the progress of group `k` in the transaction of [`begin_group`] and commits; rolls back on error.
pub async fn finish_group(tx: &dyn Transaction, plan: &Plan, k: usize) -> Result<()> {
    let (before, after) = plan.bounds()[k];
    let exists = !(plan.direction == Direction::Up && before == 0);
    match tx.batch(progress_sql(plan, exists, after, None)).await {
        Ok(_) => {
            tx.commit().await?;
            crate::db::forget_cascades(tx);
            Ok(())
        }
        Err(e) => {
            let _ = tx.rollback().await;
            Err(e.into())
        }
    }
}

/// A plan with no group to run still moves the progress to the end of its slice.
pub async fn record_empty(conn: &dyn Executor, plan: &Plan) -> Result<()> {
    let (a, b) = plan.range;
    let (before, after) = match plan.direction {
        Direction::Up => (a - 1, b),
        Direction::Down => (b, a - 1),
    };
    let sql = |exists: bool| progress_sql(plan, exists, after, None);
    let up = plan.direction == Direction::Up;
    locked(conn, |rows| {
        if !expected(plan, rows, before) {
            return None;
        }
        Some(vec![sql(!(up && before == 0))])
    })
    .await?;
    Ok(())
}

/// The refusal text of an index that a failed build left behind: nothing here, the build drops it.
fn index_ref(name: &str) -> String {
    if name.starts_with('"') { name.to_owned() } else { ident(name) }
}

/// The index that operation `number` of `file` builds, when it is a build outside a transaction.
fn building_index(file: &MigrationFile, number: usize) -> Option<String> {
    let op = file.operations.get(number.checked_sub(1)?)?;
    match &op.kind {
        OpKind::Schema(Op::CreateIndex { index, .. }) if !op.atomic => Some(index_ref(&index.name)),
        _ => None,
    }
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

/// Runs group `k` of `plan`, one operation outside a transaction, on PostgreSQL: under a session lock,
/// marks the operation started, runs it, then records the progress. A failed `CREATE INDEX CONCURRENTLY`
/// leaves an INVALID index, and a rerun drops it before the build. `false` when another migrator ran
/// the group meanwhile.
pub async fn run_outside(driver: &dyn Driver, plan: &Plan, k: usize, allow_destructive: bool) -> Result<bool> {
    let group = &plan.rendered.groups[k];
    let (before, after) = plan.bounds()[k];
    let number = group.items.first().map_or(0, |i| i.first);
    let file = plan.migration.file()?;
    ensure_table(driver).await?;
    let lock = take_migration_lock(driver).await?;
    let run = async {
        let rows = applied(driver).await?;
        if !expected(plan, &rows, before) {
            return Ok(false);
        }
        if !allow_destructive {
            gate(driver, &plan.what, group, false).await?;
        }
        let exists = rows.contains_key(&plan.migration.name);
        // Marking the start needs a row for a migration that has none yet.
        driver.batch(match (plan.direction, exists) {
            (Direction::Up, false) => progress_sql(plan, false, 0, Some(number)),
            _ => format!("UPDATE {TABLE} SET started = {number} WHERE name = {}", lit(&plan.migration.name)),
        }).await?;
        if plan.direction == Direction::Up {
            if let Some(target) = building_index(&file, number) {
                let valid = driver.query_text(format!("SELECT CAST(indisvalid AS integer) FROM pg_index WHERE indexrelid = to_regclass({})", lit(&target))).await?;
                if valid.into_iter().next().and_then(|r| r.into_iter().next().flatten()).is_some_and(|v| v == "0") {
                    driver.batch(format!("DROP INDEX CONCURRENTLY {target}")).await?;
                }
            }
        }
        for item in &group.items {
            run_item_outside(driver, item).await?;
        }
        // A statement outside a transaction commits by itself. The transaction lock would wait
        // for this process's own session lock.
        driver.batch(progress_sql(plan, true, after, None)).await?;
        Ok::<_, Error>(true)
    };
    let out = run.await;
    let released = lock.release().await;
    let ran = out?;
    released?;
    crate::db::forget_cascades(driver);
    Ok(ran)
}

async fn run_item_outside(conn: &dyn Executor, item: &Item) -> Result<()> {
    if let Body::Sql(steps) = &item.body {
        for s in steps {
            conn.batch(s.sql.clone()).await?;
        }
    }
    Ok(())
}

/// Runs the SQL of every group of `plan` (the plan has no code: [`plan_up`] refuses it without a host).
pub async fn apply(conn: &dyn Driver, plan: &Plan, allow_destructive: bool) -> Result<bool> {
    if plan.rendered.groups.is_empty() {
        record_empty(conn, plan).await?;
        return Ok(true);
    }
    for (k, g) in plan.rendered.groups.iter().enumerate() {
        if g.atomic {
            let Some(tx) = begin_group(conn, plan, k, allow_destructive).await? else { return Ok(false) };
            for item in &g.items {
                if let Err(e) = run_item(&*tx, item).await {
                    let _ = Transaction::rollback(&*tx).await;
                    return Err(e);
                }
            }
            finish_group(&*tx, plan, k).await?;
        } else if !run_outside(conn, plan, k, allow_destructive).await? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Applies pending migrations up to and including `target` (default: all); gives the ones it applied. With
/// `slice`, applies those operations of the one migration `target`. A migration with code (`run_code`)
/// is refused before anything runs: the Python or TypeScript migrator applies it.
pub async fn upgrade(conn: &dyn Driver, dir: &Path, target: Option<&str>, slice: Option<Slice>, allow_destructive: bool) -> Result<Vec<Migration>> {
    let plans = plan_up(conn, dir, target, slice, None, allow_destructive).await?;
    let mut done = vec![];
    for plan in plans {
        if apply(conn, &plan, allow_destructive).await? {
            done.push(plan.migration);
        } else if slice.is_some() {
            return Err(Error::Migration(format!("{} changed while this run waited for the lock; run it again", plan.migration.name)));
        }
    }
    Ok(done)
}

/// The names of the applied migrations, in order.
pub async fn applied_names(conn: &dyn Executor) -> Result<Vec<String>> {
    ensure_table(conn).await?;
    Ok(applied(conn).await?.into_iter().filter(|(_, r)| r.complete()).map(|(n, _)| n).collect())
}

/// Records the first migration of `dir` as applied without running it, for a database
/// that already has its schema (`monorm pull`). Refused once any migration is applied.
pub async fn baseline(conn: &dyn Executor, dir: &Path) -> Result<Migration> {
    let first = list(dir)?.into_iter().next().ok_or_else(|| Error::Migration(format!("no migration in {}", dir.display())))?;
    ensure_table(conn).await?;
    let total = first.file()?.operations.len();
    let record = format!(
        "INSERT INTO {TABLE} (name, checksum, ops_done, ops_total, baseline) VALUES ({}, {}, {total}, {total}, {})",
        lit(&first.name),
        lit(&first.checksum()?),
        if conn.dialect() == Dialect::Postgres { "true" } else { "1" }
    );
    let mut applied = vec![];
    let ran = locked(conn, |now| {
        applied = now.keys().cloned().collect();
        now.is_empty().then(|| vec![record])
    })
    .await?;
    if !ran {
        return Err(Error::Migration(format!("the database already has applied migrations ({}); baseline only marks the first", applied.join(", "))));
    }
    Ok(first)
}

/// Reverts applied migrations, newest first; gives the ones it reverted. A baselined
/// migration was never run here, so its reverse would drop live tables: refused
/// unless `force`, before anything is reverted. A reverse that is missing is refused
/// before anything is reverted, and so is a destructive one unless `allow_destructive`.
pub async fn downgrade(conn: &dyn Driver, dir: &Path, down: Down, slice: Option<(&str, Slice)>, force: bool, allow_destructive: bool) -> Result<Vec<Migration>> {
    let plans = plan_down(conn, dir, down, slice, None, force, allow_destructive).await?;
    let mut done = vec![];
    for plan in plans {
        if apply(conn, &plan, allow_destructive).await? {
            done.push(plan.migration);
        }
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(done: usize, total: usize) -> Row {
        Row { checksum: String::new(), applied_at: String::new(), done, total, started: None }
    }

    #[test]
    fn a_row_is_complete_when_all_operations_are_done() {
        assert!(row(3, 3).complete());
        assert!(row(0, 0).complete());
        assert!(!row(2, 3).complete());
    }

    #[test]
    fn a_squash_file_with_one_replaced_row_is_a_partial_database() {
        let m = Migration { name: "0003_squash".into(), path: PathBuf::new() };
        let replaces = vec!["0001_a".to_owned(), "0002_b".to_owned()];
        let rows = BTreeMap::from([("0001_a".to_owned(), row(1, 1))]);
        assert_eq!(squash_state(&m, &replaces, &rows), Squash::Partial(1));
        let rows = BTreeMap::from([("0001_a".to_owned(), row(1, 1)), ("0002_b".to_owned(), row(1, 1))]);
        assert_eq!(squash_state(&m, &replaces, &rows), Squash::Replaced);
        assert_eq!(squash_state(&m, &replaces, &BTreeMap::new()), Squash::Fresh);
        let rows = BTreeMap::from([("0001_a".to_owned(), row(1, 2))]);
        assert_eq!(squash_state(&m, &replaces, &rows), Squash::Unfinished("0001_a".into()));
    }
}
