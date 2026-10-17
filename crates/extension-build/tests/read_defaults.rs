use orm_contracts::ir::SchemaIr;
use orm_extension_build::native::{generate, NativeSpec};
use serde_json::{json, Value};

fn fingerprint(defaults: Value) -> String {
    let mut schema: SchemaIr = serde_json::from_value(json!({"models":[
        {"name":"User","table":"users","fields":[{"name":"id","column":"id","type":"int","primary_key":true}]},
        {"name":"Other","table":"others","fields":[{"name":"id","column":"id","type":"int","primary_key":true}]}
    ],"behavior":{"read_defaults":defaults}})).unwrap();
    let spec: NativeSpec = serde_json::from_value(json!({"model":"User"})).unwrap();
    generate(&mut schema, &[spec], &[]).unwrap().specializations[0].fingerprint.clone()
}

#[test]
fn native_fingerprint_tracks_every_neutral_default_part_for_its_model() {
    let base = json!({"source":"custom-extension","name":"custom","model":"User"});
    let original = fingerprint(json!([base.clone()]));
    for (key, value) in [
        ("source", json!("another-extension")), ("name", json!("another-name")),
        ("filter", json!({"t":"const","value":false})),
        ("fields", json!(["id"])), ("order", json!([{"field":"id"}])),
        ("load", json!([["owner"]])), ("annotate", json!({"label":{"t":"int","value":1}})),
    ] {
        let mut changed = base.clone(); changed[key] = value;
        assert_ne!(original, fingerprint(json!([changed])), "{key} must affect the specialization");
    }
    assert_eq!(original, fingerprint(json!([base, {"source":"custom-extension","name":"custom","model":"Other","fields":["id"]}])));
}
