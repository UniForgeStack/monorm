//! The SQL and the code calls of a migration file, in transaction groups.
//!
//! The state before each operation is the replay of the operations before it
//! ([`super::replay`]). A run of schema operations that needs no group of its own renders as one
//! unit: PostgreSQL renders each operation, SQLite rebuilds the tables once for the run. The
//! reverse of a run is the diff from its state after to its state before.

use std::path::Path;

use super::diff::{self, Op};
use super::model::{DbSchema, Renames};
use super::ops::{CodeReverse, MigrationFile, OpKind, Operation, Reverse, RunCode, RunSql};
use super::{pg, replay, sqlite, steps_with, Step};
use crate::dialect::Dialect;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
}

/// What one operation range runs: the transaction groups in run order.
#[derive(Clone, Debug, Default)]
pub struct Rendered {
    pub groups: Vec<Group>,
}

/// The items that share one transaction (`atomic`), or one item that runs outside a transaction.
#[derive(Clone, Debug)]
pub struct Group {
    pub atomic: bool,
    pub items: Vec<Item>,
}

/// The work of the operations `first` to `last` (numbers from 1).
#[derive(Clone, Debug)]
pub struct Item {
    pub first: usize,
    pub last: usize,
    pub body: Body,
}

#[derive(Clone, Debug)]
pub enum Body {
    Sql(Vec<Step>),
    Code(CodeCall),
}

/// One function for each language, as `file:function`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodeCall {
    pub python: Option<String>,
    pub typescript: Option<String>,
}

impl Rendered {
    pub fn is_empty(&self) -> bool {
        self.groups.iter().all(|g| g.items.is_empty())
    }

    pub fn items(&self) -> impl Iterator<Item = &Item> {
        self.groups.iter().flat_map(|g| g.items.iter())
    }

    /// The SQL steps of every item, in run order.
    pub fn steps(&self) -> impl Iterator<Item = &Step> {
        self.items().flat_map(|i| match &i.body {
            Body::Sql(s) => s.as_slice(),
            Body::Code(_) => &[],
        })
    }

    /// The operations as text, numbered, for `sqlmigrate` and for a person to read.
    pub fn text(&self, title: &str) -> String {
        let mut out = format!("-- {title}\n\n");
        for g in &self.groups {
            if !g.atomic {
                out.push_str("-- outside a transaction\n");
            }
            for item in &g.items {
                let label = if item.first == item.last { format!("operation {}", item.first) } else { format!("operations {}-{}", item.first, item.last) };
                match &item.body {
                    Body::Sql(steps) => {
                        for s in steps {
                            out.push_str(&format!("-- {label}: {}\n", s.summary));
                            if let Some(w) = &s.warning {
                                out.push_str(&format!("-- WARNING: {w}\n"));
                            }
                            out.push_str(s.sql.trim_end());
                            out.push_str(";\n\n");
                        }
                    }
                    Body::Code(c) => {
                        let list: Vec<String> = [("python", &c.python), ("typescript", &c.typescript)].into_iter().filter_map(|(k, v)| v.as_ref().map(|v| format!("{k} {v}"))).collect();
                        out.push_str(&format!("-- {label}: run code ({})\n\n", list.join(", ")));
                    }
                }
            }
        }
        out
    }
}

#[allow(clippy::large_enum_variant)]
enum Unit {
    /// Schema operations that run as one unit; `atomic` is false for one operation of its own.
    Schema { ops: Vec<Op>, before: DbSchema, after: DbSchema, first: usize, last: usize, atomic: bool },
    Sql { sql: RunSql, number: usize, atomic: bool },
    Code { code: RunCode, number: usize, atomic: bool },
    Commit,
}

struct Pending {
    ops: Vec<Op>,
    before: DbSchema,
    first: usize,
    last: usize,
}

fn flush(units: &mut Vec<Unit>, pending: &mut Option<Pending>, state: &DbSchema) {
    if let Some(p) = pending.take() {
        units.push(Unit::Schema { ops: p.ops, before: p.before, after: state.clone(), first: p.first, last: p.last, atomic: true });
    }
}

