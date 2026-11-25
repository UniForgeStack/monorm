//! The migrations directory: one JSON file per migration.
//!
//! ```text
//! migrations/
//!     0001_initial.json     the operation list (see [`super::ops`])
//!     0002_backfill.json
//!     0002_backfill.sql     a script that an operation names, next to the file
//! ```
//!
//! The state of the schema before a migration is the replay of the operations of the migrations
//! before it, so generating the next migration needs no database. Applying migrations is the job of
//! the engine and the bindings (they own connections).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::model::{DbSchema, SNAPSHOT_VERSION};
use super::ops::{FORMAT_VERSION, MigrationFile, OpKind, Operation};
use super::{generate, pg, replay};
use crate::dialect::Dialect;
use crate::schema::{Result, Schema};

#[derive(Debug, Clone)]
pub struct MigrationEntry {
    /// The file name without `.json`.
    pub name: String,
    pub path: PathBuf,
}

impl MigrationEntry {
    /// The directory that holds the file, and the scripts it names.
    pub fn dir(&self) -> &Path {
        self.path.parent().unwrap_or(Path::new("."))
    }
}

fn io(path: &Path, e: std::io::Error) -> String {
    format!("{}: {e}", path.display())
}

/// The order key of a name: the number in its leading digits.
pub fn order_key(name: &str) -> Option<u64> {
    let digits: String = name.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() { None } else { digits.parse().ok() }
}

const OLD_FORMAT: &str = "is a migration folder of the old format; delete the migration history and run `baseline`";

/// The start state of a history: no tables.
pub fn empty_state(dialect: Dialect) -> DbSchema {
    DbSchema { dialect, version: SNAPSHOT_VERSION, ..Default::default() }
}

/// The migrations of `dir` in order. A missing directory holds none.
pub fn list(dir: &Path) -> Result<Vec<MigrationEntry>> {
    let Ok(read) = std::fs::read_dir(dir) else { return Ok(vec![]) };
    let mut found = vec![];
    for entry in read {
        let entry = entry.map_err(|e| io(dir, e))?;
        let path = entry.path();
        let Some(file) = path.file_name().and_then(|n| n.to_str()) else { continue };
        if order_key(file).is_none() { continue; }
        if path.is_dir() {
            if path.join("up.sql").exists() { return Err(format!("{} {OLD_FORMAT}", path.display())); }
            continue;
        }
        if let Some(stem) = file.strip_suffix(".json") {
            found.push(MigrationEntry { name: stem.to_owned(), path });
        }
    }
    found.sort_by(|a, b| (order_key(&a.name), &a.name).cmp(&(order_key(&b.name), &b.name)));
    check_unique(&found)?;
    Ok(found)
}

/// Two migrations with one order key are an error, unless a squash file replaces the other.
fn check_unique(found: &[MigrationEntry]) -> Result<()> {
    for pair in found.windows(2) {
        if order_key(&pair[0].name) != order_key(&pair[1].name) { continue; }
        let replaced = |a: &MigrationEntry, b: &MigrationEntry| read(a).is_ok_and(|f| f.replaces.contains(&b.name));
        if replaced(&pair[0], &pair[1]) || replaced(&pair[1], &pair[0]) { continue; }
        return Err(format!("migrations {} and {} have the same order key; rename one", pair[0].name, pair[1].name));
    }
    Ok(())
}

/// The migration `name`: the file name, or the order key alone (a number).
pub fn find(dir: &Path, name: &str) -> Result<MigrationEntry> {
    let all = list(dir)?;
    let name = name.strip_suffix(".json").unwrap_or(name);
    let key = (!name.is_empty() && name.chars().all(|c| c.is_ascii_digit())).then(|| name.parse::<u64>().ok()).flatten();
    let hit = all.iter().find(|m| m.name == name).or_else(|| {
        let key = key?;
        let same: Vec<&MigrationEntry> = all.iter().filter(|m| order_key(&m.name) == Some(key)).collect();
        // a squash file and the files it replaces share a key: the squash file is the one
        same.iter().find(|m| read(m).is_ok_and(|f| !f.replaces.is_empty())).or(same.first()).copied()
    });
    if let Some(m) = hit { return Ok(m.clone()); }
    for m in &all {
        if read(m).is_ok_and(|f| f.replaces.iter().any(|r| r == name)) {
            return Err(format!("migration {name} is replaced by the squash file {}", m.name));
        }
    }
    Err(format!("no migration named {name} in {}", dir.display()))
}

