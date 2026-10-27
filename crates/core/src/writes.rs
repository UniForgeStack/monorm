//! Prepared write records: the columns the ORM assigns on writes, the delete rewrite and the
//! instance guards that extensions pushed into `Requirements`. Core checks neutral rules only
//! (the field exists and has a fitting type); the role rules stay in the extensions.
use std::collections::HashMap;

pub use orm_contracts::writes::{AssignValue, UpsertDefault, WriteKind};

use crate::behavior::Requirements;
use crate::ir::{ColType, Expr, FieldIr};
use crate::schema::{Model, Result};

/// One column assigned on the listed statement kinds.
#[derive(Clone, Debug)]
pub struct PreparedAssignment {
    pub source: String,
    pub field: usize,
    pub on: Vec<WriteKind>,
    pub value: AssignValue,
    pub upsert: UpsertDefault,
}

impl PreparedAssignment {
    pub fn applies_to(&self, kind: WriteKind) -> bool { self.on.contains(&kind) }
}

/// A delete that becomes an update of `field` to the current time for the rows `guard` selects.
#[derive(Clone, Debug)]
pub struct PreparedDeleteRewrite {
    pub source: String,
    pub field: usize,
    pub guard: Expr,
}

fn position(models: &[Model], index: &HashMap<String, usize>, model: &str, field: &str, what: &str, ok: impl Fn(&FieldIr) -> bool) -> Result<(usize, usize)> {
    let m = *index.get(model).ok_or_else(|| format!("{what}: unknown model {model}; rebuild schema"))?;
    let p = models[m].field_pos(field)?;
    let f = &models[m].fields()[p];
    if f.array || !ok(f) { return Err(format!("{model}.{field}: wrong type for {what}; rebuild schema")); }
    Ok((m, p))
}

pub fn prepare(models: &mut [Model], index: &HashMap<String, usize>, r: &Requirements) -> Result<()> {
    for a in &r.write_assignments {
        let (m, p) = match &a.value {
            AssignValue::Now => position(models, index, &a.model, &a.field, "a time assignment", |f| f.ty == ColType::DateTime)?,
            AssignValue::Increment => position(models, index, &a.model, &a.field, "an increment assignment", |f| matches!(f.ty, ColType::Int | ColType::BigInt))?,
            AssignValue::Hook { export } => {
                let at = position(models, index, &a.model, &a.field, "a hook assignment", |_| true)?;
                let bound = r.hooks.iter().any(|h| h.point == 4 && h.source == a.source && h.model == a.model && h.field.as_deref() == Some(a.field.as_str()) && h.export == *export);
                if !bound { return Err(format!("{}.{}: the write assignment from {} runs hook {export}, but no point-4 binding names it; bind it to the field", a.model, a.field, a.source)); }
                at
            }
        };
        if a.on.is_empty() { return Err(format!("{}.{}: a write assignment from {} names no statement kind", a.model, a.field, a.source)); }
        if models[m].assignments.iter().any(|x| x.field == p) { return Err(format!("{}.{}: assigned by two write assignments", a.model, a.field)); }
        models[m].assignments.push(PreparedAssignment { source: a.source.clone(), field: p, on: a.on.clone(), value: a.value.clone(), upsert: a.upsert });
    }
    for d in &r.delete_rewrites {
        let (m, p) = position(models, index, &d.model, &d.field, "a delete rewrite", |f| f.ty == ColType::DateTime)?;
        let guard: Expr = serde_json::from_value(d.guard.clone()).map_err(|e| format!("{} delete rewrite guard: {e}", d.model))?;
        crate::selection::validate_filter(&models[m], &guard)?;
        if let Some(first) = &models[m].delete_rewrite {
            return Err(format!("{}: delete rewrites from {} and {}; one per model", d.model, first.source, d.source));
        }
        models[m].delete_rewrite = Some(PreparedDeleteRewrite { source: d.source.clone(), field: p, guard });
    }
    for g in &r.instance_guards {
        let (m, p) = position(models, index, &g.model, &g.field, "an instance guard", |_| true)?;
        if !models[m].guards.contains(&p) { models[m].guards.push(p); }
    }
    Ok(())
}

/// A model in an ownership chain accepts no write record: a multi-table write cannot run it on every owner.
pub fn reject_multi_table(models: &[Model], owners: &[Model], owner_links: &[crate::behavior::PreparedOwnerLink], r: &Requirements) -> Result<()> {
    let mut records = r.write_assignments.iter().map(|x| (&x.model, &x.field, &x.source))
        .chain(r.delete_rewrites.iter().map(|x| (&x.model, &x.field, &x.source)))
        .chain(r.instance_guards.iter().map(|x| (&x.model, &x.field, &x.source)))
        .peekable();
    if records.peek().is_none() { return Ok(()); }
    let records: Vec<_> = records.collect();
    for m in models.iter().filter(|m| owner_links.iter().any(|l| l.child == m.owner)) {
        let mut current = m.owner;
        loop {
            let owner = &owners[current.0].ir.name;
            if let Some((_, field, source)) = records.iter().find(|(model, _, _)| *model == owner) {
                return Err(format!("model {}: a write record from {source} on {owner}.{field} is not supported in a multi-table model's ownership chain", m.ir.name));
            }
            let Some(link) = owner_links.iter().find(|l| l.child == current) else { break };
            current = link.parent;
        }
    }
    Ok(())
}
