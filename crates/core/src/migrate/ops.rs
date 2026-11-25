//! The operation list of a migration file: the JSON form of a migration.
//!
//! ```json
//! { "version": 1, "operations": [ { "add_column": { "table": "post", "column": { … } } },
//!                                  { "commit": {} },
//!                                  { "run_sql": { "sql": "…", "reverse_sql": "…" } } ] }
//! ```
//!
//! A schema operation is a [`diff::Op`]. The other kinds are `run_sql`, `run_code`, `separate`,
//! `commit` and `state`. `atomic` and `source` sit beside the kind key.

use serde::de::Error as _;
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

use super::diff::Op;

/// The format number of a migration file.
pub const FORMAT_VERSION: u32 = 1;

/// The reverse that means "nothing to undo".
pub const NOOP: &str = "noop";

/// One migration file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationFile {
    pub version: u32,
    /// The names of the migrations that this file replaces (a squash).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replaces: Vec<String>,
    pub operations: Vec<Operation>,
}

impl MigrationFile {
    pub fn new(operations: Vec<Operation>) -> MigrationFile {
        MigrationFile { version: FORMAT_VERSION, replaces: vec![], operations }
    }

    /// Reads the text of a migration file and checks its format number.
    pub fn parse(text: &str) -> Result<MigrationFile, String> {
        let value: Value = serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?;
        match value.get("version").and_then(Value::as_u64) {
            Some(v) if v == u64::from(FORMAT_VERSION) => {}
            Some(v) => return Err(format!("migration format version {v} is not supported (this MonOrm reads version {FORMAT_VERSION})")),
            None => return Err("missing \"version\"".to_owned()),
        }
        serde_json::from_value(value).map_err(|e| e.to_string())
    }

    /// The canonical text of the file: the same operations always give the same text.
    pub fn render(&self) -> String {
        let mut text = serde_json::to_string_pretty(self).expect("a migration file is always valid JSON");
        text.push('\n');
        text
    }
}

/// One entry of the operation list.
#[derive(Clone, Debug, PartialEq)]
pub struct Operation {
    pub kind: OpKind,
    /// `false` runs the operation alone, outside a transaction.
    pub atomic: bool,
    /// The extension that added the object. It has no effect on apply.
    pub source: Option<String>,
}

impl Operation {
    pub fn new(kind: OpKind) -> Operation {
        Operation { kind, atomic: true, source: None }
    }

    pub fn schema(op: Op) -> Operation {
        Operation::new(OpKind::Schema(op))
    }

    /// One line that says what the operation does.
    pub fn summary(&self) -> String {
        match &self.kind {
            OpKind::Schema(op) => super::pg::summary(op),
            OpKind::RunSql(_) => "run SQL".into(),
            OpKind::RunCode(_) => "run code".into(),
            OpKind::Separate { .. } => "separate database and state".into(),
            OpKind::Commit => "commit".into(),
        }
    }

    pub fn warning(&self) -> Option<String> {
        match &self.kind {
            OpKind::Schema(op) => op.warning(),
            _ => None,
        }
    }

    /// Whether the operation can lose rows.
    pub fn destructive(&self) -> bool {
        matches!(&self.kind, OpKind::Schema(op) if op.destructive().is_some())
    }
}

#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum OpKind {
    Schema(Op),
    RunSql(RunSql),
    RunCode(RunCode),
    /// `database` runs on the database, `state` only changes the replayed state.
    Separate { database: Vec<Operation>, state: Vec<Operation> },
    /// Ends the transaction group.
    Commit,
}

/// What a reverse is for a step with SQL or code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reverse {
    Missing,
    Noop,
    Sql(String),
    File(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunSql {
    /// Inline SQL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sql: Option<String>,
    /// A `.sql` file, relative to the migrations directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reverse_sql: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reverse_file: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub elidable: bool,
}

impl RunSql {
    pub fn reverse(&self) -> Reverse {
        match (&self.reverse_sql, &self.reverse_file) {
            (Some(s), _) if s == NOOP => Reverse::Noop,
            (Some(s), _) => Reverse::Sql(s.clone()),
            (None, Some(f)) => Reverse::File(f.clone()),
            (None, None) => Reverse::Missing,
        }
    }

    fn check(&self) -> Result<(), String> {
        if self.sql.is_some() == self.file.is_some() {
            return Err("run_sql needs exactly one of \"sql\" and \"file\"".to_owned());
        }
        if self.reverse_sql.is_some() && self.reverse_file.is_some() {
            return Err("run_sql takes only one of \"reverse_sql\" and \"reverse_file\"".to_owned());
        }
        Ok(())
    }
}

/// The reverse of a `run_code` step: `"noop"`, or one function for each language.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CodeReverse {
    Noop(String),
    Code(CodeRef),
}

/// One function for each language, as `file:function`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub typescript: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunCode {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub typescript: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reverse: Option<CodeReverse>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub elidable: bool,
}

impl RunCode {
    pub fn is_noop_reverse(&self) -> bool {
        matches!(&self.reverse, Some(CodeReverse::Noop(s)) if s == NOOP)
    }

