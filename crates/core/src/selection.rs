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
        if let Some(order) = &d.order {
            if let Some(first) = &merged.order_owner { return Err(clash(&d.model, "default order", first, &identity)); }
            for key in order { model.field(&key.field)?; }
            merged.order = order.iter().map(|k| Order { expr: Expr::Col { path: vec![], name: k.field.clone(), sub: vec![], alias: vec![] }, desc: k.desc, nulls: k.nulls }).collect();
            merged.order_owner = Some(identity.clone());
        }
        let mut scopes = vec![];
        if let Some(filter) = &filter { walk(models, model, filter, false, &mut scopes)?; }
        let mut annotate = BTreeMap::new();
        for (name, expr) in &d.annotate {
            if let Some(first) = merged.parts.iter().find(|p| p.annotate.contains_key(name)) {
                return Err(clash(&d.model, &format!("annotation {name:?}"), &first.identity, &identity));
            }
            if model.field(name).is_ok() {
                return Err(format!("{}: {} default {:?} annotation {name:?} has the name of a model field", d.model, d.source, d.name));
            }
            let expr: Expr = serde_json::from_value(expr.clone()).map_err(|e| format!("{} default annotation {name}: {e}", d.model))?;
            if !matches!(expr, Expr::Func { rel: Some(_), .. }) {
                return Err(format!("{}: {} default {:?} annotation {name:?} must be a relation aggregate", d.model, d.source, d.name));
            }
            walk(models, model, &expr, true, &mut scopes).map_err(|e| format!("{} default {:?} annotation {name:?}: {e}", d.source, d.name))?;
            annotate.insert(name.clone(), expr);
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

/// The model at the end of the relation `path` from `model`.
fn target<'m>(models: &'m [Model], model: &'m Model, path: &[String]) -> Result<&'m Model, String> {
    let mut current = model;
    for hop in path {
        let (r, _) = current.relation(hop)?;
        current = models.iter().find(|m| m.ir.name == r.target).expect("validated relation");
    }
    Ok(current)
}

/// Checks a default expression and collects its `scope.<name>` names, in one pass so the
/// two cannot differ. A filter has root columns, literals, comparisons and boolean composition.
/// An annotation is a relation aggregate; its arguments may also use arithmetic, calls and
/// columns behind relation hops.
fn walk(models: &[Model], model: &Model, expr: &Expr, annotation: bool, scopes: &mut Vec<String>) -> Result<(), String> {
    let mut children: Vec<&Expr> = vec![];
    match expr {
        Expr::Col { path, name, .. } if path.is_empty() => { model.field(name)?; }
        Expr::Col { path, name, .. } if annotation => { target(models, model, path)?.field(name)?; }
        Expr::Const { .. } | Expr::Int { .. } | Expr::Text { .. } => {}
        Expr::Scope { name } => { if !scopes.contains(name) { scopes.push(name.clone()); } }
        Expr::Cmp { l, r, .. } => children.extend([&**l, &**r]),
        Expr::And { items } | Expr::Or { items } => children.extend(items),
        Expr::Not { item } | Expr::IsNull { item, .. } => children.push(item),
        Expr::Arith { l, r, .. } if annotation => children.extend([&**l, &**r]),
        Expr::Func { args, filter, order_by, rel, .. } if annotation => {
            if let Some(rel) = rel { target(models, model, rel)?; }
            children.extend(args.iter().chain(filter.as_deref()).chain(order_by.iter().map(|o| &o.expr)));
        }
        _ if annotation => return Err(format!("{}: default annotations support relation aggregates over columns, literals, arithmetic, comparisons and boolean composition", model.ir.name)),
        _ => return Err(format!("{}: default filters support root columns, literals, comparisons and boolean composition", model.ir.name)),
    }
    children.into_iter().try_for_each(|child| walk(models, model, child, annotation, scopes))
}

pub(crate) fn validate_filter(model: &Model, filter: &Expr) -> Result<(), String> {
    walk(&[], model, filter, false, &mut vec![])
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
