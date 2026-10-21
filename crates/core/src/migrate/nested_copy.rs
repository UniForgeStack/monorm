//! The data copy of a nested type whose storage changes between `columns` and `json`.
//!
//! Both directions read the two snapshots only, so an up and a down migration use the same code.
//! A `columns` leaf carries the link `(field, path, storage)` in its snapshot column; a snapshot
//! written before the link has none, and the leaf is then the column with the default joined name
//! `<field>_<path>`.

use std::collections::HashMap;

use super::model::{Column, NestedShape, ShapeEntry, Table, quote_literal};
use super::pg::ident;
use crate::dialect::Dialect;
use crate::ir::{ColType, NestedStorage};

/// A leaf value, given as SQL text, as the JSON value the writers store: BigInt and Decimal as text,
/// DateTime as RFC 3339 UTC text.
pub fn leaf_json(d: Dialect, ty: ColType, sql: &str) -> String {
    match (d, ty) {
        (Dialect::Postgres, ColType::DateTime) => format!("to_char({sql} AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')"),
        (Dialect::Postgres, ColType::Date) => format!("to_char({sql}, 'YYYY-MM-DD')"),
        (Dialect::Postgres, ColType::BigInt | ColType::Decimal) => format!("{sql}::text"),
        (Dialect::Sqlite, ColType::BigInt) => format!("CAST({sql} AS TEXT)"),
        (Dialect::Sqlite, ColType::Bool) => format!("json(CASE WHEN {sql} IS NULL THEN NULL WHEN {sql} THEN 'true' ELSE 'false' END)"),
        (Dialect::Sqlite, ColType::Json) => format!("json({sql})"),
        _ => sql.to_owned(),
    }
}

/// `UPDATE` / `INSERT ... SELECT` values of the columns that a storage change creates, as
/// `(new column, SQL over the columns of `old`)`.
pub fn copies(old: &Table, new: &Table, d: Dialect) -> Result<Vec<(String, String)>, String> {
    let mut out = vec![];
    for nc in new.columns.iter().filter(|c| old.column(&c.name).is_none()) {
        let Some(link) = &nc.nested.0 else { continue };
        let what = format!("{}.{}", new.name, nc.name);
        match link.storage {
            NestedStorage::Json => {
                let leaves = leaf_columns(old, link, d);
                if leaves.is_empty() {
                    continue;
                }
                if link.list {
                    return Err(format!("{what}: a list of a type has no columns storage to copy from"));
                }
                if let Some((_, (c, _))) = leaves.iter().find(|(_, (c, _))| c.nested.0.as_ref().is_some_and(|l| l.list)) {
                    return Err(format!("{what}: {}.{} is an array column; a storage change copies no list subfield", old.name, c.name));
                }
                out.push((nc.name.clone(), columns_to_json(d, link, nc, &leaves)));
            }
            NestedStorage::Columns => {
                let Some((json, path)) = json_source(old, nc) else { continue };
                if link.list {
                    return Err(format!("{what}: a storage change copies no list subfield"));
                }
                let ty = link.ty.unwrap_or_else(|| derive_type(&nc.ty, d));
                out.push((nc.name.clone(), json_to_column(d, &json.name, &path, ty, &nc.ty)));
            }
        }
    }
    Ok(out)
}

/// The type a snapshot column of a leaf without a link holds.
fn derive_type(sql: &str, d: Dialect) -> ColType {
    let s = sql.to_lowercase();
    if d == Dialect::Sqlite {
        return match s.as_str() {
            "integer" => ColType::Int,
            "real" => ColType::Float,
            _ => ColType::String,
        };
    }
    match s.as_str() {
        "bigint" => ColType::BigInt,
        "integer" | "smallint" => ColType::Int,
        "double precision" | "real" => ColType::Float,
        "boolean" => ColType::Bool,
        "date" => ColType::Date,
        "uuid" => ColType::Uuid,
        "jsonb" | "json" => ColType::Json,
        "text" => ColType::Text,
        t if t.starts_with("timestamp") => ColType::DateTime,
        t if t.starts_with("numeric") => ColType::Decimal,
        _ => ColType::String,
    }
}

/// Renders the object below a path.
type Nested<'a> = dyn Fn(&[String]) -> String + 'a;
/// Renders one shape entry, given the renderer of its children.
type Value<'a> = dyn Fn(&ShapeEntry, &Nested) -> String + 'a;

type Leaves<'a> = HashMap<Vec<String>, (&'a Column, ColType)>;

/// The `columns` leaves of the field of `json` in `old`, by subfield path.
fn leaf_columns<'a>(old: &'a Table, json: &NestedShape, d: Dialect) -> Leaves<'a> {
    let mut out = Leaves::new();
    for e in json.shape.iter().filter(|e| !e.group) {
        let linked = old.columns.iter().find(|c| {
            c.nested.0.as_ref().is_some_and(|l| l.storage == NestedStorage::Columns && l.field == json.field && l.path == e.path)
        });
        let found = linked.or_else(|| old.column(&format!("{}_{}", json.field, e.path.join("_"))).filter(|c| c.nested.0.is_none()));
        if let Some(c) = found {
            let ty = c.nested.0.as_ref().and_then(|l| l.ty).or(e.ty).unwrap_or_else(|| derive_type(&c.ty, d));
            out.insert(e.path.clone(), (c, ty));
        }
    }
    out
}

