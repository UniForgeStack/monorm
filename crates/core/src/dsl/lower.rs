//! Syntax tree -> schema IR.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};

use super::expr::{Context, Diagnostic, Span};
use super::syntax::{err, Args, Attr, EnumDecl, Item, Member, ModelDecl, Path, Pos, Props, Result, TypeRef, Value};
use crate::behavior::ArgumentKind;
use crate::ext::{ExtensionDef, TypeDef};
use crate::ir::{
    ClientCall, ClientDefaultIr, ColType, ConstraintIr, Deferrable, EnumIr, EnumStorage, EnumValueIr, ExcludeElementIr, ExtensionIr, FieldIr,
    ForEach, FunctionIr, IndexColumnIr, IndexIr, ModelIr, Nulls, OnDelete, RelKind, RelationIr, SchemaIr,
    ThroughIr, TriggerEvent, TriggerIr, TriggerTiming, PartitionIr, PartitionKeyIr,
};
use crate::migrate::pg::ident;

mod nested;
use nested::Paths;
#[cfg(test)]
mod tests;

/// Reads the named arguments of one attribute, rejecting unknown or repeated ones.
struct Named<'a> {
    what: String,
    pos: Pos,
    args: &'a Args,
    used: HashSet<&'a str>,
}

impl<'a> Named<'a> {
    fn new(what: impl Into<String>, args: &'a Args) -> Self {
        Named { what: what.into(), pos: args.pos, args, used: HashSet::new() }
    }

    fn get(&mut self, key: &'a str) -> Option<(Pos, &'a Value)> {
        self.args.named.iter().find(|(k, _, _)| k == key).map(|(k, p, v)| {
            self.used.insert(k.as_str());
            (*p, v)
        })
    }

    /// The source extent (start and length) of the value of `key`; the position of the key when
    /// `named_values` has no entry for it.
    fn extent(&self, key: &str) -> Option<Pos> {
        let i = self.args.named.iter().position(|(k, _, _)| k == key)?;
        Some(self.args.named_values.get(i).copied().unwrap_or(self.args.named[i].1))
    }

    fn str(&mut self, key: &'a str) -> Result<Option<String>> {
        match self.get(key) {
            None => Ok(None),
            Some((_, Value::Str(s))) => Ok(Some(s.clone())),
            Some((p, _)) => err(p, format!("{}: {key} must be a string", self.what)),
        }
    }

    /// SQL text: `"..."` or Prisma's `raw("...")`.
    fn sql(&mut self, key: &'a str) -> Result<Option<String>> {
        match self.get(key) {
            None => Ok(None),
            Some((p, v)) => Ok(Some(raw_sql(p, v, &format!("{}: {key}", self.what))?)),
        }
    }

    /// A bare name or a string.
    fn name(&mut self, key: &'a str) -> Result<Option<String>> {
        match self.get(key) {
            None => Ok(None),
            Some((p, v)) => Ok(Some(name_of(p, v, &format!("{}: {key}", self.what))?)),
        }
    }

    /// The database name of an index or constraint: `name:` or Prisma's `map:`.
    fn db_name(&mut self) -> Result<Option<String>> {
        match (self.str("name")?, self.str("map")?) {
            (Some(_), Some(_)) => err(self.pos, format!("{}: give name: or map:, not both", self.what)),
            (a, b) => Ok(a.or(b)),
        }
    }

    fn bool(&mut self, key: &'a str) -> Result<bool> {
        match self.get(key) {
            None => Ok(false),
            Some((_, Value::Bool(b))) => Ok(*b),
            Some((p, _)) => err(p, format!("{}: {key} must be true or false", self.what)),
        }
    }

    fn names(&mut self, key: &'a str) -> Result<Vec<String>> {
        match self.get(key) {
            None => Ok(vec![]),
            Some((_, Value::List(items))) => items.iter().map(|(p, v)| name_of(*p, v, &self.what)).collect(),
            Some((p, v)) => Ok(vec![name_of(p, v, &self.what)?]),
        }
    }

    fn finish(self) -> Result<()> {
        if let Some((k, p, _)) = self.args.named.iter().find(|(k, _, _)| !self.used.contains(k.as_str())) {
            return err(*p, format!("{}: unknown argument `{k}`", self.what));
        }
        Ok(())
    }

    fn no_positional(&self) -> Result<()> {
        match self.args.positional.first() {
            Some((p, _)) => err(*p, format!("{} takes no positional arguments", self.what)),
            None => Ok(()),
        }
    }
}

fn name_of(pos: Pos, v: &Value, what: &str) -> Result<String> {
    match v {
        Value::Path(p, None) if p.len() == 1 => Ok(p[0].clone()),
        Value::Str(s) => Ok(s.clone()),
        _ => err(pos, format!("{what}: expected a name")),
    }
}

/// `"..."` or `raw("...")`.
fn raw_sql(pos: Pos, v: &Value, what: &str) -> Result<String> {
    match v {
        Value::Str(s) => Ok(s.clone()),
        Value::Path(p, Some(args)) if p.len() == 1 && p[0] == "raw" => match args.positional.as_slice() {
            [(_, Value::Str(s))] if args.named.is_empty() => Ok(s.clone()),
            _ => err(pos, format!("{what}: raw(\"...\") takes one string")),
        },
        _ => err(pos, format!("{what}: expected SQL as \"...\" or raw(\"...\")")),
    }
}

/// A call `name(...)` with one string argument, e.g. `dbgenerated("now()")`.
fn call_str<'v>(v: &'v Value, name: &str) -> Option<std::result::Result<&'v str, ()>> {
    match v {
        Value::Path(p, args) if p.len() == 1 && p[0] == name => Some(match args.as_ref().map(|a| (a.positional.as_slice(), a.named.len())) {
            Some(([(_, Value::Str(s))], 0)) => Ok(s.as_str()),
            _ => Err(()),
        }),
        _ => None,
    }
}

/// A type or storage-parameter argument as SQL text.
fn sql_text(pos: Pos, v: &Value) -> Result<String> {
    match v {
        Value::Num(n) => Ok(n.clone()),
        Value::Str(s) => Ok(s.clone()),
        Value::Bool(b) => Ok(b.to_string()),
        Value::Path(p, None) => Ok(p.join(".")),
        _ => err(pos, "expected a number, name or string"),
    }
}

const ONE_CLIENT_DEFAULT: &str = "a field takes one client default: @client_default(...) or @default(uuid())";

/// A client default literal in its stored form: enum values for member names, JSON
/// text parsed for a Json field.
fn literal(pos: Pos, v: &Value, f: &FieldIr, enum_ir: Option<&EnumIr>, what: &str) -> Result<serde_json::Value> {
    let member = |pos: Pos, v: &Value| -> Result<serde_json::Value> {
        let e = enum_ir.expect("enum field");
        let n = name_of(pos, v, what)?;
        match e.values.iter().find(|x| x.name == n) {
            Some(x) => Ok(x.value.clone()),
            None => err(pos, format!("{what}: {} has no value {n}", e.name)),
        }
    };
    match v {
        Value::Path(p, None) if p.len() == 1 && enum_ir.is_some() => member(pos, v),
        Value::List(items) if f.array && enum_ir.is_some() => {
            Ok(serde_json::Value::Array(items.iter().map(|(p, v)| member(*p, v)).collect::<Result<_>>()?))
        }
        Value::Str(_) if enum_ir.is_some() => err(pos, format!("{what}: @client_default on an enum field names a member, such as {}, not a string", enum_ir.and_then(|e| e.values.first()).map_or("ACTIVE", |m| m.name.as_str()))),
        Value::Str(s) if f.ty == ColType::Json && !f.array => serde_json::from_str(s)
            .map_err(|e| super::syntax::Error { pos, msg: format!("{what}: @client_default on a Json field is JSON text: {e}") }),
        Value::Path(p, _) => err(pos, format!("{what}: @client_default takes a literal, uuid(), uuid7() or now(), not {}", p.join("."))),
        // A JSON number is an f64; a Decimal keeps every digit as text.
        Value::Num(n) if f.ty == ColType::Decimal => Ok(serde_json::Value::String(n.clone())),
        Value::List(items) if f.ty == ColType::Decimal => {
            Ok(serde_json::Value::Array(items.iter().map(|(p, v)| literal(*p, v, f, enum_ir, what)).collect::<Result<_>>()?))
        }
        v => json_of(pos, v),
    }
}

fn json_of(pos: Pos, v: &Value) -> Result<serde_json::Value> {
    Ok(match v {
        Value::Str(s) => serde_json::Value::String(s.clone()),
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Num(n) => serde_json::from_str(n).map_err(|e| super::syntax::Error { pos, msg: e.to_string() })?,
        Value::List(items) => serde_json::Value::Array(items.iter().map(|(p, v)| json_of(*p, v)).collect::<Result<_>>()?),
        Value::Object(_) | Value::Path(..) | Value::Binary(..) | Value::Not(..) | Value::Neg(..) | Value::Method(..) => {
            return err(pos, "expected a literal")
        }
        Value::Desc(inner) => {
            let name = match &inner.1 { Value::Path(p, _) => p.join("."), _ => "field".into() };
            return err(pos, format!("`-{name}` (descending) only goes in an index key list; elsewhere quote it: \"-{name}\""));
        }
    })
}

fn deferrable(n: &mut Named<'_>) -> Result<Option<Deferrable>> {
    let pos = n.pos;
    Ok(match n.name("deferrable")?.as_deref() {
        None => None,
        Some("immediate") => Some(Deferrable::Immediate),
        Some("deferred") => Some(Deferrable::Deferred),
        Some(other) => return err(pos, format!("deferrable must be immediate or deferred, not {other}")),
    })
}

fn action(pos: Pos, s: &str) -> Result<OnDelete> {
    Ok(match s {
        "Cascade" => OnDelete::Cascade,
        "SetNull" => OnDelete::SetNull,
        "SetDefault" => OnDelete::SetDefault,
        "Restrict" => OnDelete::Restrict,
        "NoAction" => OnDelete::NoAction,
        _ => return err(pos, format!("unknown referential action {s}; use Cascade, SetNull, SetDefault, Restrict or NoAction")),
    })
}

/// Prisma's scalar types.
fn builtin_type(name: &str) -> Option<ColType> {
    Some(match name {
        "BigInt" => ColType::BigInt,
        "Int" => ColType::Int,
        "Float" => ColType::Float,
        "Boolean" => ColType::Bool,
        "String" => ColType::String,
        "DateTime" => ColType::DateTime,
        "Json" => ColType::Json,
        "Decimal" => ColType::Decimal,
        _ => return None,
    })
}

