//! The neutral contract has no public producer for multi-source contributions yet.
use orm_core::{behavior::{merge_definition, Requirements, SCHEMA_CONTRACT}, ir::SchemaIr, schema::Schema};
use serde_json::{json, Value};

fn raw(defaults: Value) -> Value {
    json!({"models":[
        {"name":"Item","table":"item","fields":[
            {"name":"id","column":"id","type":"int","primary_key":true},
            {"name":"active","column":"active","type":"bool"}],
         "relations":[{"name":"owner","kind":"one","target":"Owner","from":"id","to":"id"},{"name":"other_owner","kind":"one","target":"Owner","from":"id","to":"id"}]},
        {"name":"Owner","table":"owner","fields":[{"name":"id","column":"id","type":"int","primary_key":true}]}
    ],"behavior":{"schema_contract":SCHEMA_CONTRACT,"read_defaults":defaults}})
}
fn prepare(defaults: Value) -> Result<Schema, String> {
    Schema::from_ir(serde_json::from_value(raw(defaults)).map_err(|e| e.to_string())?)
}
fn entry(source: &str, name: &str, extra: Value) -> Value {
    let mut d = json!({"source":source,"name":name,"model":"Item"});
    d.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone()); d
}

#[test]
fn neutral_slot_merges_filters_paths_and_preserves_owners() {
    let schema = prepare(json!([
        entry("tenant-ext", "tenant", json!({"filter":{"t":"col","path":[],"name":"active"},"load":[["owner"]],"fields":["id"]})),
        entry("visibility-ext", "published", json!({"filter":{"t":"cmp","op":"gt","l":{"t":"col","path":[],"name":"id"},"r":{"t":"int","value":0}},"load":[["owner"],["other_owner"]],"order":[{"field":"id"}],"annotate":{"label":{"t":"col","path":[],"name":"id"}}}))
    ])).unwrap();
    let m = &schema.models[0];
    let Some(orm_core::ir::Expr::And { items }) = &m.read_defaults.filter else { panic!("merged AND") };
    assert_eq!(items.len(), 2);
    assert!(m.read_flags.filtered);
    assert_eq!(m.read_flags.fields.as_ref().unwrap(), &["id"]);
    assert_eq!(m.read_flags.related, vec![vec!["owner"], vec!["other_owner"]]);
    assert_eq!(m.read_defaults.parts.len(), 2);
    assert_eq!(m.read_defaults.parts[1].identity.source, "visibility-ext");
    assert!(m.read_defaults.parts[1].annotate.contains_key("label"));
    assert_eq!(m.read_defaults.order_owner.as_ref().unwrap().name, "published");
    assert_eq!(m.read_defaults.fields_owner.as_ref().unwrap().name, "tenant");
    assert_eq!(m.read_defaults.load_owners[0].len(), 2);
}

#[test]
fn conflicts_name_both_sources_and_entries_even_for_empty_owned_lists() {
    for extra in [json!({"order":[]}), json!({"fields":[]}), json!({"annotate":{"label":{"t":"int","value":1}}})] {
        let error = prepare(json!([entry("first-ext","first",extra.clone()),entry("second-ext","second",extra)])).err().unwrap();
        for text in ["first-ext", "first", "second-ext", "second"] { assert!(error.contains(text), "{error}"); }
    }
    let error = prepare(json!([entry("first-ext","same",json!({})),entry("second-ext","same",json!({}))])).err().unwrap();
    for text in ["first-ext", "second-ext", "same", "default name"] { assert!(error.contains(text), "{error}"); }
}

#[test]
fn neutral_defaults_validate_once_without_the_extension() {
    for extra in [
        json!({"filter":{"t":"col","path":[],"name":"missing"}}),
        json!({"fields":["missing"]}), json!({"fields":["id","id"]}),
        json!({"order":[{"field":"missing"}]}), json!({"load":[[]]}),
        json!({"load":[["missing"]]}), json!({"annotate":{"bad":{"t":"invalid"}}})
    ] { assert!(prepare(json!([entry("other-ext","other",extra)])).is_err()); }
    for (source, name) in [("", "other"), ("other-ext", ""), ("other-ext", "query")] {
        assert!(prepare(json!([entry(source, name, json!({}))])).is_err());
    }
    // Required-column NULL checks are deliberately exempt from strict-null validation.
    prepare(json!([entry("other-ext","other",json!({"filter":{"t":"is_null","item":{"t":"col","path":[],"name":"id"}}}))])).unwrap();
}

#[test]
fn paths_from_different_entries_are_checked_for_recursive_loading() {
    let mut schema = raw(json!([entry("a","one",json!({"load":[["owner"]]})),
        {"source":"b","name":"two","model":"Owner","load":[["item"]]}]));
    schema["models"][1]["relations"] = json!([{"name":"item","kind":"one","target":"Item","from":"id","to":"id"}]);
    let error = Schema::from_ir(serde_json::from_value(schema).unwrap()).err().unwrap();
    assert!(error.contains("recursive default loading"), "{error}");
}

#[test]
fn neutral_contract_participates_in_empty_merge_and_rebuild_checks() {
    let first: SchemaIr = serde_json::from_value(raw(json!([entry("a","one",json!({}))]))).unwrap();
    assert!(!first.behavior.is_empty());
    assert!(Requirements::default().is_empty());
    let only_defaults: Requirements = serde_json::from_value(json!({"read_defaults":[entry("a","one",json!({}))]})).unwrap();
    assert!(!only_defaults.is_empty());
    assert_eq!(SCHEMA_CONTRACT, 2);
    let mut second = raw(json!([entry("b","two",json!({}))]));
    second["models"] = json!([]);
    let merged = merge_definition(first, serde_json::from_value(second).unwrap()).unwrap();
    assert_eq!(merged.behavior.read_defaults.len(), 2);
    Schema::from_ir(merged).unwrap();
    let mut old = raw(json!([]));
    old["behavior"] = json!({"schema_contract":1});
    let error = Schema::from_ir(serde_json::from_value(old).unwrap()).err().unwrap();
    assert!(error.contains("rebuild"), "{error}");
    let mut old = raw(json!([]));
    old["behavior"] = json!({"schema_contract":1,"query_defaults":[]});
    assert!(serde_json::from_value::<SchemaIr>(old).is_err());
}
