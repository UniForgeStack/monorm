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
        let what = format!("{}.{}", new.name, nc.name);
        let link = nc.nested.0.as_ref();
        let copy = match link {
            Some(l) if l.storage == NestedStorage::Json => to_json(old, nc, l, &what, d)?,
            _ => match json_source(old, new, nc) {
                Some(source) => {
                    if link.is_some_and(|l| l.list) {
                        return Err(format!("{what}: a storage change copies no list subfield"));
                    }
                    let entry = source.shape.iter().find(|e| e.path == source.path).and_then(|e| e.ty);
                    let ty = link.and_then(|l| l.ty).or(entry).unwrap_or_else(|| derive_type(&nc.ty, d));
                    Some(json_to_leaf(d, new, nc, &source, ty))
                }
                // a json column without a link that the old columns fill
                None if link.is_none() && is_json_type(&nc.ty) && has_leaves(old, &nc.name) => return Err(too_old(&what)),
                None => None,
            },
        };
        out.extend(copy.map(|sql| (nc.name.clone(), sql)));
    }
    Ok(out)
}

/// Whether `old` holds a `columns` leaf of the field.
fn has_leaves(old: &Table, field: &str) -> bool {
    old.columns.iter().any(|c| c.nested.0.as_ref().is_some_and(|l| l.storage == NestedStorage::Columns && l.field == field))
}

/// The copy into the json column `nc`, from the leaf columns of `old`.
fn to_json(old: &Table, nc: &Column, json: &NestedShape, what: &str, d: Dialect) -> Result<Option<String>, String> {
    if json.shape.is_empty() {
        // the leaves can only be copied into a json value whose shape is known
        return if has_leaves(old, &json.field) { Err(too_old(what)) } else { Ok(None) };
    }
    let leaves = leaf_columns(old, json, d);
    if leaves.is_empty() {
        return Ok(None);
    }
    if json.list {
        return Err(format!("{what}: a list of a type has no columns storage to copy from"));
    }
    if let Some((_, (c, _))) = leaves.iter().find(|(_, (c, _))| c.nested.0.as_ref().is_some_and(|l| l.list)) {
        return Err(format!("{what}: {}.{} is an array column; a storage change copies no list subfield", old.name, c.name));
    }
    if let Some(e) = json.shape.iter().find(|e| !e.group && !e.nullable && e.default.is_none() && !leaves.contains_key(&e.path)) {
        return Err(format!("{what}: no column holds the required subfield {}; {}", e.path.join("."), too_old(what)));
    }
    Ok(Some(columns_to_json(d, json, nc, &leaves)))
}

fn too_old(what: &str) -> String {
    format!("the previous snapshot is too old to copy {what}")
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
            // the json value takes the new type, whatever the old column holds
            let ty = e.ty.or(c.nested.0.as_ref().and_then(|l| l.ty)).unwrap_or_else(|| derive_type(&c.ty, d));
            out.insert(e.path.clone(), (c, ty));
        }
    }
    out
}

/// The `json` column of `old` that holds the leaf `nc`.
struct Source<'a> {
    json: &'a Column,
    /// The subfield path of the leaf in the json value.
    path: Vec<String>,
    /// The shape of the json value; empty when the snapshot has none.
    shape: &'a [ShapeEntry],
}

fn json_source<'a>(old: &'a Table, new: &Table, nc: &Column) -> Option<Source<'a>> {
    let json_of = |field: &str| old.columns.iter().find_map(|c| {
        let l = c.nested.0.as_ref().filter(|l| l.storage == NestedStorage::Json && l.field == field)?;
        Some((c, l.shape.as_slice()))
    });
    if let Some(link) = nc.nested.0.as_ref() {
        if let Some((json, shape)) = json_of(&link.field) {
            return Some(Source { json, path: link.path.clone(), shape });
        }
        // a json snapshot without a shape holds no link: the dropped column of the field name
        let bare = old.column(&link.field).filter(|c| c.nested.0.is_none() && new.column(&c.name).is_none() && is_json_type(&c.ty));
        if let Some(json) = bare {
            return Some(Source { json, path: link.path.clone(), shape: &[] });
        }
    }
    // a leaf without a link is a shape entry with the default joined name
    old.columns.iter().find_map(|c| {
        let l = c.nested.0.as_ref().filter(|l| l.storage == NestedStorage::Json)?;
        let e = l.shape.iter().find(|e| !e.group && nc.name == format!("{}_{}", l.field, e.path.join("_")))?;
        Some(Source { json: c, path: e.path.clone(), shape: &l.shape })
    })
}

