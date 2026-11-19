//! The passes of the first-party extensions run in one fixed order, and a pass orders itself against another manifest by its phase.
use monorm::extension::Manifest;
use monorm::resolve::resolve;
use std::{fs, path::Path};

fn first_party() -> Vec<Manifest> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../extensions");
    let mut paths: Vec<_> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().path().join("monorm-extension.toml")).filter(|p| p.exists()).collect();
    paths.sort();
    paths.iter().map(|p| toml::from_str(&fs::read_to_string(p).unwrap()).unwrap()).collect()
}

fn order(manifests: Vec<Manifest>) -> Vec<String> {
    resolve(manifests, &|_, _, _| Ok(())).unwrap().passes.into_iter().map(|p| p.id).collect()
}

/// An outside library with one pass that writes `write_assignments`, in the phase `behavior`.
fn outside(pass: &str) -> Manifest {
    toml::from_str(&format!(
        "id = \"acme.assign\"\nversion = \"1.0.0\"\nabi = \"0.2\"\nlibrary = \"acme_assign\"\nschema_contract = \"{}\"\nlanguages = []\ndatabases = []\n\
         [[passes]]\nid = \"{pass}\"\nphase = \"behavior\"\neffects = [\"behavior.write_assignments.acme\"]\n",
        monorm::extension::SCHEMA_CONTRACT_VERSION
    )).unwrap()
}

// The order that the manifests gave before the phases `expansion` and `late_behavior`, when `after` crossed manifests (measured at eb1e5f25).
const OLD_ORDER: [&str; 17] = [
    "monorm.inherit.expand", "monorm.locking.fields", "monorm.soft-delete.fields", "monorm.timestamps.fields",
    "monorm.composition.lower", "monorm.proxy.lower", "monorm.history.models",
    "monorm.file-storage.fields", "monorm.generic.prepare", "monorm.locking.prepare", "monorm.proto.fields", "monorm.scope.prepare", "monorm.timestamps.prepare",
    "monorm.history.triggers", "monorm.soft-delete.prepare", "query.defaults.lower", "monorm.soft-delete.check",
];

#[test]
fn the_first_party_passes_run_in_the_order_they_had_with_cross_manifest_after() {
    assert_eq!(order(first_party()), OLD_ORDER);
}

#[test]
fn no_first_party_pass_names_a_pass_of_another_manifest_in_after() {
    for m in first_party() {
        for p in &m.passes {
            for id in &p.after { assert!(m.passes.iter().any(|own| own.id == *id), "{} lists {id}", p.id); }
        }
    }
}

// The hole that `after` left: a pass that sorts after `monorm.soft-delete.prepare` by id ran after it, and soft delete then missed its assignments.
#[test]
fn an_outside_pass_in_the_behavior_phase_runs_before_soft_delete_prepare() {
    for id in ["acme.assign.prepare", "zzz.assign.prepare"] {
        let mut manifests = first_party();
        manifests.push(outside(id));
        let ordered = order(manifests);
        let at = |name: &str| ordered.iter().position(|p| p == name).unwrap_or_else(|| panic!("{name} is not in {ordered:?}"));
        assert!(at(id) < at("monorm.soft-delete.prepare"), "{id} runs after soft-delete.prepare: {ordered:?}");
        assert!(at(id) < at("monorm.history.triggers"), "{id} runs after history.triggers: {ordered:?}");
    }
}
