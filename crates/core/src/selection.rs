//! Neutral read defaults are validated and merged once, before publishing a schema.
use std::collections::{BTreeMap, HashSet};
use crate::{behavior::ReadDefault, ir::{Expr, Order}, schema::Model};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefaultIdentity {
    pub source: String,
    pub name: String,
}

pub struct PreparedPart {
    pub identity: DefaultIdentity,
    pub filter: Option<Expr>,
    /// The filter as the extension gave it, for `current_filter()`.
    pub filter_json: Option<serde_json::Value>,
    pub annotate: BTreeMap<String, Expr>,
    /// Scope names in this entry's filter, collected once during preparation.
    pub scopes: Vec<String>,
}

#[derive(Default)]
pub struct PreparedDefaults {
    pub parts: Vec<PreparedPart>,
    /// All filters ANDed together for consumers that apply every default.
    pub filter: Option<Expr>,
    pub order: Vec<Order>,
    pub order_owner: Option<DefaultIdentity>,
    pub fields_owner: Option<DefaultIdentity>,
    /// Owners for each path in `Model.read_flags.related`, in the same order.
    pub load_owners: Vec<Vec<DefaultIdentity>>,
}

impl PreparedDefaults {
    /// The names `without_defaults()` accepts, once each, in declaration order.
    pub fn names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = vec![];
        for p in &self.parts { if !names.contains(&p.identity.name.as_str()) { names.push(&p.identity.name); } }
        names
    }
}

fn clash(model: &str, what: &str, first: &DefaultIdentity, second: &DefaultIdentity) -> String {
    format!("{model}: {what} is set by both {} default {:?} and {} default {:?}", first.source, first.name, second.source, second.name)
}

pub fn prepare(models: &mut [Model], defaults: &[ReadDefault]) -> Result<(), String> {
    // Stage all results: validation must not leave a partly prepared schema.
    let mut prepared: Vec<PreparedDefaults> = (0..models.len()).map(|_| PreparedDefaults::default()).collect();
    let mut flags: Vec<crate::schema::ReadFlags> = (0..models.len()).map(|_| Default::default()).collect();
    for d in defaults {
        let idx = models.iter().position(|m| m.ir.name == d.model).ok_or_else(|| format!("unknown read-default model {}", d.model))?;
        let model = &models[idx];
        let merged = &mut prepared[idx];
        let identity = DefaultIdentity { source: d.source.clone(), name: d.name.clone() };
        if d.source.is_empty() || d.name.is_empty() { return Err(format!("{}: a read default needs a source and name", d.model)); }
        if let Some(first) = merged.parts.iter().find(|p| p.identity.name == d.name) {
            return Err(clash(&d.model, "default name", &first.identity, &identity));
        }
        if let Some(fields) = &d.fields {
            if let Some(first) = &merged.fields_owner { return Err(clash(&d.model, "default fields", first, &identity)); }
            let mut seen = HashSet::new();
            for field in fields { model.field(field)?; if !seen.insert(field) { return Err("duplicate default selected field".into()); } }
            flags[idx].fields = Some(fields.clone());
            merged.fields_owner = Some(identity.clone());
        }
        let filter = d.filter.clone().map(serde_json::from_value::<Expr>).transpose().map_err(|e| format!("{} default filter: {e}", d.model))?;
        if let Some(filter) = &filter { validate_filter(model, filter)?; }
        if let Some(order) = &d.order {
            if let Some(first) = &merged.order_owner { return Err(clash(&d.model, "default order", first, &identity)); }
            for key in order { model.field(&key.field)?; }
            merged.order = order.iter().map(|k| Order { expr: Expr::Col { path: vec![], name: k.field.clone(), sub: vec![] }, desc: k.desc, nulls: k.nulls }).collect();
            merged.order_owner = Some(identity.clone());
        }
        let mut annotate = BTreeMap::new();
        for (name, expr) in &d.annotate {
            if let Some(first) = merged.parts.iter().find(|p| p.annotate.contains_key(name)) {
                return Err(clash(&d.model, &format!("annotation {name:?}"), &first.identity, &identity));
            }
            annotate.insert(name.clone(), serde_json::from_value(expr.clone()).map_err(|e| format!("{} default annotation {name}: {e}", d.model))?);
        }
        for path in &d.load {
            let mut current = model;
            if path.is_empty() { return Err("empty default relation path".into()); }
            for hop in path {
                let (r, _) = current.relation(hop)?;
                if r.kind != crate::ir::RelKind::One { return Err("default eager loading requires to-one references".into()); }
                current = models.iter().find(|m| m.ir.name == r.target).expect("validated relation");
            }
            if let Some(pos) = flags[idx].related.iter().position(|p| p == path) {
                if !merged.load_owners[pos].contains(&identity) { merged.load_owners[pos].push(identity.clone()); }
            } else {
                flags[idx].related.push(path.clone());
                merged.load_owners.push(vec![identity.clone()]);
            }
        }
        let mut scopes = vec![];
        if let Some(filter) = &filter { filter_scopes(filter, &mut scopes); }
        merged.parts.push(PreparedPart { identity, filter, filter_json: d.filter.clone(), annotate, scopes });
    }
    fn visit(models: &[Model], flags: &[crate::schema::ReadFlags], idx: usize, stack: &mut Vec<usize>) -> Result<(), String> {
        if stack.contains(&idx) {
            let path = stack.iter().map(|&i| models[i].ir.name.as_str()).collect::<Vec<_>>().join(" -> ");
            return Err(format!("recursive default loading: {path} -> {}", models[idx].ir.name));
        }
        stack.push(idx);
        for path in &flags[idx].related {
            let mut current = idx;
            for hop in path { current = models[current].relation(hop)?.1; visit(models, flags, current, stack)?; }
        }
        stack.pop(); Ok(())
    }
    for idx in 0..models.len() { visit(models, &flags, idx, &mut vec![])?; }
    for ((model, mut merged), flags) in models.iter_mut().zip(prepared).zip(flags) {
        let mut filters: Vec<_> = merged.parts.iter().filter_map(|p| p.filter.clone()).collect();
        merged.filter = match filters.len() { 0 => None, 1 => filters.pop(), _ => Some(Expr::And { items: filters }) };
        model.read_flags = crate::schema::ReadFlags { filtered: merged.filter.is_some(), ..flags };
        model.read_defaults = merged;
    }
    Ok(())
}

