//! Source-span conversion; no database access, migration creation or historical edits.
use std::{collections::BTreeMap, path::{Path, PathBuf}};
use super::{expr::Context, sql_expr, syntax::{self, Attr, Pos, Value}, Installed};
use crate::{ir::SchemaIr, migrate::{self, DbSchema}, schema::Schema};

type Replacement = (usize, usize, String);

struct Edit { pos: Pos, sql: String, context: Context }
fn predicate(attr: &Attr, edits: &mut Vec<Edit>, reports: &mut Vec<String>) {
    if attr.name == "check" {
        if let Some((pos, value)) = attr.args.positional.first() { add(*pos, value, Context::Check, edits); }
    }
    for (i, (name, _, value)) in attr.args.named.iter().enumerate() {
        if name == "where" && matches!(attr.name.as_str(), "index" | "unique" | "exclude") {
            add(attr.args.named_values[i], value, Context::IndexFilter, edits);
        }
    }
    if matches!(attr.name.as_str(), "index" | "unique" | "exclude") {
        if let Some((_, Value::List(keys))) = attr.args.positional.first() {
            for (pos, value) in keys {
                if let Value::Path(path, Some(args)) = value {
                    if path.as_slice() == ["sql"] && (!args.named.is_empty() || attr.name == "exclude") {
                        reports.push(format!("{pos}: expression key options require sql()"));
                    }
                    if path.as_slice() == ["sql"] && args.named.is_empty() && args.positional.len() == 1 && attr.name != "exclude" {
                        if let Value::Str(sql) = &args.positional[0].1 { edits.push(Edit { pos: *pos, sql: sql.clone(), context: Context::IndexKey }); }
                    }
                }
            }
        }
    }
}
fn add(pos: Pos, value: &Value, context: Context, edits: &mut Vec<Edit>) {
    let sql = match value {
        Value::Str(s) => Some(s),
        Value::Path(path, Some(args)) if path.as_slice() == ["raw"] && args.named.is_empty() && args.positional.len() == 1 => {
            if let Value::Str(s) = &args.positional[0].1 { Some(s) } else { None }
        }
        _ => None,
    };
    if let Some(sql) = sql { edits.push(Edit { pos, sql: sql.clone(), context }); }
}

fn offsets(source: &str, pos: Pos) -> Option<(usize, usize)> {
    let mut lines = source.split_inclusive('\n');
    let start = lines.by_ref().take(pos.line.checked_sub(1)? as usize).map(str::len).sum::<usize>();
    let line = lines.next()?;
    let byte_at = |col| line.char_indices().map(|(i,_)| i).chain(std::iter::once(line.len())).nth(col);
    let a = byte_at(pos.col.checked_sub(1)? as usize)?;
    let b = byte_at((pos.col.checked_sub(1)? + pos.len) as usize)?;
    (pos.len > 0).then_some((start+a, start+b))
}

/// Converts representable source spans. Unsupported spans remain byte-for-byte
/// intact and yield a diagnostic with their source location.
fn replacements(source: &str, ir: &SchemaIr) -> Result<(Vec<Replacement>, Vec<String>), String> {
    let items = syntax::parse(source).map_err(|e| format!("{}: {}", e.pos, e.msg))?;
    let mut replacements = vec![];
    let mut reports = vec![];
    for item in items {
        let syntax::Item::Model(model) = item else { continue; };
        let mut edits = vec![];
        for attr in model.members.iter().flat_map(|f| &f.attrs).chain(&model.blocks) { predicate(attr, &mut edits, &mut reports); }
        for edit in edits {
            match sql_expr::translate(ir, &model.name, edit.context, &edit.sql) {
                Ok(native) => match offsets(source, edit.pos) {
                    Some((a,b)) => replacements.push((a,b,native)),
                    None => reports.push(format!("{}: cannot determine a safe source extent", edit.pos)),
                },
                Err(reason) => reports.push(format!("{}: {reason}", edit.pos)),
            }
        }
    }
    replacements.sort_by_key(|(a,_,_)| *a);
    Ok((replacements, reports))
}

pub fn convert_source(source: &str, ir: &SchemaIr) -> Result<(String, Vec<String>), String> {
    let (replacements, reports) = replacements(source, ir)?;
    let mut out = source.to_owned();
    for (a,b,native) in replacements.into_iter().rev() { out.replace_range(a..b, &native); }
    Ok((out, reports))
}

pub struct Conversion {
    /// Fully validated replacements. The caller owns writing these files.
    pub files: BTreeMap<PathBuf, String>,
    pub reports: Vec<String>,
}

/// Prepare and validate all source changes before exposing any file replacement.
pub fn prepare(path: &Path, installed: &Installed, previous: &DbSchema) -> Result<Conversion, String> {
    let project = super::compile_project_file_with(path, installed)?;
    let original = migrate::plan(&Schema::from_ir_unbound(super::compile_project_file_with(path, installed)?.ir)?, previous)?;
    let root = path.canonicalize().map_err(|e| e.to_string())?;
    let root_source = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut files = BTreeMap::new();
    let mut reports = vec![];
    if previous.version == 0 { reports.push("no migration history: conversion uses canonical SQL and generated names".into()); }
    for unit in &project.units {
        // Installed app files belong to their package, not to this project.
        if unit.app.is_some() { continue; }
        let source = std::fs::read_to_string(&unit.path).map_err(|e| e.to_string())?;
        let (edits, messages) = replacements(&source, &project.ir)?;
        reports.extend(messages.into_iter().map(|m| format!("{}:{m}", unit.path.display())));
        let canonical = unit.path.canonicalize().map_err(|e| e.to_string())?;
        let mut accepted = source.clone();
        // Descending spans keep all earlier offsets valid as edits are accepted.
        for (a,b,native) in edits.into_iter().rev() {
            let mut candidate = accepted.clone();
            candidate.replace_range(a..b, &native);
            files.insert(canonical.clone(), candidate.clone());
            let proposed_source = files.get(&root).unwrap_or(&root_source);
            let target = super::compile_project_overlay(proposed_source, Some(path), None, installed, &files)
                .and_then(|p| Schema::from_ir_unbound(p.ir)).and_then(|s| migrate::plan(&s, previous));
            let reason = match target {
                Ok(target) if previous.version == 0 || target.snapshot == original.snapshot => None,
                Ok(_) => Some("cannot uniquely retain migration snapshot SQL and name".to_owned()),
                Err(error) => Some(format!("proposed conversion failed validation: {error}")),
            };
            if let Some(reason) = reason {
                let line = source[..a].chars().filter(|c| *c == '\n').count() + 1;
                let col = source[..a].rsplit('\n').next().unwrap_or("").chars().count() + 1;
                reports.push(format!("{}:{line}:{col}: left unchanged: {reason}", unit.path.display()));
                files.insert(canonical.clone(), accepted.clone());
            } else { accepted = candidate; }
        }
        if accepted == source { files.remove(&canonical); }
    }
    Ok(Conversion { files, reports })
}
