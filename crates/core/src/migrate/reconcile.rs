//! Adopt previous SQL and generated names only for checked native source slots.
//! The differ remains strict, and the reconciled target becomes the next snapshot.
use crate::{dsl::{expr::Context, sql_expr}, ir::{ConstraintIr, SchemaIr}, schema::Schema};
use super::{model::{self, Check, Index, IndexKey, Exclusion}, DbSchema};

struct Expressions<'a> { ir: &'a SchemaIr, model: &'a str, schema: &'a Schema }
impl Expressions<'_> {
    fn native(&self, slot: &str) -> bool {
        self.schema.native_expression_slots.iter().any(|(model, path)| model == self.model && path == slot)
    }
    fn same(&self, context: Context, slot: &str, old: &str, new: &str) -> bool {
        old == new || self.native(slot) && sql_expr::equivalent(self.ir, self.model, context, old, new)
    }
    fn predicate(&self, slot: &str, old: &Option<String>, new: &Option<String>) -> bool {
        match (old, new) {
            (Some(a), Some(b)) => self.same(Context::IndexFilter, slot, a, b),
            (None, None) => true, _ => false,
        }
    }
    fn key(&self, slot: &str, old: &IndexKey, new: &IndexKey) -> bool {
        let mut next = new.clone();
        match (&old.expr, &new.expr) {
            (Some(a), Some(b)) if self.same(Context::IndexKey, slot, a, b) => next.expr = old.expr.clone(),
            (Some(a), None) if new.column.as_ref().is_some_and(|column| sql_expr::equivalent(self.ir, self.model, Context::IndexKey, a, &super::pg::ident(column))) => {
                // Bare fields need no SQL provenance because the schema checker resolves them.
                next.expr = old.expr.clone(); next.column = old.column.clone();
            }
            (None, None) => {}, _ => return false,
        }
        &next == old
    }
    fn index(&self, slot: &str, old: &Index, new: &Index) -> bool {
        if old.keys.len() != new.keys.len() || !old.keys.iter().zip(&new.keys).enumerate().all(|(k, (a,b))| self.key(&format!("{slot}:key:{k}"), a, b))
            || !self.predicate(&format!("{slot}:where"), &old.where_, &new.where_) { return false; }
        let mut next = new.clone();
        next.name = old.name.clone(); next.keys = old.keys.clone(); next.where_ = old.where_.clone();
        &next == old
    }
    fn exclusion(&self, i: usize, old: &Exclusion, new: &Exclusion) -> bool {
        if old.elements.len() != new.elements.len() || !old.elements.iter().zip(&new.elements).enumerate().all(|(k, (a,b))| a.operator == b.operator && self.key(&format!("exclude:{i}:key:{k}"), &a.key, &b.key))
            || !self.predicate(&format!("exclude:{i}:where"), &old.where_, &new.where_) { return false; }
        let mut next = new.clone();
        next.name = old.name.clone(); next.elements = old.elements.clone(); next.where_ = old.where_.clone();
        &next == old
    }
}

// A match must be unique in both directions; never adopt an old object twice.
fn adopt<T: Clone>(new: &mut [T], old: &[T], matches: impl Fn(usize, &T, &T) -> bool) {
    let candidates: Vec<Vec<usize>> = new.iter().enumerate().map(|(i,n)| old.iter().enumerate().filter_map(|(j,o)| matches(i,n,o).then_some(j)).collect()).collect();
    for (i, js) in candidates.iter().enumerate() {
        if js.len() == 1 && candidates.iter().filter(|xs| xs.contains(&js[0])).count() == 1 { new[i] = old[js[0]].clone(); }
    }
}

pub(super) fn reconcile(schema: &Schema, previous: &DbSchema, current: &mut DbSchema) {
    if schema.native_expression_slots.is_empty() && !previous.tables.iter().any(|t| {
        t.indexes.iter().any(|ix| ix.keys.iter().any(|k| k.expr.is_some()))
            || t.exclusions.iter().any(|x| x.elements.iter().any(|e| e.key.expr.is_some()))
    }) { return; }
    // Reuse the real fields, mappings and enum types; no second type system.
    let ir: SchemaIr = serde_json::from_value(serde_json::json!({
        "dialect": schema.dialect, "models": schema.models.iter().map(|m| &m.ir).collect::<Vec<_>>(),
        "enums": schema.enums, "types": schema.types,
    })).expect("schema IR round trip");
    for table in &mut current.tables {
        let Some(old) = previous.tables.iter().find(|t| t.name == table.name) else { continue; };
        let Some(m) = schema.models.iter().find(|m| m.table() == table.name) else { continue; };
        let expr = Expressions { ir: &ir, model: &m.ir.name, schema };
        let mut checks: Vec<(String, bool, String)> = m.fields().iter().filter(|f| f.check.is_some()).map(|f| (model::object_name(&[m.table(), &f.column, "check"]), false, format!("field:{}", f.name))).collect();
        for (i,c) in m.ir.constraints.iter().enumerate() {
            if let ConstraintIr::Check { name, expr: sql } = c {
                checks.push((name.clone().unwrap_or_else(|| model::object_name(&[m.table(), &model::expression_tag(sql), "check"])), name.is_none(), format!("check:{i}")));
            }
        }
        adopt(&mut table.checks, &old.checks, |_, n: &Check, o| {
            let Some((_, generated, slot)) = checks.iter().find(|(name,_,_)| name == &n.name) else { return false; };
            // A generated-name transition may not steal an object whose name is
            // still requested by another target definition.
            let name_ok = n.name == o.name || *generated && o.name == model::object_name(&[m.table(), &model::expression_tag(&o.expr), "check"]) && !checks.iter().any(|(name,_,_)| name == &o.name);
            name_ok && expr.same(Context::Check, slot, &o.expr, &n.expr)
        });
        let indexes = table.indexes.clone();
        adopt(&mut table.indexes, &old.indexes, |_, n, o| {
            let (slot, generated) = match m.ir.indexes.iter().enumerate().find(|(_, ix)| model::index_name(m, ix).ok().as_ref() == Some(&n.name)) {
                Some((i, source)) => (format!("index:{i}"), source.name.is_none()),
                // Lowering folds a plain @@index([field]) into @index.
                None if m.fields().iter().any(|f| f.index && n.name == model::object_name(&[m.table(), &f.column, "idx"])) => ("field-index".into(), true),
                None => return n == o,
            };
            (n.name == o.name || generated && o.name == generated_keys_name(m.table(), &o.keys, "idx") && !indexes.iter().any(|ix| ix.name == o.name)) && expr.index(&slot, o, n)
        });
        let sources: Vec<_> = m.ir.constraints.iter().enumerate().filter_map(|(i,c)| if let ConstraintIr::Exclude { name, .. } = c { Some((i, name)) } else { None }).collect();
        let exclusions = table.exclusions.clone();
        adopt(&mut table.exclusions, &old.exclusions, |j, n, o| {
            let Some((i, name)) = sources.get(j) else { return false; };
            (n.name == o.name || name.is_none() && o.name == generated_keys_name(m.table(), &o.elements.iter().map(|e| e.key.clone()).collect::<Vec<_>>(), "excl") && !exclusions.iter().any(|x| x.name == o.name)) && expr.exclusion(*i, o, n)
        });
    }
}

fn generated_keys_name(table: &str, keys: &[IndexKey], suffix: &str) -> String {
    let tag = keys.iter().map(|k| k.column.clone().unwrap_or_else(|| model::expression_tag(k.expr.as_deref().unwrap_or("")))).collect::<Vec<_>>().join("_");
    model::object_name(&[table, &tag, suffix])
}
