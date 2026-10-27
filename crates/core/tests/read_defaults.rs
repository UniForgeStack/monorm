//! The neutral `read_defaults` slot, built by hand: the merge and validation rules need no extension.
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
fn count_of(rel: &str) -> Value { json!({"t":"func","name":"count","rel":[rel]}) }
fn entry(source: &str, name: &str, extra: Value) -> Value {
    let mut d = json!({"source":source,"name":name,"model":"Item"});
    d.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone()); d
}

#[test]
fn neutral_slot_merges_filters_paths_and_preserves_owners() {
    let schema = prepare(json!([
        entry("tenant-ext", "tenant", json!({"filter":{"t":"col","path":[],"name":"active"},"load":[["owner"]],"fields":["id"]})),
        entry("visibility-ext", "published", json!({"filter":{"t":"cmp","op":"gt","l":{"t":"col","path":[],"name":"id"},"r":{"t":"int","value":0}},"load":[["owner"],["other_owner"]],"order":[{"field":"id"}],"annotate":{"label":count_of("owner")}}))
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
    for extra in [json!({"order":[]}), json!({"fields":[]}), json!({"annotate":{"label":count_of("owner")}})] {
        let error = prepare(json!([entry("first-ext","first",extra.clone()),entry("second-ext","second",extra)])).err().unwrap();
        for text in ["first-ext", "first", "second-ext", "second"] { assert!(error.contains(text), "{error}"); }
    }
    let error = prepare(json!([entry("first-ext","same",json!({})),entry("second-ext","same",json!({}))])).err().unwrap();
    for text in ["first-ext", "second-ext", "same", "default name"] { assert!(error.contains(text), "{error}"); }
    let error = prepare(json!([entry("first-ext","query",json!({})),entry("second-ext","query",json!({}))])).err().unwrap();
    for text in ["first-ext", "second-ext", "query", "default name"] { assert!(error.contains(text), "{error}"); }
}

#[test]
fn a_default_on_an_unknown_model_is_rejected() {
    let error = prepare(json!([entry("ext", "name", json!({"model":"Nope"}))])).err().unwrap();
    assert!(error.contains("unknown read-default model Nope"), "{error}");
}

#[test]
fn neutral_defaults_validate_once_without_the_extension() {
    for extra in [
        json!({"filter":{"t":"col","path":[],"name":"missing"}}),
        json!({"fields":["missing"]}), json!({"fields":["id","id"]}),
        json!({"order":[{"field":"missing"}]}), json!({"load":[[]]}),
        json!({"load":[["missing"]]}), json!({"annotate":{"bad":{"t":"invalid"}}})
    ] { assert!(prepare(json!([entry("other-ext","other",extra)])).is_err()); }
    for (source, name) in [("", "other"), ("other-ext", "")] {
        assert!(prepare(json!([entry(source, name, json!({}))])).is_err());
    }
    // Core reserves no name: "query" is the query-defaults convention, and the first entry wins.
    prepare(json!([entry("other-ext", "query", json!({}))])).unwrap();
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
    assert_eq!(SCHEMA_CONTRACT, 3);
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

#[test]
fn generated_code_types_the_default_names() {
    use orm_core::codegen::{python, typescript, Options};
    let value = raw(json!([
        entry("tenant-ext", "tenant", json!({"filter":{"t":"col","path":[],"name":"active"}})),
        entry("visibility-ext", "published", json!({}))
    ]));
    let ir: SchemaIr = serde_json::from_value(value.clone()).unwrap();
    let schema = Schema::from_ir(serde_json::from_value(value).unwrap()).unwrap();
    let options = Options::default();
    let py = python::generate_with(&ir, &schema, "schema.prisma", &options).unwrap();
    assert!(py.stub.contains(r#"def without_defaults(self, *names: Literal["tenant", "published"]) -> Self: ...  # type: ignore[override]"#), "{}", py.stub);
    assert_eq!(py.stub.matches("def without_defaults").count(), 1, "Owner has no defaults");
    let ts = typescript::generate_with(&ir, &schema, "schema.prisma", "orm", &options).unwrap();
    assert_eq!(ts.matches(r#"readonly defaults: "tenant" | "published";"#).count(), 1, "{ts}");
}

#[test]
fn annotation_expressions_validate_at_prepare() {
    let sum = |arg: Value| json!({"t":"func","name":"sum","rel":["owner"],"args":[arg]});
    prepare(json!([entry("ext", "ok", json!({"annotate":{
        "n": sum(json!({"t":"col","path":["owner"],"name":"id"})),
        "m": json!({"t":"func","name":"count","rel":["owner"],"filter":{"t":"cmp","op":"gt","l":{"t":"col","path":[],"name":"id"},"r":{"t":"scope","name":"floor"}}})
    }}))])).unwrap();
    for (bad, text) in [
        (sum(json!({"t":"col","path":["owner"],"name":"missing"})), "missing"),
        (sum(json!({"t":"col","path":[],"name":"missing"})), "missing"),
        (sum(json!({"t":"col","path":["nope"],"name":"id"})), "nope"),
        (json!({"t":"func","name":"count","rel":["nope"]}), "nope"),
        (sum(json!({"t":"exists","select":{"model":"Owner"}})), "default annotations support"),
    ] {
        let error = prepare(json!([entry("ext", "bad", json!({"annotate":{"n":bad}}))])).err().expect("rejected");
        assert!(error.contains(text), "{error}");
    }
}

#[test]
fn annotation_names_clash_with_model_fields() {
    let error = prepare(json!([entry("ext", "labels", json!({"annotate":{"active":count_of("owner")}}))])).err().unwrap();
    for text in ["Item", "ext", "labels", "active", "model field"] { assert!(error.contains(text), "{error}"); }
}

#[test]
fn annotation_must_be_a_relation_aggregate() {
    for expr in [json!({"t":"col","path":[],"name":"id"}), json!({"t":"int","value":1}),
        json!({"t":"arith","op":"add","l":{"t":"col","path":[],"name":"id"},"r":{"t":"int","value":10}}),
        json!({"t":"func","name":"lower","args":[{"t":"text","value":"x"}]})] {
        let error = prepare(json!([entry("ext", "labels", json!({"annotate":{"n":expr}}))])).err().unwrap();
        for text in ["ext", "labels", "\"n\"", "relation aggregate"] { assert!(error.contains(text), "{error}"); }
    }
}
