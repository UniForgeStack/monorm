//! The rules for removing fields from a logical model while the physical table keeps them.
//! `@deprecated` and the proxy extension share them.
use crate::ir::ModelIr;
use std::collections::BTreeSet;

/// Selects the wording of an error. The rules are the same.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hide {
    /// A proxy omits the field.
    Omitted,
    /// `@deprecated` removes the field.
    Deprecated,
}

/// Checks that `hidden` can leave `source`, the logical model called `model`.
/// `physical` is the captured table of the same model; `models` are all logical models.
pub fn check(models: &[ModelIr], model: &str, source: &ModelIr, physical: &ModelIr, hidden: &BTreeSet<String>, kind: Hide) -> Result<(), String> {
    let (verb, insert) = match kind {
        Hide::Omitted => ("omitted", "an insert through the proxy would fail"),
        Hide::Deprecated => ("deprecated", "an insert would fail"),
    };
    for f in source.fields.iter().filter(|f| hidden.contains(&f.name)) {
        if f.primary_key || source.primary_key.contains(&f.name) {
            return Err(format!("{model}.{}: the primary key cannot be {verb}", f.name));
        }
        let stored = physical.fields.iter().find(|p| p.column == f.column);
        if stored.is_some_and(|p| !p.nullable && p.default.is_none() && !p.default_now && p.default_sql.is_none() && !p.auto_increment && p.generated.is_none()) {
            return Err(format!("{model}.{}: a NOT NULL field without a database default cannot be {verb}, because {insert}", f.name));
        }
    }
    let own = source.relations.iter().filter(|r| !hidden.contains(&r.name));
    if let Some((r, key)) = own.into_iter().find_map(|r| r.from.iter().find(|f| hidden.contains(*f)).map(|f| (r, f))) {
        let hint = match kind {
            Hide::Omitted => "omit the relation too",
            Hide::Deprecated => "remove the relation first",
        };
        return Err(format!("{model}.{key}: relation {} uses it as its key; {hint}", r.name));
    }
    for other in models {
        if let Some((r, key)) = other.relations.iter().filter(|r| r.target == model).find_map(|r| r.to.iter().find(|f| hidden.contains(*f)).map(|f| (r, f))) {
            return Err(format!("{model}.{key}: relation {}.{} references it", other.name, r.name));
        }
    }
    Ok(())
}
