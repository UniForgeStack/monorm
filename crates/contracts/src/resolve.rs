//! One check and one order for the manifests of an artifact: the compiled ones and the ones of libraries.
use crate::extension::{AttributeTarget, Export, LanguageSection, Manifest, Pass, HOST_CAPABILITIES, LANGUAGE_CONTRACT};
use std::collections::{BTreeMap, BTreeSet};

/// The manifests, sorted by id, and their passes in run order: by `phase`, then `after`, then pass id.
#[derive(Debug)]
pub struct Resolved {
    pub manifests: Vec<Manifest>,
    pub passes: Vec<Pass>,
}

/// The DSL type names that no attribute can own: the scalar types and `Unsupported`.
pub const RESERVED_TYPES: [&str; 9] = ["BigInt", "Int", "Float", "Boolean", "String", "DateTime", "Json", "Decimal", "Unsupported"];

/// Reject source injection in metadata before emitting Rust paths.
pub fn rust_path(path: &str) -> Result<(), String> {
    if path.split("::").any(|part| {
        part.is_empty() || !part.bytes().enumerate().all(|(i, c)| c == b'_' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit()))
    }) { return Err(format!("invalid Rust export path {path:?}")); }
    Ok(())
}

/// `check_language` validates one language section; the build owns its sources.
pub fn resolve(mut manifests: Vec<Manifest>, check_language: &dyn Fn(&Manifest, &str, &LanguageSection) -> Result<(), String>) -> Result<Resolved, String> {
    manifests.sort_by(|a, b| a.id.cmp(&b.id));
    let mut extensions = BTreeMap::new();
    let mut attributes = BTreeMap::new();
    let mut field_types: BTreeMap<&str, &str> = BTreeMap::new();
    let mut exports = BTreeSet::new();
    let mut passes = BTreeMap::new();
    let mut effects: BTreeMap<&str, &str> = BTreeMap::new();
    for m in &manifests {
        m.check_contracts()?;
        match (m.is_dynamic(), &m.library) {
            (true, None) => return Err(format!("{}: a manifest with `abi` needs `library`, the file name of the native library", m.id)),
            (false, Some(_)) => return Err(format!("{}: `library` needs `abi`", m.id)),
            _ => {}
        }
        let sections: Vec<_> = m.language_sections().collect();
        if !sections.is_empty() && m.language_contract != Some(LANGUAGE_CONTRACT) {
            return Err(format!("{}: incompatible language contract; rebuild", m.id));
        }
        for (language, section) in sections { check_language(m, language, section)?; }
        for capability in &m.capabilities {
            if !HOST_CAPABILITIES.contains(&capability.as_str()) {
                return Err(format!("{} requires unavailable host capability {capability}; rebuild with a compatible host contract", m.id));
            }
        }
        let version = semver::Version::parse(&m.version).map_err(|e| format!("{}: {e}", m.id))?;
        if extensions.insert(&m.id, version).is_some() {
            return Err(format!("duplicate extension {}; keep one of its sources", m.id));
        }
        for a in &m.attributes {
            if !a.name.contains('.') { return Err(format!("{}: attribute {} must be namespaced", m.id, a.name)); }
            if let Some(owner) = attributes.insert((&a.name, a.target == AttributeTarget::Field), &m.id) {
                return Err(format!("attribute {} has duplicate owners {owner} and {}; uninstall one of them", a.name, m.id));
            }
        }
        for a in &m.attributes {
            if a.target != AttributeTarget::Field && (a.field_type.is_some() || !a.member) {
                return Err(format!("{}: attribute {} has `field_type` or `member = false`, which only a field attribute takes; set `target = \"field\"`", m.id, a.name));
            }
            let Some(ty) = a.field_type.as_deref() else { continue };
            if ty.is_empty() || !ty.starts_with(|c: char| c.is_ascii_uppercase()) || !ty.bytes().all(|c| c == b'_' || c.is_ascii_alphanumeric()) {
                return Err(format!("{}: attribute {} owns the type {ty:?}; a type name starts with an uppercase letter and has letters, digits and `_` only", m.id, a.name));
            }
            if RESERVED_TYPES.contains(&ty) {
                return Err(format!("{}: attribute {} owns the type {ty}, which is a built-in type; choose another name", m.id, a.name));
            }
            if let Some(owner) = field_types.insert(ty, &m.id) {
                return Err(format!("the type {ty} has two owners, {owner} and {}; uninstall one of them", m.id));
            }
        }
        for e in &m.exports {
            check_export(m, e)?;
            if !exports.insert(&e.id) { return Err(format!("duplicate export {}", e.id)); }
        }
        for p in &m.passes {
            match (&p.rust, m.is_dynamic()) {
                (Some(rust), false) => rust_path(rust)?,
                (None, false) => return Err(format!("{}: pass {} has no `rust` path", m.id, p.id)),
                (_, true) => {}
            }
            if passes.insert(p.id.clone(), p.clone()).is_some() { return Err(format!("duplicate pass {}", p.id)); }
            for effect in &p.effects {
                for (existing, owner) in &effects {
                    if *existing == effect || existing.starts_with(&format!("{effect}.")) || effect.starts_with(&format!("{existing}.")) {
                        return Err(format!("conflicting effect {effect} / {existing}: {owner} and {}; uninstall one of the extensions", p.id));
                    }
                }
                effects.insert(effect, &p.id);
            }
        }
    }
    for m in &manifests {
        for (id, requirement) in &m.dependencies {
            let version = extensions.get(id).ok_or_else(|| format!("{} requires missing extension {id}", m.id))?;
            let requirement = semver::VersionReq::parse(requirement).map_err(|e| e.to_string())?;
            if !requirement.matches(version) { return Err(format!("{} requires {id}@{requirement}, resolved {version}", m.id)); }
        }
    }
    for p in passes.values() {
        // An `after` pass that no selected extension provides is skipped: it orders only passes that are present.
        for id in &p.after {
            let Some(dependency) = passes.get(id) else { continue };
            if dependency.phase > p.phase { return Err(format!("{} depends on later phase {id}", p.id)); }
        }
    }
    let present: BTreeSet<String> = passes.keys().cloned().collect();
    let mut ordered = vec![];
    let mut complete = BTreeSet::new();
    while !passes.is_empty() {
        let earliest = passes.values().map(|p| p.phase).min().unwrap();
        let next = passes.values().find(|p| p.phase == earliest && p.after.iter().all(|id| complete.contains(id) || !present.contains(id))).map(|p| p.id.clone());
        let Some(next) = next else { return Err(format!("pass dependency cycle: {:?}", passes.keys().collect::<Vec<_>>())); };
        complete.insert(next.clone());
        ordered.push(passes.remove(&next).unwrap());
    }
    Ok(Resolved { manifests, passes: ordered })
}

