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

pub fn checksum(text: &str) -> String {
    Sha256::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}