/// The `json` column of `old` that holds the leaf `nc`, and the subfield path of `nc` in it.
fn json_source<'a>(old: &'a Table, nc: &Column) -> Option<(&'a Column, Vec<String>)> {
    let link = nc.nested.0.as_ref()?;
    let json = |c: &&Column| c.nested.0.as_ref().is_some_and(|l| l.storage == NestedStorage::Json && l.field == link.field);
    if let Some(c) = old.columns.iter().find(json) {
        return Some((c, link.path.clone()));
    }
    // a snapshot whose leaves carry no link: the leaf is a shape entry with the default joined name
    old.columns.iter().find_map(|c| {
        let l = c.nested.0.as_ref().filter(|l| l.storage == NestedStorage::Json)?;
        let e = l.shape.iter().find(|e| !e.group && nc.name == format!("{}_{}", l.field, e.path.join("_")))?;
        Some((c, e.path.clone()))
    })
}

fn object_fn(d: Dialect) -> &'static str {
    if d == Dialect::Postgres { "jsonb_build_object" } else { "json_object" }
}

fn json_null(d: Dialect) -> &'static str {
    if d == Dialect::Postgres { "'null'::jsonb" } else { "NULL" }
}

fn children<'a>(shape: &'a [ShapeEntry], at: &'a [String]) -> impl Iterator<Item = &'a ShapeEntry> {
    shape.iter().filter(move |e| e.path.len() == at.len() + 1 && e.path.starts_with(at))
}

/// The first leaf below `at` that is set whenever the group at `at` is set.
fn required_leaf(shape: &[ShapeEntry], at: &[String]) -> Option<Vec<String>> {
    children(shape, at).filter(|e| !e.nullable).find_map(|e| if e.group { required_leaf(shape, &e.path) } else { Some(e.path.clone()) })
}

/// A SQLite DateTime column holds `YYYY-MM-DD HH:MM:SS[.f]`; the JSON value is RFC 3339 UTC with microseconds.
fn stored(d: Dialect, ty: ColType, col: &str) -> String {
    if d == Dialect::Sqlite && ty == ColType::DateTime {
        return format!("replace(substr({col}, 1, 19), ' ', 'T') || '.' || substr(substr({col}, 21) || '000000', 1, 6) || 'Z'");
    }
    col.to_owned()
}

fn columns_to_json(d: Dialect, json: &NestedShape, nc: &Column, leaves: &Leaves<'_>) -> String {
    let present = |at: &[String]| {
        required_leaf(&json.shape, at).and_then(|p| leaves.get(&p)).map(|(c, _)| format!("{} IS NOT NULL", ident(&c.name)))
    };
    let value = |e: &ShapeEntry, at: &Nested<'_>| -> String {
        if e.group {
            let obj = at(&e.path);
            let obj = match (e.nullable, present(&e.path)) {
                (true, Some(p)) => format!("CASE WHEN {p} THEN {obj} ELSE {} END", json_null(d)),
                _ => obj,
            };
            return if d == Dialect::Sqlite { format!("json({obj})") } else { obj };
        }
        match leaves.get(&e.path) {
            Some((c, ty)) => leaf_json(d, *ty, &stored(d, *ty, &ident(&c.name))),
            None => match &e.default {
                Some(v) if d == Dialect::Postgres => format!("{}::jsonb", quote_literal(&v.to_string())),
                Some(v) => format!("json({})", quote_literal(&v.to_string())),
                None => json_null(d).to_owned(),
            },
        }
    };
    fn object(d: Dialect, shape: &[ShapeEntry], at: &[String], value: &Value<'_>) -> String {
        let parts = children(shape, at)
            .map(|e| {
                let nested = |p: &[String]| object(d, shape, p, value);
                format!("{}, {}", quote_literal(e.path.last().expect("a path is not empty")), value(e, &nested))
            })
            .collect::<Vec<_>>();
        format!("{}({})", object_fn(d), parts.join(", "))
    }
    let root = object(d, &json.shape, &[], &value);
    match (nc.nullable, present(&[])) {
        (true, Some(p)) => format!("CASE WHEN {p} THEN {root} END"),
        _ => root,
    }
}

fn json_to_column(d: Dialect, json: &str, path: &[String], ty: ColType, sql_type: &str) -> String {
    let c = ident(json);
    if d == Dialect::Postgres {
        let at = format!("ARRAY[{}]", path.iter().map(|k| quote_literal(k)).collect::<Vec<_>>().join(", "));
        return match ty {
            ColType::Json => format!("NULLIF({c} #> {at}, 'null'::jsonb)"),
            _ => format!("({c} #>> {at})::{sql_type}"),
        };
    }
    let at = quote_literal(&format!("${}", path.iter().map(|k| format!(".\"{k}\"")).collect::<String>()));
    let get = format!("json_extract({c}, {at})");
    let kind = format!("json_type({c}, {at})");
    match ty {
        ColType::Int | ColType::BigInt => format!("CAST({get} AS INTEGER)"),
        ColType::Float => format!("CAST({get} AS REAL)"),
        ColType::DateTime => format!("replace(rtrim({get}, 'Z'), 'T', ' ')"),
        ColType::Json => format!("CASE WHEN {kind} IS NULL OR {kind} = 'null' THEN NULL WHEN {kind} IN ('object', 'array') THEN {get} ELSE json_quote({get}) END"),
        _ => get,
    }
}