/// Prisma's Postgres native types (`@db.X`) as SQL, and whether `X` is the SQL type
/// the field type already maps to.
fn native_type(name: &str, ty: ColType) -> Option<(&'static str, bool)> {
    let sql = match name {
        "Text" => "text",
        "VarChar" => "varchar",
        "Char" => "char",
        "Bit" => "bit",
        "VarBit" => "varbit",
        "Uuid" => "uuid",
        "Xml" => "xml",
        "Inet" => "inet",
        "Boolean" => "boolean",
        "Integer" => "integer",
        "SmallInt" => "smallint",
        "Oid" => "oid",
        "BigInt" => "bigint",
        "DoublePrecision" => "double precision",
        "Real" => "real",
        "Decimal" => "numeric",
        "Money" => "money",
        "Timestamp" => "timestamp",
        "Timestamptz" => "timestamptz",
        "Date" => "date",
        "Time" => "time",
        "Timetz" => "timetz",
        "Json" => "json",
        "JsonB" => "jsonb",
        "ByteA" => "bytea",
        _ => return None,
    };
    let same = matches!(
        (name, ty),
        ("BigInt", ColType::BigInt)
            | ("Integer", ColType::Int)
            | ("DoublePrecision", ColType::Float)
            | ("Boolean", ColType::Bool)
            | ("Timestamptz", ColType::DateTime)
            | ("JsonB", ColType::Json)
            | ("Decimal", ColType::Decimal)
    );
    Some((sql, same))
}

/// A native expression of a check, filter or key. It waits until every model is lowered, so it can name any field.
struct PendingExpr {
    model: String,
    context: Context,
    pos: Pos,
    value: Value,
}

/// The native expressions of one schema, and the warnings found while lowering.
#[derive(Default)]
struct Pendings {
    list: RefCell<Vec<PendingExpr>>,
    warnings: RefCell<Vec<Diagnostic>>,
}

/// The first character of the text that stands in an IR slot for a pending expression.
const MARK: char = '\u{1}';

impl Pendings {
    /// The text that stands in the IR for the expression until `resolve_pending` replaces it.
    fn defer(&self, model: &str, context: Context, pos: Pos, value: &Value) -> String {
        let mut list = self.list.borrow_mut();
        list.push(PendingExpr { model: model.to_owned(), context, pos, value: value.clone() });
        format!("{MARK}{}", list.len() - 1)
    }

    fn warn(&self, pos: Pos, message: String) {
        let span = Span { line: pos.line, column: pos.col, len: pos.len.max(1) };
        self.warnings.borrow_mut().push(Diagnostic { span, message });
    }
}

/// The SQL of a check or an index filter. A plain string warns and stays as written, `raw("...")`
/// stays as written, and any other value is a native expression. `pos` is the extent of `v`.
fn expression(pending: Option<&Pendings>, model: &str, context: Context, what: &str, pos: Pos, v: &Value) -> Result<String> {
    match v {
        Value::Str(s) => {
            if let Some(pending) = pending {
                pending.warn(pos, format!("{what}: this SQL text is not checked; write raw(\"...\") to keep it as SQL, or write a native expression"));
            }
            Ok(s.clone())
        }
        Value::Path(path, Some(_)) if path.as_slice() == ["raw"] => raw_sql(pos, v, what),
        _ => match pending {
            Some(pending) => Ok(pending.defer(model, context, pos, v)),
            None => err(pos, format!("{what}: a native expression is not available here; write raw(\"...\")")),
        },
    }
}

const FIELD_ATTRS: &str = "@id, @unique, @default, @client_default, @map, @db.*, @check, @comment, @renamed_from";

pub struct Lowering<'l> {
    /// Reads an imported file (path as written in the schema).
    pub load: &'l dyn Fn(&str) -> std::result::Result<String, String>,
    /// Model declarations selected for compiler contributions may derive identity.
    /// Final identity validation runs after the selected passes.
    pub deferred_identity: &'l HashSet<String>,
    /// Proxy models (`@@proxy.of`): only their redeclared fields take an enum subset.
    pub proxies: &'l HashSet<String>,
}

struct Ctx<'l> {
    types: HashMap<String, (String, TypeDef)>,
    enums: HashMap<String, EnumIr>,
    nested: HashMap<String, crate::ir::TypeIr>,
    /// Position and `@map` of each type member, by type and subfield name; `TypeFieldIr` holds neither.
    members: HashMap<(String, String), nested::TypeMember>,
    proxies: &'l HashSet<String>,
    pending: Pendings,
}

impl Lowering<'_> {
    pub fn lower(&self, items: Vec<Item>) -> Result<(SchemaIr, Vec<Diagnostic>)> {
        let mut ir = SchemaIr::default();
        let mut ctx = Ctx { types: HashMap::new(), enums: HashMap::new(), nested: HashMap::new(), members: HashMap::new(), proxies: self.proxies, pending: Pendings::default() };
        for def in ExtensionDef::builtin() {
            for (name, t) in &def.types {
                ctx.types.insert(name.clone(), (def.name.clone(), t.clone()));
            }
        }
        // imports first: their types are usable anywhere in the file
        for item in &items {
            if let Item::Import { pos, path, .. } = item {
                let src = (self.load)(path).map_err(|e| super::syntax::Error { pos: *pos, msg: format!("import {path:?}: {e}") })?;
                let def = ExtensionDef::parse(&src)
                    .map_err(|e| super::syntax::Error { pos: *pos, msg: format!("extension file {path:?}: {e}") })?;
                for (name, t) in &def.types {
                    ctx.types.insert(name.clone(), (def.name.clone(), t.clone()));
                }
                ir.catalog.push(ExtensionIr { name: def.name.clone(), schema: None, version: None, provides: def.provides() });
            }
        }
        let models: Vec<&ModelDecl> = items.iter().filter_map(|i| if let Item::Model(m) = i { Some(m) } else { None }).collect();
        let model_names: HashMap<&str, &ModelDecl> = models.iter().map(|m| (m.name.as_str(), *m)).collect();
        if model_names.len() != models.len() {
            let mut seen = HashSet::new();
            let dup = models.iter().find(|m| !seen.insert(m.name.as_str())).unwrap();
            return err(dup.pos, format!("model {} is declared twice", dup.name));
        }

        for item in &items {
            if let Item::Enum(e) = item {
                if model_names.contains_key(e.name.as_str()) || ctx.enums.contains_key(&e.name) {
                    return err(e.pos, format!("{} is declared twice", e.name));
                }
                let lowered = enum_block(e)?;
                ctx.enums.insert(e.name.clone(), lowered.clone());
                ir.enums.push(lowered);
            }
        }
        ir.types = nested::declare(&items, &model_names, &mut ctx)?;
        let mut db_names = HashSet::new();
        for e in ir.enums.iter().filter(|e| e.storage == EnumStorage::Native) {
            if !db_names.insert(e.db_name.as_str()) {
                return err(Pos::default(), format!("two enums use the database type {:?}", e.db_name));
            }
        }

        let mut datasource = false;
        for item in &items {
            match item {
                Item::Datasource { pos, props } => {
                    if std::mem::replace(&mut datasource, true) {
                        return err(*pos, "only one datasource block is allowed");
                    }
                    datasource_block(&mut ir, *pos, props)?;
                }
                Item::Function { pos, name, props } => ir.functions.push(function(*pos, name, props)?),
                Item::Model(_) | Item::Type(_) | Item::Enum(_) | Item::Import { .. } | Item::Partition { .. } | Item::Generator { .. } => {}
            }
        }

        // fields first (relations need every model's primary key)
        let mut lowered: Vec<ModelIr> = vec![];
        let mut all_paths: Vec<Paths> = vec![];
        for m in &models {
            let (ir, paths) = model_fields(m, &model_names, &ctx, &items)?;
            lowered.push(ir);
            all_paths.push(paths);
        }
        let pks: HashMap<String, Option<String>> = lowered
            .iter()
            .map(|m| (m.name.clone(), m.fields.iter().find(|f| f.primary_key).map(|f| f.name.clone())))
            .collect();
        for ((m, decl), paths) in lowered.iter_mut().zip(&models).zip(&all_paths) {
            if pks[&m.name].is_none() && !self.deferred_identity.contains(&m.name) {
                return err(decl.pos, format!("model {} has no @id field", decl.name));
            }
            for member in decl.members.iter().filter(|mm| model_names.contains_key(mm.ty.name.as_str())) {
                let r = relation(decl, member, &m.fields, &model_names, self.deferred_identity)?;
                m.relations.push(r);
            }
            for block in &decl.blocks {
                model_block(m, &nested::resolve_paths(block, paths, &decl.name, ir.dialect)?, Some(&ctx.pending))?;
            }
        }
        ir.models = lowered;
        lower_partitions(&items, &mut ir)?;
        let warnings = resolve_pending(&mut ir, &ctx.pending)?;
        Ok((ir, warnings))
    }
}

/// `enum Name { a b @map("B") ... @@map("db_name") @@storage(text) }`
fn enum_block(e: &EnumDecl) -> Result<EnumIr> {
    let what = format!("enum {}", e.name);
    let mut out = EnumIr {
        name: e.name.clone(),
        db_name: e.name.to_lowercase(),
        storage: EnumStorage::Native,
        values: vec![],
        comment: None,
    };
    for a in &e.blocks {
        let one = || -> Result<&Value> {
            match a.args.positional.as_slice() {
                [(_, v)] if a.args.named.is_empty() => Ok(v),
                _ => err(a.pos, format!("{what}: @@{} takes one argument", a.name)),
            }
        };
        match (a.name.as_str(), one()?) {
            ("map", Value::Str(s)) => out.db_name = s.clone(),
            ("comment", Value::Str(s)) => out.comment = Some(s.clone()),
            ("storage", v) => {
                out.storage = match name_of(a.pos, v, &what)?.as_str() {
                    "native" => EnumStorage::Native,
                    "text" => EnumStorage::Text,
                    "int" => EnumStorage::Int,
                    o => return err(a.pos, format!("{what}: @@storage is native, text or int, not {o}")),
                }
            }
            ("map" | "comment", _) => return err(a.pos, format!("{what}: @@{}(\"...\") takes a string", a.name)),
            (o, _) => return err(a.pos, format!("{what}: unknown attribute @@{o}; use @@map, @@storage or @@comment")),
        }
    }
    if e.values.is_empty() {
        return err(e.pos, format!("{what} has no values"));
    }
    let mut seen_names = HashSet::new();
    let mut seen_values = HashSet::new();
    for (pos, name, attrs) in &e.values {
        if !seen_names.insert(name.as_str()) {
            return err(*pos, format!("{what}: {name} is declared twice"));
        }
        let mut label: Option<String> = None;
        let mut number: Option<i64> = None;
        for a in attrs {
            let v = match a.args.positional.as_slice() {
                [(_, v)] if a.args.named.is_empty() => v,
                _ => return err(a.pos, format!("{what}.{name}: @{} takes one argument", a.name)),
            };
            match (a.name.as_str(), v) {
                ("map", Value::Str(s)) => label = Some(s.clone()),
                ("value", Value::Num(n)) => {
                    number = Some(n.parse().map_err(|_| super::syntax::Error {
                        pos: a.pos,
                        msg: format!("{what}.{name}: @value takes an integer"),
                    })?)
                }
                ("map", _) => return err(a.pos, format!("{what}.{name}: @map takes a string")),
                ("value", _) => return err(a.pos, format!("{what}.{name}: @value takes an integer")),
                (o, _) => return err(a.pos, format!("{what}.{name}: unknown attribute @{o}; use @map or @value")),
            }
        }
        let value = match out.storage {
            EnumStorage::Int => {
                if label.is_some() {
                    return err(*pos, format!("{what}.{name}: an int enum stores @value(n), not @map"));
                }
                match number {
                    Some(n) => serde_json::Value::from(n),
                    None => return err(*pos, format!("{what}.{name}: values of an int enum need @value(n)")),
                }
            }
            _ => {
                if number.is_some() {
                    return err(*pos, format!("{what}.{name}: @value(n) is for @@storage(int); use @map(\"...\")"));
                }
                serde_json::Value::String(label.unwrap_or_else(|| name.clone()))
            }
        };
        if !seen_values.insert(value.to_string()) {
            return err(*pos, format!("{what}: two values are stored as {value}"));
        }
        out.values.push(EnumValueIr { name: name.clone(), value });
    }
    Ok(out)
}

