//! One check and one order for the manifests of an artifact: the compiled ones and the ones of libraries.
use crate::extension::{AttributeTarget, LanguageSection, Manifest, Pass, HOST_CAPABILITIES, LANGUAGE_CONTRACT};
use std::collections::{BTreeMap, BTreeSet};

/// The manifests, sorted by id, and their passes in run order: by `phase`, then `after`, then pass id.
#[derive(Debug)]
pub struct Resolved {
    pub manifests: Vec<Manifest>,
    pub passes: Vec<Pass>,
}

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
        if m.is_dynamic() && !m.exports.is_empty() {
            return Err(format!("{}: exports need hooks, which ABI {} does not have; remove them", m.id, m.abi.unwrap()));
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
        for e in &m.exports {
            rust_path(&e.rust)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn library(extra: serde_json::Value) -> Manifest {
        let mut value = serde_json::json!({
            "id": "acme.lib", "version": "1.0.0", "schema_contract": "2.0", "abi": "0.1", "library": "lib",
            "languages": ["python"], "databases": ["sqlite"],
        });
        value.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        serde_json::from_value(value).unwrap()
    }

    fn resolve_all(manifests: Vec<Manifest>) -> Result<Resolved, String> { resolve(manifests, &|_, _, _| Ok(())) }

    #[test]
    fn a_library_manifest_checks_language_contract_and_has_no_hook_export() {
        assert!(resolve_all(vec![library(serde_json::json!({}))]).is_ok());
        let section = serde_json::json!({"source": "py", "query_set": "QuerySet", "instance": "Instance"});
        assert!(resolve_all(vec![library(serde_json::json!({"python": section}))]).unwrap_err().contains("incompatible language contract"));
        assert!(resolve_all(vec![library(serde_json::json!({"python": section, "language_contract": LANGUAGE_CONTRACT}))]).is_ok());
        let export = serde_json::json!([{"id": "acme.lib.x", "rust": "crate::x", "kind": "string_validator", "input": "string"}]);
        let error = resolve_all(vec![library(serde_json::json!({"exports": export}))]).unwrap_err();
        assert!(error.contains("exports need hooks"), "{error}");
    }
}
