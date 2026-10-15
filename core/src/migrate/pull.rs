//! `orm pull`: a [`DbSchema`] read from a live database, written as a schema file.
//!
//! This is the reverse of [`super::model::build`]. Types, defaults and expressions are
//! the database's own text; the SQL-to-schema type map lives only here. What the schema
//! language cannot say is left out and reported as a gap, and listed at the end of the
//! file, so a reader sees what the pulled schema does not reproduce.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::model::{object_name, DbSchema, ForeignKey, Function, Index, IndexKey, Table, Unique};
use crate::dialect::Dialect;
use crate::ir::{Deferrable, ForEach, Nulls, OnDelete, TriggerEvent, TriggerTiming};

/// A pulled schema file and what it leaves out.
#[derive(Debug, Clone, Default)]
pub struct Pulled {
    pub schema: String,
    pub gaps: Vec<String>,
}

/// Type names the schema language reserves.
const RESERVED: [&str; 14] = [
    "BigInt", "Int", "Float", "Boolean", "String", "DateTime", "Json", "Decimal", "Unsupported", "Generic", "Bytes",
    "model", "enum", "function",
];

/// Extension types the built-in catalog knows (`Unsupported("...")`).
const EXTENSION_TYPES: [&str; 7] = ["vector", "halfvec", "sparsevec", "geometry", "geography", "hstore", "ltree"];

