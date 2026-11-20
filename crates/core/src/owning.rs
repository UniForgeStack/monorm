//! Schema checks of owning relations: the owner holds the key of a row that lives only while an owner points at it.
use crate::{dialect::Dialect, ir::RelKind, schema::Model};
use crate::{behavior::{FieldStorage, OwnerLink}, writes::ScopedField};
use std::collections::BTreeSet;

pub fn validate(models: &[Model], model_index: &std::collections::HashMap<String, usize>, dialect: Dialect, scoped: &[ScopedField], links: &[OwnerLink], storage: &[FieldStorage]) -> Result<(), String> {
    let keys = |model: &str| -> BTreeSet<&str> { scoped.iter().filter(|s| s.model == model).map(|s| s.key.as_str()).collect() };
    for m in models {
        for r in m.ir.relations.iter().filter(|r| r.owns) {
            let what = |e: &str| format!("relation {}.{}: owns: {e}", m.ir.name, r.name);
            if r.kind != RelKind::One || !r.foreign_key || r.from.is_empty() {
                return Err(what("goes on a to-one side that has fields: and references:"));
            }
            let target = &models[model_index[&r.target]];
            if r.to.iter().map(String::as_str).ne(target.ir.key_fields()) {
                return Err(what(&format!("references: must be the primary key of {}, in key order", target.ir.name)));
            }
            if keys(&m.ir.name) != keys(&target.ir.name) {
                return Err(what(&format!("{} and {} must have the same scope keys, or the owned row can leave its scope", m.ir.name, target.ir.name)));
            }
            let level = |owner: &str| links.iter().any(|l| l.child == owner);
            let on_level = level(&m.ir.name) || storage.iter().any(|s| s.model == m.ir.name && r.from.contains(&s.field) && level(&s.owner));
            if on_level {
                return Err(what("an owning field belongs to the root of a composed model, not to a level below it"));
            }
            if level(&target.ir.name) && r.from.len() != 1 {
                return Err(what(&format!("{} is a level of a composed model, so the owning relation needs one column in fields:", target.ir.name)));
            }
            if dialect == Dialect::Sqlite {
                if r.target == m.ir.name {
                    return Err(what("a model can't own itself on SQLite, which keeps no row order for the delete trigger"));
                }
                if let Some(path) = owning_path(models, model_index, &r.target, &m.ir.name) {
                    return Err(what(&format!("an owning cycle ({} -> {}) is not allowed on SQLite: a trigger does not fire again while it runs, so a longer chain leaves an orphan row", m.ir.name, path.join(" -> "))));
                }
            }
        }
    }
    Ok(())
}

/// The models from `from` to `to` along owning relations (both ends included), if there is a path.
fn owning_path(models: &[Model], model_index: &std::collections::HashMap<String, usize>, from: &str, to: &str) -> Option<Vec<String>> {
    let mut seen = BTreeSet::from([from.to_owned()]);
    let mut queue = vec![vec![from.to_owned()]];
    while !queue.is_empty() {
        let path = queue.remove(0);
        let last = path.last()?;
        if last == to { return Some(path); }
        for r in models[model_index[last]].ir.relations.iter().filter(|r| r.owns) {
            if seen.insert(r.target.clone()) {
                queue.push(path.iter().cloned().chain([r.target.clone()]).collect());
            }
        }
    }
    None
}
