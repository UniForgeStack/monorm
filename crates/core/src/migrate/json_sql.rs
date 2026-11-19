//! The SQL of the data operations on a `json` column: the fill of a new subfield and the rename of
//! a key (`Op::BackfillJson`, `Op::RenameJsonKey`).
//!
//! Both work on the objects at `holder`. `lists[i]` says whether the value at the level `i` is an
//! array of such holders (level 0 is the column, level `i` the key `holder[i - 1]`); the action
//! then runs on every element. A row whose value has another type is left as it is.

use serde_json::Value;

use super::model::quote_literal;
use super::pg::ident;

pub enum Act<'a> {
    /// Set `key` to the value where it is missing or `null`.
    Fill { key: &'a str, value: &'a Value },
    /// Move the value of `from` to `to` where `from` exists and `to` does not.
    Rename { from: &'a str, to: &'a str },
}

fn any_list(lists: &[bool]) -> bool {
    lists.iter().any(|l| *l)
}

// ---------------------------------------------------------------------------------------
// PostgreSQL

fn pg_array(p: &[String]) -> String {
    format!("ARRAY[{}]", p.iter().map(|k| quote_literal(k)).collect::<Vec<_>>().join(", "))
}

pub fn pg(table: &str, column: &str, holder: &[String], lists: &[bool], act: &Act) -> String {
    let c = ident(column);
    if any_list(lists) {
        let expr = pg_level(&c, 0, holder, lists, act);
        return format!("UPDATE {} SET {c} = {expr} WHERE {c} IS NOT NULL AND {c} IS DISTINCT FROM {expr}", ident(table));
    }
    let h = if holder.is_empty() { c.clone() } else { format!("({c} #> {})", pg_array(holder)) };
    match act {
        Act::Fill { key, value } => {
            let mut path = holder.to_vec();
            path.push((*key).to_owned());
            let shown = if holder.is_empty() { c.clone() } else { format!("{c} #> {}", pg_array(holder)) };
            format!(
                "UPDATE {} SET {c} = jsonb_set({c}, {}, {}::jsonb) WHERE jsonb_typeof({shown}) = 'object' AND COALESCE(jsonb_typeof(({shown}) -> {}), 'null') = 'null'",
                ident(table), pg_array(&path), quote_literal(&value.to_string()), quote_literal(key)
            )
        }
        Act::Rename { from, to } => {
            let moved = format!("({h} - {}) || jsonb_build_object({}, {h} -> {})", quote_literal(from), quote_literal(to), quote_literal(from));
            let set = if holder.is_empty() { moved } else { format!("jsonb_set({c}, {}, {moved})", pg_array(holder)) };
            format!(
                "UPDATE {} SET {c} = {set} WHERE jsonb_typeof({h}) = 'object' AND jsonb_exists({h}, {}) AND NOT jsonb_exists({h}, {})",
                ident(table), quote_literal(from), quote_literal(to)
            )
        }
    }
}

/// `v` is the value at the level `depth`.
fn pg_level(v: &str, depth: usize, holder: &[String], lists: &[bool], act: &Act) -> String {
    if !lists[depth] {
        return pg_object(v, depth, holder, lists, act);
    }
    let (e, o) = (format!("e{depth}"), format!("o{depth}"));
    let inner = pg_object(&e, depth, holder, lists, act);
    format!(
        "CASE WHEN jsonb_typeof({v}) = 'array' THEN (SELECT COALESCE(jsonb_agg({inner} ORDER BY {o}), '[]'::jsonb) FROM jsonb_array_elements({v}) WITH ORDINALITY AS t{depth}({e}, {o})) ELSE {v} END"
    )
}

/// `v` is an object at the level `depth`, or any other value that stays as it is.
fn pg_object(v: &str, depth: usize, holder: &[String], lists: &[bool], act: &Act) -> String {
    if depth == holder.len() {
        return match act {
            Act::Fill { key, value } => format!(
                "CASE WHEN jsonb_typeof({v}) = 'object' AND COALESCE(jsonb_typeof(({v}) -> {k}), 'null') = 'null' THEN jsonb_set({v}, ARRAY[{k}], {}::jsonb) ELSE {v} END",
                quote_literal(&value.to_string()), k = quote_literal(key)
            ),
            Act::Rename { from, to } => format!(
                "CASE WHEN jsonb_typeof({v}) = 'object' AND jsonb_exists({v}, {f}) AND NOT jsonb_exists({v}, {t}) THEN ({v} - {f}) || jsonb_build_object({t}, ({v}) -> {f}) ELSE {v} END",
                f = quote_literal(from), t = quote_literal(to)
            ),
        };
    }
    let k = quote_literal(&holder[depth]);
    let child = pg_level(&format!("(({v}) -> {k})"), depth + 1, holder, lists, act);
    format!("CASE WHEN jsonb_typeof({v}) = 'object' AND jsonb_exists({v}, {k}) THEN jsonb_set({v}, ARRAY[{k}], {child}) ELSE {v} END")
}