fn is_ident(s: &str) -> bool {
    let mut c = s.chars();
    matches!(c.next(), Some(ch) if ch == '_' || ch.is_ascii_alphabetic()) && c.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

/// `s` as a schema string literal.
fn quoted(s: &str) -> String {
    let mut out = String::from("\"");
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Python keywords: a field or member with such a name would not compile in models.py.
const KEYWORDS: [&str; 35] = [
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif",
    "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda", "nonlocal", "not", "or",
    "pass", "raise", "return", "try", "while", "with", "yield",
];

/// A name usable in the schema and the generated models: `s` when it is an identifier
/// and not a keyword, else `s` cleaned up.
fn identifier(s: &str) -> String {
    let mut out: String = s.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    if !out.starts_with(|c: char| c == '_' || c.is_ascii_alphabetic()) {
        out.insert(0, '_');
    }
    if KEYWORDS.contains(&out.as_str()) {
        out.push('_');
    }
    out
}

fn pascal(s: &str) -> String {
    let mut out = String::new();
    for part in s.split(|c: char| !c.is_ascii_alphanumeric()).filter(|p| !p.is_empty()) {
        let mut c = part.chars();
        if let Some(first) = c.next() {
            out.push(first.to_ascii_uppercase());
            out.push_str(c.as_str());
        }
    }
    if !out.starts_with(|c: char| c.is_ascii_alphabetic()) {
        out.insert(0, 'T');
    }
    out
}

fn unique_name(base: String, used: &mut BTreeSet<String>) -> String {
    let mut name = base.clone();
    let mut i = 2;
    while used.contains(&name) || RESERVED.contains(&name.as_str()) {
        name = format!("{base}{i}");
        i += 1;
    }
    used.insert(name.clone());
    name
}

/// The schema type of a column: `(type, @db attribute, array)`, or `None` when the
/// schema language has no type for it.
fn field_type(ty: &str, dialect: Dialect, enums: &HashMap<String, String>) -> Option<(String, Option<String>, bool)> {
    let (base, array) = match ty.strip_suffix("[]") {
        Some(b) => (b.trim(), true),
        None => (ty.trim(), false),
    };
    if dialect == Dialect::Sqlite {
        // the storage class, with the declared names other tools use for booleans and dates
        let upper = base.to_ascii_uppercase();
        let t = if upper.contains("INT") {
            "BigInt"
        } else if upper.starts_with("BOOL") {
            "Boolean"
        } else if upper.starts_with("DATE") || upper.starts_with("TIME") {
            "DateTime"
        } else if upper.contains("REAL") || upper.contains("FLOA") || upper.contains("DOUB") || upper.starts_with("DEC") || upper.starts_with("NUMERIC") {
            "Float"
        } else {
            "String"
        };
        return Some((t.into(), None, false));
    }
    let unquoted = base.trim_matches('"');
    if let Some(e) = enums.get(unquoted) {
        return Some((e.clone(), None, array));
    }
    let (name, args) = match base.split_once('(') {
        Some((n, rest)) => {
            let (args, tail) = rest.split_once(')')?;
            (format!("{}{}", n.trim(), tail), Some(args.split(',').map(str::trim).collect::<Vec<_>>().join(", ")))
        }
        None => (base.to_owned(), None),
    };
    let db = |attr: &str| Some(match &args { Some(a) => format!("@db.{attr}({a})"), None => format!("@db.{attr}") });
    let (t, attr) = match (name.as_str(), &args) {
        ("bigint", None) => ("BigInt", None),
        ("integer", None) => ("Int", None),
        ("smallint", None) => ("Int", db("SmallInt")),
        ("oid", None) => ("Int", db("Oid")),
        ("double precision", None) => ("Float", None),
        ("real", None) => ("Float", db("Real")),
        ("boolean", None) => ("Boolean", None),
        ("text", None) => ("String", db("Text")),
        ("character varying", _) => ("String", args.as_ref().and(db("VarChar"))),
        ("character", _) => ("String", db("Char")),
        ("uuid", None) => ("String", db("Uuid")),
        ("citext", None) => ("String", db("Citext")),
        ("inet", None) => ("String", db("Inet")),
        ("xml", None) => ("String", db("Xml")),
        ("bit", _) => ("String", db("Bit")),
        ("bit varying", _) => ("String", db("VarBit")),
        ("bytea", None) => ("String", db("ByteA")),
        ("jsonb", None) => ("Json", None),
        ("json", None) => ("Json", db("Json")),
        ("numeric", None) => ("Decimal", None),
        ("numeric", Some(_)) => ("Decimal", db("Decimal")),
        ("money", None) => ("Decimal", db("Money")),
        ("timestamp with time zone", None) => ("DateTime", None),
        ("timestamp with time zone", Some(_)) => ("DateTime", db("Timestamptz")),
        ("timestamp without time zone", _) => ("DateTime", db("Timestamp")),
        ("date", None) => ("DateTime", db("Date")),
        ("time without time zone", _) => ("DateTime", db("Time")),
        ("time with time zone", _) => ("DateTime", db("Timetz")),
        _ => {
            let ext = base.split('(').next().unwrap_or("").trim();
            if !array && EXTENSION_TYPES.contains(&ext) {
                return Some((format!("Unsupported({})", quoted(base)), None, false));
            }
            return None;
        }
    };
    if array && matches!(attr.as_deref(), Some("@db.Citext")) {
        return None;
    }
    Some((t.into(), attr, array))
}

/// A quoted SQL literal at the start of `s`: its value and the text after it.
fn sql_string(s: &str) -> Option<(String, &str)> {
    let rest = s.strip_prefix('\'')?;
    let mut value = String::new();
    let mut chars = rest.char_indices().peekable();
    while let Some((i, ch)) = chars.next() {
        if ch == '\'' {
            if chars.peek().map(|(_, c)| *c) == Some('\'') {
                value.push('\'');
                chars.next();
            } else {
                return Some((value, &rest[i + 1..]));
            }
        } else {
            value.push(ch);
        }
    }
    None
}

/// A number the schema writes back with the same text (`0.00` would become `0.0`).
fn is_number(s: &str) -> bool {
    !s.contains(['e', 'E', 'i', 'n', 'N']) && s.parse::<f64>().is_ok_and(|v| v.to_string() == s.trim_start_matches('+'))
}

struct FieldKind<'a> {
    ty: &'a str,
    array: bool,
    enum_values: Option<&'a [(String, String)]>,
}

/// The `@default(...)` of a column, from its SQL default.
fn default_attr(default: &str, kind: &FieldKind<'_>) -> String {
    let d = default.trim();
    let raw = || format!("@default(dbgenerated({}))", quoted(d));
    if kind.array {
        return raw();
    }
    if d == "now()" && kind.ty == "DateTime" {
        return "@default(now())".into();
    }
    let numeric = matches!(kind.ty, "BigInt" | "Int" | "Float" | "Decimal");
    if let Some((value, rest)) = sql_string(d) {
        let cast_only = rest.is_empty() || (rest.starts_with("::") && !rest[2..].contains([' ', '(']) || rest.starts_with("::character varying") || rest.starts_with("::timestamp with") || rest.starts_with("::timestamp without"));
        if !cast_only {
            return raw();
        }
        if let Some(values) = kind.enum_values {
            return match values.iter().find(|(_, v)| *v == value) {
                Some((member, _)) => format!("@default({member})"),
                None => raw(),
            };
        }
        return match kind.ty {
            "String" | "DateTime" | "Json" => format!("@default({})", quoted(&value)),
            _ if numeric && is_number(&value) => format!("@default({value})"),
            _ => raw(),
        };
    }
    if numeric && is_number(d) {
        return format!("@default({d})");
    }
    if kind.ty == "Boolean" && (d == "true" || d == "false") {
        return format!("@default({d})");
    }
    raw()
}

fn action(a: OnDelete) -> &'static str {
    match a {
        OnDelete::Cascade => "Cascade",
        OnDelete::SetNull => "SetNull",
        OnDelete::SetDefault => "SetDefault",
        OnDelete::Restrict => "Restrict",
        OnDelete::NoAction => "NoAction",
    }
}