    fn check(&self) -> Result<(), String> {
        if self.python.is_none() && self.typescript.is_none() {
            return Err("run_code needs \"python\", \"typescript\" or both".to_owned());
        }
        for f in [&self.python, &self.typescript].into_iter().flatten() {
            if !f.contains(':') { return Err(format!("run_code entry {f:?} is not \"file:function\"")); }
        }
        if let Some(CodeReverse::Noop(s)) = &self.reverse {
            if s != NOOP { return Err(format!("run_code \"reverse\" is \"noop\" or an object, not {s:?}")); }
        }
        Ok(())
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SeparateBody {
    #[serde(default)]
    database: Vec<Operation>,
    #[serde(default)]
    state: Vec<Operation>,
}

#[derive(Serialize)]
struct SeparateOut<'a> {
    database: &'a [Operation],
    state: &'a [Operation],
}

impl Serialize for Operation {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let (key, body): (String, Value) = match &self.kind {
            OpKind::Schema(op) => {
                let Value::Object(m) = serde_json::to_value(op).map_err(serde::ser::Error::custom)? else {
                    return Err(serde::ser::Error::custom("a schema operation serializes to one key"));
                };
                m.into_iter().next().ok_or_else(|| serde::ser::Error::custom("empty operation"))?
            }
            OpKind::RunSql(r) => ("run_sql".into(), serde_json::to_value(r).map_err(serde::ser::Error::custom)?),
            OpKind::RunCode(r) => ("run_code".into(), serde_json::to_value(r).map_err(serde::ser::Error::custom)?),
            OpKind::Separate { database, state } => {
                ("separate".into(), serde_json::to_value(SeparateOut { database, state }).map_err(serde::ser::Error::custom)?)
            }
            OpKind::Commit => ("commit".into(), Value::Object(Map::new())),
        };
        let mut m = s.serialize_map(None)?;
        m.serialize_entry(&key, &body)?;
        if !self.atomic { m.serialize_entry("atomic", &false)?; }
        if let Some(src) = &self.source { m.serialize_entry("source", src)?; }
        m.end()
    }
}

impl<'de> Deserialize<'de> for Operation {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Operation, D::Error> {
        let Value::Object(mut m) = Value::deserialize(d)? else {
            return Err(D::Error::custom("an operation is an object with one operation key"));
        };
        let atomic = match m.remove("atomic") {
            None => true,
            Some(Value::Bool(b)) => b,
            Some(_) => return Err(D::Error::custom("\"atomic\" is true or false")),
        };
        let source = match m.remove("source") {
            None => None,
            Some(Value::String(s)) => Some(s),
            Some(_) => return Err(D::Error::custom("\"source\" is a string")),
        };
        if m.len() != 1 {
            let keys: Vec<&str> = m.keys().map(String::as_str).collect();
            return Err(D::Error::custom(format!("an operation has exactly one operation key, found {keys:?}")));
        }
        let (key, body) = m.into_iter().next().expect("one key");
        let kind = match key.as_str() {
            "run_sql" => {
                let r: RunSql = serde_json::from_value(body).map_err(|e| D::Error::custom(format!("run_sql: {e}")))?;
                r.check().map_err(D::Error::custom)?;
                OpKind::RunSql(r)
            }
            "run_code" => {
                let r: RunCode = serde_json::from_value(body).map_err(|e| D::Error::custom(format!("run_code: {e}")))?;
                r.check().map_err(D::Error::custom)?;
                OpKind::RunCode(r)
            }
            "separate" => {
                let b: SeparateBody = serde_json::from_value(body).map_err(|e| D::Error::custom(format!("separate: {e}")))?;
                OpKind::Separate { database: b.database, state: b.state }
            }
            "commit" => {
                if body.as_object().is_none_or(|o| !o.is_empty()) { return Err(D::Error::custom("\"commit\" takes an empty object")); }
                OpKind::Commit
            }
            k if k.contains('.') => {
                return Err(D::Error::custom(format!("operation key {k:?} is reserved for extension operations")));
            }
            k => {
                let known = k.to_owned();
                let one = Value::Object(Map::from_iter([(key, body)]));
                let op: Op = serde_json::from_value(one).map_err(|e| {
                    let msg = e.to_string();
                    if msg.contains("unknown variant") { D::Error::custom(format!("unknown operation {known:?}")) } else { D::Error::custom(format!("{known}: {msg}")) }
                })?;
                OpKind::Schema(op)
            }
        };
        Ok(Operation { kind, atomic, source })
    }
}

#[cfg(test)]
mod tests {
    use super::MigrationFile;

    #[test]
    fn the_format_number_is_checked() {
        assert!(MigrationFile::parse(r#"{"version": 1, "operations": []}"#).is_ok());
        assert!(MigrationFile::parse(r#"{"version": 2, "operations": []}"#).unwrap_err().contains("version 2 is not supported"));
        assert!(MigrationFile::parse(r#"{"operations": []}"#).unwrap_err().contains("missing \"version\""));
        assert!(MigrationFile::parse("[").unwrap_err().contains("not valid JSON"));
    }
}
