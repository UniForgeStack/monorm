//! Neutral write records resolve to field positions on the prepared model.
use orm_core::{behavior::SCHEMA_CONTRACT, ir::Expr, schema::Schema, writes::WriteKind};
use serde_json::{json, Value};

fn prepare(records: Value) -> Result<Schema, String> {
    let mut behavior = json!({"schema_contract": SCHEMA_CONTRACT});
    behavior.as_object_mut().unwrap().extend(records.as_object().unwrap().clone());
    let ir = json!({"models": [{"name": "Post", "table": "posts", "fields": [
        {"name": "id", "column": "id", "type": "int", "primary_key": true},
        {"name": "changed", "column": "changed_at", "type": "date_time"},
        {"name": "removed", "column": "removed_at", "type": "date_time", "nullable": true},
        {"name": "rev", "column": "rev", "type": "int"},
        {"name": "tags", "column": "tags", "type": "string", "array": true},
        {"name": "title", "column": "title", "type": "string"}]}], "behavior": behavior});
    Schema::from_ir(serde_json::from_value(ir).map_err(|e| e.to_string())?)
}

fn assign(field: &str, value: &str, on: Value) -> Value {
    json!({"source": "ext-a", "model": "Post", "field": field, "on": on, "value": value})
}

#[test]
fn records_resolve_to_positions_and_defaults() {
    let schema = prepare(json!({
        "write_assignments": [assign("changed", "now", json!(["update", "upsert"])), assign("rev", "increment", json!(["update", "delete_rewrite"]))],
        "delete_rewrites": [{"source": "ext-b", "model": "Post", "field": "removed", "guard": {"t": "is_null", "item": {"t": "col", "path": [], "name": "removed"}, "neg": false}}],
        "instance_guards": [{"source": "ext-a", "model": "Post", "field": "rev"}, {"source": "ext-c", "model": "Post", "field": "rev"}],
    })).unwrap();
    let m = &schema.models[0];
    let positions: Vec<_> = m.assignments.iter().map(|a| (m.fields()[a.field].name.as_str(), a.applies_to(WriteKind::Update), a.applies_to(WriteKind::DeleteRewrite), a.upsert)).collect();
    assert_eq!(positions, [("changed", true, false, Default::default()), ("rev", true, true, Default::default())]);
    let rewrite = m.delete_rewrite.as_ref().unwrap();
    assert_eq!((m.fields()[rewrite.field].name.as_str(), rewrite.source.as_str()), ("removed", "ext-b"));
    assert!(matches!(rewrite.guard, Expr::IsNull { .. }));
    assert_eq!(m.guards, [3]);
}

#[test]
fn a_model_without_records_has_none() {
    let m = &prepare(json!({})).unwrap().models[0];
    assert!(m.assignments.is_empty() && m.delete_rewrite.is_none() && m.guards.is_empty());
}

#[test]
fn neutral_rules_reject_bad_records() {
    let err = |records: Value| prepare(records).err().unwrap();
    assert!(err(json!({"write_assignments": [assign("title", "now", json!(["update"]))]})).contains("Post.title: wrong type for a time assignment"));
    assert!(err(json!({"write_assignments": [assign("changed", "increment", json!(["update"]))]})).contains("wrong type for an increment assignment"));
    assert!(err(json!({"write_assignments": [assign("tags", "increment", json!(["update"]))]})).contains("wrong type"));
    assert!(err(json!({"write_assignments": [assign("nope", "now", json!(["update"]))]})).contains("nope"));
    assert!(err(json!({"write_assignments": [assign("changed", "now", json!([]))]})).contains("names no statement kind"));
    assert!(err(json!({"write_assignments": [assign("changed", "now", json!(["update"])), assign("changed", "now", json!(["upsert"]))]})).contains("assigned by two write assignments"));
    let rewrite = |source: &str, field: &str| json!({"source": source, "model": "Post", "field": field, "guard": {"t": "is_null", "item": {"t": "col", "path": [], "name": "removed"}, "neg": false}});
    assert!(err(json!({"delete_rewrites": [rewrite("ext-a", "title")]})).contains("wrong type for a delete rewrite"));
    let two = err(json!({"delete_rewrites": [rewrite("ext-a", "removed"), rewrite("ext-b", "removed")]}));
    assert!(two.contains("ext-a") && two.contains("ext-b") && two.contains("one per model"), "{two}");
    let mut bad_guard = rewrite("ext-a", "removed");
    bad_guard["guard"]["item"]["name"] = "nope".into();
    assert!(err(json!({"delete_rewrites": [bad_guard]})).contains("nope"));
    assert!(err(json!({"instance_guards": [{"source": "a", "model": "Missing", "field": "rev"}]})).contains("unknown model Missing"));
}

#[cfg(feature = "composition")]
#[test]
fn a_composed_chain_rejects_every_write_record() {
    let physical = json!([
        {"name": "Parent", "table": "parents", "fields": [{"name": "id", "column": "id", "type": "int", "primary_key": true}, {"name": "changed", "column": "changed_at", "type": "date_time"}]},
        {"name": "Child", "table": "children", "fields": [{"name": "id", "column": "id", "type": "int", "primary_key": true}],
         "relations": [{"name": "parent", "kind": "one", "target": "Parent", "from": "id", "to": "id", "foreign_key": true}]}
    ]);
    let mut logical = physical.clone();
    logical[1]["fields"].as_array_mut().unwrap().push(json!({"name": "changed", "column": "changed_at", "type": "date_time"}));
    let build = |records: Value| {
        let mut behavior = json!({"schema_contract": SCHEMA_CONTRACT, "storage": {"models": physical},
            "field_storage": [{"model": "Child", "field": "changed", "owner": "Parent", "column": "changed_at"}],
            "owner_links": [{"child": "Child", "parent": "Parent", "child_key": "id", "parent_key": "id"}]});
        behavior.as_object_mut().unwrap().extend(records.as_object().unwrap().clone());
        Schema::from_ir(serde_json::from_value(json!({"models": logical, "behavior": behavior})).unwrap())
    };
    assert!(build(json!({})).is_ok());
    let mut parent = assign("changed", "now", json!(["update"]));
    parent["model"] = "Parent".into();
    let err = build(json!({"write_assignments": [parent]})).err().unwrap();
    assert!(err.contains("write record from ext-a on Parent.changed is not supported in a composed model's ownership chain"), "{err}");
}