/// Reads and checks one migration file.
pub fn read(entry: &MigrationEntry) -> Result<MigrationFile> {
    let text = std::fs::read_to_string(&entry.path).map_err(|e| io(&entry.path, e))?;
    MigrationFile::parse(&text).map_err(|e| format!("{}: {e}", entry.path.display()))
}

/// The files of `dir` in order, with the files that a squash file replaces left out.
pub fn effective(dir: &Path) -> Result<Vec<(MigrationEntry, MigrationFile)>> {
    let all: Vec<(MigrationEntry, MigrationFile)> = list(dir)?.into_iter().map(|m| read(&m).map(|f| (m, f))).collect::<Result<_>>()?;
    let skip: HashSet<&String> = all.iter().flat_map(|(_, f)| f.replaces.iter()).collect();
    Ok(all.iter().filter(|(m, _)| !skip.contains(&m.name)).cloned().collect())
}

/// The state after the migrations of `dir`.
pub fn latest_state(dir: &Path, dialect: Dialect) -> Result<DbSchema> {
    let mut state = empty_state(dialect);
    for (m, f) in effective(dir)? {
        replay::replay(&mut state, &f.operations).map_err(|e| format!("{}: {e}", m.name))?;
    }
    Ok(state)
}

/// The state before the migration `name`: the replay of the migrations that sort before it.
pub fn state_before(dir: &Path, name: &str, dialect: Dialect) -> Result<DbSchema> {
    let all: Vec<(MigrationEntry, MigrationFile)> = list(dir)?.into_iter().map(|m| read(&m).map(|f| (m, f))).collect::<Result<_>>()?;
    let key = order_key(name).ok_or_else(|| format!("{name} has no order key"))?;
    // the files that replace `name` hold it already
    let skip: HashSet<&String> = all.iter().filter(|(_, f)| !f.replaces.iter().any(|r| r == name)).flat_map(|(_, f)| f.replaces.iter()).collect();
    let mut state = empty_state(dialect);
    for (m, f) in &all {
        let covers = f.replaces.iter().any(|r| r == name);
        if skip.contains(&m.name) || covers || m.name == name || order_key(&m.name).is_some_and(|k| k >= key) { continue; }
        replay::replay(&mut state, &f.operations).map_err(|e| format!("{}: {e}", m.name))?;
    }
    Ok(state)
}

/// The lowercase alphanumeric words of `text`, joined by `_`, at most 40 characters.
pub fn slug(text: &str) -> String {
    let mut s = String::new();
    for c in text.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c);
        } else if !s.ends_with('_') {
            s.push('_');
        }
    }
    let s: String = s.trim_matches('_').chars().take(40).collect();
    let s = s.trim_end_matches('_').to_owned();
    if s.is_empty() { "auto".into() } else { s }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Part {
    Number,
    Timestamp,
    Slug,
    Literal(String),
}

/// The template of new file names: `{number}_{slug}` by default. It starts with one order key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NameTemplate {
    parts: Vec<Part>,
    text: String,
}

impl Default for NameTemplate {
    fn default() -> NameTemplate {
        NameTemplate::parse("{number}_{slug}").expect("the default template is valid")
    }
}