/// `datasource db { provider = "postgresql" extensions = [...] }`
fn datasource_block(ir: &mut SchemaIr, pos: Pos, props: &Props) -> Result<()> {
    let args = Args { pos, positional: vec![], named: props.clone(), named_values: vec![] };
    let mut n = Named::new("datasource", &args);
    match n.get("provider") {
        Some((_, Value::Str(p))) if p == "postgresql" || p == "postgres" => {}
        Some((_, Value::Str(p))) if p == "sqlite" => ir.dialect = crate::dialect::Dialect::Sqlite,
        Some((p, _)) => return err(p, r#"datasource: provider must be "postgresql" or "sqlite""#),

        None => return err(pos, "datasource: provider = \"postgresql\" is missing"),
    }
    // The connection comes from ORM_DATABASE_URL / --url; Prisma's keys are accepted.
    for key in ["url", "directUrl", "shadowDatabaseUrl"] {
        n.get(key);
    }
    if let Some((p, v)) = n.get("extensions") {
        let Value::List(list) = v else { return err(p, "datasource: extensions takes a list, e.g. [postgis, citext]") };
        for (p, v) in list {
            let (ident, args) = match v {
                Value::Path(path, args) if path.len() == 1 => (&path[0], args.clone().unwrap_or(Args { pos: *p, ..Default::default() })),
                _ => return err(*p, "datasource: an extension is a name, e.g. postgis(schema: \"ext\", version: \"3.4\")"),
            };
            let mut e = Named::new(format!("extension {ident}"), &args);
            e.no_positional()?;
            let name = e.str("map")?.unwrap_or_else(|| ident.clone());
            let schema = e.str("schema")?;
            let version = e.str("version")?;
            e.finish()?;
            if ir.extensions.iter().any(|x| x.name == name) {
                return err(*p, format!("extension {name} is listed twice"));
            }
            let provides = ir.catalog.iter().find(|x| x.name == name).map(|x| x.provides.clone()).unwrap_or_default();
            ir.extensions.push(ExtensionIr { name, schema, version, provides });
        }
    }
    n.finish()
}

fn function(pos: Pos, name: &str, props: &Props) -> Result<FunctionIr> {
    let args = Args { pos, positional: vec![], named: props.to_vec(), named_values: vec![] };
    let mut n = Named::new(format!("function {name}"), &args);
    let body = n.str("body")?;
    let f = FunctionIr {
        name: name.to_owned(),
        args: n.str("args")?.unwrap_or_default(),
        returns: n.name("returns")?.unwrap_or_else(|| "trigger".into()),
        language: n.name("language")?,
        body: match body {
            Some(b) => b,
            None => return err(pos, format!("function {name} needs a body")),
        },
        volatility: n.name("volatility")?,
        security_definer: n.bool("security_definer")?,
    };
    n.finish()?;
    Ok(f)
}

fn model_fields(m: &ModelDecl, models: &HashMap<&str, &ModelDecl>, ctx: &Ctx<'_>, items: &[Item]) -> Result<(ModelIr, Paths)> {
    let mut ir = ModelIr {
        partition: None,
        name: m.name.clone(),
        table: m.name.to_lowercase(),
        fields: vec![],
        primary_key: vec![],
        pk_name: None,
        relations: vec![],
        indexes: vec![],
        constraints: vec![],
        triggers: vec![],
        renamed_from: None,
        comment: None,
        protected_write: false,
        query_set: None,
        model_methods: None,
        classes_from: None,
    };
    // model-level attributes that aren't schema objects; the rest come after relations
    for a in &m.blocks {
        let text = |a: &Attr| -> Result<String> {
            match a.args.positional.as_slice() {
                [(_, Value::Str(s))] if a.args.named.is_empty() => Ok(s.clone()),
                _ => err(a.pos, format!("{}: @@{}(\"...\") takes one string", m.name, a.name)),
            }
        };
        match a.name.as_str() {
            "map" => ir.table = text(a)?,
            "comment" => ir.comment = Some(text(a)?),
            "renamed_from" => ir.renamed_from = Some(text(a)?),
            "protected_write" if a.args.positional.is_empty() && a.args.named.is_empty() => ir.protected_write = true,
            "protected_write" => return err(a.pos, format!("{}: @@protected_write takes no arguments", m.name)),
            "query_set" => ir.query_set = Some(text(a)?),
            "model_methods" => ir.model_methods = Some(text(a)?),
            _ => {}
        }
    }
    let mut seen = HashSet::new();
    let mut paths = Paths::new();
    let mut origins: Vec<&Member> = vec![];
    for member in &m.members {
        if !seen.insert(member.name.as_str()) {
            return err(member.pos, format!("{}.{} is declared twice", m.name, member.name));
        }
        if models.contains_key(member.ty.name.as_str()) {
            continue;
        }
        if nested::is_nested(ctx, member) {
            let expansion = nested::expand(m, member, items, ctx)?;
            origins.extend(expansion.fields.iter().map(|_| member));
            ir.fields.extend(expansion.fields);
            ir.constraints.extend(expansion.checks);
            paths.extend(expansion.paths);
            continue;
        }
        ir.fields.push(field(m, member, ctx, Some(&ctx.pending))?);
        origins.push(member);
    }
    check_collisions(&ir, &origins)?;
    for a in m.blocks.iter().filter(|a| a.name == "id") {
        composite_id(&mut ir, a)?;
    }
    Ok((ir, paths))
}

/// No two fields of a model share a name or a column. `origins` holds the member that made each field.
fn check_collisions(ir: &ModelIr, origins: &[&Member]) -> Result<()> {
    let (mut names, mut columns): (HashMap<&str, usize>, HashMap<&str, usize>) = (HashMap::new(), HashMap::new());
    for (i, f) in ir.fields.iter().enumerate() {
        let what = format!("{}.{}", ir.name, origins[i].name);
        if let Some(&j) = names.get(f.name.as_str()) {
            let other = &ir.fields[j];
            return err(origins[i].pos, format!("{what}: field {} collides with the field {} of {}; rename a field", f.name, other.name, origins[j].name));
        }
        if let Some(&j) = columns.get(f.column.as_str()) {
            let other = &ir.fields[j];
            return err(
                origins[i].pos,
                format!("{what}: column {} of the field {} collides with the field {} of {}; set @storage(columns, prefix: \"...\") or @map", f.column, f.name, other.name, origins[j].name),
            );
        }
        names.insert(&f.name, i);
        columns.insert(&f.column, i);
    }
    Ok(())
}

/// `@@id([a, b], map: "name")`: marks the fields as the key, in this order.
fn composite_id(ir: &mut ModelIr, a: &Attr) -> Result<()> {
    let what = format!("{}: @@id", ir.name);
    if !ir.primary_key.is_empty() {
        return err(a.pos, format!("{what} is declared twice"));
    }
    if let Some(f) = ir.fields.iter().find(|f| f.primary_key) {
        return err(a.pos, format!("{what}: {}.{} has @id; a model has @id or @@id, not both", ir.name, f.name));
    }
    let Some((lp, Value::List(items))) = a.args.positional.first() else {
        return err(a.pos, format!("{what} takes a list of fields, e.g. @@id([id, type])"));
    };
    if a.args.positional.len() > 1 {
        return err(a.args.positional[1].0, format!("{what} takes one list of fields"));
    }
    if items.len() < 2 {
        return err(*lp, format!("{what} takes two fields or more; use @id on one field"));
    }
    let mut n = Named::new(what.clone(), &a.args);
    let name = n.str("name")?;
    let map = n.str("map")?;
    n.finish()?;
    for (p, v) in items {
        let field = name_of(*p, v, &what)?;
        let Some(f) = ir.fields.iter_mut().find(|f| f.name == field) else {
            return err(*p, format!("{what}: no field {field} in this model"));
        };
        if ir.primary_key.contains(&field) {
            return err(*p, format!("{what}: {field} is listed twice"));
        }
        if f.nullable {
            return err(*p, format!("{what}: {}.{field} is optional; a primary key can't be", ir.name));
        }
        if f.array {
            return err(*p, format!("{what}: {}.{field} is an array; an array can't be a primary key", ir.name));
        }
        f.primary_key = true;
        ir.primary_key.push(field);
    }
    ir.pk_name = map.or(name);
    Ok(())
}

fn field(m: &ModelDecl, member: &Member, ctx: &Ctx<'_>, pending: Option<&Pendings>) -> Result<FieldIr> {
    let TypeRef { pos, name, args, list, optional } = &member.ty;
    let what = format!("{}.{}", m.name, member.name);
    let mut f = FieldIr {
        name: member.name.clone(),
        column: member.name.clone(),
        ty: ColType::Text,
        nullable: *optional,
        array: *list,
        enum_name: None,
        enum_idx: None,
        primary_key: false,
        auto_increment: false,
        unique: false,
        index: false,
        max_length: None,
        default: None,
        default_now: false,
        default_sql: None,
        client_default: None,
        enum_subset: None,
        db_type: None,
        read_sql: None,
        write_sql: None,
        check: None,
        renamed_from: None,
        comment: None,
        requires: vec![],
        hints: BTreeMap::new(),
        nested: None,
    };
    let resolve = |f: &mut FieldIr, ty: &str, values: &[String]| -> Result<()> {
        let Some((ext, def)) = ctx.types.get(ty) else {
            return err(*pos, format!("{what}: unknown type {ty}; extension types come from the built-in extensions or an imported file"));
        };
        let r = def.resolve(ext, ty, values).map_err(|e| super::syntax::Error { pos: *pos, msg: format!("{what}: {e}") })?;
        f.ty = r.value;
        f.db_type = Some(r.db_type);
        f.read_sql = r.read_sql;
        f.write_sql = r.write_sql;
        f.hints = r.hints;
        f.requires = vec![r.extension];
        Ok(())
    };
    let decimal = name == "Decimal";
    let enum_ir = ctx.enums.get(name.as_str());
    if let Some(e) = enum_ir {
        if !args.named.is_empty() {
            return err(args.pos, format!("{what}: {name} takes no named arguments"));
        }
        // `Status(ACTIVE, OLD)`: the members a proxy view expects.
        if !args.positional.is_empty() {
            if !ctx.proxies.contains(&m.name) {
                return err(args.pos, format!("{what}: an enum subset {name}(...) is only allowed on a field of a proxy model"));
            }
            let mut members: Vec<String> = vec![];
            for (p, v) in &args.positional {
                let n = match v {
                    Value::Path(path, None) if path.len() == 1 => path[0].clone(),
                    _ => return err(*p, format!("{what}: an enum subset lists member names")),
                };
                if !e.values.iter().any(|x| x.name == n) {
                    return err(*p, format!("{what}: {} has no value {n}", e.name));
                }
                if members.contains(&n) {
                    return err(*p, format!("{what}: {n} is listed twice"));
                }
                members.push(n);
            }
            f.enum_subset = Some(members);
        }
        f.enum_name = Some(e.name.clone());
        match e.storage {
            EnumStorage::Native => {
                f.ty = ColType::String;
                let t = ident(&e.db_name);
                f.write_sql = Some(format!("CAST({{}} AS {t}{})", if *list { "[]" } else { "" }));
                f.db_type = Some(t);
            }
            EnumStorage::Text => f.ty = ColType::Text,
            EnumStorage::Int => f.ty = ColType::Int,
        }
    } else if name == "Unsupported" {
        if *list {
            return err(*pos, format!("{what}: arrays of extension types aren't supported"));
        }
        // Unsupported("vector(384)"): a type from the extension catalog
        let sql = match args.positional.as_slice() {
            [(_, Value::Str(s))] if args.named.is_empty() => s.trim(),
            _ => return err(args.pos, format!("{what}: Unsupported takes the SQL type as a string, e.g. Unsupported(\"vector(384)\")")),
        };
        let (ty, values) = match sql.split_once('(') {
            Some((ty, rest)) => {
                let Some(inner) = rest.trim_end().strip_suffix(')') else {
                    return err(*pos, format!("{what}: unbalanced parentheses in {sql:?}"));
                };
                (ty.trim(), inner.split(',').map(|v| v.trim().to_owned()).filter(|v| !v.is_empty()).collect())
            }
            None => (sql, vec![]),
        };
        resolve(&mut f, ty, &values)?;
    } else if let Some(ty) = builtin_type(name) {
        f.ty = ty;
        if !args.positional.is_empty() || !args.named.is_empty() {
            return err(args.pos, format!("{what}: {name} takes no arguments"));
        }
        if decimal {
            f.db_type = Some("numeric".into());
        }
    } else if name == "Generic" {
        return err(*pos, format!("{what}: unknown type Generic; a Generic field needs @generic.relation(...) from the orm.generic extension"));
    } else {
        return err(
            *pos,
            format!(
                "{what}: unknown type {name}; expected a model or BigInt, Int, Float, Boolean, String, DateTime, \
                 Json, Decimal, an enum or Unsupported(\"<extension type>\")"
            ),
        );
    }

    let mut native = false;
    for a in &member.attrs {
        fn one<'a>(a: &'a Attr, what: &str) -> Result<&'a Value> {
            match a.args.positional.as_slice() {
                [(_, v)] if a.args.named.is_empty() => Ok(v),
                _ => err(a.pos, format!("{what}: @{} takes one argument", a.name)),
            }
        }
        let text = |a: &Attr| -> Result<String> {
            match one(a, &what)? {
                Value::Str(s) => Ok(s.clone()),
                _ => err(a.pos, format!("{what}: @{}(...) takes a string", a.name)),
            }
        };
        let flag = |a: &Attr| -> Result<bool> {
            if a.args.positional.is_empty() && a.args.named.is_empty() {
                Ok(true)
            } else {
                err(a.pos, format!("{what}: @{} takes no arguments", a.name))
            }
        };
        if let Some(db) = a.name.strip_prefix("db.") {
            if std::mem::replace(&mut native, true) {
                return err(a.pos, format!("{what}: only one @db.* attribute"));
            }
            if name == "Unsupported" {
                return err(a.pos, format!("{what}: Unsupported(...) already names the SQL type"));
            }
            if enum_ir.is_some() {
                return err(a.pos, format!("{what}: an enum field's type comes from the enum (@@storage)"));
            }
            if !a.args.named.is_empty() {
                return err(a.pos, format!("{what}: @db.{db} takes positional arguments"));
            }
            let values = a.args.positional.iter().map(|(p, v)| sql_text(*p, v)).collect::<Result<Vec<_>>>()?;
            match (f.ty, db, values.as_slice()) {
                (ColType::String, "VarChar", [n]) if !decimal => {
                    f.max_length = Some(n.parse().map_err(|_| super::syntax::Error { pos: a.pos, msg: format!("{what}: @db.VarChar(n) needs a positive integer") })?)
                }
                (ColType::String, "VarChar", []) if !decimal => {}
                (ColType::String, "Text", []) if !decimal => f.ty = ColType::Text,
                (ColType::String, "Uuid", []) if !decimal => f.ty = ColType::Uuid,
                (ColType::String, "Citext", []) if !decimal && !*list => resolve(&mut f, "citext", &[])?,
                (ColType::DateTime, "Date", []) => f.ty = ColType::Date,
                (ty, _, _) => {
                    let Some((sql, same)) = native_type(db, ty) else {
                        return err(a.pos, format!("{what}: unknown native type @db.{db}"));
                    };
                    if !(same && values.is_empty()) {
                        f.db_type = Some(if values.is_empty() { sql.to_owned() } else { format!("{sql}({})", values.join(", ")) });
                    }
                }
            }
            continue;
        }
        match a.name.as_str() {
            "id" => f.primary_key = flag(a)?,
            "unique" => f.unique = flag(a)?,
            "check" => {
                if a.args.positional.len() != 1 || !a.args.named.is_empty() {
                    return err(a.pos, format!("{what}: @check takes one argument"));
                }
                let (pos, v) = &a.args.positional[0];
                f.check = Some(expression(pending, &m.name, Context::Check, &format!("{what}: @check"), *pos, v)?);
            }
            "comment" => f.comment = Some(text(a)?),
            "map" => f.column = text(a)?,
            "renamed_from" => f.renamed_from = Some(text(a)?),
            "default" => {
                let v = one(a, &what)?;
                let bad_call = |c: &str| err(a.pos, format!("{what}: {c}(\"...\") takes one string"));
                match v {
                    Value::Path(p, Some(args)) if p.len() == 1 && p[0] == "autoincrement" && args.positional.is_empty() && args.named.is_empty() => {
                        f.auto_increment = true
                    }
                    Value::Path(p, Some(args)) if p.len() == 1 && p[0] == "now" && args.positional.is_empty() && args.named.is_empty() => {
                        f.default_now = true
                    }
                    // Prisma makes `uuid()` in its client, so an imported schema keeps it there.
                    Value::Path(p, Some(args)) if p.len() == 1 && p[0] == "uuid" && args.named.is_empty() => {
                        let call = match args.positional.as_slice() {
                            [] => ClientCall::Uuid,
                            [(_, Value::Num(n))] if n == "4" => ClientCall::Uuid,
                            [(_, Value::Num(n))] if n == "7" => ClientCall::Uuid7,
                            _ => return err(a.pos, format!("{what}: uuid() takes no argument, 4 or 7")),
                        };
                        if f.client_default.is_some() { return err(a.pos, format!("{what}: {ONE_CLIENT_DEFAULT}")); }
                        f.client_default = Some(ClientDefaultIr::Call(call));
                    }
                    Value::Path(p, Some(_)) if p.len() == 1 && (p[0] == "cuid" || p[0] == "nanoid" || p[0] == "ulid") => {
                        return err(a.pos, format!("{what}: {}() is not supported; use @client_default(uuid()) or @client_default(uuid7())", p[0]))
                    }
                    // an enum value: `@default(draft)`
                    Value::Path(p, None) if p.len() == 1 && enum_ir.is_some() => {
                        let e = enum_ir.unwrap();
                        match e.values.iter().find(|x| x.name == p[0]) {
                            Some(x) => f.default = Some(x.value.clone()),
                            None => return err(a.pos, format!("{what}: {} has no value {}", e.name, p[0])),
                        }
                    }
                    Value::List(items) if *list && enum_ir.is_some() => {
                        let e = enum_ir.unwrap();
                        let mut out = vec![];
                        for (p, v) in items {
                            let n = name_of(*p, v, &what)?;
                            match e.values.iter().find(|x| x.name == n) {
                                Some(x) => out.push(x.value.clone()),
                                None => return err(*p, format!("{what}: {} has no value {n}", e.name)),
                            }
                        }
                        f.default = Some(serde_json::Value::Array(out));
                    }
                    _ => match call_str(v, "dbgenerated") {
                        Some(Ok(s)) => f.default_sql = Some(s.to_owned()),
                        Some(Err(())) => return bad_call("dbgenerated"),
                        None => match v {
                            Value::Path(p, _) => {
                                return err(
                                    a.pos,
                                    format!(
                                        "{what}: @default takes a literal, autoincrement(), now() or dbgenerated(\"...\"), not {}",
                                        p.join(".")
                                    ),
                                )
                            }
                            // a Json default is its JSON text
                            Value::Str(s) if f.ty == ColType::Json => {
                                f.default = Some(serde_json::from_str(s).map_err(|e| super::syntax::Error {
                                    pos: a.pos,
                                    msg: format!("{what}: @default on a Json field is JSON text: {e}"),
                                })?)
                            }
                            v => f.default = Some(json_of(a.pos, v)?),
                        },
                    },
                }
            }
            "client_default" => {
                if f.client_default.is_some() { return err(a.pos, format!("{what}: {ONE_CLIENT_DEFAULT}")); }
                let v = one(a, &what)?;
                f.client_default = Some(match v {
                    Value::Path(p, Some(args)) if p.len() == 1 => {
                        let call = match p[0].as_str() {
                            "uuid" => ClientCall::Uuid,
                            "uuid7" => ClientCall::Uuid7,
                            "now" => ClientCall::Now,
                            "autoincrement" | "dbgenerated" => {
                                return err(a.pos, format!("{what}: only the database can make {}(); use @default", p[0]))
                            }
                            other => return err(a.pos, format!("{what}: @client_default takes a literal, uuid(), uuid7() or now(), not {other}()")),
                        };
                        if !args.positional.is_empty() || !args.named.is_empty() {
                            return err(a.pos, format!("{what}: {}() takes no arguments", p[0]));
                        }
                        ClientDefaultIr::Call(call)
                    }
                    v => ClientDefaultIr::Value(literal(a.pos, v, &f, enum_ir, &what)?),
                });
            }
            "relation" => return err(a.pos, format!("{what}: @relation goes on a field whose type is a model")),
            other => return err(a.pos, format!("{what}: unknown attribute @{other}; use {FIELD_ATTRS}")),
        }
    }
    // parameters of other array types are cast to the column's (`varchar(20)[]`, ...)
    if f.array && f.write_sql.is_none() && (f.db_type.is_some() || f.max_length.is_some()) {
        f.write_sql = Some(format!("CAST({{}} AS {})", crate::migrate::model::sql_type(&f)));
    }
    if f.array && (f.primary_key || f.auto_increment) {
        return err(member.pos, format!("{what}: an array can't be a primary key"));
    }
    if f.primary_key && f.nullable {
        return err(member.pos, format!("{what}: a primary key can't be optional"));
    }
    Ok(f)
}

