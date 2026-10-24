//! A proxy takes the native spec of its nearest ancestor unless it has its own.
use orm_contracts::{extension::{Export, ExportKind, Manifest, HOST_CONTRACT_VERSION, SCHEMA_CONTRACT_VERSION}, ir::{ColType, SchemaIr}};
use orm_extension_build::native::{generate, NativeSpec};
use serde_json::{json, Value};
use std::collections::BTreeMap;

fn manifest() -> Manifest {
    let export = |id: &str, kind, output| Export { id: id.into(), rust: format!("app::{}", id.replace('.', "_")), kind, input: ColType::String, output };
    Manifest {
        id: "app".into(), version: "1.0.0".into(), host_contract: Some(HOST_CONTRACT_VERSION), abi: None, library: None, schema_contract: SCHEMA_CONTRACT_VERSION,
        language_contract: None, python: None, typescript: None, dependencies: BTreeMap::new(), capabilities: vec![], requires: vec![],
        languages: vec!["python".into(), "typescript".into()], databases: vec!["postgres".into(), "sqlite".into()],
        attributes: vec![], passes: vec![],
        exports: vec![export("app.check", ExportKind::StringValidator, None), export("app.shout", ExportKind::StringComputed, Some(ColType::String))],
    }
}

fn field(name: &str) -> Value { json!({"name":name,"column":name,"type":"string"}) }
fn model(name: &str, fields: &[&str]) -> Value {
    let mut all = vec![json!({"name":"id","column":"id","type":"int","primary_key":true})];
    all.extend(fields.iter().map(|f| field(f)));
    json!({"name":name,"table":"users","fields":all})
}
fn schema() -> SchemaIr {
    serde_json::from_value(json!({"models":[
        model("User", &["name", "note"]), model("Admin", &["name", "note"]), model("Slim", &["note"]), model("Own", &["name", "note"]), model("Deep", &["name", "note"]),
    ],"behavior":{"proxy_models":[
        {"model":"Admin","parent":"User"}, {"model":"Slim","parent":"User"}, {"model":"Own","parent":"User"}, {"model":"Deep","parent":"Admin"},
    ]}})).unwrap()
}
fn spec(value: Value) -> NativeSpec { serde_json::from_value(value).unwrap() }
fn user() -> NativeSpec {
    spec(json!({"model":"User","fields":[{"field":"name","validators":["app.check"]}],"computed":[{"field":"loud","dependency":"name","export":"app.shout"}],
        "methods":[{"name":"check_name","export":"app.check"}]}))
}
fn models_of(schema: &SchemaIr) -> BTreeMap<String, usize> { schema.models.iter().map(|m| (m.name.clone(), m.fields.len())).collect() }

#[test]
fn a_proxy_takes_the_methods_results_and_specialization_of_its_root() {
    let mut schema = schema();
    let sources = generate(&mut schema, &[user()], &[manifest()]).unwrap();
    let covered: Vec<_> = sources.specializations.iter().map(|s| s.model.as_str()).collect();
    assert_eq!(covered, ["Admin", "Deep", "Own", "Slim", "User"]);
    let computed = |name: &str| schema.models.iter().find(|m| m.name == name).unwrap().fields.iter().any(|f| f.name == "loud");
    assert!(computed("User") && computed("Admin") && computed("Deep") && computed("Own"));
    for model in ["Admin", "Deep", "Own"] {
        assert!(sources.core.contains(&format!("{model:?} =>")), "{model} must be bound");
    }
    // Both languages get one method adapter per model that has the method.
    assert_eq!(sources.python.matches("#[pyfunction]").count(), 5);
    assert_eq!(sources.node.matches("#[napi(").count(), 5);
}

#[test]
fn a_proxy_leaves_out_a_rule_whose_field_it_does_not_keep() {
    let mut schema = schema();
    let sources = generate(&mut schema, &[user()], &[manifest()]).unwrap();
    let slim = schema.models.iter().find(|m| m.name == "Slim").unwrap();
    assert!(slim.fields.iter().all(|f| f.name != "loud"), "the computed field depends on the excluded `name`");
    assert!(sources.specializations.iter().any(|s| s.model == "Slim"), "the methods still reach Slim");
}

#[test]
fn a_proxy_with_its_own_spec_still_inherits_its_root() {
    let mut schema = schema();
    let own = spec(json!({"model":"Own","methods":[{"name":"own_check","export":"app.check"}]}));
    let sources = generate(&mut schema, &[user(), own], &[manifest()]).unwrap();
    let own_model = schema.models.iter().find(|m| m.name == "Own").unwrap();
    assert!(own_model.fields.iter().any(|f| f.name == "loud"), "Own takes the computed field of User");
    // Sorted models: Admin S0, Deep S1, Own S2, Slim S3, User S4; `name` is field 1.
    assert!(sources.engine.contains("(NativeModel::S2, 1) => field_s2_1"), "Own takes the `name` validator of User");
    let own_spec = sources.specializations.iter().find(|s| s.model == "Own").unwrap();
    assert_eq!(own_spec.exports, ["app.check", "app.shout"]);
    assert!(sources.python.contains("orm_native_method_"));
    assert_eq!(sources.python.matches("#[pyfunction]").count(), 6, "Own has check_name and own_check");
}

#[test]
fn an_own_entry_replaces_the_inherited_one_of_the_same_name() {
    let mut schema = schema();
    let own = spec(json!({"model":"Own","fields":[{"field":"name"}],"methods":[{"name":"check_name","export":"app.shout"}]}));
    let sources = generate(&mut schema, &[user(), own], &[manifest()]).unwrap();
    let own_spec = sources.specializations.iter().find(|s| s.model == "Own").unwrap();
    assert_eq!(own_spec.exports, ["app.shout"], "the own method and the own `name` rule replace the inherited ones");
    assert_eq!(sources.python.matches("#[pyfunction]").count(), 5, "Own still has one `check_name`");
}

#[test]
fn a_root_without_a_spec_gives_its_proxies_nothing() {
    let mut schema = schema();
    let before = models_of(&schema);
    let sources = generate(&mut schema, &[], &[manifest()]).unwrap();
    assert!(sources.specializations.is_empty());
    assert_eq!(before, models_of(&schema));
}

#[test]
fn a_proxy_that_drops_a_field_of_an_inherited_record_rule_fails_the_build() {
    let record = json!({"model":"User","records":[{"dependencies":["name"],"export":"app.check_record"}]});
    let mut manifest = manifest();
    let mut export = manifest.exports[0].clone();
    export.id = "app.check_record".into();
    export.kind = ExportKind::StringRecordValidator;
    manifest.exports.push(export);
    let error = generate(&mut schema(), &[spec(record.clone())], &[manifest.clone()]).err().expect("Slim drops `name`");
    for part in ["Slim", "app.check_record", "name", "keep name", "own record rule"] {
        assert!(error.contains(part), "{part} is missing from: {error}");
    }
    let own = spec(json!({"model":"Slim","records":[{"dependencies":["note"],"export":"app.check_record"}]}));
    generate(&mut schema(), &[spec(record), own.clone()], &[manifest.clone()]).expect("an own record rule of the same export is the way out");

    let mut other = manifest.exports[2].clone();
    other.id = "app.other_record".into();
    manifest.exports.push(other);
    let both = spec(json!({"model":"User","records":[
        {"dependencies":["name"],"export":"app.check_record"}, {"dependencies":["name"],"export":"app.other_record"}]}));
    let error = generate(&mut schema(), &[both, own], &[manifest]).err().expect("an own rule of another export does not replace app.other_record");
    assert!(error.contains("app.other_record"), "{error}");
}