fn deferrable(d: Option<Deferrable>) -> Option<&'static str> {
    d.map(|d| match d {
        Deferrable::Immediate => "deferrable: immediate",
        Deferrable::Deferred => "deferrable: deferred",
    })
}

/// One model being written: its table, name and column-to-field map.
struct ModelOut<'a> {
    table: &'a Table,
    name: String,
    fields: BTreeMap<String, String>,
    used: BTreeSet<String>,
    lines: Vec<String>,
    attrs: Vec<String>,
}

impl ModelOut<'_> {
    fn field(&self, column: &str) -> String {
        self.fields.get(column).cloned().unwrap_or_else(|| column.to_owned())
    }

    fn key(&self, k: &IndexKey, op: Option<&str>) -> String {
        let mut opts = vec![];
        if k.desc && k.expr.is_some() {
            opts.push("sort: Desc".to_owned());
        }
        match k.nulls {
            Some(Nulls::First) => opts.push("nulls: first".into()),
            Some(Nulls::Last) => opts.push("nulls: last".into()),
            None => {}
        }
        if let Some(o) = &k.opclass {
            opts.push(format!("ops: raw({})", quoted(o)));
        }
        if let Some(c) = &k.collation {
            opts.push(format!("collate: {}", quoted(c)));
        }
        if let Some(op) = op {
            opts.push(format!("op: {}", quoted(op)));
        }
        match (&k.column, &k.expr) {
            (Some(c), _) => {
                let name = format!("{}{}", if k.desc { "-" } else { "" }, self.field(c));
                if opts.is_empty() { name } else { format!("{name}({})", opts.join(", ")) }
            }
            (None, e) => {
                let mut args = vec![quoted(e.as_deref().unwrap_or(""))];
                args.extend(opts);
                format!("sql({})", args.join(", "))
            }
        }
    }

    fn fields_list(&self, columns: &[String]) -> String {
        format!("[{}]", columns.iter().map(|c| self.field(c)).collect::<Vec<_>>().join(", "))
    }
}