/// The parts of `@relation(...)` both sides of a relation need.
struct RelArgs {
    name: Option<String>,
    fields: Vec<String>,
    references: Vec<String>,
}

fn rel_args(a: &Attr) -> RelArgs {
    let names = |key: &str| match a.args.named.iter().find(|(k, _, _)| k == key) {
        Some((_, _, Value::List(items))) => items.iter().filter_map(|(p, v)| name_of(*p, v, "").ok()).collect(),
        _ => vec![],
    };
    let name = match (a.args.positional.first(), a.args.named.iter().find(|(k, _, _)| k == "name")) {
        (Some((_, Value::Str(s))), _) | (None, Some((_, _, Value::Str(s)))) => Some(s.clone()),
        _ => None,
    };
    RelArgs { name, fields: names("fields"), references: names("references") }
}

fn relation(m: &ModelDecl, member: &Member, fields: &[FieldIr], models: &HashMap<&str, &ModelDecl>, deferred_identity: &HashSet<String>) -> Result<RelationIr> {
    let target = &member.ty.name;
    let what = format!("relation {}.{}", m.name, member.name);
    if let Some(a) = member.attrs.iter().find(|a| a.name != "relation") {
        return err(a.pos, format!("{what}: relations take only @relation(...)"));
    }
    let empty = Attr { pos: member.pos, name: "relation".into(), args: Args { pos: member.pos, ..Default::default() } };
    let rel = member.attrs.iter().find(|a| a.name == "relation").unwrap_or(&empty);
    let mut n = Named::new(what.clone(), &rel.args);
    let name = match rel.args.positional.as_slice() {
        [] => n.str("name")?,
        [(_, Value::Str(s))] => Some(s.clone()),
        [(p, _), ..] => return err(*p, format!("{what}: the only positional argument is the relation name, a string")),
    };
    let target_decl = models[target.as_str()];
    let target_fields: Vec<&str> = target_decl
        .members
        .iter()
        .filter(|m| !models.contains_key(m.ty.name.as_str()))
        .map(|m| m.name.as_str())
        .collect();
    let field_list = |n: &mut Named<'_>, key: &'static str| -> Result<Option<(Pos, Vec<String>)>> {
        match n.get(key) {
            None => Ok(None),
            Some((p, Value::List(items))) if items.is_empty() => err(p, format!("{what}: {key}: takes one field or more")),
            Some((p, Value::List(items))) => Ok(Some((p, items.iter().map(|(fp, v)| name_of(*fp, v, &what)).collect::<Result<_>>()?))),
            Some((p, _)) => err(p, format!("{what}: {key}: takes a list, e.g. [author_id]")),
        }
    };
    if let Some((tp, v)) = n.get("through") {
        let join = name_of(tp, v, &what)?;
        let through_fields = n.names("through_fields")?;
        n.finish()?;
        if join.contains("{model}") {
            // A model with deferred identity gets its join model from a pass, which fills the relation from `join_label`.
            if !deferred_identity.contains(&m.name) {
                return err(
                    member.pos,
                    format!("{what}: {{model}} in a through: name is only expanded in a relation that an extension pass derives; name the join model without it"),
                );
            }
            if !member.ty.list || member.ty.optional {
                return err(member.ty.pos, format!("{what}: a many-to-many relation is a list: {target}[]"));
            }
            if !through_fields.is_empty() {
                return err(tp, format!("{what}: through_fields: does not go with a through: name that has {{model}}"));
            }
            return Ok(RelationIr {
                name: member.name.clone(),
                kind: RelKind::Many,
                target: target.clone(),
                from: vec![],
                to: vec![],
                foreign_key: false,
                on_delete: None,
                on_update: None,
                deferrable: None,
                fk_name: None,
                through: None,
                label: Some(name.unwrap_or_default()),
                join_label: Some(join),
            });
        }
        return many_to_many(m, member, &what, tp, &join, &through_fields, models);
    }
    let from = field_list(&mut n, "fields")?;
    let to = field_list(&mut n, "references")?;

    let Some((fpos, from)) = from else {
        // The back side: the other model's relation to this one holds the key.
        if to.is_some() {
            return err(rel.pos, format!("{what}: references: needs fields:"));
        }
        let placeholder_on_delete = match n.get("onDelete") {
            Some((p, v)) if deferred_identity.contains(&m.name) => Some((p, action(p, &name_of(p, v, &what)?)?)),
            _ => None,
        };
        n.finish()?;
        let back: Vec<RelArgs> = target_decl
            .members
            .iter()
            .filter(|o| o.ty.name == m.name && !std::ptr::eq(*o, member))
            .filter_map(|o| o.attrs.iter().find(|a| a.name == "relation").map(rel_args))
            .filter(|r| !r.fields.is_empty() && r.name == name)
            .collect();
        let back = match back.as_slice() {
            [_] if placeholder_on_delete.is_some() => {
                return err(placeholder_on_delete.map_or(member.pos, |(p, _)| p), format!("{what}: onDelete: belongs on the relation that has fields:"));
            }
            [b] => b,
            // A model with declarations gets its owning relation from a pass: the pass fills `from`, `to` and `kind`'s key from `label`.
            [] if deferred_identity.contains(&m.name) => {
                if member.ty.list && member.ty.optional {
                    return err(member.ty.pos, format!("{what}: a to-many relation can't be optional"));
                }
                return Ok(RelationIr {
                    name: member.name.clone(),
                    kind: if member.ty.list { RelKind::Many } else { RelKind::One },
                    target: target.clone(),
                    from: vec![],
                    to: vec![],
                    foreign_key: false,
                    on_delete: placeholder_on_delete.map(|(_, a)| a),
                    on_update: None,
                    deferrable: None,
                    fk_name: None,
                    through: None,
                    label: Some(name.unwrap_or_default()),
                    join_label: None,
                });
            }
            [] if name.as_deref().is_some_and(|n| n.contains("{model}")) => {
                return err(
                    member.pos,
                    format!("{what}: {{model}} in a relation name is only expanded in a relation that an extension pass derives; name this relation without it"),
                )
            }
            [] => {
                return err(
                    member.pos,
                    format!(
                        "{what}: {target} has no relation to {} with fields: / references:{}",
                        m.name,
                        name.as_ref().map(|n| format!(" named {n:?}")).unwrap_or_default()
                    ),
                )
            }
            _ => return err(member.pos, format!("{what}: {target} has several relations to {}; name them: @relation(\"name\", ...) on both sides", m.name)),
        };
        let (from, to) = (&back.references, &back.fields);
        if from.len() != to.len() {
            return err(member.pos, format!("{what}: the relation on {target} needs references: with one field for each of fields:"));
        }
        let kind = if member.ty.list {
            if member.ty.optional {
                return err(member.ty.pos, format!("{what}: a to-many relation can't be optional"));
            }
            RelKind::Many
        } else {
            // has-one: the key on the other side must be unique
            if !member.ty.optional {
                return err(member.ty.pos, format!("{what}: a one-to-one back relation is optional: {target}?"));
            }
            if !is_unique(target_decl, to) {
                return err(
                    member.pos,
                    format!(
                        "{what}: {target}.{} must be unique for a one-to-one relation (@unique, or {target}[] for to-many)",
                        key_text(to)
                    ),
                );
            }
            RelKind::One
        };
        return Ok(RelationIr {
            name: member.name.clone(),
            kind,
            target: target.clone(),
            from: from.clone(),
            to: to.clone(),
            foreign_key: false,
            on_delete: None,
            on_update: None,
            deferrable: None,
            fk_name: None,
            through: None,
            label: None,
            join_label: None,
        });
    };
    if member.ty.list {
        return err(rel.pos, format!("{what}: fields: goes on the to-one side, not on {target}[]"));
    }
    for name in &from {
        let Some(local) = fields.iter().find(|f| &f.name == name) else {
            return err(fpos, format!("{what}: no field {name} in this model"));
        };
        if local.nullable != member.ty.optional {
            let hint = if local.nullable { format!("{target}?") } else { target.clone() };
            return err(member.ty.pos, format!("{what}: {name} is {}nullable, so the relation type is `{hint}`", if local.nullable { "" } else { "not " }));
        }
    }
    let Some((tpos, to)) = to else { return err(rel.pos, format!("{what}: fields: needs references: [<field of {target}>]")) };
    if to.len() != from.len() {
        return err(tpos, format!("{what}: references: needs one field for each of fields: ({} for {})", to.len(), from.len()));
    }
    if !deferred_identity.contains(target) {
        if let Some(missing) = to.iter().find(|t| !target_fields.contains(&t.as_str())) {
            return err(tpos, format!("{what}: {target} has no field {missing}"));
        }
        if to.len() > 1 && !is_unique(target_decl, &to) {
            return err(
                tpos,
                format!("{what}: references: {} must be the primary key or a unique constraint of {target}, in that order", key_text(&to)),
            );
        }
    }
    let on_delete = match n.get("onDelete") {
        Some((p, v)) => Some(action(p, &name_of(p, v, &what)?)?),
        None => None,
    };
    let on_update = match n.get("onUpdate") {
        Some((p, v)) => Some(action(p, &name_of(p, v, &what)?)?),
        None => None,
    };
    let deferrable = deferrable(&mut n)?;
    let fk_name = n.str("map")?;
    n.finish()?;
    Ok(RelationIr {
        name: member.name.clone(),
        kind: RelKind::One,
        target: target.clone(),
        from,
        to,
        foreign_key: true,
        on_delete,
        on_update,
        deferrable,
        fk_name,
        through: None,
        label: None,
        join_label: None,
    })
}

