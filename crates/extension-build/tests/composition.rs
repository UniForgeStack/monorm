use orm_contracts::extension::*;
use orm_extension_build::Composition;
use std::collections::BTreeMap;

fn manifest(id: &str, pass: &str, after: &[&str], effects: &[&str]) -> Manifest {
    Manifest {
        id: id.into(), version: "1.0.0".into(), host_contract: Some(HOST_CONTRACT_VERSION), abi: None, library: None,
        schema_contract: SCHEMA_CONTRACT_VERSION, language_contract: None, python: None, typescript: None, dependencies: BTreeMap::new(),
        capabilities: vec![], requires: vec![], languages: vec!["python".into(), "typescript".into()],
        databases: vec!["postgres".into(), "sqlite".into()], attributes: vec![], exports: vec![],
        passes: vec![Pass { id: pass.into(), phase: Phase::Logical, rust: Some("extension::lower".into()),
            after: after.iter().map(|s| (*s).into()).collect(),
            effects: effects.iter().map(|s| (*s).into()).collect() }],
    }
}

#[test]
fn reproducible_order_and_direct_composition() {
    let a = manifest("a", "a.lower", &[], &["User.name"]);
    let b = manifest("b", "b.lower", &["a.lower"], &["User.age"]);
    let x = Composition::resolve(vec![b.clone(), a.clone()]).unwrap();
    let y = Composition::resolve(vec![a, b]).unwrap();
    assert_eq!(x.pass_ids(), vec!["a.lower", "b.lower"]);
    assert_eq!(x.source().unwrap(), y.source().unwrap());
    assert!(x.source().unwrap().contains("extension::lower(ir)?"));
}

#[test]
fn invalid_combinations_fail_before_build() {
    let a = manifest("a", "a.lower", &[], &["User.name"]);
    let b = manifest("b", "b.lower", &[], &["User.name"]);
    let err = Composition::resolve(vec![a.clone(), b]).unwrap_err();
    assert!(err.contains("a.lower") && err.contains("b.lower") && err.contains("User.name"));
    let cycle_a = manifest("a", "a.lower", &["b.lower"], &[]);
    let cycle_b = manifest("b", "b.lower", &["a.lower"], &[]);
    assert!(Composition::resolve(vec![cycle_a, cycle_b]).unwrap_err().contains("cycle"));
    let mut incompatible = a;
    incompatible.dependencies.insert("missing".into(), "^1".into());
    assert!(Composition::resolve(vec![incompatible]).unwrap_err().contains("missing"));
}

#[test]
fn an_after_pass_that_is_not_selected_orders_nothing() {
    let alone = manifest("c", "c.lower", &["missing.lower"], &[]);
    assert_eq!(Composition::resolve(vec![alone.clone()]).unwrap().pass_ids(), vec!["c.lower"]);
    let first = manifest("a", "a.lower", &[], &[]);
    let late = manifest("b", "b.lower", &["missing.lower", "a.lower"], &[]);
    assert_eq!(Composition::resolve(vec![late, first, alone]).unwrap().pass_ids(), vec!["a.lower", "b.lower", "c.lower"]);
}

#[test]
fn attribute_ownership_and_phase_boundaries_are_checked() {
    let mut a = manifest("a", "a.lower", &[], &[]);
    a.attributes.push(Attribute { name: "a.rename".into(), target: AttributeTarget::Field,
        arguments: BTreeMap::new(), positional: vec![], field_type: None, member: true, identities: false, view_source: false });
    let mut b = manifest("b", "b.lower", &[], &[]);
    b.attributes = a.attributes.clone();
    assert!(Composition::resolve(vec![a.clone(), b]).unwrap_err().contains("a.rename"));
    let mut both = a.clone();
    both.attributes.push(Attribute { name: "a.rename".into(), target: AttributeTarget::Model,
        arguments: BTreeMap::new(), positional: vec![], field_type: None, member: true, identities: false, view_source: false });
    assert!(Composition::resolve(vec![both.clone()]).is_ok());
    both.attributes.push(both.attributes[1].clone());
    assert!(Composition::resolve(vec![both]).unwrap_err().contains("a.rename"));
    let mut late = manifest("late", "late.lower", &[], &[]);
    late.passes[0].phase = Phase::Generation;
    a.passes[0].after.push("late.lower".into());
    assert!(Composition::resolve(vec![a, late]).unwrap_err().contains("phase"));
}

#[test]
fn unavailable_host_capabilities_and_overlapping_effects_are_rejected() {
    let mut a = manifest("a", "a.lower", &[], &["User.name"]);
    a.capabilities.push("unimplemented-execution-primitive".into());
    assert!(Composition::resolve(vec![a]).unwrap_err().contains("capability"));
    let a = manifest("a", "a.lower", &[], &["User.name"]);
    let b = manifest("b", "b.lower", &[], &["User.name.type"]);
    assert!(Composition::resolve(vec![a,b]).unwrap_err().contains("conflicting"));
}

#[test]
fn lowering_keeps_a_physical_snapshot_for_logical_field_changes() {
    let a = manifest("a", "a.lower", &[], &["User.name"]);
    let source = Composition::resolve(vec![a]).unwrap().source().unwrap();
    assert!(source.contains("capture_storage(ir)?"));
}

#[test]
fn python_keyword_methods_fail_before_artifact_generation() {
    let mut schema = serde_json::from_value(serde_json::json!({"models":[{"name":"User","table":"users","fields":[{"name":"id","column":"id","type":"int","primary_key":true}]}]})).unwrap();
    for name in ["class", "async", "None", "__debug__"] {
        let specs: Vec<orm_extension_build::native::NativeSpec> = serde_json::from_value(serde_json::json!([{"model":"User","methods":[{"name":name,"export":"example.rule"}]}])).unwrap();
        let error = orm_extension_build::native::generate(&mut schema, &specs, &[]).err().unwrap();
        assert!(error.contains("Python keyword"), "{error}");
    }
}

#[test]
fn a_language_section_needs_the_contract_and_its_language() {
    let section = || Some(orm_contracts::extension::LanguageSection { source: "python/x".into(), query_set: "Q".into(), instance: "I".into(), bound_query_set: String::new(), overrides: vec![] });
    let mut m = manifest("a", "a.lower", &[], &[]);
    m.python = section();
    assert!(Composition::resolve(vec![m.clone()]).unwrap_err().contains("language contract"));
    m.language_contract = Some(orm_contracts::extension::LANGUAGE_CONTRACT);
    assert!(Composition::resolve(vec![m.clone()]).is_ok());
    m.languages = vec!["typescript".into()];
    assert!(Composition::resolve(vec![m]).unwrap_err().contains("requires python in languages"));
}

#[test]
fn manifest_requires_reach_the_artifact_by_extension() {
    let mut a = manifest("a", "a.lower", &[], &["User.name"]);
    a.requires.push(ASYNC_FIELD_ADAPTER_V1.into());
    let b = manifest("b", "b.lower", &["a.lower"], &["User.age"]);
    let artifact = serde_json::to_value(Composition::resolve(vec![a, b]).unwrap().artifact(None)).unwrap();
    assert_eq!(artifact["requires"], serde_json::json!({"a": ["async-field-adapter.v1"]}));
}

#[test]
fn an_artifact_without_requires_keeps_its_old_shape() {
    let artifact = serde_json::to_value(Composition::resolve(vec![manifest("a", "a.lower", &[], &[])]).unwrap().artifact(None)).unwrap();
    assert!(artifact.get("requires").is_none());
}