/// Writes `db` as a schema file. `gaps` are the introspection's own gaps; the result
/// adds what the writer leaves out.
pub fn pull(db: &DbSchema, gaps: &[String]) -> Pulled {
    let mut gaps = gaps.to_vec();
    let dialect = db.dialect;
    let mut type_names = BTreeSet::new();

    // -- enums ---------------------------------------------------------------------------
    let mut enums: HashMap<String, String> = HashMap::new();
    let mut enum_values: HashMap<String, Vec<(String, String)>> = HashMap::new();
    let mut enum_blocks = vec![];
    for e in &db.enums {
        let name = unique_name(pascal(&e.name), &mut type_names);
        let mut used = BTreeSet::new();
        let mut members = vec![];
        let mut lines = vec![];
        for v in &e.values {
            let member = unique_name(identifier(v), &mut used);
            lines.push(if member == *v { format!("  {member}") } else { format!("  {member} @map({})", quoted(v)) });
            members.push((member, v.clone()));
        }
        if name.to_lowercase() != e.name {
            lines.push(format!("  @@map({})", quoted(&e.name)));
        }
        if let Some(c) = &e.comment {
            lines.push(format!("  @@comment({})", quoted(c)));
        }
        enum_blocks.push(format!("enum {name} {{\n{}\n}}", lines.join("\n")));
        enums.insert(e.name.clone(), name.clone());
        enum_values.insert(name, members);
    }

    // -- models and fields -----------------------------------------------------------------
    let mut models: Vec<ModelOut<'_>> = vec![];
    for t in &db.tables {
        if t.partition_of.is_some() {
            models.push(ModelOut { table: t, name: t.name.clone(), fields: BTreeMap::new(), used: BTreeSet::new(), lines: vec![], attrs: vec![] });
            continue;
        }
        let pk = match &t.primary_key {
            Some(pk) if !pk.columns.is_empty() => pk,
            _ => {
                gaps.push(format!("table {} has no primary key; it is left out", t.name));
                continue;
            }
        };
        if pk.columns.iter().any(|c| t.column(c).is_none_or(|c| field_type(&c.ty, dialect, &enums).is_none())) {
            gaps.push(format!("table {} has a primary key column the schema can't declare; it is left out", t.name));
            continue;
        }
        let expected_pk = object_name(&[&t.name, "pkey"]);
        if pk.columns.len() == 1 && pk.name != expected_pk {
            gaps.push(format!("primary key {} of {} is named {expected_pk} by the schema", pk.name, t.name));
        }
        let name = unique_name(pascal(&t.name), &mut type_names);
        let mut m = ModelOut { table: t, name, fields: BTreeMap::new(), used: BTreeSet::new(), lines: vec![], attrs: vec![] };
        for c in &t.columns {
            let field = unique_name(identifier(&c.name), &mut m.used);
            m.fields.insert(c.name.clone(), field);
        }
        models.push(m);
    }
    let model_of: HashMap<String, usize> = models.iter().enumerate().map(|(i, m)| (m.table.name.clone(), i)).collect();

    // Partitions share their root's column-to-field mapping, even through multiple levels.
    for i in 0..models.len() {
        let mut root = i;
        let mut seen = BTreeSet::new();
        while let Some(p) = &models[root].table.partition_of {
            if !seen.insert(root) { break; }
            let Some(&parent) = model_of.get(&p.parent) else { break };
            root = parent;
        }
        if root != i { models[i].fields = models[root].fields.clone(); }
    }
    for m in &mut models {
        let t = m.table;
        if let Some(key) = &t.partition_by { m.attrs.push(partition_attr(m, key)); }
        let pk = t.primary_key.as_ref().filter(|p| p.columns.len() == 1).map(|p| p.columns[0].clone()).unwrap_or_default();
        let single_uniques: BTreeSet<&str> = t
            .uniques
            .iter()
            .filter(|u| shorthand_unique(t, u))
            .map(|u| u.columns[0].as_str())
            .collect();
        for c in &t.columns {
            let field = m.field(&c.name);
            let Some((ty, db_attr, array)) = field_type(&c.ty, dialect, &enums) else {
                gaps.push(format!("column {}.{} has type {}, which the schema can't declare; it is left out", t.name, c.name, c.ty));
                m.fields.remove(&c.name);
                continue;
            };
            let mut line = format!("{field} {ty}{}{}", if array { "[]" } else { "" }, if c.nullable { "?" } else { "" });
            let mut attrs = vec![];
            if c.name == pk {
                attrs.push("@id".to_owned());
            }
            let serial = c.default.as_deref().is_some_and(|d| d.starts_with("nextval(") && d.ends_with("::regclass)"));
            if c.identity || serial {
                attrs.push("@default(autoincrement())".into());
                if serial {
                    gaps.push(format!("column {}.{} uses a sequence ({}); the schema makes it an identity column", t.name, c.name, c.default.as_deref().unwrap_or("")));
                }
            } else if let Some(d) = &c.default {
                let kind = FieldKind { ty: &ty, array, enum_values: enum_values.get(&ty).map(Vec::as_slice) };
                attrs.push(default_attr(d, &kind));
            }
            if single_uniques.contains(c.name.as_str()) {
                attrs.push("@unique".into());
            }
            if field != c.name {
                attrs.push(format!("@map({})", quoted(&c.name)));
            }
            attrs.extend(db_attr);
            if let Some(cm) = &c.comment {
                attrs.push(format!("@comment({})", quoted(cm)));
            }
            if !attrs.is_empty() {
                line.push(' ');
                line.push_str(&attrs.join(" "));
            }
            m.lines.push(line);
        }
        let keep = |cols: &[String], m: &ModelOut<'_>| cols.iter().all(|c| m.fields.contains_key(c));
        if let Some(pk) = t.primary_key.as_ref().filter(|p| p.columns.len() > 1) {
            let mut args = vec![m.fields_list(&pk.columns)];
            if pk.name != object_name(&[&t.name, "pkey"]) {
                args.push(format!("map: {}", quoted(&pk.name)));
            }
            m.attrs.push(format!("@@id({})", args.join(", ")));
        }

        for u in &t.uniques {
            if shorthand_unique(t, u) {
                continue;
            }
            if !keep(&u.columns, m) {
                gaps.push(format!("unique {}.{} uses a column that is left out", t.name, u.name));
                continue;
            }
            let mut args = vec![m.fields_list(&u.columns), format!("map: {}", quoted(&u.name))];
            if u.nulls_not_distinct {
                args.push("nulls_not_distinct: true".into());
            }
            args.extend(deferrable(u.deferrable).map(str::to_owned));
            m.attrs.push(format!("@@unique({})", args.join(", ")));
        }
        for ix in &t.indexes {
            if let Some(line) = index_attr(m, ix, &single_uniques) {
                m.attrs.push(line);
            } else {
                gaps.push(format!("index {} uses a column that is left out", ix.name));
            }
        }
        for c in &t.checks {
            m.attrs.push(format!("@@check({}, name: {})", quoted(&c.expr), quoted(&c.name)));
        }
        for x in &t.exclusions {
            let keys: Vec<String> = x.elements.iter().map(|e| m.key(&e.key, Some(&e.operator))).collect();
            let mut args = vec![format!("[{}]", keys.join(", ")), format!("type: {}", x.method), format!("name: {}", quoted(&x.name))];
            if let Some(w) = &x.where_ {
                args.push(format!("where: {}", quoted(w)));
            }
            args.extend(deferrable(x.deferrable).map(str::to_owned));
            m.attrs.push(format!("@@exclude({})", args.join(", ")));
        }
    }

    // -- relations -------------------------------------------------------------------------
    let mut pairs: HashMap<(usize, usize), usize> = HashMap::new();
    let mut links: Vec<(usize, usize, &ForeignKey)> = vec![];
    for (ci, m) in models.iter().enumerate() {
        for fk in &m.table.foreign_keys {
            let Some(&pi) = model_of.get(&fk.ref_table) else {
                gaps.push(format!("foreign key {}.{} references {}, which is left out", m.table.name, fk.name, fk.ref_table));
                continue;
            };
            let parent = &models[pi];
            let target_unique = parent.table.primary_key.as_ref().is_some_and(|p| p.columns == fk.ref_columns)
                || parent.table.uniques.iter().any(|u| u.columns == fk.ref_columns);
            if !target_unique || !fk.columns.iter().all(|c| m.fields.contains_key(c)) || !fk.ref_columns.iter().all(|c| parent.fields.contains_key(c)) {
                gaps.push(format!("foreign key {}.{} references a column that is not a key; it is left out", m.table.name, fk.name));
                continue;
            }
            *pairs.entry((ci.min(pi), ci.max(pi))).or_default() += 1;
            links.push((ci, pi, fk));
        }
    }
    for (ci, pi, fk) in links {
        let named = ci == pi || pairs[&(ci.min(pi), ci.max(pi))] > 1;
        let (child_name, parent_name) = (models[ci].name.clone(), models[pi].name.clone());
        let child = &models[ci];
        let nullable = fk.columns.iter().any(|column| child.table.column(column).is_some_and(|c| c.nullable));
        let one_to_one = child.table.primary_key.as_ref().is_some_and(|p| p.columns == fk.columns)
            || child.table.uniques.iter().any(|u| u.columns == fk.columns);
        let key_fields = child.fields_list(&fk.columns);
        let ref_fields = models[pi].fields_list(&fk.ref_columns);
        let base = match fk.columns.as_slice() {
            [column] => column.strip_suffix("_id").filter(|b| !b.is_empty()).map(identifier).unwrap_or_else(|| format!("{}_rel", identifier(column))),
            _ => identifier(&fk.ref_table),
        };
        let rel_field = unique_name(base, &mut models[ci].used);
        let back_base = if named {
            format!("{}_{}_set", identifier(&models[ci].table.name), rel_field)
        } else {
            format!("{}_set", identifier(&models[ci].table.name))
        };
        let back_base = if one_to_one { back_base.trim_end_matches("_set").to_owned() } else { back_base };
        let back_field = unique_name(back_base, &mut models[pi].used);
        let rel_name = format!("{}_{}", child_name, rel_field);

        let mut args = vec![];
        if named {
            args.push(quoted(&rel_name));
        }
        args.push(format!("fields: {key_fields}"));
        args.push(format!("references: {ref_fields}"));
        if let Some(a) = fk.on_delete {
            args.push(format!("onDelete: {}", action(a)));
        }
        if let Some(a) = fk.on_update {
            args.push(format!("onUpdate: {}", action(a)));
        }
        args.extend(deferrable(fk.deferrable).map(str::to_owned));
        let mut default_name = vec![models[ci].table.name.as_str()];
        default_name.extend(fk.columns.iter().map(String::as_str));
        default_name.push("fkey");
        if fk.name != object_name(&default_name) {
            args.push(format!("map: {}", quoted(&fk.name)));
        }
        models[ci].lines.push(format!("{rel_field} {parent_name}{} @relation({})", if nullable { "?" } else { "" }, args.join(", ")));
        let back = if one_to_one { format!("{child_name}?") } else { format!("{child_name}[]") };
        let back_attr = if named { format!(" @relation({})", quoted(&rel_name)) } else { String::new() };
        models[pi].lines.push(format!("{back_field} {back}{back_attr}"));
    }

    // -- triggers --------------------------------------------------------------------------
    let functions: BTreeSet<&str> = db.functions.iter().filter(|f| writable(f)).map(|f| f.name.as_str()).collect();
    for m in &mut models {
        for tr in &m.table.triggers {
            if dialect == Dialect::Postgres && !functions.contains(tr.function.as_str()) {
                gaps.push(format!("trigger {}.{} calls {}, which is not pulled; it is left out", m.table.name, tr.name, tr.function));
                continue;
            }
            let events = |list: &[TriggerEvent]| -> String {
                let names: Vec<&str> = list
                    .iter()
                    .map(|e| match e {
                        TriggerEvent::Insert => "insert",
                        TriggerEvent::Update => "update",
                        TriggerEvent::Delete => "delete",
                        TriggerEvent::Truncate => "truncate",
                    })
                    .collect();
                format!("[{}]", names.join(", "))
            };
            let timing = match tr.timing {
                TriggerTiming::Before => "before",
                TriggerTiming::After => "after",
                TriggerTiming::InsteadOf => "instead_of",
            };
            let name = if is_ident(&tr.name) { tr.name.clone() } else { quoted(&tr.name) };
            let mut args = vec![name, format!("{timing}: {}", events(&tr.events))];
            if !tr.update_of.is_empty() {
                args.push(format!("update_of: {}", m.fields_list(&tr.update_of)));
            }
            if tr.for_each == ForEach::Statement {
                args.push("for_each: statement".into());
            }
            if let Some(w) = &tr.when {
                args.push(format!("when: {}", quoted(w)));
            }
            args.push(format!("function: {}", tr.function));
            if !tr.args.is_empty() {
                args.push(format!("args: [{}]", tr.args.iter().map(|a| quoted(a)).collect::<Vec<_>>().join(", ")));
            }
            m.attrs.push(format!("@@trigger({})", args.join(", ")));
        }
    }

    // -- output ----------------------------------------------------------------------------
    let mut out = String::from("// Pulled from a live database by `orm pull`. Review it before the first migration.\n\n");
    out.push_str("datasource db {\n");
    out.push_str(&format!("  provider = {}\n", if dialect == Dialect::Sqlite { "\"sqlite\"" } else { "\"postgresql\"" }));
    if !db.extensions.is_empty() {
        let list: Vec<String> = db
            .extensions
            .iter()
            .map(|e| {
                let mut opts = vec![];
                let ident = if is_ident(&e.name) { e.name.clone() } else {
                    opts.push(format!("map: {}", quoted(&e.name)));
                    identifier(&e.name)
                };
                if let Some(s) = &e.schema {
                    opts.push(format!("schema: {}", quoted(s)));
                }
                if opts.is_empty() { ident } else { format!("{ident}({})", opts.join(", ")) }
            })
            .collect();
        out.push_str(&format!("  extensions = [{}]\n", list.join(", ")));
    }
    out.push_str("}\n");
    for f in &db.functions {
        if !writable(f) {
            gaps.push(format!("function {}({}) can't be written as a function block; it is left out", f.name, f.args));
            continue;
        }
        let returns = if is_ident(&f.returns) { f.returns.clone() } else { quoted(&f.returns) };
        out.push_str(&format!("\nfunction {} {{\nreturns = {returns}\nargs = {}\nlanguage = {}\n", f.name, quoted(&f.args), f.language));
        if let Some(v) = &f.volatility {
            out.push_str(&format!("volatility = {v}\n"));
        }
        if f.security_definer {
            out.push_str("security_definer = true\n");
        }
        out.push_str(&format!("body = \"\"\"\n{}\n\"\"\"\n}}\n", f.body));
    }
    for e in &enum_blocks {
        out.push('\n');
        out.push_str(e);
        out.push('\n');
    }
    for m in &models {
        if let Some(p) = &m.table.partition_of {
            let Some(&pi) = model_of.get(&p.parent) else { gaps.push(format!("partition {}: missing parent {}", m.name, p.parent)); continue };
            let parent = &models[pi];
            out.push_str(&format!("\npartition {} {{\nof = {}\n{}", m.name, parent.name, partition_props(&p.bound)));
            for a in &m.attrs { out.push_str(&format!("{a}\n")); }
            if let Some(c) = &m.table.comment { out.push_str(&format!("@@comment({})\n", quoted(c))); }
            out.push_str("}\n");
            continue;
        }
        out.push_str(&format!("\nmodel {} {{\n", m.name));
        for l in &m.lines {
            out.push_str(&format!("  {l}\n"));
        }
        let mut attrs = m.attrs.clone();
        if m.name.to_lowercase() != m.table.name {
            attrs.insert(0, format!("@@map({})", quoted(&m.table.name)));
        }
        if let Some(c) = &m.table.comment {
            attrs.push(format!("@@comment({})", quoted(c)));
        }
        if !attrs.is_empty() {
            out.push('\n');
            for a in attrs {
                out.push_str(&format!("  {a}\n"));
            }
        }
        out.push_str("}\n");
    }
    if !gaps.is_empty() {
        out.push_str("\n// Not reproduced by this schema:\n");
        for g in &gaps {
            out.push_str(&format!("// gap: {}\n", g.replace('\n', " ")));
        }
    }
    Pulled { schema: out, gaps }
}