/// `[a, b]` for several key fields, `a` for one.
fn key_text(fields: &[String]) -> String {
    match fields {
        [one] => one.clone(),
        many => format!("[{}]", many.join(", ")),
    }
}

/// Whether `fields` of `model` hold unique values together: `@id` / `@unique` on the one
/// field, or `@@id` / `@@unique` with exactly these fields in this order.
fn is_unique(model: &ModelDecl, fields: &[String]) -> bool {
    let own = match fields {
        [field] => model.members.iter().any(|mm| &mm.name == field && mm.attrs.iter().any(|a| a.name == "unique" || a.name == "id")),
        _ => false,
    };
    own || model.blocks.iter().any(|b| {
        (b.name == "unique" || b.name == "id")
            && matches!(b.args.positional.first(), Some((_, Value::List(items)))
                if items.len() == fields.len()
                    && items.iter().zip(fields).all(|((_, v), f)| matches!(v, Value::Path(p, None) if p.len() == 1 && &p[0] == f)))
    })
}

/// `tags Tag[] @relation(through: PostTag)`: a many-to-many relation through the
/// to-one relations of a join model, one to each side (`through_fields: [post, tag]`
/// names them when the join model has several).
fn many_to_many(
    m: &ModelDecl,
    member: &Member,
    what: &str,
    pos: Pos,
    join: &str,
    through_fields: &[String],
    models: &HashMap<&str, &ModelDecl>,
) -> Result<RelationIr> {
    let target = &member.ty.name;
    if !member.ty.list || member.ty.optional {
        return err(member.ty.pos, format!("{what}: a many-to-many relation is a list: {target}[]"));
    }
    let Some(join_decl) = models.get(join) else {
        return err(pos, format!("{what}: through: {join} is not a model"));
    };
    // the join model's to-one relations: (relation name, target, fields, references)
    let links: Vec<(&str, &str, RelArgs)> = join_decl
        .members
        .iter()
        .filter(|o| models.contains_key(o.ty.name.as_str()) && !o.ty.list)
        .filter_map(|o| o.attrs.iter().find(|a| a.name == "relation").map(|a| (o.name.as_str(), o.ty.name.as_str(), rel_args(a))))
        .filter(|(_, _, r)| !r.fields.is_empty())
        .collect();
    let pick = |rel: Option<&String>, to: &str, side: &str| -> Result<&RelArgs> {
        let found: Vec<&(&str, &str, RelArgs)> = match rel {
            Some(r) => links.iter().filter(|(n, _, _)| n == r).collect(),
            None => links.iter().filter(|(_, t, _)| *t == to).collect(),
        };
        match found.as_slice() {
            [(_, t, r)] if *t == to => Ok(r),
            [(n, t, _)] => err(pos, format!("{what}: {join}.{n} points at {t}, not {to}")),
            [] => err(pos, format!("{what}: {join} has no relation with fields: {} for the {side} side", match rel {
                Some(r) => format!("named {r}"),
                None => format!("to {to}"),
            })),
            _ => err(pos, format!("{what}: {join} has several relations to {to}; say which with through_fields: [<{side} side>, ...]")),
        }
    };
    let (source, target_link) = match through_fields {
        [] if m.name == *target => {
            return err(pos, format!("{what}: a many-to-many relation of {target} to itself needs through_fields: [<from>, <to>]"))
        }
        [] => (pick(None, &m.name, "source")?, pick(None, target, "target")?),
        [a, b] => (pick(Some(a), &m.name, "source")?, pick(Some(b), target, "target")?),
        _ => return err(pos, format!("{what}: through_fields takes two relations of {join}, e.g. [post, tag]")),
    };
    let ([from], [src]) = (source.references.as_slice(), source.fields.as_slice()) else {
        return err(pos, format!("{what}: the relation of {join} to {} needs fields: and references: with one field each", m.name));
    };
    let ([to], [dst]) = (target_link.references.as_slice(), target_link.fields.as_slice()) else {
        return err(pos, format!("{what}: the relation of {join} to {target} needs fields: and references: with one field each"));
    };
    Ok(RelationIr {
        name: member.name.clone(),
        kind: RelKind::Many,
        target: target.clone(),
        from: vec![from.clone()],
        to: vec![to.clone()],
        foreign_key: false,
        on_delete: None,
        on_update: None,
        deferrable: None,
        fk_name: None,
        through: Some(ThroughIr { model: join.to_owned(), source: src.clone(), target: dst.clone() }),
        label: None,
        join_label: None,
    })
}