/// The units of `ops`, and `state` moved to the state after them. `number` maps the position of an
/// operation to its number in the file.
fn build(ops: &[Operation], number: &dyn Fn(usize) -> usize, state: &mut DbSchema, name: &str) -> Result<Vec<Unit>, String> {
    let mut units = vec![];
    let mut pending: Option<Pending> = None;
    for (i, op) in ops.iter().enumerate() {
        let n = number(i);
        let at = |e: String| format!("{name}: operation {n}: {e}");
        match &op.kind {
            OpKind::Schema(s) if !op.atomic => {
                flush(&mut units, &mut pending, state);
                let before = state.clone();
                replay::apply(state, s).map_err(at)?;
                units.push(Unit::Schema { ops: vec![s.clone()], before, after: state.clone(), first: n, last: n, atomic: false });
            }
            OpKind::Schema(s) => {
                let p = pending.get_or_insert_with(|| Pending { ops: vec![], before: state.clone(), first: n, last: n });
                p.ops.push(s.clone());
                p.last = n;
                replay::apply_operation(state, op).map_err(at)?;
            }
            OpKind::RunSql(r) => {
                flush(&mut units, &mut pending, state);
                units.push(Unit::Sql { sql: r.clone(), number: n, atomic: op.atomic });
            }
            OpKind::RunCode(r) => {
                if !op.atomic {
                    return Err(at("run_code cannot be \"atomic\": false; code runs in the transaction of its group".into()));
                }
                flush(&mut units, &mut pending, state);
                units.push(Unit::Code { code: r.clone(), number: n, atomic: op.atomic });
            }
            OpKind::Commit => {
                flush(&mut units, &mut pending, state);
                units.push(Unit::Commit);
            }
            OpKind::Separate { database, state: changes } => {
                // With no `database` operation nothing runs, so the pending group stays open and is built with the final state.
                if !database.is_empty() {
                    flush(&mut units, &mut pending, state);
                    let mut inner = state.clone();
                    units.extend(build(database, &|_| n, &mut inner, name)?);
                }
                for c in changes {
                    replay::apply_operation(state, c).map_err(at)?;
                }
            }
        }
    }
    flush(&mut units, &mut pending, state);
    Ok(units)
}

/// The renames of a run: new table to the table of the state before, and new column to its old one.
fn run_renames(ops: &[Op]) -> Renames {
    let mut r = Renames::default();
    for op in ops {
        match op {
            Op::RenameTable { from, to } => {
                let origin = r.tables.remove(from).unwrap_or_else(|| from.clone());
                r.tables.insert(to.clone(), origin);
                let moved: Vec<_> = r.columns.keys().filter(|(t, _)| t == from).cloned().collect();
                for key in moved {
                    if let Some(v) = r.columns.remove(&key) { r.columns.insert((to.clone(), key.1), v); }
                }
            }
            Op::RenameColumn { table, from, to } => {
                let origin = r.columns.remove(&(table.clone(), from.clone())).unwrap_or_else(|| from.clone());
                r.columns.insert((table.clone(), to.clone()), origin);
            }
            _ => {}
        }
    }
    r
}

/// The key renames of the run seen from the state after it, as they run on the state before.
fn reverse_fills(ops: &[Op], renames: &Renames) -> Vec<Op> {
    ops.iter().rev().filter_map(|op| match op {
        Op::RenameJsonKey { table, column, holder, lists, from, to } => {
            let old_table = renames.tables.get(table).cloned().unwrap_or_else(|| table.clone());
            let old_column = renames.columns.get(&(table.clone(), column.clone())).cloned().unwrap_or_else(|| column.clone());
            Some(Op::RenameJsonKey { table: old_table, column: old_column, holder: holder.clone(), lists: lists.clone(), from: to.clone(), to: from.clone() })
        }
        _ => None,
    }).collect()
}

fn schema_steps(ops: &[Op], before: &DbSchema, after: &DbSchema, direction: Direction, atomic: bool, name: &str, number: usize) -> Result<Vec<Step>, String> {
    let at = |e: String| format!("{name}: operation {number}: {e}");
    let sqlite_run = after.dialect == Dialect::Sqlite;
    if !atomic && sqlite_run {
        return Err(at("\"atomic\": false is not supported on SQLite; it has no statement that cannot run in a transaction".into()));
    }
    let renames = run_renames(ops);
    let online = move |_: &Op| pg::Online { concurrently: !atomic };
    let has_schema = !ops.is_empty();
    match (direction, sqlite_run) {
        (_, _) if !has_schema => Ok(vec![]),
        (Direction::Up, false) => Ok(steps_with(ops, after, online)),
        (Direction::Up, true) => {
            let fills: Vec<Op> = ops.iter().filter(|o| matches!(o, Op::BackfillJson { .. } | Op::RenameJsonKey { .. })).cloned().collect();
            sqlite::steps_with(before, after, &renames, &fills).map_err(at)
        }
        (Direction::Down, false) => {
            let mut down = diff::diff(after, before, &renames.reversed()).map_err(at)?;
            down.extend(reverse_fills(ops, &renames));
            Ok(steps_with(&down, before, online))
        }
        (Direction::Down, true) => sqlite::steps_with(after, before, &renames.reversed(), &reverse_fills(ops, &renames)).map_err(at),
    }
}