/// Whether a function can be written as a function block.
fn writable(f: &Function) -> bool {
    is_ident(&f.name) && !f.body.contains("\"\"\"")
}

/// Whether `u` is written as `@unique` on its field: the one the schema would make.
fn shorthand_unique(t: &Table, u: &Unique) -> bool {
    t.partition_of.is_none() && u.columns.len() == 1 && u.name == object_name(&[&t.name, &u.columns[0], "key"]) && !u.nulls_not_distinct && u.deferrable.is_none()
}

/// `@@index(...)` for an index, or `None` when it uses a column that is left out.
/// `unique` are the `@unique` columns: the schema drops a plain index on one of them.
fn index_attr(m: &ModelOut<'_>, ix: &Index, unique: &BTreeSet<&str>) -> Option<String> {
    if ix.keys.iter().any(|k| k.column.as_ref().is_some_and(|c| !m.fields.contains_key(c))) || ix.include.iter().any(|c| !m.fields.contains_key(c)) {
        return None;
    }
    let keys: Vec<String> = ix.keys.iter().map(|k| m.key(k, None)).collect();
    let plain = ix.keys.len() == 1
        && !ix.unique
        && ix.method.is_none()
        && ix.include.is_empty()
        && ix.where_.is_none()
        && ix.with.is_empty()
        && ix.keys[0].column.as_ref().is_some_and(|c| ix.name == object_name(&[&m.table.name, c, "idx"]) && !unique.contains(c.as_str()))
        && ix.keys[0] == IndexKey { column: ix.keys[0].column.clone(), expr: None, collation: None, opclass: None, desc: false, nulls: None };
    if plain {
        return Some(format!("@@index([{}])", keys.join(", ")));
    }
    let mut args = vec![format!("[{}]", keys.join(", ")), format!("name: {}", quoted(&ix.name))];
    if ix.unique {
        args.push("unique: true".into());
    }
    if let Some(mt) = &ix.method {
        args.push(format!("type: {}", if is_ident(mt) { mt.clone() } else { quoted(mt) }));
    }
    if !ix.include.is_empty() {
        args.push(format!("include: {}", m.fields_list(&ix.include)));
    }
    if ix.nulls_not_distinct {
        args.push("nulls_not_distinct: true".into());
    }
    if !ix.with.is_empty() {
        let params: Vec<String> = ix
            .with
            .iter()
            .map(|(k, v)| format!("{k}: {}", if is_number(v) { v.clone() } else { quoted(v.trim_matches('\'')) }))
            .collect();
        args.push(format!("with: {{ {} }}", params.join(", ")));
    }
    if let Some(w) = &ix.where_ {
        args.push(format!("where: raw({})", quoted(w)));
    }
    Some(format!("@@index({})", args.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrate::model::{Column, PrimaryKey};

    fn col(name: &str, ty: &str) -> Column {
        Column { name: name.into(), ty: ty.into(), nullable: false, default: None, identity: false, comment: None }
    }

    #[test]
    fn types_and_defaults() {
        let enums = HashMap::new();
        let t = |s| field_type(s, Dialect::Postgres, &enums).map(|(a, b, c)| (a, b, c));
        assert_eq!(t("character varying(200)"), Some(("String".into(), Some("@db.VarChar(200)".into()), false)));
        assert_eq!(t("numeric(10,2)"), Some(("Decimal".into(), Some("@db.Decimal(10, 2)".into()), false)));
        assert_eq!(t("text[]"), Some(("String".into(), Some("@db.Text".into()), true)));
        assert_eq!(t("timestamp(3) with time zone"), Some(("DateTime".into(), Some("@db.Timestamptz(3)".into()), false)));
        assert_eq!(t("interval"), None);
        let k = |ty| FieldKind { ty, array: false, enum_values: None };
        assert_eq!(default_attr("'x''y'::character varying", &k("String")), "@default(\"x'y\")");
        assert_eq!(default_attr("'-1'::integer", &k("Int")), "@default(-1)");
        assert_eq!(default_attr("0", &k("Int")), "@default(0)");
        assert_eq!(default_attr("gen_random_uuid()", &k("String")), "@default(dbgenerated(\"gen_random_uuid()\"))");
        assert_eq!(default_attr("now()", &k("DateTime")), "@default(now())");
    }

    #[test]
    fn writes_models_and_relations() {
        let mut users = Table { name: "users".into(), columns: vec![col("id", "bigint")], ..Default::default() };
        users.columns[0].identity = true;
        users.primary_key = Some(PrimaryKey { name: "users_pkey".into(), columns: vec!["id".into()] });
        let mut posts = Table { name: "blog_post".into(), columns: vec![col("id", "bigint"), col("author_id", "bigint")], ..Default::default() };
        posts.primary_key = Some(PrimaryKey { name: "blog_post_pkey".into(), columns: vec!["id".into()] });
        posts.foreign_keys.push(ForeignKey {
            name: "blog_post_author_id_fk".into(),
            columns: vec!["author_id".into()],
            ref_table: "users".into(),
            ref_columns: vec!["id".into()],
            on_delete: Some(OnDelete::Cascade),
            on_update: None,
            deferrable: Some(Deferrable::Deferred),
        });
        let db = DbSchema { tables: vec![posts, users], version: 1, ..Default::default() };
        let out = pull(&db, &[]).schema;
        assert!(out.contains("model BlogPost {"), "{out}");
        assert!(out.contains("@@map(\"blog_post\")"), "{out}");
        assert!(out.contains("author Users @relation(fields: [author_id], references: [id], onDelete: Cascade, deferrable: deferred, map: \"blog_post_author_id_fk\")"), "{out}");
        assert!(out.contains("blog_post_set BlogPost[]"), "{out}");
        assert!(out.contains("id BigInt @id @default(autoincrement())"), "{out}");
    }

    #[test]
    fn writes_composite_keys() {
        let mut items = Table { name: "item".into(), columns: vec![col("shop", "bigint"), col("id", "bigint")], ..Default::default() };
        items.primary_key = Some(PrimaryKey { name: "item_key".into(), columns: vec!["shop".into(), "id".into()] });
        let mut lines = Table { name: "line".into(), columns: vec![col("id", "bigint"), col("shop", "bigint"), col("item_id", "bigint")], ..Default::default() };
        lines.primary_key = Some(PrimaryKey { name: "line_pkey".into(), columns: vec!["id".into()] });
        lines.foreign_keys.push(ForeignKey {
            name: "line_shop_item_id_fkey".into(),
            columns: vec!["shop".into(), "item_id".into()],
            ref_table: "item".into(),
            ref_columns: vec!["shop".into(), "id".into()],
            on_delete: None,
            on_update: None,
            deferrable: None,
        });
        let db = DbSchema { tables: vec![items, lines], version: 1, ..Default::default() };
        let pulled = pull(&db, &[]);
        let out = &pulled.schema;
        assert!(pulled.gaps.is_empty(), "{:?}", pulled.gaps);
        assert!(out.contains("@@id([shop, id], map: \"item_key\")"), "{out}");
        assert!(out.contains("item Item @relation(fields: [shop, item_id], references: [shop, id])"), "{out}");
        let ir = crate::dsl::compile(out, None).unwrap();
        assert_eq!(ir.models[0].primary_key, ["shop", "id"]);
        assert_eq!(ir.models[0].pk_name.as_deref(), Some("item_key"));
    }
}

fn partition_attr(m: &ModelOut<'_>, key: &str) -> String {
    let (strategy, keys) = key.split_once(" (").unwrap_or((key, ""));
    let keys = keys.strip_suffix(')').unwrap_or(keys);
    let keys = crate::partition::split_sql(keys).into_iter().map(|k| {
        let column = k.strip_prefix('"').and_then(|k| k.strip_suffix('"')).map(|k| k.replace("\"\"", "\"")).unwrap_or_else(|| k.to_owned());
        if m.fields.contains_key(&column) { m.field(&column) } else { format!("sql({})", quoted(k)) }
    }).collect::<Vec<_>>().join(", ");
    format!("@@partition({}: [{keys}])", strategy.to_lowercase())
}

fn partition_props(bound: &str) -> String {
    let values = |s: &str| crate::partition::split_sql(s).into_iter().map(|v| {
        if let Some((s, _)) = sql_string(v) { quoted(&s) }
        else if v == "MINVALUE" || v == "MAXVALUE" || is_number(v) { v.to_owned() }
        else { format!("sql({})", quoted(v)) }
    }).collect::<Vec<_>>().join(", ");
    if bound == "DEFAULT" { return "default = true\n".into(); }
    if let Some(v) = bound.strip_prefix("FOR VALUES IN (").and_then(|v| v.strip_suffix(')')) { return format!("values = [{}]\n", values(v)); }
    if let Some(v) = bound.strip_prefix("FOR VALUES FROM (").and_then(|v| v.strip_suffix(')')) {
        if let Some((from, to)) = v.split_once(") TO (") { return format!("from = [{}]\nto = [{}]\n", values(from), values(to)); }
    }
    if let Some(v) = bound.strip_prefix("FOR VALUES WITH (").and_then(|v| v.strip_suffix(')')) {
        if let Some((m, r)) = v.split_once(", remainder ") { return format!("modulus = {}\nremainder = {r}\n", m.trim_start_matches("modulus ")); }
    }
    format!("// Unsupported bound: {bound}\n")
}
