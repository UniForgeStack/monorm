//! Prepared column roles of the `updated-at`, `soft-delete` and `optimistic-locking`
//! extensions. A build without a feature rejects schemas that use it.
use std::collections::HashMap;

use crate::behavior::Requirements;
#[cfg_attr(not(any(feature = "updated-at", feature = "soft-delete", feature = "optimistic-locking")), allow(unused_imports))]
use crate::ir::ColType;
use crate::schema::{Model, Result};

#[cfg_attr(not(any(feature = "updated-at", feature = "soft-delete", feature = "optimistic-locking")), allow(dead_code))]
fn position(models: &[Model], index: &HashMap<String, usize>, model: &str, field: &str, what: &str, ok: impl Fn(&crate::ir::FieldIr) -> bool) -> Result<(usize, usize)> {
    let m = *index.get(model).ok_or_else(|| format!("{what}: unknown model {model}; rebuild schema"))?;
    let p = models[m].field_pos(field)?;
    let f = &models[m].fields()[p];
    if f.array || !ok(f) { return Err(format!("{model}.{field}: wrong type for {what}; rebuild schema")); }
    Ok((m, p))
}

#[cfg_attr(not(any(feature = "updated-at", feature = "soft-delete", feature = "optimistic-locking")), allow(unused_variables))]
pub fn prepare(models: &mut [Model], index: &HashMap<String, usize>, r: &Requirements) -> Result<()> {
    #[cfg(not(feature = "updated-at"))]
    if !r.updated_at.is_empty() { return Err("@timestamps.updated_at requires an enabled updated-at artifact; rebuild".into()); }
    #[cfg(feature = "updated-at")]
    for u in &r.updated_at {
        let (m, p) = position(models, index, &u.model, &u.field, "updated_at", |f| f.ty == ColType::DateTime)?;
        models[m].updated_at.push(p);
    }
    #[cfg(not(feature = "soft-delete"))]
    if !r.soft_delete.is_empty() { return Err("@soft_delete.deleted_at requires an enabled soft-delete artifact; rebuild".into()); }
    #[cfg(feature = "soft-delete")]
    for s in &r.soft_delete {
        let (m, p) = position(models, index, &s.model, &s.field, "soft delete", |f| f.ty == ColType::DateTime && f.nullable)?;
        if models[m].soft_delete.replace(p).is_some() { return Err(format!("{}: one soft-delete field only", s.model)); }
    }
    #[cfg(not(feature = "optimistic-locking"))]
    if !r.versions.is_empty() { return Err("@locking.version requires an enabled optimistic-locking artifact; rebuild".into()); }
    #[cfg(feature = "optimistic-locking")]
    for v in &r.versions {
        let (m, p) = position(models, index, &v.model, &v.field, "version", |f| matches!(f.ty, ColType::Int | ColType::BigInt))?;
        if models[m].version.replace(p).is_some() { return Err(format!("{}: one version field only", v.model)); }
    }
    Ok(())
}

/// A proxy shares its root's table, so it takes the root's column roles by field name.
#[cfg(feature = "proxy-models")]
#[cfg_attr(not(any(feature = "updated-at", feature = "soft-delete", feature = "optimistic-locking")), allow(unused_variables, unused_mut))]
pub fn inherit_proxy_roles(models: &mut [Model], index: &HashMap<String, usize>, proxies: &[orm_contracts::extension::ProxyModel]) -> Result<()> {
    for proxy in proxies {
        let mut root = proxy.parent.as_str();
        while let Some(next) = proxies.iter().find(|p| p.model == root) { root = &next.parent; }
        let (p, r) = (index[&proxy.model], index[root]);
        #[cfg(feature = "updated-at")]
        {
            let names: Vec<String> = models[r].updated_at.iter().map(|&i| models[r].fields()[i].name.clone()).collect();
            // An excluded updated_at field stays with the database trigger.
            let kept = names.iter().filter_map(|n| models[p].field_pos(n).ok()).collect();
            models[p].updated_at = kept;
        }
        #[cfg(feature = "soft-delete")]
        if let Some(i) = models[r].soft_delete {
            let name = models[r].fields()[i].name.clone();
            let pos = models[p].field_pos(&name).map_err(|_| format!("{}: proxy must keep the soft-delete field {name}", proxy.model))?;
            models[p].soft_delete = Some(pos);
        }
        #[cfg(feature = "optimistic-locking")]
        if let Some(i) = models[r].version {
            let name = models[r].fields()[i].name.clone();
            let pos = models[p].field_pos(&name).map_err(|_| format!("{}: proxy must keep the version field {name}", proxy.model))?;
            models[p].version = Some(pos);
        }
    }
    Ok(())
}