/// What follows the field or expression of an index or exclusion key.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeyOptions {
    pub desc: bool,
    pub nulls: Option<Nulls>,
    pub opclass: Option<String>,
    pub collation: Option<String>,
    /// The exclusion operator; only an exclusion key has it.
    pub op: Option<String>,
}

/// The options of one key: a leading `-` (`minus`) or `sort:`, then `nulls:`, `ops:`, `collate:`
/// and, with `with_op`, `op:`. The first `lead` positional arguments are not options.
pub(super) fn key_options(p: Pos, args: &Args, what: &str, minus: bool, lead: usize, with_op: bool) -> Result<KeyOptions> {
    if args.positional.len() > lead {
        return err(p, format!("{what}: key options are named (sort:, nulls:, ops:, collate:{})", if with_op { ", op:" } else { "" }));
    }
    let label = if with_op { format!("{what}: exclusion key") } else { format!("{what} key") };
    let mut n = Named::new(label.clone(), args);
    let desc = match n.name("sort")?.as_deref() {
        None => minus,
        Some(_) if minus => return err(p, format!("{what}: give `-` or sort:, not both")),
        Some("Asc") => false,
        Some("Desc") => true,
        Some(o) => return err(p, format!("sort must be Asc or Desc, not {o}")),
    };
    let nulls = match n.name("nulls")?.as_deref() {
        None => None,
        Some("first") => Some(Nulls::First),
        Some("last") => Some(Nulls::Last),
        Some(o) => return err(p, format!("nulls must be first or last, not {o}")),
    };
    let opclass = match n.get("ops") {
        None => None,
        Some((op, v @ Value::Path(path, Some(_)))) if path.len() == 1 && path[0] == "raw" => Some(raw_sql(op, v, what)?),
        Some((op, v)) => Some(name_of(op, v, &format!("{what}: ops"))?),
    };
    let collation = n.str("collate")?;
    let op = if with_op {
        n.str("op")?
    } else if let Some((op, _)) = n.get("op") {
        return err(op, format!("{label}: op: is for exclusion keys; use @@exclude"));
    } else {
        None
    };
    n.finish()?;
    Ok(KeyOptions { desc, nulls, opclass, collation, op })
}

/// `[field, -field, field(sort: Desc, ops: raw("x")), sql("expr", ...)]` -> index keys
/// (and the exclusion operator of each, when `with_op`).
fn keys(m: &ModelIr, pending: Option<&Pendings>, pos: Pos, v: Option<&Value>, what: &str, with_op: bool) -> Result<Vec<(IndexColumnIr, Option<String>)>> {
    let Some(Value::List(items)) = v else {
        return err(pos, format!("{what}: the first argument is a list of keys, e.g. [author_id, created_at(sort: Desc)]"));
    };
    if items.is_empty() {
        return err(pos, format!("{what}: no keys"));
    }
    let mut out = vec![];
    for (p, item) in items {
        let (minus, operand_pos, item) = match item {
            Value::Desc(inner) => (true, inner.0, &inner.1),
            item => (false, *p, item),
        };
        // A leading `-` on a function call is negation; on a field it is `sort: Desc`.
        let mut negated = None;
        let (field, expr, args) = match item {
            Value::Path(path, _) if minus && path.len() == 1 && path[0] == "sql" => {
                return err(*p, format!("{what}: an expression key takes sort: Desc, not `-`"));
            }
            Value::Path(path, args) if path.len() == 1 && path[0] == "sql" => {
                let Some(args) = args else { return err(*p, "sql(\"...\") needs an expression") };
                let Some((_, Value::Str(e))) = args.positional.first() else {
                    return err(*p, "sql(\"...\") takes the expression as a string");
                };
                (None, Some(e.clone()), args.clone())
            }
            Value::Path(path, Some(_)) if path.len() == 1 && !m.fields.iter().any(|f| f.name == path[0]) && super::expr::function(&path[0]).is_some() => {
                if minus {
                    negated = Some(Value::Neg(*p, Box::new((operand_pos, item.clone()))));
                }
                (None, None, Args::default())
            }
            Value::Path(path, args) if path.len() == 1 => {
                (Some(path[0].clone()), None, args.clone().unwrap_or(Args { pos: *p, ..Default::default() }))
            }
            Value::Binary(..) | Value::Method(..) | Value::Neg(..) | Value::Not(..) => (None, None, Args::default()),
            _ => return err(*p, format!("{what}: a key is a field name, an expression or sql(\"...\")")),
        };
        if field.is_none() && expr.is_none() {
            if (minus && negated.is_none()) || with_op || !args.named.is_empty() {
                let why = if with_op { "an exclusion key needs op:, so write it as sql(\"...\", op: \"...\")" } else { "an expression key takes no options, so write it as sql(\"...\", sort: Desc, ...)" };
                return err(*p, format!("{what}: {why}"));
            }
            let Some(pending) = pending else { return err(*p, format!("{what}: a native expression key is not available here; write sql(\"...\")")) };
            out.push((IndexColumnIr { field: None, expr: Some(pending.defer(&m.name, Context::IndexKey, *p, negated.as_ref().unwrap_or(item))), opclass: None, collation: None, desc: false, nulls: None }, None));
            continue;
        }
        let KeyOptions { desc, nulls, opclass, collation, op } = key_options(*p, &args, what, minus, usize::from(expr.is_some()), with_op)?;
        if with_op && op.is_none() {
            return err(*p, format!("{what}: each exclusion key needs op: \"=\" / \"&&\" / ..."));
        }
        out.push((IndexColumnIr { field, expr, opclass, collation, desc, nulls }, op));
    }
    Ok(out)
}

/// The `where:` of an index or an exclusion.
fn filter(n: &mut Named<'_>, pending: Option<&Pendings>, model: &str, what: &str) -> Result<Option<String>> {
    let Some(extent) = n.extent("where") else { return Ok(None) };
    let Some((_, value)) = n.get("where") else { return Ok(None) };
    Ok(Some(expression(pending, model, Context::IndexFilter, &format!("{what}: where"), extent, value)?))
}