// ---------------------------------------------------------------------------------------
// SQLite

fn at(p: &[String]) -> String {
    quote_literal(&format!("${}", p.iter().map(|k| format!(".\"{k}\"")).collect::<String>()))
}

pub fn sqlite(table: &str, column: &str, holder: &[String], lists: &[bool], act: &Act) -> String {
    let c = ident(column);
    if any_list(lists) {
        let expr = sqlite_level(&c, 0, holder, lists, act);
        // `json()` minifies the stored text, as the rebuilt array is
        return format!("UPDATE {} SET {c} = {expr} WHERE {c} IS NOT NULL AND {expr} IS NOT json({c})", ident(table));
    }
    let holder_type = if holder.is_empty() { format!("json_type({c})") } else { format!("json_type({c}, {})", at(holder)) };
    let with = |k: &str| {
        let mut p = holder.to_vec();
        p.push(k.to_owned());
        at(&p)
    };
    match act {
        Act::Fill { key, value } => format!(
            "UPDATE {} SET {c} = json_set({c}, {}, json({})) WHERE {holder_type} = 'object' AND COALESCE(json_type({c}, {}), 'null') = 'null'",
            ident(table), with(key), quote_literal(&value.to_string()), with(key)
        ),
        Act::Rename { from, to } => format!(
            "UPDATE {} SET {c} = json_set(json_remove({c}, {f}), {t}, json({c} -> {f})) WHERE {holder_type} = 'object' AND json_type({c}, {f}) IS NOT NULL AND json_type({c}, {t}) IS NULL",
            ident(table), f = with(from), t = with(to)
        ),
    }
}

fn sqlite_level(v: &str, depth: usize, holder: &[String], lists: &[bool], act: &Act) -> String {
    if !lists[depth] {
        return sqlite_object(v, depth, holder, lists, act);
    }
    let (e, t, k) = (format!("e{depth}"), format!("t{depth}"), format!("k{depth}"));
    let inner = sqlite_object(&e, depth, holder, lists, act);
    // An element that is not an object stays as it is: `->` gives its text as stored, where `json_each`
    // would turn an integer above 64 bits into a float.
    let other = format!("{v} -> ('$[' || {k} || ']')");
    format!(
        "CASE WHEN json_type({v}) = 'array' THEN (SELECT json_group_array(json(CASE {t} WHEN 'object' THEN {inner} ELSE {other} END)) FROM (SELECT value AS {e}, type AS {t}, key AS {k} FROM json_each({v}) ORDER BY key)) ELSE {v} END"
    )
}

fn sqlite_object(v: &str, depth: usize, holder: &[String], lists: &[bool], act: &Act) -> String {
    if depth == holder.len() {
        return match act {
            Act::Fill { key, value } => format!(
                "CASE WHEN json_type({v}) = 'object' AND COALESCE(json_type({v}, {k}), 'null') = 'null' THEN json_set({v}, {k}, json({})) ELSE {v} END",
                quote_literal(&value.to_string()), k = at(&[(*key).to_owned()])
            ),
            Act::Rename { from, to } => {
                let (f, t) = (at(&[(*from).to_owned()]), at(&[(*to).to_owned()]));
                format!("CASE WHEN json_type({v}) = 'object' AND json_type({v}, {f}) IS NOT NULL AND json_type({v}, {t}) IS NULL THEN json_set(json_remove({v}, {f}), {t}, json({v} -> {f})) ELSE {v} END")
            }
        };
    }
    let key = at(std::slice::from_ref(&holder[depth]));
    let child = sqlite_level(&format!("({v} -> {key})"), depth + 1, holder, lists, act);
    format!("CASE WHEN json_type({v}) = 'object' AND json_type({v}, {key}) IS NOT NULL THEN json_set({v}, {key}, json({child})) ELSE {v} END")
}
