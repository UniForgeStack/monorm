use monorm::ir::SchemaIr;
use serde_json::{json, Value};
fn definition() -> Value {
    json!({"models":[
        {"name":"Manager","table":"manager","fields":[{"name":"level","column":"level","type":"int"}]},
        {"name":"Person","table":"person","fields":[{"name":"id","column":"id","type":"int","primary_key":true,"auto_increment":true},{"name":"name","column":"name","type":"string"}]},
        {"name":"Employee","table":"employee","fields":[{"name":"salary","column":"salary","type":"int"}]},
        {"name":"Customer","table":"customer","fields":[{"name":"points","column":"points","type":"int"}]}
    ],"behavior":{"declarations":[
        {"attribute":"composition.model","model":"Manager","field":null,"arguments":{"parent":"Employee","parentRef":"employee","childRef":"manager"},"positional":[],"location":{"file":"schema","line":1,"column":1}},
        {"attribute":"composition.model","model":"Customer","field":null,"arguments":{"parent":"Person","parentRef":"person","childRef":"customer"},"positional":[],"location":{"file":"schema","line":1,"column":1}},
        {"attribute":"composition.model","model":"Employee","field":null,"arguments":{"parent":"Person","parentRef":"person","childRef":"employee"},"positional":[],"location":{"file":"schema","line":1,"column":1}}
    ]}})
}
/// Declared effect `composition.storage-ownership`: inherited logical fields, the shared
/// key columns of the storage snapshot, and the owners and links that resolve them.
#[test]
fn lowering_changes_only_storage_ownership_once() {
    let ir: SchemaIr = serde_json::from_value(definition()).unwrap();
    let effects = monorm::extension::pass_effects(&ir, monorm_composition::lower).unwrap();
    assert_eq!(effects.into_iter().collect::<Vec<_>>(), ["behavior.field_storage", "behavior.owner_links", "behavior.storage", "models"]);
}
#[test]
fn chains_and_siblings_keep_physical_storage_separate() {
    let mut ir: SchemaIr = serde_json::from_value(definition()).unwrap();
    monorm_composition::lower(&mut ir).unwrap();
    assert_eq!(
        ir.models[0]
            .fields
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>(),
        ["id", "level", "salary", "name"]
    );
    let physical = ir.behavior.storage.as_ref().unwrap();
    assert_eq!(physical.models[0].fields.len(), 2);
    assert!(ir.models.iter().flat_map(|m| &m.fields).chain(physical.models.iter().flat_map(|m| &m.fields)).all(|f| f.hints.keys().all(|k| !k.starts_with("composition."))));
    assert_eq!(physical.models[2].fields.len(), 2);
    assert!(physical.models[1].fields[0].auto_increment);
    assert!(!physical.models[0].fields[0].auto_increment);
    assert_eq!(ir.behavior.owner_links.len(), 3);
    assert_eq!(
        ir.behavior
            .field_storage
            .iter()
            .find(|m| m.model == "Manager" && m.field == "name")
            .unwrap()
            .owner,
        "Person"
    );
    assert_eq!(
        ir.behavior
            .field_storage
            .iter()
            .find(|m| m.model == "Manager" && m.field == "salary")
            .unwrap()
            .owner,
        "Employee"
    );
    assert!(ir.models[1].relations.iter().any(|r| r.name == "employee"));
    assert!(ir.models[1].relations.iter().any(|r| r.name == "customer"));
    assert!(physical.models[2].relations[0].foreign_key);
    for d in &mut ir.behavior.declarations {
        d.lowered = true;
    }
    let before = serde_json::to_value(&ir).unwrap();
    monorm_composition::lower(&mut ir).unwrap();
    assert_eq!(serde_json::to_value(ir).unwrap(), before);
}
#[test]
fn rejected_candidates_are_unchanged() {
    for mutation in 0..8 {
        let mut v = definition();
        match mutation {
            0 => v["behavior"]["declarations"][0]["arguments"]["parent"] = json!("Manager"),
            1 => v["behavior"]["declarations"][0]["arguments"]["parent"] = json!("Missing"),
            2 => v["models"][2]["fields"].as_array_mut().unwrap().push(json!({"name":"name","column":"other","type":"string"})),
            3 => v["models"][2]["fields"].as_array_mut().unwrap().push(json!({"name":"id","column":"id","type":"int","primary_key":true,"auto_increment":true})),
            4 => v["behavior"]["declarations"][2]["arguments"]["parentRef"] = json!("salary"),
            6 => v["models"][2]["fields"].as_array_mut().unwrap().push(json!({"name":"id","column":"id","type":"string","primary_key":true})),
            7 => v["models"][2]["fields"].as_array_mut().unwrap().push(json!({"name":"id","column":"id","type":"int","primary_key":true,"enum":"Role"})),
            _ => v["behavior"]["declarations"][2]["arguments"]["childRef"] = json!("name"),
        }
        let mut ir: SchemaIr = serde_json::from_value(v).unwrap();
        let before = serde_json::to_value(&ir).unwrap();
        assert!(
            monorm_composition::lower(&mut ir).is_err(),
            "mutation {mutation}"
        );
        assert_eq!(serde_json::to_value(ir).unwrap(), before);
    }
}