fn read_sql(dir: &Path, file: &str, name: &str, number: usize) -> Result<String, String> {
    let path = dir.join(file);
    std::fs::read_to_string(&path).map_err(|e| format!("{name}: operation {number}: {}: {e}", path.display()))
}

fn sql_step(number: usize, text: String) -> Step {
    Step::new(format!("Run SQL (operation {number})"), text, None, vec![])
}

fn item(unit: &Unit, direction: Direction, dir: &Path, name: &str) -> Result<Option<Item>, String> {
    Ok(match unit {
        Unit::Commit => None,
        Unit::Schema { ops, before, after, first, last, atomic } => {
            let steps = schema_steps(ops, before, after, direction, *atomic, name, *first)?;
            (!steps.is_empty()).then_some(Item { first: *first, last: *last, body: Body::Sql(steps) })
        }
        Unit::Sql { sql, number, .. } => {
            let text = match direction {
                Direction::Up => Some(match (&sql.sql, &sql.file) {
                    (Some(s), _) => s.clone(),
                    (None, Some(f)) => read_sql(dir, f, name, *number)?,
                    (None, None) => unreachable!("run_sql is checked when the file is read"),
                }),
                Direction::Down => match sql.reverse() {
                    Reverse::Noop => None,
                    Reverse::Sql(s) => Some(s),
                    Reverse::File(f) => Some(read_sql(dir, &f, name, *number)?),
                    Reverse::Missing => return Err(format!("{name}: operation {number} (run_sql) has no reverse; add \"reverse_sql\" (or \"reverse_file\"), or \"noop\" if there is nothing to undo")),
                },
            };
            text.map(|t| Item { first: *number, last: *number, body: Body::Sql(vec![sql_step(*number, t)]) })
        }
        Unit::Code { code, number, .. } => {
            let call = match direction {
                Direction::Up => Some(CodeCall { python: code.python.clone(), typescript: code.typescript.clone() }),
                Direction::Down => match &code.reverse {
                    None => return Err(format!("{name}: operation {number} (run_code) has no reverse; add \"reverse\" (or \"noop\" if there is nothing to undo)")),
                    Some(CodeReverse::Noop(_)) => None,
                    Some(CodeReverse::Code(c)) => Some(CodeCall { python: c.python.clone(), typescript: c.typescript.clone() }),
                },
            };
            call.map(|c| Item { first: *number, last: *number, body: Body::Code(c) })
        }
    })
}

fn atomic_of(unit: &Unit) -> bool {
    match unit {
        Unit::Schema { atomic, .. } | Unit::Sql { atomic, .. } | Unit::Code { atomic, .. } => *atomic,
        Unit::Commit => true,
    }
}

/// The upper-case words of `sql` outside comments, string literals, quoted names and dollar-quoted bodies.
fn code_words(sql: &str) -> Vec<String> {
    let chars: Vec<char> = sql.chars().collect();
    let (mut words, mut word, mut i) = (vec![], String::new(), 0);
    while i < chars.len() {
        let c = chars[i];
        let skip_to = |from: usize, end: &[char]| (from..chars.len()).find(|&j| chars[j..].starts_with(end)).map_or(chars.len(), |j| j + end.len());
        let skipped = if c == '-' && chars.get(i + 1) == Some(&'-') {
            Some(skip_to(i, &['\n']))
        } else if c == '/' && chars.get(i + 1) == Some(&'*') {
            Some(skip_to(i + 2, &['*', '/']))
        } else if c == '\'' || c == '"' {
            Some(skip_to(i + 1, &[c]))
        } else if c == '$' {
            let tag: String = chars[i + 1..].iter().take_while(|t| t.is_alphanumeric() || **t == '_').collect();
            let open = 1 + tag.chars().count();
            (chars.get(i + open) == Some(&'$') && !tag.starts_with(|t: char| t.is_ascii_digit())).then(|| skip_to(i + open + 1, &chars[i..=i + open]))
        } else {
            None
        };
        if let Some(next) = skipped {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word).to_uppercase());
            }
            i = next;
            continue;
        }
        if c.is_alphanumeric() || c == '_' {
            word.push(c);
        } else if !word.is_empty() {
            words.push(std::mem::take(&mut word).to_uppercase());
        }
        i += 1;
    }
    if !word.is_empty() {
        words.push(word.to_uppercase());
    }
    words
}

