//! Every built-in extension resolves together: the loader selects extensions by the attributes of a schema,
//! so a schema that uses several of them resolves exactly these manifests at once.
use monorm::extension::Manifest;
use monorm::resolve::resolve;
use std::path::Path;

/// The extensions that write the same per-model lists; the ordering test reads their passes.
const LIST_WRITERS: [&str; 4] = ["monorm.history", "monorm.query-defaults", "monorm.scope", "monorm.soft-delete"];

fn manifests() -> Vec<Manifest> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../extensions");
    let mut found: Vec<_> = std::fs::read_dir(&root).expect("extensions directory").filter_map(|e| {
        let path = e.unwrap().path().join("monorm-extension.toml");
        path.exists().then(|| toml::from_str::<Manifest>(&std::fs::read_to_string(&path).unwrap()).unwrap_or_else(|e| panic!("{}: {e}", path.display())))
    }).collect();
    found.sort_by(|a, b| a.id.cmp(&b.id));
    found
}

#[test]
fn all_built_in_extensions_resolve_together() {
    let all = manifests();
    for id in LIST_WRITERS { assert!(all.iter().any(|m| m.id == id), "missing {id}"); }
    let resolved = resolve(all, &|_, _, _| Ok(())).unwrap_or_else(|e| panic!("{e}"));
    let position = |id: &str| resolved.passes.iter().position(|p| p.id == id).unwrap_or_else(|| panic!("{id}"));
    // `after` overrides the id order: `monorm.soft-delete.prepare` sorts before `monorm.timestamps.prepare` by id.
    for pass in ["monorm.locking.prepare", "monorm.timestamps.prepare"] {
        assert!(position(pass) < position("monorm.soft-delete.prepare"), "{pass}");
    }
    // The check sees the read defaults of every pass that adds them.
    for pass in ["monorm.scope.prepare", "monorm.soft-delete.prepare", "query.defaults.lower"] {
        assert!(position(pass) < position("monorm.soft-delete.check"), "{pass}");
    }
}

#[test]
fn every_pair_of_built_in_extensions_resolves() {
    let all = manifests();
    for (i, a) in all.iter().enumerate() {
        for b in &all[i + 1..] {
            resolve(vec![a.clone(), b.clone()], &|_, _, _| Ok(())).unwrap_or_else(|e| panic!("{} + {}: {e}", a.id, b.id));
        }
    }
}