/// Checks every pending expression against the whole schema, lowers it to SQL text and puts the
/// text in the IR slot that holds its marker. Returns the warnings, in source order.
fn resolve_pending(ir: &mut SchemaIr, pending: &Pendings) -> Result<Vec<Diagnostic>> {
    let list = pending.list.take();
    let mut warnings = pending.warnings.take();
    let as_error = |d: Diagnostic| super::syntax::Error { pos: Pos { line: d.span.line, col: d.span.column, len: 0 }, msg: d.message };
    let mut texts = Vec::with_capacity(list.len());
    for e in &list {
        let checked = super::expr::check(ir, &e.model, e.context, e.pos, &e.value).map_err(as_error)?;
        warnings.extend(checked.warnings);
        texts.push(super::render::render(ir, ir.dialect, &checked.root).map_err(as_error)?);
    }
    let fill = |slot: &mut String| {
        if let Some(i) = slot.strip_prefix(MARK).and_then(|n| n.parse::<usize>().ok()) {
            *slot = texts[i].clone();
        }
    };
    let fill_key = |key: &mut IndexColumnIr| key.expr.iter_mut().for_each(fill);
    for m in &mut ir.models {
        m.fields.iter_mut().filter_map(|f| f.check.as_mut()).for_each(fill);
        for ix in &mut m.indexes {
            ix.columns.iter_mut().for_each(fill_key);
            ix.where_.iter_mut().for_each(fill);
        }
        for c in &mut m.constraints {
            match c {
                ConstraintIr::Check { expr, .. } => fill(expr),
                ConstraintIr::Exclude { elements, where_, .. } => {
                    elements.iter_mut().for_each(|e| fill_key(&mut e.column));
                    where_.iter_mut().for_each(fill);
                }
                _ => {}
            }
        }
    }
    warnings.sort_by_key(|w| (w.span.line, w.span.column));
    Ok(warnings)
}

/// `type: Gin` (Prisma's names) or `type: hnsw` -> the access method.
fn method(n: &mut Named<'_>) -> Result<Option<String>> {
    Ok(n.name("type")?.map(|t| t.to_lowercase()))
}

fn model_block(m: &mut ModelIr, b: &Attr, pending: Option<&Pendings>) -> Result<()> {
    let what = format!("{}: @@{}", m.name, b.name);
    let first = b.args.positional.first().map(|(_, v)| v);
    let extra_positional = |max: usize| -> Result<()> {
        match b.args.positional.get(max) {
            Some((p, _)) => err(*p, format!("{what}: too many positional arguments")),
            None => Ok(()),
        }
    };
    match b.name.as_str() {
        "partition" => {
            if m.partition.is_some() { return err(b.pos, format!("{what}: declared twice")); }
            let mut n = Named::new(what.clone(), &b.args);
            n.no_positional()?;
            let mut partition = None;
            for strategy in ["list", "range", "hash"] {
                if let Some((p, v)) = n.get(strategy) {
                    if partition.is_some() { return err(p, format!("{what}: give one strategy")); }
                    let keys = keys(m, None, p, Some(v), &what, false)?.into_iter().map(|(k, _)| k).collect();
                    partition = Some(PartitionKeyIr { strategy: strategy.into(), keys });
                }
            }
            n.finish()?;
            m.partition = Some(partition.ok_or_else(|| super::syntax::Error { pos: b.pos, msg: format!("{what}: give list:, range: or hash:") })?);
        }
        "map" | "comment" | "renamed_from" | "protected_write" | "query_set" | "model_methods" => {}
        "index" => {
            extra_positional(1)?;
            let columns: Vec<IndexColumnIr> = keys(m, pending, b.pos, first, &what, false)?.into_iter().map(|(k, _)| k).collect();
            // `@@index([col])` is the column's own index
            if let ([c], []) = (columns.as_slice(), b.args.named.as_slice()) {
                if c.expr.is_none() && c.opclass.is_none() && c.collation.is_none() && !c.desc && c.nulls.is_none() {
                    if let Some(f) = m.fields.iter_mut().find(|f| Some(&f.name) == c.field.as_ref()) {
                        f.index = true;
                        return Ok(());
                    }
                }
            }
            let mut n = Named::new(what.clone(), &b.args);
            let method = method(&mut n)?;
            let with = match n.get("with") {
                None => vec![],
                Some((_, Value::Object(entries))) => {
                    entries.iter().map(|(k, p, v)| Ok((k.clone(), param(*p, v)?))).collect::<Result<_>>()?
                }
                Some((p, _)) => return err(p, format!("{what}: with: takes {{ name: value, ... }}")),
            };
            let ix = IndexIr {
                name: n.db_name()?,
                columns,
                unique: n.bool("unique")?,
                method,
                where_: filter(&mut n, pending, &m.name, &what)?,
                include: n.names("include")?,
                with,
                nulls_not_distinct: n.bool("nulls_not_distinct")?,
                requires: vec![],
            };
            n.finish()?;
            m.indexes.push(ix);
        }
        "unique" => {
            extra_positional(1)?;
            let fields = match first {
                Some(Value::List(items)) => items
                    .iter()
                    .map(|(p, v)| match v {
                        // a UNIQUE constraint has no key order; a unique index has
                        Value::Desc(_) => err(*p, format!("{what}: a unique constraint has no order; use @@index([...], unique: true) for a descending key")),
                        v => name_of(*p, v, &what),
                    })
                    .collect::<Result<Vec<_>>>()?,
                _ => return err(b.pos, format!("{what}: takes a list of fields, e.g. @@unique([author_id, slug])")),
            };
            let mut n = Named::new(what.clone(), &b.args);
            let c = ConstraintIr::Unique {
                name: n.db_name()?,
                fields,
                nulls_not_distinct: n.bool("nulls_not_distinct")?,
                deferrable: deferrable(&mut n)?,
            };
            n.finish()?;
            m.constraints.push(c);
        }
        "check" => {
            extra_positional(1)?;
            let Some((pos, value)) = b.args.positional.first() else {
                return err(b.pos, format!("{what}: takes an expression, e.g. @@check(price > 0)"));
            };
            let mut n = Named::new(what.clone(), &b.args);
            let c = ConstraintIr::Check { name: n.str("name")?, expr: expression(pending, &m.name, Context::Check, &what, *pos, value)? };
            n.finish()?;
            m.constraints.push(c);
        }
        "exclude" => {
            extra_positional(1)?;
            let ks = keys(m, pending, b.pos, first, &what, true)?;
            let mut n = Named::new(what.clone(), &b.args);
            let method = method(&mut n)?.unwrap_or_else(|| "gist".into());
            // `=` on a plain column inside GiST needs btree_gist.
            let requires = if method == "gist" && ks.iter().any(|(k, op)| k.field.is_some() && op.as_deref() == Some("=")) {
                vec!["btree_gist".to_owned()]
            } else {
                vec![]
            };
            let c = ConstraintIr::Exclude {
                name: n.str("name")?,
                method: Some(method),
                elements: ks
                    .into_iter()
                    .map(|(column, op)| ExcludeElementIr { column, operator: op.unwrap_or_default() })
                    .collect(),
                where_: filter(&mut n, pending, &m.name, &what)?,
                deferrable: deferrable(&mut n)?,
                requires,
            };
            n.finish()?;
            m.constraints.push(c);
        }
        "trigger" => {
            extra_positional(1)?;
            let name = match b.args.positional.first() {
                Some((p, v)) => name_of(*p, v, &what)?,
                None => return err(b.pos, format!("{what}: the first argument is the trigger name")),
            };
            let mut n = Named::new(format!("{what}({name})"), &b.args);
            let mut timing = None;
            for (key, t) in [("before", TriggerTiming::Before), ("after", TriggerTiming::After), ("instead_of", TriggerTiming::InsteadOf)] {
                let events = n.names(key)?;
                if !events.is_empty() {
                    if timing.is_some() {
                        return err(b.pos, format!("{what}({name}): give one of before: / after: / instead_of:"));
                    }
                    let events = events
                        .iter()
                        .map(|e| match e.as_str() {
                            "insert" => Ok(TriggerEvent::Insert),
                            "update" => Ok(TriggerEvent::Update),
                            "delete" => Ok(TriggerEvent::Delete),
                            "truncate" => Ok(TriggerEvent::Truncate),
                            o => err(b.pos, format!("unknown trigger event {o}")),
                        })
                        .collect::<Result<Vec<_>>>()?;
                    timing = Some((t, events));
                }
            }
            let Some((timing, events)) = timing else {
                return err(b.pos, format!("{what}({name}): say when it fires: before: [update], after: [insert, delete], ..."));
            };
            let for_each = match n.name("for_each")?.as_deref() {
                None | Some("row") => ForEach::Row,
                Some("statement") => ForEach::Statement,
                Some(o) => return err(b.pos, format!("for_each must be row or statement, not {o}")),
            };
            let t = TriggerIr {
                name,
                timing,
                events,
                update_of: n.names("update_of")?,
                for_each,
                when: n.sql("when")?,
                function: n.name("function")?,
                args: match n.get("args") {
                    None => vec![],
                    Some((_, Value::List(items))) => items.iter().map(|(p, v)| sql_text(*p, v)).collect::<Result<_>>()?,
                    Some((p, _)) => return err(p, "args: takes a list"),
                },
                body: n.str("body")?,
                language: n.name("language")?,
            };
            n.finish()?;
            if t.body.is_some() == t.function.is_some() {
                return err(b.pos, format!("{what}({}): give body: \"...\" or function: name (a `function` block)", t.name));
            }
            m.triggers.push(t);
        }
        "id" => {}
        other => {
            return err(
                b.pos,
                format!(
                    "{}: unknown attribute @@{other}; use @@map, @@index, @@unique, @@check, @@exclude, @@trigger, \
                     @@comment or @@renamed_from",
                    m.name
                ),
            )
        }
    }
    Ok(())
}

/// A storage parameter value as SQL.
fn param(pos: Pos, v: &Value) -> Result<String> {
    Ok(match v {
        Value::Str(s) => format!("'{}'", s.replace('\'', "''")),
        other => sql_text(pos, other)?,
    })
}

/// A behavioral declaration argument as literal JSON: a bare identifier is a string.
/// Bare names, `null` and objects are literals only for arguments of kind `value`
/// (or of an attribute no compiled manifest declares, which validation rejects later).
fn declaration_value(pos: Pos, value: &Value, kind: Option<ArgumentKind>) -> Result<serde_json::Value> {
    if kind.is_some_and(|k| k != ArgumentKind::Value) {
        return json_of(pos, value);
    }
    Ok(match value {
        Value::Path(path, None) if path.as_slice() == ["null"] => serde_json::Value::Null,
        Value::Path(path, None) => serde_json::Value::String(path.join(".")),
        Value::Path(_, Some(_)) => return err(pos, "behavioral arguments require literal values, not function calls"),
        Value::List(items) => serde_json::Value::Array(items.iter().map(|(p, v)| declaration_value(*p, v, None)).collect::<Result<_>>()?),
        Value::Object(entries) => {
            let mut object = serde_json::Map::new();
            for (key, pos, value) in entries {
                if object.insert(key.clone(), declaration_value(*pos, value, None)?).is_some() {
                    return err(*pos, format!("duplicate behavioral object key {key}"));
                }
            }
            serde_json::Value::Object(object)
        }
        value => json_of(pos, value)?,
    })
}

