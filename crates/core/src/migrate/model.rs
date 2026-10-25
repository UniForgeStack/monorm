//! The database-level schema: what the migration generator diffs.
//!
//! [`DbSchema`] is built from the validated ORM schema and describes tables, columns
//! and database objects in SQL terms (types, quoted-free names, raw expressions). It is
//! also the snapshot format stored next to each migration, so it only holds what the
//! database holds: rename hints live in [`Renames`] and never reach a snapshot.
//!
//! Object names follow Postgres' own conventions (`users_pkey`, `users_email_key`,
//! `posts_author_id_fkey`, `posts_author_id_idx`, `posts_views_check`), so they match
//! what Postgres would pick for the same DDL.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use crate::ext::Catalog;
use crate::ir::{
    ColType, ConstraintIr, EnumStorage, Deferrable, FieldIr, ForEach, IndexColumnIr, Nulls, OnDelete, TriggerEvent,
    TriggerTiming,
};
use crate::schema::{Model, Result, Schema};

pub const SNAPSHOT_VERSION: u32 = 4;
/// Postgres truncates identifiers longer than this (NAMEDATALEN - 1).
const MAX_IDENT: usize = 63;

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct DbSchema {
    /// Frozen ID-to-model routing is migration state even without a registry table.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identities: Option<crate::identity::IdentityManifest>,
    #[serde(default, skip_serializing_if = "crate::dialect::Dialect::is_postgres")]
    pub dialect: crate::dialect::Dialect,
    pub version: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<Extension>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub functions: Vec<Function>,
    /// Enum types (`CREATE TYPE ... AS ENUM`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enums: Vec<EnumType>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tables: Vec<Table>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct EnumType {
    pub name: String,
    /// The labels, in order.
    pub values: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

impl EnumType {
    /// The type as a column type: always quoted, so any name works.
    pub fn sql(&self) -> String {
        super::pg::ident(&self.name)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Extension {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Function {
    pub name: String,
    #[serde(default)]
    pub args: String,
    pub returns: String,
    pub language: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volatility: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub security_definer: bool,
}

impl Function {
    /// Functions are identified by name and argument types (overloads are distinct).
    pub fn key(&self) -> (String, String) {
        (self.name.clone(), normalize_ws(&self.args).to_ascii_lowercase())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Table {
    pub name: String,
    /// PostgreSQL's PARTITION BY clause, without the leading keywords.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition_of: Option<PartitionOf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    pub columns: Vec<Column>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_key: Option<PrimaryKey>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uniques: Vec<Unique>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<Check>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclusions: Vec<Exclusion>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub foreign_keys: Vec<ForeignKey>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub indexes: Vec<Index>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub triggers: Vec<Trigger>,
    /// Fills for the columns a nullable group holds. The snapshot stores them, so a downgrade
    /// that re-adds such a column can fill it; they do not take part in a snapshot comparison.
    #[serde(default, skip_serializing_if = "Backfills::is_empty")]
    pub backfills: Backfills,
}

/// A default that the database does not carry (`NestedIr::under_null`): a migration that
/// adds `column` sets it to `value` in each row where one of the `others` is not NULL.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Backfill {
    pub column: String,
    pub value: String,
    pub others: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(transparent)]
pub struct Backfills(pub Vec<Backfill>);

impl Backfills {
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl PartialEq for Backfills {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Table {
    pub fn backfill(&self, column: &str) -> Option<&Backfill> {
        self.backfills.0.iter().find(|b| b.column == column)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PartitionOf {
    pub parent: String,
    pub bound: String,
}

impl Table {
    pub fn column(&self, name: &str) -> Option<&Column> {
        self.columns.iter().find(|c| c.name == name)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Column {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub nullable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// `GENERATED BY DEFAULT AS IDENTITY`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub identity: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// The nested type a `json` column holds. It is not a database fact, so it does not take
    /// part in a snapshot comparison: only the differ reads it, to backfill a new subfield.
    #[serde(default, skip_serializing_if = "NestedLink::is_none")]
    pub nested: NestedLink,
}

/// Where a column's value comes from in the schema: the shape of a `json` column, or the
/// `(field, path)` of a `columns` leaf.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(transparent)]
pub struct NestedLink(pub Option<NestedShape>);

impl NestedLink {
    fn is_none(&self) -> bool {
        self.0.is_none()
    }
}

impl PartialEq for NestedLink {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct NestedShape {
    pub field: String,
    /// A snapshot that holds only the shape of a `json` column has no storage.
    #[serde(default = "json_storage", skip_serializing_if = "is_json_storage")]
    pub storage: crate::ir::NestedStorage,
    /// The model field is a list of the type (`json`), or the leaf is an array (`columns`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub list: bool,
    /// Subfield names from the model field to a `columns` leaf.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<String>,
    /// The type of a `columns` leaf: the database type does not tell a SQLite BigInt from an Int.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ty: Option<ColType>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shape: Vec<ShapeEntry>,
}

fn json_storage() -> crate::ir::NestedStorage {
    crate::ir::NestedStorage::Json
}

fn is_json_storage(s: &crate::ir::NestedStorage) -> bool {
    *s == crate::ir::NestedStorage::Json
}

/// One subfield of a nested type, by its path of subfield names from the model field.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ShapeEntry {
    pub path: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub nullable: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub list: bool,
    /// A nested type rather than a scalar.
    #[serde(default, skip_serializing_if = "is_false")]
    pub group: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    /// The type of a scalar subfield.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ty: Option<ColType>,
}

/// A leaf default as the writers store it in a JSON value: BigInt and Decimal as text, a DateTime in UTC with microseconds.
fn json_default(ty: crate::ir::ColType, v: &serde_json::Value) -> serde_json::Value {
    use crate::ir::ColType;
    use serde_json::Value;
    match (ty, v) {
        (ColType::BigInt | ColType::Decimal, Value::Number(n)) => Value::String(n.to_string()),
        (ColType::DateTime, Value::String(s)) => match chrono::DateTime::parse_from_rfc3339(s) {
            Ok(t) => Value::String(t.with_timezone(&chrono::Utc).format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string()),
            Err(_) => v.clone(),
        },
        _ => v.clone(),
    }
}

fn shape_of(types: &[crate::ir::TypeIr], ty: &str, path: &mut Vec<String>, out: &mut Vec<ShapeEntry>) {
    let Some(ty) = types.iter().find(|t| t.name == ty) else { return };
    for f in &ty.fields {
        path.push(f.name.clone());
        out.push(ShapeEntry {
            path: path.clone(),
            nullable: f.nullable,
            list: f.list,
            group: f.type_name.is_some(),
            default: f.leaf.as_ref().and_then(|l| l.default.as_ref().map(|v| json_default(l.ty, v))),
            ty: f.leaf.as_ref().map(|l| l.ty),
        });
        if let Some(n) = &f.type_name {
            shape_of(types, n, path, out);
        }
        path.pop();
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PrimaryKey {
    pub name: String,
    pub columns: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Unique {
    pub name: String,
    pub columns: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub nulls_not_distinct: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferrable: Option<Deferrable>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Check {
    pub name: String,
    pub expr: String,
}

/// One key of an index or exclusion constraint: a column or an expression.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct IndexKey {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expr: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opclass: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub desc: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nulls: Option<Nulls>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ExclusionElement {
    #[serde(flatten)]
    pub key: IndexKey,
    pub operator: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Exclusion {
    pub name: String,
    pub method: String,
    pub elements: Vec<ExclusionElement>,
    #[serde(default, rename = "where", skip_serializing_if = "Option::is_none")]
    pub where_: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferrable: Option<Deferrable>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ForeignKey {
    pub name: String,
    pub columns: Vec<String>,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_delete: Option<OnDelete>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_update: Option<OnDelete>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferrable: Option<Deferrable>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Index {
    pub name: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub unique: bool,
    /// Access method; `None` is btree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    pub keys: Vec<IndexKey>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<String>,
    #[serde(default, rename = "where", skip_serializing_if = "Option::is_none")]
    pub where_: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub with: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub nulls_not_distinct: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Trigger {
    pub name: String,
    pub timing: TriggerTiming,
    pub events: Vec<TriggerEvent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub update_of: Vec<String>,
    pub for_each: ForEach,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    pub function: String,
    /// SQLite executes an inline body rather than a SQL function.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
}

/// Rename hints from the ORM schema (`renamed_from`), keyed by the new names.
#[derive(Clone, Debug, Default)]
pub struct Renames {
    /// new table -> old table
    pub tables: HashMap<String, String>,
    /// (new table, new column) -> old column
    pub columns: HashMap<(String, String), String>,
}

impl Renames {
    /// The old column of `column` in `old` when a hint renames it: `old` has the hinted
    /// column and `new` no longer does.
    pub fn renamed_column(&self, new: &Table, old: &Table, column: &str) -> Option<&String> {
        self.columns.get(&(new.name.clone(), column.to_owned())).filter(|h| old.column(h).is_some() && new.column(h).is_none())
    }

    /// The same renames seen from the other side, for down migrations.
    pub fn reversed(&self) -> Renames {
        let tables: HashMap<String, String> =
            self.tables.iter().map(|(new, old)| (old.clone(), new.clone())).collect();
        let columns = self
            .columns
            .iter()
            .map(|((table, new), old)| {
                let old_table = self.tables.get(table).cloned().unwrap_or_else(|| table.clone());
                ((old_table, old.clone()), new.clone())
            })
            .collect();
        Renames { tables, columns }
    }
}

// ---------------------------------------------------------------------------------------
// Naming
// ---------------------------------------------------------------------------------------

fn fnv1a(s: &str) -> u32 {
    s.bytes().fold(0x811c_9dc5u32, |h, b| (h ^ b as u32).wrapping_mul(0x0100_0193))
}

/// `parts` joined with `_`, shortened to fit Postgres' identifier limit with a stable
/// hash suffix so two long names don't collide after truncation.
pub fn object_name(parts: &[&str]) -> String {
    let name = parts.join("_");
    if name.len() <= MAX_IDENT {
        return name;
    }
    let suffix = format!("_{:08x}", fnv1a(&name));
    let mut cut = MAX_IDENT - suffix.len();
    while !name.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}{suffix}", &name[..cut])
}

/// The database name of an index: its `map:` name, else `<table>_<keys>_idx`.
pub fn index_name(m: &Model, ix: &crate::ir::IndexIr) -> Result<String> {
    if let Some(n) = &ix.name {
        return Ok(n.clone());
    }
    let keys = ix
        .columns
        .iter()
        .map(|c| match (&c.field, &c.expr) {
            (Some(f), None) => m.field(f).map(|f| f.column.clone()).map_err(|e| e.to_string()),
            (None, Some(e)) => Ok(expression_tag(e)),
            _ => Err(format!("model {}: each index key needs exactly one of field / expr", m.ir.name)),
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(object_name(&[m.table(), &keys.join("_"), "idx"]))
}

/// The names of the unique indexes of the model: the values `Model.indexes` offers.
pub fn unique_index_names(m: &Model) -> Vec<String> {
    m.ir.indexes.iter().filter(|ix| ix.unique).filter_map(|ix| index_name(m, ix).ok()).collect()
}

/// Stable short tag for expressions in generated names.
pub(super) fn expression_tag(expr: &str) -> String {
    format!("expr{:08x}", fnv1a(&normalize_ws(expr)))
}

pub fn normalize_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

// Retain the historical layout outside quotes without changing literal contents.
fn expression_sql(sql: &str) -> String {
    let mut out = String::new();
    let mut quote = None;
    let mut space = false;
    for c in sql.trim().chars() {
        if let Some(q) = quote {
            out.push(c);
            if c == q { quote = None; }
        } else if c.is_whitespace() {
            space = true;
        } else {
            if space && !out.is_empty() { out.push(' '); }
            space = false;
            out.push(c);
            if matches!(c, '\'' | '"' | '`') { quote = Some(c); }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------
// Building from the ORM schema
// ---------------------------------------------------------------------------------------

pub fn sql_type(f: &FieldIr) -> String {
    let base = scalar_sql_type(f);
    if f.array {
        format!("{base}[]")
    } else {
        base
    }
}

fn scalar_sql_type(f: &FieldIr) -> String {
    if let Some(t) = &f.db_type {
        return normalize_ws(t);
    }
    match f.ty {
        ColType::BigInt => "bigint".into(),
        ColType::Int => "integer".into(),
        ColType::Float => "double precision".into(),
        ColType::Bool => "boolean".into(),
        ColType::String => match f.max_length {
            Some(n) => format!("varchar({n})"),
            // text[], as Prisma does: array operators take text[] parameters as they are
            None if f.array => "text".into(),
            None => "varchar".into(),
        },
        ColType::Text => "text".into(),
        ColType::DateTime => "timestamp with time zone".into(),
        ColType::Date => "date".into(),
        ColType::Uuid => "uuid".into(),
        ColType::Json => "jsonb".into(),
        ColType::Decimal => "numeric".into(),
    }
}

/// A literal as SQL: numbers and booleans as written, strings quoted.
fn literal(v: &serde_json::Value) -> Option<String> {
    Some(match v {
        serde_json::Value::Bool(b) => if *b { "true" } else { "false" }.into(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => quote_literal(s),
        _ => return None,
    })
}

pub fn quote_literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

pub fn default_sql(f: &FieldIr) -> Result<Option<String>> {
    if f.default_now {
        return Ok(Some("now()".into()));
    }
    if let Some(sql) = &f.default_sql {
        return Ok(Some(normalize_ws(sql)));
    }
    let Some(v) = &f.default else { return Ok(None) };
    if f.array {
        let serde_json::Value::Array(items) = v else {
            return Err(format!("the default of array field {} must be a list", f.name));
        };
        let ty = sql_type(f);
        if items.is_empty() {
            return Ok(Some(format!("'{{}}'::{ty}")));
        }
        let items = items
            .iter()
            .map(|i| match i {
                serde_json::Value::Array(_) | serde_json::Value::Object(_) if f.ty == ColType::Json => {
                    Some(format!("{}::jsonb", quote_literal(&i.to_string())))
                }
                i => literal(i),
            })
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| format!("unsupported default {v} for field {}", f.name))?;
        return Ok(Some(format!("ARRAY[{}]::{ty}", items.join(", "))));
    }
    Ok(Some(match v {
        other if f.ty == ColType::Json && !other.is_string() => format!("{}::jsonb", quote_literal(&other.to_string())),
        other => literal(other).ok_or_else(|| format!("unsupported default {other} for field {}", f.name))?,
    }))
}

struct Builder<'a> {
    schema: &'a Schema,
    catalog: Catalog,
    /// extension -> first object that needs it
    required: BTreeMap<String, String>,
    names: BTreeSet<String>,
    local_names: BTreeSet<(String, String)>,
    trigger_functions: Vec<Function>,
}

impl Builder<'_> {
    fn require(&mut self, ext: Option<&str>, what: impl FnOnce() -> String) {
        if let Some(ext) = ext {
            self.required.entry(ext.to_owned()).or_insert_with(what);
        }
    }

    fn require_expr(&mut self, sql: &str, what: &str) {
        for ext in self.catalog.for_expr(sql).into_iter().map(str::to_owned).collect::<Vec<_>>() {
            self.require(Some(&ext), || what.to_owned());
        }
    }

    /// Indexes and the constraints that own one share one namespace per database schema.
    fn claim(&mut self, name: String, model: &Model) -> Result<String> {
        if !self.names.insert(name.clone()) {
            return Err(Self::duplicate(&name, model));
        }
        self.claim_local(name, model)
    }

    /// Check and foreign-key names are unique per table only.
    fn claim_local(&mut self, name: String, model: &Model) -> Result<String> {
        if !self.local_names.insert((model.table().to_owned(), name.clone())) {
            return Err(Self::duplicate(&name, model));
        }
        Ok(name)
    }

    fn duplicate(name: &str, model: &Model) -> String {
        format!("model {}: duplicate index or constraint name {name:?}; give one of them an explicit name=", model.ir.name)
    }

    fn column_of(&self, m: &Model, field: &str) -> Result<String> {
        Ok(m.field(field).map_err(|e| format!("{e} (referenced by an index, constraint or trigger)"))?.column.clone())
    }

    fn key(&mut self, m: &Model, c: &IndexColumnIr, what: &str) -> Result<IndexKey> {
        let (column, expr) = match (&c.field, &c.expr) {
            (Some(f), None) => (Some(self.column_of(m, f)?), None),
            (None, Some(e)) => {
                self.require_expr(e, what);
                (None, Some(expression_sql(e)))
            }
            _ => return Err(format!("model {}: each {what} key needs exactly one of field / expr", m.ir.name)),
        };
        if let Some(op) = &c.opclass {
            let ext = self.catalog.for_opclass(op).map(str::to_owned);
            self.require(ext.as_deref(), || format!("{what} operator class {op}"));
        }
        Ok(IndexKey {
            column,
            expr,
            collation: c.collation.clone(),
            opclass: c.opclass.clone(),
            desc: c.desc,
            nulls: c.nulls,
        })
    }

    /// Name part for a list of keys: column names, or a hash for expressions.
    fn keys_tag(keys: &[IndexKey]) -> String {
        keys.iter()
            .map(|k| k.column.clone().unwrap_or_else(|| expression_tag(k.expr.as_deref().unwrap_or(""))))
            .collect::<Vec<_>>()
            .join("_")
    }

    fn table(&mut self, m: &Model) -> Result<Table> {
        let schema = self.schema;
        let t = m.table().to_owned();
        let mut table = Table { name: t.clone(), comment: m.ir.comment.clone(), ..Default::default() };
        if let Some(p) = &m.ir.partition {
            let keys = p.keys.iter().map(|k| match (&k.field, &k.expr) {
                (Some(f), _) => self.column_of(m, f).map(|c| super::pg::ident(&c)),
                (_, Some(e)) => Ok(format!("({e})")),
                _ => Err(format!("{}: invalid partition key", m.ir.name)),
            }).collect::<Result<Vec<_>>>()?;
            table.partition_by = Some(format!("{} ({})", p.strategy.to_uppercase(), keys.join(", ")));
        }

        for (position, f) in m.fields().iter().enumerate() {
            #[cfg(feature = "composition")]
            if m.native.computed().contains(&position) { continue; }
            #[cfg(not(feature = "composition"))]
            let _ = position;
            let ty = if schema.dialect == crate::dialect::Dialect::Sqlite {
                match f.ty {
                    ColType::BigInt | ColType::Int | ColType::Bool => "INTEGER",
                    ColType::Float => "REAL",
                    _ => "TEXT",
                }.to_owned()
            } else { sql_type(f) };
            self.require(self.catalog.for_type(&ty).map(str::to_owned).as_deref(), || {
                format!("column {t}.{} of type {ty}", f.column)
            });
            for ext in &f.requires {
                self.require(Some(ext), || format!("column {t}.{}", f.column));
            }
            let default = if schema.dialect == crate::dialect::Dialect::Sqlite && f.default_now {
                Some("CURRENT_TIMESTAMP".to_owned())
            } else if schema.dialect == crate::dialect::Dialect::Sqlite && f.ty == ColType::Json {
                f.default_sql.clone().or_else(|| f.default.as_ref().map(|v| quote_literal(&match v {
                    serde_json::Value::String(s) => s.clone(), v => v.to_string(),
                })))
            } else { default_sql(f)? };
            if let Some(d) = &default {
                self.require_expr(d, &format!("default of {t}.{}", f.column));
            }
            let (default, backfill) = match f.nested.as_ref().filter(|n| n.under_null) {
                Some(n) => (None, default.filter(|_| !n.present_with.is_empty()).map(|value| Backfill { column: f.column.clone(), value, others: n.present_with.clone() })),
                None => (default, None),
            };
            table.backfills.0.extend(backfill);
            if f.auto_increment && !matches!(f.ty, ColType::BigInt | ColType::Int) {
                return Err(format!("{}.{}: auto_increment needs an integer field", m.ir.name, f.name));
            }
            table.columns.push(Column {
                name: f.column.clone(),
                ty,
                nullable: f.nullable && !f.primary_key,
                default,
                identity: f.auto_increment,
                comment: f.comment.clone(),
                nested: NestedLink(f.nested.as_ref().map(|n| match n.storage {
                    crate::ir::NestedStorage::Json => {
                        let mut shape = vec![];
                        shape_of(&schema.types, &n.ty, &mut vec![], &mut shape);
                        NestedShape { field: n.field.clone(), storage: n.storage, list: n.list, path: vec![], ty: None, shape }
                    }
                    crate::ir::NestedStorage::Columns => NestedShape { field: n.field.clone(), storage: n.storage, list: f.array, path: n.path.clone(), ty: Some(f.ty), shape: vec![] },
                })),
            });
            if f.unique && !f.primary_key {
                table.uniques.push(Unique {
                    name: self.claim(object_name(&[&t, &f.column, "key"]), m)?,
                    columns: vec![f.column.clone()],
                    nulls_not_distinct: false,
                    deferrable: None,
                });
            }
            if f.index && !f.unique && !f.primary_key {
                table.indexes.push(Index {
                    name: self.claim(object_name(&[&t, &f.column, "idx"]), m)?,
                    unique: false,
                    method: None,
                    keys: vec![IndexKey {
                        column: Some(f.column.clone()),
                        expr: None,
                        collation: None,
                        opclass: None,
                        desc: false,
                        nulls: None,
                    }],
                    include: vec![],
                    where_: None,
                    with: vec![],
                    nulls_not_distinct: false,
                });
            }
            if let Some(e) = f.enum_idx.map(|i| &schema.enums[i as usize]).filter(|e| e.storage != EnumStorage::Native) {
                let values = e.values.iter().filter_map(|v| literal(&v.value)).collect::<Vec<_>>().join(", ");
                let c = super::pg::ident(&f.column);
                let expr = if f.array {
                    format!("{c} <@ ARRAY[{values}]::{}", sql_type(f))
                } else {
                    format!("{c} IN ({values})")
                };
                table.checks.push(Check { name: self.claim_local(object_name(&[&t, &f.column, "enum", "check"]), m)?, expr });
            }
            if let Some(expr) = &f.check {
                self.require_expr(expr, &format!("check on {t}.{}", f.column));
                table.checks.push(Check {
                    name: self.claim_local(object_name(&[&t, &f.column, "check"]), m)?,
                    expr: expression_sql(expr),
                });
            }
        }

        if schema.dialect == crate::dialect::Dialect::Sqlite && m.key.len() > 1 {
            if let Some(f) = m.fields().iter().find(|f| f.auto_increment) {
                return Err(format!("{}.{}: SQLite has no autoincrement in a primary key of several columns", m.ir.name, f.name));
            }
        }
        let name = m.ir.pk_name.clone().unwrap_or_else(|| object_name(&[&t, "pkey"]));
        table.primary_key =
            Some(PrimaryKey { name: self.claim(name, m)?, columns: m.key_fields().iter().map(|f| f.column.clone()).collect() });

        for r in m.ir.relations.iter().filter(|r| r.foreign_key) {
            let target = schema.model(schema.model_idx(&r.target)?);
            let columns = r.from.iter().map(|f| Ok(m.field(f)?.column.clone())).collect::<Result<Vec<_>>>()?;
            let default_name = {
                let mut parts: Vec<&str> = vec![&t];
                parts.extend(columns.iter().map(String::as_str));
                parts.push("fkey");
                object_name(&parts)
            };
            table.foreign_keys.push(ForeignKey {
                name: self.claim_local(r.fk_name.clone().unwrap_or(default_name), m)?,
                columns,
                ref_table: target.table().to_owned(),
                ref_columns: r.to.iter().map(|f| Ok(target.field(f)?.column.clone())).collect::<Result<_>>()?,
                on_delete: r.on_delete.filter(|a| *a != OnDelete::NoAction),
                on_update: r.on_update.filter(|a| *a != OnDelete::NoAction),
                deferrable: r.deferrable,
            });
        }

        for ix in &m.ir.indexes {
            if ix.columns.is_empty() {
                return Err(format!("model {}: index without columns", m.ir.name));
            }
            let keys = ix.columns.iter().map(|c| self.key(m, c, "index")).collect::<Result<Vec<_>>>()?;
            let method = ix.method.as_ref().map(|s| s.to_ascii_lowercase()).filter(|s| s != "btree");
            if let Some(mt) = &method {
                let ext = self.catalog.for_index_method(mt).map(str::to_owned);
                self.require(ext.as_deref(), || format!("index method {mt}"));
            }
            for ext in &ix.requires {
                self.require(Some(ext), || "an index".into());
            }
            if let Some(w) = &ix.where_ {
                self.require_expr(w, "index predicate");
            }
            table.indexes.push(Index {
                name: self.claim(index_name(m, ix)?, m)?,
                unique: ix.unique,
                method,
                keys,
                include: ix.include.iter().map(|f| self.column_of(m, f)).collect::<Result<_>>()?,
                where_: ix.where_.as_deref().map(expression_sql),
                with: ix.with.clone(),
                nulls_not_distinct: ix.nulls_not_distinct,
            });
        }

        for c in &m.ir.constraints {
            match c {
                ConstraintIr::Unique { name, fields, nulls_not_distinct, deferrable } => {
                    if fields.is_empty() {
                        return Err(format!("model {}: unique constraint without fields", m.ir.name));
                    }
                    let columns = fields.iter().map(|f| self.column_of(m, f)).collect::<Result<Vec<_>>>()?;
                    let name = name.clone().unwrap_or_else(|| {
                        let mut parts = vec![t.as_str()];
                        parts.extend(columns.iter().map(String::as_str));
                        parts.push("key");
                        object_name(&parts)
                    });
                    table.uniques.push(Unique {
                        name: self.claim(name, m)?,
                        columns,
                        nulls_not_distinct: *nulls_not_distinct,
                        deferrable: *deferrable,
                    });
                }
                ConstraintIr::Check { name, expr } => {
                    self.require_expr(expr, "check constraint");
                    let name = name.clone().unwrap_or_else(|| object_name(&[&t, &expression_tag(expr), "check"]));
                    table.checks.push(Check { name: self.claim_local(name, m)?, expr: expression_sql(expr) });
                }
                ConstraintIr::Exclude { name, method, elements, where_, deferrable, requires } => {
                    let mut els = vec![];
                    for e in elements {
                        els.push(ExclusionElement {
                            key: self.key(m, &e.column, "exclusion constraint")?,
                            operator: e.operator.trim().to_owned(),
                        });
                    }
                    if els.is_empty() {
                        return Err(format!("model {}: exclusion constraint without elements", m.ir.name));
                    }
                    let method = method.clone().unwrap_or_else(|| "gist".into()).to_ascii_lowercase();
                    let ext = self.catalog.for_index_method(&method).map(str::to_owned);
                    self.require(ext.as_deref(), || format!("exclusion method {method}"));
                    for ext in requires {
                        self.require(Some(ext), || "an exclusion constraint".into());
                    }
                    let keys: Vec<IndexKey> = els.iter().map(|e| e.key.clone()).collect();
                    let name = name.clone().unwrap_or_else(|| object_name(&[&t, &Self::keys_tag(&keys), "excl"]));
                    table.exclusions.push(Exclusion {
                        name: self.claim(name, m)?,
                        method,
                        elements: els,
                        where_: where_.as_deref().map(expression_sql),
                        deferrable: *deferrable,
                    });
                }
            }
        }

        for tr in &m.ir.triggers {
            if tr.events.is_empty() {
                return Err(format!("trigger {t}.{}: no events", tr.name));
            }
            if !tr.update_of.is_empty() && !tr.events.contains(&TriggerEvent::Update) {
                return Err(format!("trigger {t}.{}: update_of needs the update event", tr.name));
            }
            let function = match (&tr.function, &tr.body) {
                (Some(f), None) => f.clone(),
                (None, Some(body)) => {
                    let name = object_name(&[&t, &tr.name]);
                    if schema.dialect != crate::dialect::Dialect::Sqlite { self.trigger_functions.push(Function {
                        name: name.clone(),
                        args: String::new(),
                        returns: "trigger".into(),
                        language: tr.language.clone().unwrap_or_else(|| "plpgsql".into()),
                        body: body.trim().to_owned(),
                        volatility: None,
                        security_definer: false,
                    }); }
                    name
                }
                _ => return Err(format!("trigger {t}.{}: give exactly one of function / body", tr.name)),
            };
            let mut events = tr.events.clone();
            events.sort();
            events.dedup();
            if table.triggers.iter().any(|x| x.name == tr.name) {
                return Err(format!("table {t}: duplicate trigger {:?}", tr.name));
            }
            table.triggers.push(Trigger {
                name: tr.name.clone(),
                timing: tr.timing,
                events,
                update_of: tr.update_of.iter().map(|f| self.column_of(m, f)).collect::<Result<_>>()?,
                for_each: tr.for_each,
                when: tr.when.as_deref().map(normalize_ws),
                function,
                body: if schema.dialect == crate::dialect::Dialect::Sqlite { tr.body.clone() } else { None },
                args: tr.args.clone(),
            });
        }
        Ok(table)
    }
}

/// The database schema the ORM schema describes, plus its rename hints.
pub fn build(schema: &Schema) -> Result<(DbSchema, Renames)> {
    let mut b = Builder {
        schema,
        catalog: Catalog::new(&[schema.catalog.as_slice(), schema.extensions.as_slice()].concat()),
        required: BTreeMap::new(),
        names: BTreeSet::new(),
        local_names: BTreeSet::new(),
        trigger_functions: vec![],
    };
    let mut db = DbSchema { identities: schema.identities.clone(), dialect: schema.dialect, version: if !schema.partitions.is_empty() { SNAPSHOT_VERSION } else if schema.identities.is_some() { 3 } else if schema.dialect.is_postgres() { 1 } else { 2 }, ..Default::default() };
    let mut renames = Renames::default();
    let mut tables = BTreeSet::new();
    for m in &schema.models {
        let table = b.table(m)?;
        if !tables.insert(table.name.clone()) {
            return Err(format!("two models use table {:?}", table.name));
        }
        // One old name renames to one new name, as `diff::diff` does on every dialect.
        if let Some(old) = &m.ir.renamed_from {
            if let Some((other, _)) = renames.tables.iter().find(|(_, o)| *o == old) {
                return Err(format!("tables {other:?} and {:?} both have the rename hint {old:?}; keep it on one", table.name));
            }
            renames.tables.insert(table.name.clone(), old.clone());
        }
        for f in m.fields() {
            if let Some(old) = &f.renamed_from {
                if let Some(((_, other), _)) = renames.columns.iter().find(|((t, _), o)| *t == table.name && *o == old) {
                    return Err(format!("{}: columns {other:?} and {:?} both have the rename hint {old:?}; keep it on one", table.name, f.column));
                }
                renames.columns.insert((table.name.clone(), f.column.clone()), old.clone());
            }
        }
        db.tables.push(table);
    }

    for p in &schema.partitions {
        let objects: crate::ir::ModelIr = serde_json::from_value(serde_json::to_value(&p.objects).unwrap()).unwrap();
        let mut model = Model::derived(&objects.name, objects.fields.clone())?;
        model.ir = objects;
        let mut table = b.table(&model)?;
        // PostgreSQL inherits columns and ancestor rules. Only local objects belong in snapshots.
        table.columns.clear();
        table.primary_key = None;
        let parent = schema.models.iter().find(|m| m.ir.name == p.of).map(|m| m.table())
            .or_else(|| schema.partitions.iter().find(|q| q.objects.name == p.of).map(|q| q.objects.table.as_str()))
            .ok_or_else(|| format!("partition {}: unknown parent {}", p.objects.name, p.of))?;
        table.partition_of = Some(PartitionOf { parent: parent.into(), bound: p.bound.clone() });
        if !tables.insert(table.name.clone()) { return Err(format!("duplicate partition table {}", table.name)); }
        if let Some(old) = &p.objects.renamed_from { renames.tables.insert(table.name.clone(), old.clone()); }
        db.tables.push(table);
    }

    for f in &schema.functions {
        b.require_expr(&f.body, &format!("function {}", f.name));
        db.functions.push(Function {
            name: f.name.clone(),
            args: normalize_ws(&f.args),
            returns: normalize_ws(&f.returns),
            language: f.language.clone().unwrap_or_else(|| "plpgsql".into()).to_ascii_lowercase(),
            body: f.body.trim().to_owned(),
            volatility: f.volatility.as_ref().map(|v| v.to_ascii_lowercase()),
            security_definer: f.security_definer,
        });
    }
    db.functions.append(&mut b.trigger_functions);
    for e in schema.enums.iter().filter(|e| e.storage == EnumStorage::Native) {
        let values = e.values.iter().map(|v| v.value.as_str().unwrap_or_default().to_owned()).collect();
        db.enums.push(EnumType { name: e.db_name.clone(), values, comment: e.comment.clone() });
    }
    let mut keys = BTreeSet::new();
    for f in &db.functions {
        if !keys.insert(f.key()) {
            return Err(format!("function {}({}) is declared twice", f.name, f.args));
        }
    }

    let mut extensions: BTreeMap<String, Extension> = b
        .required
        .keys()
        .map(|name| (name.clone(), Extension { name: name.clone(), schema: None, version: None }))
        .collect();
    for e in &schema.extensions {
        extensions.insert(
            e.name.clone(),
            Extension { name: e.name.clone(), schema: e.schema.clone(), version: e.version.clone() },
        );
    }
    db.extensions = extensions.into_values().collect();
    Ok((db, renames))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_names_are_truncated_with_a_hash() {
        let long = "a".repeat(80);
        let n = object_name(&[&long, "idx"]);
        assert_eq!(n.len(), MAX_IDENT);
        assert_ne!(n, object_name(&[&long, "key"]));
        assert_eq!(object_name(&["users", "email", "key"]), "users_email_key");
    }
}