fn is_json_type(sql: &str) -> bool {
    matches!(sql.to_lowercase().as_str(), "jsonb" | "json" | "text")
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

/// The first leaf below `at` that is set whenever the group at `at` is set, and has an old column.
fn required_leaf(shape: &[ShapeEntry], at: &[String], leaves: &Leaves<'_>) -> Option<Vec<String>> {
    children(shape, at).filter(|e| !e.nullable).find_map(|e| {
        if e.group { required_leaf(shape, &e.path, leaves) } else { leaves.contains_key(&e.path).then(|| e.path.clone()) }
    })
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
        required_leaf(&json.shape, at, leaves).and_then(|p| leaves.get(&p)).map(|(c, _)| format!("{} IS NOT NULL", ident(&c.name)))
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

/// The value of a `columns` leaf, read from the json value. A leaf that the old shape lacks takes
/// its default, when the object around it is set.
fn json_to_leaf(d: Dialect, new: &Table, nc: &Column, source: &Source<'_>, ty: ColType) -> String {
    let json = ident(&source.json.name);
    let in_shape = source.shape.iter().any(|e| !e.group && e.path == source.path);
    if source.shape.is_empty() || in_shape {
        return json_to_column(d, &json, &source.path, ty, &nc.ty);
    }
    let default = nc.default.clone().or_else(|| new.backfill(&nc.name).map(|b| b.value.clone()));
    let Some(default) = default else { return json_to_column(d, &json, &source.path, ty, &nc.ty) };
    let parent = &source.path[..source.path.len().saturating_sub(1)];
    if d == Dialect::Postgres {
        let at = if parent.is_empty() { json } else { format!("({json} #> {})", path_array(parent)) };
        format!("CASE WHEN jsonb_typeof({at}) = 'object' THEN ({default})::{} END", nc.ty)
    } else {
        format!("CASE WHEN json_type({json}, {}) = 'object' THEN {default} END", quote_literal(&json_path(parent)))
    }
}

fn path_array(path: &[String]) -> String {
    format!("ARRAY[{}]", path.iter().map(|k| quote_literal(k)).collect::<Vec<_>>().join(", "))
}

fn json_path(path: &[String]) -> String {
    format!("${}", path.iter().map(|k| format!(".\"{k}\"")).collect::<String>())
}

fn json_to_column(d: Dialect, json: &str, path: &[String], ty: ColType, sql_type: &str) -> String {
    let c = json;
    if d == Dialect::Postgres {
        let at = path_array(path);
        return match ty {
            ColType::Json => format!("NULLIF({c} #> {at}, 'null'::jsonb)"),
            _ => format!("({c} #>> {at})::{sql_type}"),
        };
    }
    let at = quote_literal(&json_path(path));
    let get = format!("json_extract({c}, {at})");
    let kind = format!("json_type({c}, {at})");
    match ty {
        ColType::Int | ColType::BigInt => format!("CAST({get} AS INTEGER)"),
        ColType::Float => format!("CAST({get} AS REAL)"),
        ColType::DateTime => {
            // the writer stores chrono `%.f`: no fraction, or 3, 6 or 9 digits
            let t = format!("replace(rtrim({get}, 'Z'), 'T', ' ')");
            let f = format!("substr(substr({t}, 21) || '000000000', 1, 9)");
            format!(
                "CASE WHEN {get} IS NULL THEN NULL ELSE substr({t}, 1, 19) || CASE WHEN {f} = '000000000' THEN '' WHEN substr({f}, 4) = '000000' THEN '.' || substr({f}, 1, 3) WHEN substr({f}, 7) = '000' THEN '.' || substr({f}, 1, 6) ELSE '.' || {f} END END"
            )
        }
        ColType::Json => format!("CASE WHEN {kind} IS NULL OR {kind} = 'null' THEN NULL ELSE {c} -> {at} END"),
        _ => get,
    }
}