/// A library exports typed hooks (`point`), and a compiled extension exports string functions (`rust`, `kind`).
fn check_export(m: &Manifest, e: &Export) -> Result<(), String> {
    let id = &e.id;
    if m.is_dynamic() {
        if e.rust.is_some() || e.kind.is_some() {
            return Err(format!("{}: export {id} of a library has no `rust` or `kind`; it takes a `point`", m.id));
        }
        let point = e.point.ok_or_else(|| format!("{}: export {id} needs a `point` from 1 to 7", m.id))?;
        if !(1..=7).contains(&point) { return Err(format!("{}: export {id} has point {point}; use 1 to 7", m.id)); }
        if matches!(point, 1 | 2) && e.input.is_none() { return Err(format!("{}: export {id} at point {point} needs an `input` type", m.id)); }
        return Ok(());
    }
    if e.point.is_some() { return Err(format!("{}: export {id} has a `point`, and a compiled extension has no hooks; use `rust` and `kind`", m.id)); }
    let (Some(rust), Some(_), Some(_)) = (&e.rust, e.kind, e.input) else {
        return Err(format!("{}: export {id} needs `rust`, `kind` and `input`", m.id));
    };
    rust_path(rust)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library(extra: serde_json::Value) -> Manifest {
        let mut value = serde_json::json!({
            "id": "acme.lib", "version": "1.0.0", "schema_contract": "3.0", "abi": "0.2", "library": "lib",
            "languages": ["python"], "databases": ["sqlite"],
        });
        value.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        serde_json::from_value(value).unwrap()
    }

    fn resolve_all(manifests: Vec<Manifest>) -> Result<Resolved, String> { resolve(manifests, &|_, _, _| Ok(())) }

    #[test]
    fn a_library_manifest_checks_language_contract_and_the_form_of_its_exports() {
        assert!(resolve_all(vec![library(serde_json::json!({}))]).is_ok());
        let section = serde_json::json!({"source": "py", "query_set": "QuerySet", "instance": "Instance"});
        assert!(resolve_all(vec![library(serde_json::json!({"python": section}))]).unwrap_err().contains("incompatible language contract"));
        assert!(resolve_all(vec![library(serde_json::json!({"python": section, "language_contract": LANGUAGE_CONTRACT}))]).is_ok());
        let export = |value: serde_json::Value| resolve_all(vec![library(serde_json::json!({"exports": [value]}))]);
        let error = export(serde_json::json!({"id": "acme.lib.x", "rust": "crate::x", "kind": "string_validator", "input": "string"})).unwrap_err();
        assert!(error.contains("takes a `point`"), "{error}");
        assert!(export(serde_json::json!({"id": "acme.lib.x", "input": "string"})).unwrap_err().contains("needs a `point`"));
        assert!(export(serde_json::json!({"id": "acme.lib.x", "point": 8, "input": "string"})).unwrap_err().contains("use 1 to 7"));
        assert!(export(serde_json::json!({"id": "acme.lib.x", "point": 2})).unwrap_err().contains("needs an `input` type"));
        assert!(export(serde_json::json!({"id": "acme.lib.x", "point": 2, "input": "string"})).is_ok());
        assert!(export(serde_json::json!({"id": "acme.lib.x", "point": 6, "output": "json"})).is_ok());
    }

    #[test]
    fn an_attribute_owns_a_field_type_once_and_only_on_a_field() {
        let owner = |id: &str, attribute: serde_json::Value| -> Manifest {
            serde_json::from_value(serde_json::json!({
                "id": id, "version": "1.0.0", "schema_contract": "3.2", "host_contract": "1.0", "languages": [], "databases": [],
                "attributes": [attribute],
            })).unwrap()
        };
        let money = |id: &str, name: &str| owner(id, serde_json::json!({"name": name, "target": "field", "field_type": "Money", "identities": true}));
        let ok = resolve_all(vec![money("acme.a", "a.money")]).unwrap();
        assert_eq!(ok.manifests[0].attributes[0].field_type.as_deref(), Some("Money"));
        assert!(ok.manifests[0].attributes[0].member && ok.manifests[0].attributes[0].identities);
        let error = resolve_all(vec![money("acme.a", "a.money"), money("acme.b", "b.money")]).unwrap_err();
        assert!(error.contains("the type Money has two owners, acme.a and acme.b"), "{error}");
        for ty in RESERVED_TYPES {
            let error = resolve_all(vec![owner("acme.a", serde_json::json!({"name": "a.x", "target": "field", "field_type": ty}))]).unwrap_err();
            assert!(error.contains("acme.a") && error.contains("built-in type"), "{ty}: {error}");
        }
        let error = resolve_all(vec![owner("acme.a", serde_json::json!({"name": "a.x", "target": "field", "field_type": "money"}))]).unwrap_err();
        assert!(error.contains("starts with an uppercase letter"), "{error}");
        for key in [serde_json::json!({"field_type": "Money"}), serde_json::json!({"member": false})] {
            let mut attribute = serde_json::json!({"name": "a.x", "target": "model"});
            attribute.as_object_mut().unwrap().extend(key.as_object().unwrap().clone());
            let error = resolve_all(vec![owner("acme.a", attribute)]).unwrap_err();
            assert!(error.contains("acme.a") && error.contains("only a field attribute takes"), "{error}");
        }
        // `identities` is valid on a model attribute.
        assert!(resolve_all(vec![owner("acme.a", serde_json::json!({"name": "a.x", "target": "model", "identities": true}))]).is_ok());
    }

    #[test]
    fn a_compiled_manifest_keeps_the_string_form() {
        let compiled = |export: serde_json::Value| -> Manifest {
            serde_json::from_value(serde_json::json!({"id": "acme.c", "version": "1.0.0", "schema_contract": "3.0", "host_contract": "1.0", "languages": [], "databases": [], "exports": [export]})).unwrap()
        };
        let valid = serde_json::json!({"id": "acme.c.x", "rust": "crate::x", "kind": "string_validator", "input": "string"});
        assert!(resolve_all(vec![compiled(valid)]).is_ok());
        let error = resolve_all(vec![compiled(serde_json::json!({"id": "acme.c.x", "point": 2, "input": "string"}))]).unwrap_err();
        assert!(error.contains("has no hooks"), "{error}");
        assert!(resolve_all(vec![compiled(serde_json::json!({"id": "acme.c.x", "kind": "string_validator", "input": "string"}))]).unwrap_err().contains("needs `rust`"));
    }
}