/// An empty relation name is no member name: the declaration check and the shared member check both refuse it.
#[test]
fn an_empty_relation_name_is_rejected() {
    for key in ["parentRef", "childRef"] {
        let mut v = definition();
        v["behavior"]["declarations"][2]["arguments"][key] = json!("");
        let mut ir: SchemaIr = serde_json::from_value(v).unwrap();
        assert_eq!(monorm_composition::lower(&mut ir).unwrap_err(), format!("Employee: composition requires {key}"));
    }
    let ir: SchemaIr = serde_json::from_value(definition()).unwrap();
    assert_eq!(ir.models[1].check_new_member(""), Err(monorm::ir::NewMemberError::Invalid));
}

/// A name that is both no identifier and a member of the model gets the "invalid" answer.
#[test]
fn an_invalid_name_that_collides_is_invalid() {
    let mut ir: SchemaIr = serde_json::from_value(definition()).unwrap();
    ir.models[1].fields.push(
        serde_json::from_value(json!({"name":"bad name","column":"bad","type":"int"})).unwrap(),
    );
    assert_eq!(
        ir.models[1].check_new_member("bad name"),
        Err(monorm::ir::NewMemberError::Invalid)
    );
    assert_eq!(
        ir.models[1].check_new_member("name"),
        Err(monorm::ir::NewMemberError::Collides)
    );
}

/// A generated column is a server default: a child key that the database generates is not the parent key.
#[test]
fn a_generated_child_key_is_rejected() {
    let key = |generated: Value| {
        let mut v = definition();
        let mut field = json!({"name":"id","column":"id","type":"int","primary_key":true});
        if !generated.is_null() {
            field["generated"] = generated;
        }
        v["models"][2]["fields"].as_array_mut().unwrap().push(field);
        serde_json::from_value::<SchemaIr>(v).unwrap()
    };
    monorm_composition::lower(&mut key(Value::Null)).unwrap();
    let error = monorm_composition::lower(&mut key(json!({"sql":"1","stored":true}))).unwrap_err();
    assert_eq!(
        error,
        "Employee: child primary key must match parent without an independent default"
    );
}

/// A deprecated parent field leaves the logical models and stays in the storage that core captured first.
#[test]
fn a_deprecated_parent_field_keeps_the_physical_storage_and_leaves_the_children() {
    let legacy = json!({"name":"legacy","column":"legacy","type":"string","default":"x"});
    let with_legacy = || {
        let mut value = definition();
        value["models"][1]["fields"].as_array_mut().unwrap().push(legacy.clone());
        serde_json::from_value::<SchemaIr>(value).unwrap()
    };
    let mut plain = with_legacy();
    monorm_composition::lower(&mut plain).unwrap();
    let mut hidden = with_legacy();
    monorm::extension::capture_storage(&mut hidden).unwrap();
    hidden.behavior.schema_contract = monorm::extension::SCHEMA_CONTRACT;
    hidden.models[1].fields.retain(|f| f.name != "legacy");
    monorm_composition::lower(&mut hidden).unwrap();
    assert_eq!(serde_json::to_value(&plain.behavior.storage).unwrap(), serde_json::to_value(&hidden.behavior.storage).unwrap());
    assert!(plain.models.iter().all(|m| m.name == "Person" || m.fields.iter().any(|f| f.name == "legacy")));
    assert!(hidden.models.iter().all(|m| m.fields.iter().all(|f| f.name != "legacy")));
}