impl NameTemplate {
    /// `{number}` (4 digits), `{timestamp}` (UTC `YYYYMMDDHHMMSS`) and `{slug}`; `_` and `-` separate.
    pub fn parse(text: &str) -> Result<NameTemplate> {
        let bad = |why: &str| format!("migration name template {text:?}: {why}");
        let mut parts = vec![];
        let mut rest = text;
        while !rest.is_empty() {
            if let Some(after) = rest.strip_prefix('{') {
                let end = after.find('}').ok_or_else(|| bad("a `{` has no `}`"))?;
                parts.push(match &after[..end] {
                    "number" => Part::Number,
                    "timestamp" => Part::Timestamp,
                    "slug" => Part::Slug,
                    other => return Err(bad(&format!("unknown placeholder {{{other}}}; use {{number}}, {{timestamp}} or {{slug}}"))),
                });
                rest = &after[end + 1..];
            } else {
                let end = rest.find('{').unwrap_or(rest.len());
                let lit = &rest[..end];
                if let Some(c) = lit.chars().find(|c| !(c.is_ascii_alphanumeric() || *c == '_' || *c == '-')) {
                    return Err(bad(&format!("the character {c:?} is not allowed in a file name here")));
                }
                parts.push(Part::Literal(lit.to_owned()));
                rest = &rest[end..];
            }
        }
        if !matches!(parts.first(), Some(Part::Number | Part::Timestamp)) {
            return Err(bad("it must start with {number} or {timestamp}"));
        }
        if parts.iter().filter(|p| matches!(p, Part::Number | Part::Timestamp)).count() != 1 {
            return Err(bad("it needs exactly one of {number} and {timestamp}"));
        }
        let digits_run_on = parts.windows(2).any(|w| matches!(w[0], Part::Number | Part::Timestamp) && !matches!(&w[1], Part::Literal(l) if l.starts_with(['_', '-'])));
        if digits_run_on {
            return Err(bad("{number} and {timestamp} must be followed by `_` or `-`, or end the template"));
        }
        if parts.iter().filter(|p| **p == Part::Slug).count() > 1 {
            return Err(bad("{slug} may appear once"));
        }
        Ok(NameTemplate { parts, text: text.to_owned() })
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    fn timestamp(&self) -> bool {
        self.parts.first() == Some(&Part::Timestamp)
    }

    /// The order key of the next file: one more than the newest key, or the clock for `{timestamp}`
    /// (never below the newest key, so the new file sorts last).
    pub fn next_key(&self, existing: &[MigrationEntry]) -> u64 {
        let newest = existing.iter().filter_map(|m| order_key(&m.name)).max().unwrap_or(0);
        if self.timestamp() {
            let now: u64 = chrono::Utc::now().format("%Y%m%d%H%M%S").to_string().parse().unwrap_or(0);
            now.max(newest + 1)
        } else {
            newest + 1
        }
    }

    /// The file name (without `.json`) for `key` and `slug`.
    pub fn render(&self, key: u64, slug: &str) -> String {
        let mut out = String::new();
        for p in &self.parts {
            match p {
                Part::Number => out.push_str(&format!("{key:04}")),
                Part::Timestamp => out.push_str(&format!("{key:014}")),
                Part::Slug => out.push_str(slug),
                Part::Literal(l) => out.push_str(l),
            }
        }
        // a template without a slug (or with an empty one) leaves no separator at the end
        out.trim_end_matches(['_', '-']).to_owned()
    }
}

pub struct MakeOptions<'a> {
    pub name: Option<&'a str>,
    pub template: &'a NameTemplate,
    /// Write a file with no operations when there is no change.
    pub empty: bool,
    pub online: bool,
    pub baseline: bool,
}

impl Default for MakeOptions<'_> {
    fn default() -> Self {
        static DEFAULT: std::sync::OnceLock<NameTemplate> = std::sync::OnceLock::new();
        MakeOptions { name: None, template: DEFAULT.get_or_init(NameTemplate::default), empty: false, online: false, baseline: false }
    }
}

