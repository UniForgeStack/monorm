//! One check and one order for the manifests of an artifact: the compiled ones and the ones of libraries.
use crate::extension::{AttributeTarget, Export, LanguageSection, Manifest, Pass, Phase, HOST_CAPABILITIES, LANGUAGE_CONTRACT};
use std::collections::{BTreeMap, BTreeSet};

/// The manifests, sorted by id, and their passes in run order: by `phase`, then `after` (inside one manifest), then pass id.
#[derive(Debug)]
pub struct Resolved {
    pub manifests: Vec<Manifest>,
    pub passes: Vec<Pass>,
}

/// One pass claims one effect path. Two additive claims of the same path do not conflict; any other overlap does.
struct Claim<'a> {
    effect: &'a String,
    pass: &'a String,
    additive: bool,
}

/// The DSL type names that no attribute can own: the scalar types and `Unsupported`.
pub const RESERVED_TYPES: [&str; 10] = ["BigInt", "Int", "Float", "Boolean", "String", "DateTime", "Json", "Decimal", "Bytes", "Unsupported"];

/// `check_language` validates one language section; the build owns its sources.
pub fn resolve(mut manifests: Vec<Manifest>, check_language: &dyn Fn(&Manifest, &str, &LanguageSection) -> Result<(), String>) -> Result<Resolved, String> {
    manifests.sort_by(|a, b| a.id.cmp(&b.id));
    let mut extensions = BTreeMap::new();
    let mut attributes = BTreeMap::new();
    let mut field_types: BTreeMap<&str, &str> = BTreeMap::new();
    let mut exports = BTreeSet::new();
    let mut passes = BTreeMap::new();
    let mut effects: Vec<Claim> = vec![];
    for m in &manifests {
        m.check_contracts()?;
        if m.library.is_none() { return Err(format!("{}: a manifest needs `library`, the file name of the native library", m.id)); }
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
            if a.view_source && a.target != AttributeTarget::Model {
                return Err(format!("{}: attribute {} has `view_source`, which only a model attribute takes; set `target = \"model\"`", m.id, a.name));
            }
            if a.no_table && a.target != AttributeTarget::Model {
                return Err(format!("{}: attribute {} has `no_table`, which only a model attribute takes; set `target = \"model\"`", m.id, a.name));
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
            if passes.insert(p.id.clone(), p.clone()).is_some() { return Err(format!("duplicate pass {}", p.id)); }
            let claims = p.effects.iter().map(|e| (e, false)).chain(p.additive.iter().map(|e| (e, true)));
            for (effect, additive) in claims {
                for Claim { effect: existing, pass: owner, additive: existing_additive } in &effects {
                    let overlap = *existing == effect || existing.starts_with(&format!("{effect}.")) || effect.starts_with(&format!("{existing}."));
                    if overlap && !(additive && *existing_additive && *existing == effect) {
                        return Err(format!("conflicting effect {effect} / {existing}: {owner} and {}; uninstall one of the extensions", p.id));
                    }
                }
                effects.push(Claim { effect, pass: &p.id, additive });
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
    // `after` names passes of the same manifest; the phase orders a pass against another manifest.
    let known: BTreeMap<&str, (&str, Phase)> = manifests.iter().flat_map(|m| m.passes.iter().map(move |p| (p.id.as_str(), (m.id.as_str(), p.phase)))).collect();
    for m in &manifests {
        for p in &m.passes {
            for id in &p.after {
                if m.passes.iter().any(|own| own.id == *id) { continue; }
                let dependency = known.get(id.as_str());
                // A pass id need not start with its manifest id, so the pass wins over the prefix.
                let by_prefix = manifests.iter().filter(|o| id.strip_prefix(o.id.as_str()).is_some_and(|rest| rest.starts_with('.'))).max_by_key(|o| o.id.len()).map(|o| o.id.as_str());
                let Some(owner) = dependency.map(|d| d.0).or(by_prefix).filter(|owner| *owner != m.id) else {
                    return Err(format!("{}: pass {} lists `after = [\"{id}\"]`, which is not a pass of {}; `after` names passes of the same manifest, so name one of them or use the phase", m.id, p.id, m.id));
                };
                let advice = match dependency {
                    Some((_, phase)) if *phase < p.phase => format!("remove it: the phase `{}` already runs after the phase `{}` of {id}", p.phase.name(), phase.name()),
                    Some((_, phase)) => match phase.next() {
                        Some(next) => format!("set `phase = \"{}\"` on {}, which then runs after the phase `{}` of {id}", next.name(), p.id, phase.name()),
                        None => format!("{id} is in the last phase, `{}`, so no phase runs after it", phase.name()),
                    },
                    None => format!("give {} a phase later than the phase of {id}", p.id),
                };
                return Err(format!("{}: pass {} lists `after = [\"{id}\"]`, a pass of {owner}; `after` orders the passes of one manifest, so use the phase: {advice}", m.id, p.id));
            }
        }
    }
    for p in passes.values() {
        for id in &p.after {
            if passes[id].phase > p.phase { return Err(format!("{} depends on later phase {id}", p.id)); }
        }
    }
    let mut ordered = vec![];
    let mut complete = BTreeSet::new();
    while !passes.is_empty() {
        let earliest = passes.values().map(|p| p.phase).min().unwrap();
        let next = passes.values().find(|p| p.phase == earliest && p.after.iter().all(|id| complete.contains(id))).map(|p| p.id.clone());
        let Some(next) = next else { return Err(format!("pass dependency cycle: {:?}", passes.keys().collect::<Vec<_>>())); };
        complete.insert(next.clone());
        ordered.push(passes.remove(&next).unwrap());
    }
    Ok(Resolved { manifests, passes: ordered })
}

/// A library exports typed hooks, each at a `point` from 1 to 7.
fn check_export(m: &Manifest, e: &Export) -> Result<(), String> {
    let id = &e.id;
    let point = e.point.ok_or_else(|| format!("{}: export {id} needs a `point` from 1 to 7", m.id))?;
    if !(1..=7).contains(&point) { return Err(format!("{}: export {id} has point {point}; use 1 to 7", m.id)); }
    if matches!(point, 1 | 2) && e.input.is_none() { return Err(format!("{}: export {id} at point {point} needs an `input` type", m.id)); }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library(extra: serde_json::Value) -> Manifest {
        let mut value = serde_json::json!({
            "id": "acme.lib", "version": "1.0.0", "schema_contract": "5.0", "abi": "0.2", "library": "lib",
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
                "id": id, "version": "1.0.0", "schema_contract": "5.0", "abi": "0.2", "library": "lib", "languages": [], "databases": [],
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
        assert!(resolve_all(vec![owner("acme.a", serde_json::json!({"name": "a.x", "target": "model", "view_source": true}))]).is_ok());
        let error = resolve_all(vec![owner("acme.a", serde_json::json!({"name": "a.x", "target": "field", "view_source": true}))]).unwrap_err();
        assert!(error.contains("acme.a") && error.contains("only a model attribute takes"), "{error}");
        assert!(resolve_all(vec![owner("acme.a", serde_json::json!({"name": "a.x", "target": "model", "no_table": true}))]).is_ok());
        let error = resolve_all(vec![owner("acme.a", serde_json::json!({"name": "a.x", "target": "field", "no_table": true}))]).unwrap_err();
        assert!(error.contains("acme.a") && error.contains("`no_table`") && error.contains("only a model attribute takes"), "{error}");
    }

    fn claiming(id: &str, effects: serde_json::Value, additive: serde_json::Value) -> Manifest {
        library(serde_json::json!({"id": id, "passes": [{"id": format!("{id}.run"), "phase": "behavior", "effects": effects, "additive": additive}]}))
    }

    #[test]
    fn an_additive_effect_takes_several_claims_and_an_exclusive_effect_takes_one() {
        let add = |id: &str| claiming(id, serde_json::json!([]), serde_json::json!(["behavior.read_defaults"]));
        let set = |id: &str, effect: &str| claiming(id, serde_json::json!([effect]), serde_json::json!([]));
        // The order of the passes is by pass id, and `after` overrides it.
        let order = |r: Resolved| r.passes.into_iter().map(|p| p.id).collect::<Vec<_>>();
        assert_eq!(order(resolve_all(vec![add("acme.b"), add("acme.a"), add("acme.c")]).unwrap()), ["acme.a.run", "acme.b.run", "acme.c.run"]);
        // `after` overrides the id order inside one manifest: `acme.a.run` waits for `acme.a.zz`.
        let mut pair = add("acme.a");
        pair.passes[0].after = vec!["acme.a.zz".into()];
        let mut zz = pair.passes[0].clone();
        zz.id = "acme.a.zz".into();
        zz.after = vec![];
        pair.passes.push(zz);
        assert_eq!(order(resolve_all(vec![pair, add("acme.b")]).unwrap()), ["acme.a.zz", "acme.a.run", "acme.b.run"]);
        // An exclusive claim of the same effect, or of a parent or child path, conflicts with an additive one in both orders.
        for exclusive in ["behavior.read_defaults", "behavior", "behavior.read_defaults.x"] {
            for (first, second) in [(add("acme.a"), set("acme.b", exclusive)), (set("acme.a", exclusive), add("acme.b"))] {
                let error = resolve_all(vec![first, second]).unwrap_err();
                assert!(error.contains("conflicting effect") && error.contains("acme.a.run") && error.contains("acme.b.run"), "{exclusive}: {error}");
            }
        }
        assert!(resolve_all(vec![set("acme.a", "behavior.x"), set("acme.b", "behavior.x")]).is_err());
        // An additive claim of a parent path does not share with a child path.
        let child = claiming("acme.b", serde_json::json!([]), serde_json::json!(["behavior.read_defaults.x"]));
        assert!(resolve_all(vec![add("acme.a"), child]).is_err());
    }

    fn with_pass(id: &str, pass: &str, phase: &str, after: &[&str]) -> Manifest {
        library(serde_json::json!({"id": id, "passes": [{"id": pass, "phase": phase, "after": after}]}))
    }

    #[test]
    fn after_names_a_pass_of_the_same_manifest_only() {
        let order = |r: Resolved| r.passes.into_iter().map(|p| p.id).collect::<Vec<_>>();
        let two = |first: &str| library(serde_json::json!({"id": "acme.a", "passes": [
            {"id": "acme.a.one", "phase": "behavior", "after": [first]}, {"id": "acme.a.two", "phase": "behavior"}]}));
        assert_eq!(order(resolve_all(vec![two("acme.a.two")]).unwrap()), ["acme.a.two", "acme.a.one"]);
        // A pass of another manifest, by pass id: the message names the manifest and the phase to use.
        let error = resolve_all(vec![with_pass("acme.a", "acme.a.run", "behavior", &["acme.b.run"]), with_pass("acme.b", "acme.b.run", "behavior", &[])]).unwrap_err();
        assert!(error.contains("acme.a: pass acme.a.run lists `after = [\"acme.b.run\"]`, a pass of acme.b") && error.contains("phase = \"late_behavior\"") && error.contains("phase `behavior` of acme.b.run"), "{error}");
        // The pass id need not start with the manifest id.
        let error = resolve_all(vec![with_pass("acme.a", "acme.a.run", "declaration", &["other.run"]), with_pass("acme.b", "other.run", "declaration", &[])]).unwrap_err();
        assert!(error.contains("a pass of acme.b") && error.contains("phase = \"logical\""), "{error}");
        // The later phase already orders the pass: the message says to drop `after`.
        let error = resolve_all(vec![with_pass("acme.a", "acme.a.run", "late_behavior", &["acme.b.run"]), with_pass("acme.b", "acme.b.run", "behavior", &[])]).unwrap_err();
        assert!(error.contains("remove it") && error.contains("phase `late_behavior` already runs after the phase `behavior`"), "{error}");
        // The last phase has no later one.
        let error = resolve_all(vec![with_pass("acme.a", "acme.a.run", "generation", &["acme.b.run"]), with_pass("acme.b", "acme.b.run", "generation", &[])]).unwrap_err();
        assert!(error.contains("no phase runs after it"), "{error}");
        // A manifest that is not installed has no pass to find, and its id as a prefix of the `after` id still refuses: the longest manifest id wins.
        let error = resolve_all(vec![with_pass("acme.a", "acme.a.run", "behavior", &["acme.b.x.run"]), with_pass("acme.b", "acme.b.run", "behavior", &[])]).unwrap_err();
        assert!(error.contains("a pass of acme.b") && error.contains("give acme.a.run a phase later"), "{error}");
        let nested = resolve_all(vec![with_pass("acme.b", "acme.b.one", "behavior", &["acme.b.sub.run"]), with_pass("acme.b.sub", "acme.b.sub.run", "behavior", &[])]).unwrap_err();
        assert!(nested.contains("a pass of acme.b.sub"), "{nested}");
        // An id that no installed manifest owns is refused too, so a manifest loads the same way with or without the other extension.
        let error = resolve_all(vec![with_pass("acme.a", "acme.a.run", "behavior", &["elsewhere.run"])]).unwrap_err();
        assert!(error.contains("acme.a: pass acme.a.run lists `after = [\"elsewhere.run\"]`, which is not a pass of acme.a"), "{error}");
        // A pass of another manifest, with that manifest absent: the same refusal, not a skip.
        let error = resolve_all(vec![with_pass("acme.a", "acme.a.run", "behavior", &["acme.b.run"])]).unwrap_err();
        assert!(error.contains("which is not a pass of acme.a"), "{error}");
        // A dangling own id is a plain refusal.
        let error = resolve_all(vec![with_pass("acme.b", "acme.b.one", "behavior", &["acme.b.typo"]), with_pass("acme.b.sub", "acme.b.sub.run", "behavior", &[])]).unwrap_err();
        assert!(error.contains("which is not a pass of acme.b"), "{error}");
    }

    #[test]
    fn the_phases_run_in_the_order_of_the_contract() {
        let phases = ["expansion", "declaration", "logical", "storage", "behavior", "late_behavior", "validation", "generation"];
        let manifests = phases.iter().rev().enumerate().map(|(i, phase)| with_pass(&format!("acme.p{i}"), &format!("acme.p{i}.run"), phase, &[])).collect();
        let ordered = resolve_all(manifests).unwrap().passes.into_iter().map(|p| p.phase.name()).collect::<Vec<_>>();
        assert_eq!(ordered, phases);
        let mut phase = Some(Phase::Expansion);
        for name in phases {
            assert_eq!(phase.unwrap().name(), name);
            phase = phase.unwrap().next();
        }
        assert_eq!(phase, None);
    }

    #[test]
    fn a_manifest_of_the_removed_static_path_fails_and_names_the_field() {
        let parse = |extra: serde_json::Value| -> Result<Manifest, String> {
            let mut value = serde_json::json!({"id": "acme.old", "version": "1.0.0", "schema_contract": "5.0", "languages": [], "databases": []});
            value.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            serde_json::from_value(value).map_err(|e| e.to_string())
        };
        for field in ["host_contract", "requires"] {
            let error = parse(serde_json::json!({field: "x"})).unwrap_err();
            assert!(error.contains(&format!("unknown field `{field}`")), "{error}");
        }
        for field in ["rust", "kind"] {
            let export = serde_json::json!({"id": "acme.old.x", "point": 2, "input": "string", field: "x"});
            let error = parse(serde_json::json!({"abi": "0.2", "library": "lib", "exports": [export]})).unwrap_err();
            assert!(error.contains(&format!("unknown field `{field}`")), "{error}");
        }
        let pass = serde_json::json!({"id": "acme.old.p", "phase": "declaration", "rust": "x"});
        let error = parse(serde_json::json!({"abi": "0.2", "library": "lib", "passes": [pass]})).unwrap_err();
        assert!(error.contains("unknown field `rust`"), "{error}");
        let error = resolve_all(vec![parse(serde_json::json!({})).unwrap()]).unwrap_err();
        assert!(error.contains("acme.old: a manifest needs `abi`"), "{error}");
    }
}