// Filters have already passed validate_filter, so only its supported nodes can occur.
fn filter_scopes(expr: &Expr, scopes: &mut Vec<String>) {
    match expr {
        Expr::Scope { name } => { if !scopes.contains(name) { scopes.push(name.clone()); } }
        Expr::Cmp { l, r, .. } => { filter_scopes(l, scopes); filter_scopes(r, scopes); }
        Expr::And { items } | Expr::Or { items } => for item in items { filter_scopes(item, scopes); },
        Expr::Not { item } | Expr::IsNull { item, .. } => filter_scopes(item, scopes),
        _ => {}
    }
}

pub(crate) fn validate_filter(model: &Model, filter: &Expr) -> Result<(), String> {
    match filter {
        Expr::Col { path, name, .. } if path.is_empty() => { model.field(name)?; }
        Expr::Const { .. } | Expr::Int { .. } | Expr::Text { .. } | Expr::Scope { .. } => {}
        Expr::Cmp { l, r, .. } => { validate_filter(model, l)?; validate_filter(model, r)?; }
        Expr::And { items } | Expr::Or { items } => for item in items { validate_filter(model, item)?; },
        Expr::Not { item } | Expr::IsNull { item, .. } => validate_filter(model, item)?,
        _ => return Err(format!("{}: default filters support root columns, literals, comparisons and boolean composition", model.ir.name)),
    }
    Ok(())
}

/// Finite eager-loading expansion, with prefixes before descendants.
pub fn expanded_related(models: &[Model], root: usize) -> Result<Vec<Vec<String>>, String> {
    fn expand(models: &[Model], model: usize, prefix: &[String], out: &mut Vec<Vec<String>>) -> Result<(), String> {
        for path in &models[model].read_flags.related {
            let mut current = model;
            let mut full = prefix.to_vec();
            for hop in path {
                current = models[current].relation(hop)?.1;
                full.push(hop.clone());
                if !out.contains(&full) { out.push(full.clone()); }
                expand(models, current, &full, out)?;
            }
        }
        Ok(())
    }
    let mut out = vec![]; expand(models, root, &[], &mut out)?; Ok(out)
}