/// The namespaces (`outside` of `@@outside.tag`) of the behavioral attributes that the items use.
pub(super) fn used_namespaces(items: &[Item]) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    let mut add = |attrs: &[Attr]| for a in attrs {
        if let Some((namespace, _)) = a.name.split_once('.').filter(|(namespace, _)| *namespace != "db") { out.insert(namespace.to_owned()); }
    };
    for item in items {
        if let Item::Model(m) = item {
            add(&m.blocks);
            for member in &m.members { add(&member.attrs); }
        }
    }
    out
}

/// The `extensions = ["id", ...]` of every `generator` block.
pub(super) fn generator_extensions(items: &[Item]) -> Result<Vec<String>> {
    let mut out = vec![];
    for item in items {
        let Item::Generator { props } = item else { continue };
        for (key, pos, value) in props.iter().filter(|(key, _, _)| key == "extensions") {
            let Value::List(list) = value else { return err(*pos, format!("generator {key} is a list of extension ids, for example [\"acme.audit\"]")) };
            for (pos, id) in list {
                let Value::Str(id) = id else { return err(*pos, "generator extensions are strings: write each id in quotes") };
                if !out.contains(id) { out.push(id.clone()); }
            }
        }
    }
    Ok(out)
}

/// Preserve namespaced behavioral declarations before ordinary schema lowering.
/// Database type attributes keep their existing interpretation.
pub(super) fn behavior_declarations(items: &mut [Item], file: &str, manifests: &[crate::behavior::Manifest]) -> Result<Vec<crate::behavior::Declaration>> {
    let mut out = vec![];
    let mut collect = |attrs: &mut Vec<Attr>, model: &str, field: Option<(&str, String)>| -> Result<()> {
        let mut ordinary = vec![];
        for a in attrs.drain(..) {
            if !a.name.contains('.') || a.name.starts_with("db.") {
                ordinary.push(a);
                continue;
            }
            let declared = declared(manifests, &a.name, field.is_some());
            let mut arguments = BTreeMap::new();
            for (name, pos, value) in &a.args.named {
                let kind = declared.and_then(|d| d.arguments.get(name)).map(|arg| arg.kind);
                if arguments.insert(name.clone(), declaration_value(*pos, value, kind)?).is_some() {
                    return err(*pos, format!("@{}: duplicate argument {name}", a.name));
                }
            }
            out.push(crate::behavior::Declaration {
                lowered: false,
                attribute: a.name, model: model.to_owned(),
                field: field.as_ref().map(|(name, _)| (*name).to_owned()), field_type: field.as_ref().map(|(_, ty)| ty.clone()),
                arguments,
                positional: a.args.positional.iter().enumerate()
                    .map(|(i, (pos, v))| declaration_value(*pos, v, declared.and_then(|d| d.positional_at(i)).map(|arg| arg.kind)))
                    .collect::<Result<_>>()?,
                location: crate::behavior::SourceLocation { file: file.to_owned(), line: a.pos.line, column: a.pos.col },
            });
        }
        *attrs = ordinary;
        Ok(())
    };
    for item in items {
        if let Item::Model(m) = item {
            collect(&mut m.blocks, &m.name, None)?;
            let mut members = vec![];
            for mut member in m.members.drain(..) {
                let ty = format!("{}{}{}", member.ty.name, if member.ty.list { "[]" } else { "" }, if member.ty.optional { "?" } else { "" });
                let has = |name: &str| member.attrs.iter().any(|a| a.name == name);
                // The generic pass owns these members: a reverse field is no relation, and a
                // `Generic` field becomes its type and key fields at this position.
                let reverse = has("generic.reverse");
                let generic = member.ty.name == "Generic" && has("generic.relation") && declared(manifests, "generic.relation", true).is_some();
                collect(&mut member.attrs, &m.name, Some((&member.name, ty)))?;
                if let Some(a) = member.attrs.iter().find(|_| reverse || generic) {
                    // A reverse field has no explicit form; a Generic field has @@generic.relation.
                    let (kind, cure) = if reverse { ("@generic.reverse", "remove it") } else { ("Generic", "use the explicit @@generic.relation form for @map, @unique, @db.* and defaults") };
                    return err(a.pos, format!("{}.{}: @{} is not allowed on a {kind} field; {cure}", m.name, member.name, a.name));
                }
                if reverse { continue; }
                if generic { member.ty.name = "Int".into(); }
                members.push(member);
            }
            m.members = members;
        }
    }
    Ok(out)
}

/// The manifest entry of attribute `name` on a field (`@name`) or a model (`@@name`).
fn declared<'m>(manifests: &'m [crate::behavior::Manifest], name: &str, field: bool) -> Option<&'m crate::behavior::Attribute> {
    let named = || manifests.iter().flat_map(|m| &m.attributes).filter(|d| d.name == name);
    named().find(|d| (d.target == crate::behavior::AttributeTarget::Field) == field).or_else(|| named().next())
}

fn lower_partitions(items: &[Item], ir: &mut SchemaIr) -> Result<()> {
    let mut pending: Vec<_> = items.iter().filter_map(|i| match i {
        Item::Partition { pos, name, props, blocks } => Some((*pos, name, props, blocks)), _ => None,
    }).collect();
    while !pending.is_empty() {
        let before = pending.len();
        let mut rest = vec![];
        for (pos, name, props, blocks) in pending {
            let of = props.iter().find(|(k, _, _)| k == "of")
                .ok_or_else(|| super::syntax::Error { pos, msg: format!("partition {name}: missing of") })?;
            let of = name_of(of.1, &of.2, "partition of")?;
            let parent = ir.models.iter().find(|m| m.name == of)
                .or_else(|| ir.partitions.iter().find(|p| p.objects.name == of).map(|p| &p.objects));
            let Some(parent) = parent else { rest.push((pos, name, props, blocks)); continue };
            let Some(key) = &parent.partition else { return err(pos, format!("partition {name}: {of} has no @@partition")); };
            let get = |key: &str| props.iter().find(|(k, _, _)| k == key).map(|(_, _, v)| v);
            let value = |v: &Value, field: Option<&FieldIr>| -> Result<String> {
                Ok(match v {
                    Value::Str(s) => crate::migrate::model::quote_literal(s),
                    Value::Num(s) => s.clone(),
                    Value::Bool(b) => b.to_string(),
                    Value::Path(p, None) if p.len() == 1 && (p[0] == "MINVALUE" || p[0] == "MAXVALUE") => p[0].clone(),
                    Value::Path(p, None) if p.len() == 1 => {
                        let s = &p[0];
                        let member = field.and_then(|f| f.enum_name.as_ref()).and_then(|n| ir.enums.iter().find(|e| &e.name == n))
                            .and_then(|e| e.values.iter().find(|v| &v.name == s));
                        match member {
                            Some(m) => match &m.value { serde_json::Value::String(s) => crate::migrate::model::quote_literal(s), v => v.to_string() },
                            None => return err(pos, format!("partition {name}: {s} is not a member of the partition key enum")),
                        }
                    }
                    Value::Path(_, Some(_)) => match call_str(v, "sql").or_else(|| call_str(v, "raw")) {
                        Some(Ok(s)) => s.to_owned(),
                        _ => return err(pos, format!("partition {name}: expected sql(\"...\")")),
                    },
                    _ => return err(pos, format!("partition {name}: expected a bound literal")),
                })
            };
            let tuple = |v: Option<&Value>, list: bool| -> Result<String> {
                let Some(Value::List(vs)) = v else { return err(pos, format!("partition {name}: expected a bound list")); };
                if vs.is_empty() || (!list && vs.len() != key.keys.len()) { return err(pos, format!("partition {name}: bound arity does not match {} keys", key.keys.len())); }
                vs.iter().enumerate().map(|(i, (_, v))| {
                    let field = key.keys.get(if list { 0 } else { i }).and_then(|k| k.field.as_ref()).and_then(|n| parent.fields.iter().find(|f| &f.name == n));
                    value(v, field)
                }).collect::<Result<Vec<_>>>().map(|v| v.join(", "))
            };
            if matches!(get("default"), Some(Value::Bool(false))) {
                return err(pos, format!("partition {name}: use `default = true` or remove it"));
            }
            let allowed: &[&str] = if matches!(get("default"), Some(Value::Bool(true))) { &["of", "default"] }
                else { match key.strategy.as_str() { "list" => &["of", "values"], "range" => &["of", "from", "to"], _ => &["of", "modulus", "remainder"] } };
            for (k, _, _) in props { if !allowed.contains(&k.as_str()) { return err(pos, format!("partition {name}: {k} does not match {} strategy", key.strategy)); } }
            let bound = if allowed.contains(&"default") {
                if key.strategy == "hash" { return err(pos, "hash partition cannot be default"); }
                "DEFAULT".into()
            } else { match key.strategy.as_str() {
                "list" => format!("FOR VALUES IN ({})", tuple(get("values"), true)?),
                "range" => format!("FOR VALUES FROM ({}) TO ({})", tuple(get("from"), false)?, tuple(get("to"), false)?),
                _ => {
                    let integer = |k: &str| -> Result<u32> { match get(k) {
                        Some(Value::Num(s)) => s.parse().map_err(|_| super::syntax::Error { pos, msg: format!("partition {name}: {k} must be a nonnegative integer") }),
                        _ => err(pos, format!("partition {name}: missing integer {k}")),
                    }};
                    let (m, r) = (integer("modulus")?, integer("remainder")?);
                    if m == 0 || r >= m { return err(pos, format!("partition {name}: require modulus > remainder >= 0")); }
                    format!("FOR VALUES WITH (modulus {m}, remainder {r})")
                }
            }};
            let mut fields = parent.fields.clone();
            for f in &mut fields { f.primary_key = false; f.unique = false; f.index = false; f.check = None; }
            let mut objects: ModelIr = serde_json::from_value(serde_json::json!({"name": name, "table": name, "fields": fields})).unwrap();
            for block in blocks {
                if !["partition", "check", "index", "unique", "exclude", "trigger", "comment", "renamed_from"].contains(&block.name.as_str()) {
                    return err(block.pos, format!("partition {name}: unsupported @@{}", block.name));
                }
                if block.name == "comment" || block.name == "renamed_from" {
                    let [(_, Value::Str(s))] = block.args.positional.as_slice() else { return err(block.pos, "expected one string"); };
                    if !block.args.named.is_empty() { return err(block.pos, "expected one string"); }
                    if block.name == "comment" { objects.comment = Some(s.clone()); } else { objects.renamed_from = Some(s.clone()); }
                } else { model_block(&mut objects, block, None)?; }
            }
            ir.partitions.push(PartitionIr { of, bound, objects });
        }
        if rest.len() == before { return err(rest[0].0, format!("partition {}: unknown or cyclic parent", rest[0].1)); }
        pending = rest;
    }
    Ok(())
}