/// The operations of the next migration for `schema`, with the file name it would get.
pub fn next(dir: &Path, schema: &Schema, opts: &MakeOptions) -> Result<(String, MigrationFile)> {
    let existing = list(dir)?;
    let previous = latest_state(dir, schema.dialect)?;
    let operations = generate(schema, &previous, opts.baseline, opts.online)?;
    let label = match opts.name {
        Some(n) => slug(n),
        None if existing.is_empty() => "initial".into(),
        None => operations.iter().find_map(|o| match &o.kind { OpKind::Schema(op) => Some(slug(&pg::summary(op))), _ => None }).unwrap_or_else(|| "manual".into()),
    };
    let name = opts.template.render(opts.template.next_key(&existing), &label);
    Ok((name, MigrationFile::new(operations)))
}

/// Writes the next migration. `None` when there is nothing to change and `empty` is off.
pub fn make(dir: &Path, schema: &Schema, opts: &MakeOptions) -> Result<Option<(MigrationEntry, MigrationFile)>> {
    let (name, file) = next(dir, schema, opts)?;
    if file.operations.is_empty() && !opts.empty { return Ok(None); }
    std::fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
    let path = dir.join(format!("{name}.json"));
    std::fs::write(&path, file.render()).map_err(|e| io(&path, e))?;
    Ok(Some((MigrationEntry { name, path }, file)))
}

/// The operations of a barrier-free run from an empty database: the diff to the state the run
/// reaches, plus one `separate` operation with no `database` list for the state that no schema
/// operation carries.
fn fold(dialect: Dialect, run: &[&Operation]) -> Result<Vec<Operation>> {
    if run.is_empty() { return Ok(vec![]); }
    let start = empty_state(dialect);
    let mut target = start.clone();
    for op in run { replay::apply_operation(&mut target, op)?; }
    let list = super::diff::diff(&start, &target, &Default::default())?;
    let mut replayed = start;
    let mut operations: Vec<Operation> = list.into_iter().map(Operation::schema).collect();
    replay::replay(&mut replayed, &operations)?;
    operations.extend(replay::state_operation(&replayed, &target));
    Ok(operations)
}

/// Folds the migrations of `dir` up to `to` (a name or a number) into one file that lists them in
/// `replaces`. The first run of schema operations (before any barrier) becomes the diff from an
/// empty database, because the squash file only runs on one. A `run_sql` or `run_code` that is
/// `elidable` is dropped; one that is not stops the squash. `separate`, `commit` and every
/// operation after a barrier stay as written, so a rename after data was written stays a rename.
pub fn squash(dir: &Path, dialect: Dialect, to: &str, name: Option<&str>, template: &NameTemplate) -> Result<(MigrationEntry, MigrationFile)> {
    let last = find(dir, to)?;
    let last_key = order_key(&last.name).ok_or_else(|| format!("{} has no order key", last.name))?;
    let folded: Vec<(MigrationEntry, MigrationFile)> = effective(dir)?.into_iter().filter(|(m, _)| order_key(&m.name).is_some_and(|k| k <= last_key)).collect();
    if folded.len() < 2 { return Err(format!("squashing needs two or more migrations up to {}; found {}", last.name, folded.len())); }
    let mut operations: Vec<Operation> = vec![];
    let mut run: Vec<&Operation> = vec![];
    let mut first = true;
    let flush = |run: &mut Vec<&Operation>, first: &mut bool, operations: &mut Vec<Operation>| -> Result<()> {
        if *first { operations.extend(fold(dialect, run)?); } else { operations.extend(run.iter().map(|o| (*o).clone())); }
        run.clear();
        *first = false;
        Ok(())
    };
    for (m, f) in &folded {
        for op in &f.operations {
            match &op.kind {
                OpKind::RunSql(r) if r.elidable => flush(&mut run, &mut first, &mut operations)?,
                OpKind::RunCode(r) if r.elidable => flush(&mut run, &mut first, &mut operations)?,
                OpKind::RunSql(_) | OpKind::RunCode(_) => {
                    return Err(format!("{} has the operation `{}`, which is not elidable; mark it \"elidable\": true to drop it from the squash, or keep the migrations", m.name, op.summary()));
                }
                OpKind::Separate { .. } | OpKind::Commit => {
                    flush(&mut run, &mut first, &mut operations)?;
                    operations.push(op.clone());
                }
                _ => run.push(op),
            }
        }
    }
    flush(&mut run, &mut first, &mut operations)?;
    let key = order_key(&folded[0].0.name).unwrap_or(1);
    let label = match name {
        Some(n) => slug(n),
        None => format!("squashed_{last_key:04}"),
    };
    let new_name = template.render(key, &label);
    let path = dir.join(format!("{new_name}.json"));
    if path.exists() { return Err(format!("{} exists; pass another --name", path.display())); }
    let mut replaces: Vec<String> = vec![];
    for (m, f) in &folded {
        replaces.extend(f.replaces.iter().cloned());
        replaces.push(m.name.clone());
    }
    replaces.sort();
    replaces.dedup();
    let file = MigrationFile { version: FORMAT_VERSION, replaces, operations };
    std::fs::write(&path, file.render()).map_err(|e| io(&path, e))?;
    Ok((MigrationEntry { name: new_name, path }, file))
}