/// Statements that PostgreSQL refuses inside a transaction block.
pub fn needs_no_transaction(sql: &str) -> bool {
    const PAIRS: [(&str, &str); 5] = [("CREATE", "DATABASE"), ("DROP", "DATABASE"), ("ALTER", "SYSTEM"), ("CREATE", "TABLESPACE"), ("DROP", "TABLESPACE")];
    let words = code_words(sql);
    words.iter().any(|w| w == "CONCURRENTLY" || w == "VACUUM") || words.windows(2).any(|w| PAIRS.contains(&(w[0].as_str(), w[1].as_str())))
}

/// Renders operations `range` (numbers from 1, both ends included; all by default) of `file`. `start`
/// is the state before the file. A reverse that is missing, a file that is missing and a statement
/// that needs `"atomic": false` are errors here, so the caller finds them before it changes anything.
pub fn render(name: &str, dir: &Path, file: &MigrationFile, start: &DbSchema, direction: Direction, range: Option<(usize, usize)>) -> Result<Rendered, String> {
    let ops = &file.operations;
    let (a, b) = range.unwrap_or((1, ops.len()));
    if ops.is_empty() {
        return Ok(Rendered::default());
    }
    if a < 1 || a > b || b > ops.len() {
        return Err(format!("{name}: operations {a}-{b} are outside 1-{}", ops.len()));
    }
    let mut state = start.clone();
    build(&ops[..a - 1], &|i| i + 1, &mut state, name)?;
    let units = build(&ops[a - 1..b], &|i| a + i, &mut state, name)?;
    let order: Vec<&Unit> = if direction == Direction::Up { units.iter().collect() } else { units.iter().rev().collect() };
    let mut groups = vec![];
    let mut current = Group { atomic: true, items: vec![] };
    for unit in order {
        let made = item(unit, direction, dir, name)?;
        if matches!(unit, Unit::Commit) {
            groups.push(std::mem::replace(&mut current, Group { atomic: true, items: vec![] }));
        } else if !atomic_of(unit) {
            groups.push(std::mem::replace(&mut current, Group { atomic: true, items: vec![] }));
            if let Some(i) = made {
                groups.push(Group { atomic: false, items: vec![i] });
            }
        } else if let Some(i) = made {
            current.items.push(i);
        }
    }
    groups.push(current);
    groups.retain(|g| !g.items.is_empty());
    for g in groups.iter().filter(|g| g.atomic) {
        for i in &g.items {
            if let Body::Sql(steps) = &i.body {
                if let Some(s) = steps.iter().find(|s| needs_no_transaction(&s.sql)) {
                    return Err(format!("{name}: operation {} runs a statement that PostgreSQL refuses inside a transaction ({}); set \"atomic\": false on it", i.first, s.summary));
                }
            }
        }
    }
    Ok(Rendered { groups })
}

#[cfg(test)]
mod tests {
    use super::{build, needs_no_transaction, Unit};
    use crate::migrate::diff::Op;
    use crate::migrate::model::{DbSchema, Table};
    use crate::migrate::ops::{OpKind, Operation};

    fn create(name: &str) -> Operation {
        Operation::schema(Op::CreateTable { table: Table { name: name.into(), ..Default::default() }, foreign_keys: vec![] })
    }

    #[test]
    fn a_separate_with_no_database_list_keeps_the_group_open() {
        let state_only = Operation::new(OpKind::Separate { database: vec![], state: vec![create("c")] });
        let ops = [create("a"), state_only, create("b")];
        let mut state = DbSchema::default();
        let units = build(&ops, &|i| i + 1, &mut state, "0001_x").unwrap();
        assert_eq!(units.len(), 1);
        let Unit::Schema { ops, last, .. } = &units[0] else { panic!("one schema unit") };
        assert_eq!((ops.len(), *last), (2, 3));
        assert_eq!(state.tables.len(), 3);
    }

    #[test]
    fn a_forbidden_statement_is_found_in_code_only() {
        assert!(needs_no_transaction("CREATE INDEX CONCURRENTLY i ON t (a)"));
        assert!(needs_no_transaction("-- note\n  vacuum analyze t"));
        assert!(needs_no_transaction("drop   database  x"));
        assert!(needs_no_transaction("SELECT 1; /* c */ ALTER SYSTEM SET work_mem = '1MB'"));
        assert!(!needs_no_transaction("UPDATE t SET vacuum_at = now()"));
        assert!(!needs_no_transaction("INSERT INTO log (msg) VALUES ('run VACUUM later; CREATE DATABASE')"));
        assert!(!needs_no_transaction("SELECT \"concurrently\" FROM t -- VACUUM"));
        assert!(!needs_no_transaction("DO $body$ BEGIN PERFORM 'VACUUM'; END $body$"));
        assert!(!needs_no_transaction("SELECT $1, 'x'"));
    }
}