fn sql_files(ops: &[Operation], out: &mut Vec<String>) {
    for op in ops {
        match &op.kind {
            OpKind::RunSql(r) => out.extend(r.file.iter().chain(r.reverse_file.iter()).cloned()),
            OpKind::Separate { database, state } => { sql_files(database, out); sql_files(state, out); }
            _ => {}
        }
    }
}

/// The SHA-256 of a migration: its canonical JSON and the `.sql` files it names, in operation order.
/// A `run_code` script is not part of it.
pub fn file_checksum(entry: &MigrationEntry) -> Result<String> {
    let file = read(entry)?;
    let mut hash = Sha256::new();
    hash.update(file.render().as_bytes());
    let mut names = vec![];
    sql_files(&file.operations, &mut names);
    for n in names {
        let path = entry.dir().join(&n);
        hash.update(std::fs::read(&path).map_err(|e| io(&path, e))?);
    }
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrate::render::Direction;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn temp() -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!("monorm-files-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn put(dir: &Path, name: &str, text: &str) {
        std::fs::write(dir.join(format!("{name}.json")), text).unwrap();
    }

    fn file(ops: &str) -> String {
        format!(r#"{{"version": 1, "operations": [{ops}]}}"#)
    }

    const CREATE: &str = r#"{"create_table": {"table": {"name": "t", "columns": [{"name": "id", "type": "bigint"}]}, "foreign_keys": []}}"#;
    const ADD: &str = r#"{"add_column": {"table": "t", "column": {"name": "a", "type": "text", "nullable": true}}}"#;
    const ADD_B: &str = r#"{"add_column": {"table": "t", "column": {"name": "b", "type": "text", "nullable": true}}}"#;
    const DROP: &str = r#"{"drop_column": {"table": "t", "column": "a", "generated": false}}"#;
    const RENAME: &str = r#"{"rename_column": {"table": "t", "from": "b", "to": "c"}}"#;

    fn entry(name: &str) -> MigrationEntry {
        MigrationEntry { name: name.into(), path: PathBuf::from(format!("{name}.json")) }
    }

    fn squash_default(dir: &Path, to: &str) -> Result<(MigrationEntry, MigrationFile)> {
        squash(dir, Dialect::Postgres, to, None, &NameTemplate::default())
    }

    fn kinds(file: &MigrationFile) -> Vec<String> {
        file.operations.iter().map(|o| o.summary()).collect()
    }

    #[test]
    fn name_template_renders_number_timestamp_and_slug() {
        assert_eq!(NameTemplate::default().render(7, "add_x"), "0007_add_x");
        let t = NameTemplate::parse("{timestamp}_{slug}").unwrap();
        assert_eq!(t.render(20260102030405, "x"), "20260102030405_x");
        let t = NameTemplate::parse("m{number}-{slug}").unwrap_err();
        assert!(t.contains("must start with"), "{t}");
    }

    #[test]
    fn name_template_without_a_slug_leaves_no_separator() {
        let t = NameTemplate::parse("{number}_{slug}").unwrap();
        assert_eq!(t.render(3, ""), "0003");
        assert_eq!(NameTemplate::parse("{number}").unwrap().render(3, "ignored"), "0003");
    }

    #[test]
    fn name_template_rejects_bad_templates() {
        for (text, why) in [
            ("{number}_{nope}", "unknown placeholder"),
            ("{slug}", "must start with"),
            ("{number}_{timestamp}", "exactly one"),
            ("{number}_{slug}_{slug}", "may appear once"),
            ("{number", "no `}`"),
            ("{number}/x", "not allowed"),
            ("{number}{slug}", "followed by"),
            ("{number}2_{slug}", "followed by"),
        ] {
            let e = NameTemplate::parse(text).unwrap_err();
            assert!(e.contains(why), "{text}: {e}");
        }
    }

    #[test]
    fn the_next_key_follows_the_newest_in_a_mixed_history() {
        let existing = [entry("0001_a"), entry("20260101000000_b"), entry("0002_c")];
        assert_eq!(NameTemplate::default().next_key(&existing), 20260101000001);
        let stamp = NameTemplate::parse("{timestamp}_{slug}").unwrap();
        assert!(stamp.next_key(&existing) > 20260101000000);
        assert_eq!(NameTemplate::default().next_key(&[]), 1);
    }

    #[test]
    fn order_is_the_number_of_the_leading_digits() {
        let dir = temp();
        for n in ["0002_b", "0010_c", "0001_a"] { put(&dir, n, &file("")); }
        let names: Vec<String> = list(&dir).unwrap().into_iter().map(|m| m.name).collect();
        assert_eq!(names, ["0001_a", "0002_b", "0010_c"]);
        assert_eq!(find(&dir, "10").unwrap().name, "0010_c");
    }

    #[test]
    fn a_duplicate_order_key_is_an_error() {
        let dir = temp();
        put(&dir, "0001_a", &file(CREATE));
        put(&dir, "0001_b", &file(""));
        let e = list(&dir).unwrap_err();
        assert!(e.contains("same order key"), "{e}");
    }

    #[test]
    fn an_old_migration_folder_gets_a_clear_error() {
        let dir = temp();
        std::fs::create_dir_all(dir.join("0001_initial")).unwrap();
        std::fs::write(dir.join("0001_initial/up.sql"), "SELECT 1").unwrap();
        let e = list(&dir).unwrap_err();
        assert!(e.contains("old format") && e.contains("baseline"), "{e}");
    }

    #[test]
    fn a_file_checks_its_version_and_its_operation_names() {
        assert!(MigrationFile::parse(r#"{"operations": []}"#).unwrap_err().contains("missing \"version\""));
        assert!(MigrationFile::parse(r#"{"version": 2, "operations": []}"#).unwrap_err().contains("version 2 is not supported"));
        assert!(MigrationFile::parse(r#"{"version": 1, "operations": [{"no_such_op": {}}]}"#).is_err());
        assert!(MigrationFile::parse("nope").unwrap_err().contains("not valid JSON"));
        let f = MigrationFile::parse(&file(&format!("{CREATE}, {ADD}"))).unwrap();
        assert_eq!(MigrationFile::parse(&f.render()).unwrap(), f);
    }

    #[test]
    fn run_code_cannot_be_outside_a_transaction() {
        let dir = temp();
        put(&dir, "0001_a", &file(r#"{"run_code": {"python": "fix.py:run", "reverse": "noop"}, "atomic": false}"#));
        let e = crate::migrate::render_migration(&dir, "0001_a", Dialect::Postgres, Direction::Up).unwrap_err();
        assert!(e.contains("run_code cannot be"), "{e}");
    }

    #[test]
    fn a_squash_folds_the_first_run_into_one_diff() {
        let dir = temp();
        put(&dir, "0001_a", &file(CREATE));
        put(&dir, "0002_b", &file(&format!("{ADD}, {ADD_B}")));
        put(&dir, "0003_c", &file(DROP));
        let (made, squashed) = squash_default(&dir, "0003").unwrap();
        assert_eq!(made.name, "0001_squashed_0003");
        assert_eq!(squashed.replaces, ["0001_a", "0002_b", "0003_c"]);
        // the dropped column never reaches the squash file
        assert_eq!(squashed.operations.len(), 1);
        let json = squashed.render();
        assert!(json.contains("\"b\"") && !json.contains("\"a\""), "{json}");
        // the squash file stands for the three: the state is the same and the old files are skipped
        let names: Vec<String> = effective(&dir).unwrap().into_iter().map(|(m, _)| m.name).collect();
        assert_eq!(names, ["0001_squashed_0003"]);
        let after = latest_state(&dir, Dialect::Postgres).unwrap();
        assert_eq!(after.tables[0].columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["id", "b"]);
    }

    #[test]
    fn a_squash_keeps_what_follows_a_barrier_as_written() {
        let dir = temp();
        put(&dir, "0001_a", &file(&format!("{CREATE}, {ADD_B}")));
        put(&dir, "0002_b", &file(&format!(r#"{{"commit": {{}}}}, {RENAME}"#)));
        let (_, squashed) = squash_default(&dir, "0002").unwrap();
        // create_table (with b), then the commit, then the rename as a rename
        let k = kinds(&squashed);
        assert_eq!(k.len(), 3, "{k:?}");
        assert_eq!(k[1], "commit");
        assert!(squashed.render().contains("rename_column"));
    }

    #[test]
    fn a_squash_drops_an_elidable_operation_and_refuses_another() {
        let dir = temp();
        put(&dir, "0001_a", &file(CREATE));
        put(&dir, "0002_b", &file(r#"{"run_sql": {"sql": "UPDATE t SET id = id", "reverse_sql": "noop", "elidable": true}}"#));
        let (_, squashed) = squash_default(&dir, "0002").unwrap();
        assert!(!squashed.render().contains("run_sql"));

        let dir = temp();
        put(&dir, "0001_a", &file(CREATE));
        put(&dir, "0002_b", &file(r#"{"run_sql": {"sql": "UPDATE t SET id = id", "reverse_sql": "noop"}}"#));
        let e = squash_default(&dir, "0002").unwrap_err();
        assert!(e.contains("0002_b") && e.contains("not elidable"), "{e}");
        assert_eq!(list(&dir).unwrap().len(), 2, "a refused squash writes nothing");
    }

    #[test]
    fn a_squash_needs_two_migrations_and_a_free_name() {
        let dir = temp();
        put(&dir, "0001_a", &file(CREATE));
        assert!(squash_default(&dir, "0001").unwrap_err().contains("two or more"));
        put(&dir, "0002_b", &file(ADD));
        put(&dir, "0003_c", &file(ADD_B));
        let e = squash(&dir, Dialect::Postgres, "0003", Some("a"), &NameTemplate::default()).unwrap_err();
        assert!(e.contains("exists"), "{e}");
    }

    #[test]
    fn a_second_squash_carries_the_first_replaces() {
        let dir = temp();
        put(&dir, "0001_a", &file(CREATE));
        put(&dir, "0002_b", &file(ADD));
        put(&dir, "0003_c", &file(ADD_B));
        let (first, _) = squash(&dir, Dialect::Postgres, "0002", Some("one"), &NameTemplate::default()).unwrap();
        assert_eq!(first.name, "0001_one");
        let (_, second) = squash(&dir, Dialect::Postgres, "0003", Some("two"), &NameTemplate::default()).unwrap();
        assert_eq!(second.replaces, ["0001_a", "0001_one", "0002_b", "0003_c"]);
    }
}
