//! Neutral write records resolve to field positions on the prepared model.
use monorm_core::{behavior::SCHEMA_CONTRACT, ir::Expr, schema::Schema, writes::WriteKind};
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
    let mut missing = assign("changed", "now", json!(["update"]));
    missing["model"] = "Missing".into();
    assert!(err(json!({"write_assignments": [missing]})).contains("unknown model Missing"));
    let mut missing = rewrite("ext-a", "removed");
    missing["model"] = "Missing".into();
    assert!(err(json!({"delete_rewrites": [missing]})).contains("unknown model Missing"));
    assert!(err(json!({"delete_rewrites": [rewrite("ext-a", "nope")]})).contains("nope"));
}

fn composed(records: Value) -> Result<Schema, String> {
    let physical = json!([
        {"name": "Parent", "table": "parents", "fields": [{"name": "id", "column": "id", "type": "int", "primary_key": true}, {"name": "changed", "column": "changed_at", "type": "date_time"}, {"name": "removed", "column": "removed_at", "type": "date_time", "nullable": true}, {"name": "rev", "column": "rev", "type": "int"}]},
        {"name": "Child", "table": "children", "fields": [{"name": "id", "column": "id", "type": "int", "primary_key": true}],
         "relations": [{"name": "parent", "kind": "one", "target": "Parent", "from": "id", "to": "id", "foreign_key": true}]}
    ]);
    let mut logical = physical.clone();
    // The logical child lists its own field first, so the inherited positions differ from the root's.
    let inherited: Vec<Value> = ["changed", "removed", "rev"].iter().map(|n| physical[0]["fields"].as_array().unwrap().iter().find(|f| f["name"] == *n).unwrap().clone()).collect();
    logical[1]["fields"].as_array_mut().unwrap().extend(inherited.into_iter().rev());
    let field_storage: Vec<Value> = ["changed", "removed", "rev"].iter().map(|n| json!({"model": "Child", "field": n, "owner": "Parent", "column": physical[0]["fields"].as_array().unwrap().iter().find(|f| f["name"] == *n).unwrap()["column"]})).collect();
    let mut behavior = json!({"schema_contract": SCHEMA_CONTRACT, "storage": {"models": physical}, "field_storage": field_storage,
        "owner_links": [{"child": "Child", "parent": "Parent", "child_key": "id", "parent_key": "id"}]});
    behavior.as_object_mut().unwrap().extend(records.as_object().unwrap().clone());
    Schema::from_ir(serde_json::from_value(json!({"models": logical, "behavior": behavior})).unwrap())
}

fn on(model: &str, mut record: Value) -> Value {
    record["model"] = model.into();
    record
}

fn rewrite(model: &str) -> Value {
    json!({"source": "ext-b", "model": model, "field": "removed", "guard": {"t": "is_null", "item": {"t": "col", "path": [], "name": "removed"}, "neg": false}})
}

#[test]
fn a_composed_chain_holds_the_write_records_of_its_root() {
    let schema = composed(json!({
        "write_assignments": [on("Parent", assign("changed", "now", json!(["update"]))), on("Parent", assign("rev", "increment", json!(["update", "delete_rewrite"])))],
        "delete_rewrites": [rewrite("Parent")],
        "instance_guards": [{"source": "ext-c", "model": "Parent", "field": "rev"}],
    })).unwrap();
    let child = schema.models.iter().find(|m| m.ir.name == "Child").unwrap();
    let names: Vec<_> = child.assignments.iter().map(|a| child.fields()[a.field].name.as_str()).collect();
    assert_eq!(names, ["changed", "rev"]);
    assert_eq!(child.fields()[child.delete_rewrite.as_ref().unwrap().field].name, "removed");
    assert_eq!(child.guards.iter().map(|&p| child.fields()[p].name.as_str()).collect::<Vec<_>>(), ["rev"]);
    let root = schema.models.iter().find(|m| m.ir.name == "Parent").unwrap();
    assert_eq!(root.assignments.len(), 2);
}

#[test]
fn a_write_record_below_the_root_names_the_root() {
    for (records, source) in [
        (json!({"write_assignments": [on("Child", assign("changed", "now", json!(["update"])))]}), "ext-a"),
        (json!({"delete_rewrites": [rewrite("Child")]}), "ext-b"),
        (json!({"instance_guards": [{"source": "ext-c", "model": "Child", "field": "rev"}]}), "ext-c"),
    ] {
        let err = composed(records).err().unwrap();
        assert!(err.contains(&format!("model Child: a write record from {source} on Child.")) && err.contains("below the root") && err.contains("declare it on Parent"), "{err}");
    }
}

#[test]
fn a_write_record_on_the_root_of_a_deep_chain_reaches_every_level() {
    let model = |name: &str, table: &str, parent: Option<(&str, &str)>| {
        let mut m = json!({"name": name, "table": table, "fields": [{"name": "id", "column": "id", "type": "int", "primary_key": true}]});
        if let Some((target, relation)) = parent {
            m["relations"] = json!([{"name": relation, "kind": "one", "target": target, "from": "id", "to": "id", "foreign_key": true}]);
        } else {
            m["fields"].as_array_mut().unwrap().push(json!({"name": "changed", "column": "changed_at", "type": "date_time"}));
        }
        m
    };
    // The leaf comes first, so the walk needs two links to reach the root.
    let physical = json!([model("Child", "children", Some(("Parent", "parent"))), model("Parent", "parents", Some(("Grand", "grand"))), model("Grand", "grands", None)]);
    let mut logical = physical.clone();
    for i in [0, 1] {
        logical[i]["fields"].as_array_mut().unwrap().push(json!({"name": "changed", "column": "changed_at", "type": "date_time"}));
    }
    let build = |records: Value| {
        let mut behavior = json!({"schema_contract": SCHEMA_CONTRACT, "storage": {"models": physical},
            "field_storage": [{"model": "Parent", "field": "changed", "owner": "Grand", "column": "changed_at"}, {"model": "Child", "field": "changed", "owner": "Grand", "column": "changed_at"}],
            "owner_links": [{"child": "Parent", "parent": "Grand", "child_key": "id", "parent_key": "id"}, {"child": "Child", "parent": "Parent", "child_key": "id", "parent_key": "id"}]});
        behavior.as_object_mut().unwrap().extend(records.as_object().unwrap().clone());
        Schema::from_ir(serde_json::from_value(json!({"models": logical, "behavior": behavior})).unwrap())
    };
    assert!(build(json!({})).is_ok());
    let schema = build(json!({"write_assignments": [on("Grand", assign("changed", "now", json!(["update"])))]})).unwrap();
    for name in ["Child", "Parent", "Grand"] {
        let m = schema.models.iter().find(|m| m.ir.name == name).unwrap();
        assert_eq!(m.assignments.len(), 1, "{name}");
    }
    let err = build(json!({"write_assignments": [on("Parent", assign("changed", "now", json!(["update"])))]})).err().unwrap();
    assert!(err.contains("model Parent:") && err.contains("declare it on Grand"), "{err}");
}

#[test]
fn published_scope_keys_are_scope_keys() {
    let none = prepare(json!({})).unwrap();
    assert!(none.scope_keys().is_empty());
    let schema = prepare(json!({"published_scope_keys": [{"source": "ext-a", "key": "user"}]})).unwrap();
    assert_eq!(schema.scope_keys().into_iter().collect::<Vec<_>>(), ["user"]);
    assert_eq!(schema.published_scope_keys().collect::<Vec<_>>(), ["user"]);
}

#[test]
fn an_owning_field_belongs_to_the_root_of_a_composed_model() {
    let build = |owner: &str| {
        let id = json!({"name": "id", "column": "id", "type": "int", "primary_key": true});
        let spec_id = json!({"name": "spec_id", "column": "spec_id", "type": "int"});
        let rev = json!({"name": "rev", "column": "rev", "type": "int"});
        let spec = json!({"name": "Spec", "table": "specs", "fields": [id]});
        let owning = json!({"name": "spec", "kind": "one", "target": "Spec", "from": "spec_id", "to": "id", "foreign_key": true, "owns": true});
        let link = json!({"name": "parent", "kind": "one", "target": "Parent", "from": "id", "to": "id", "foreign_key": true});
        let (parent, child) = if owner == "Parent" { (json!([owning]), json!([link])) } else { (json!([]), json!([link, owning])) };
        let physical = json!([
            {"name": "Parent", "table": "parents", "fields": [id, spec_id, rev], "relations": parent},
            {"name": "Child", "table": "children", "fields": [id, spec_id], "relations": child},
            spec]);
        let mut logical = physical.clone();
        logical[1]["fields"].as_array_mut().unwrap().push(rev.clone());
        let behavior = json!({"schema_contract": SCHEMA_CONTRACT, "schema_contract_minor": 6, "storage": {"models": physical},
            "field_storage": [{"model": "Child", "field": "rev", "owner": "Parent", "column": "rev"}],
            "owner_links": [{"child": "Child", "parent": "Parent", "child_key": "id", "parent_key": "id"}]});
        Schema::from_ir(serde_json::from_value(json!({"models": logical, "behavior": behavior})).unwrap())
    };
    if let Err(e) = build("Parent") { panic!("{e}") }
    let e = build("Child").err().expect("a level below the root can't own");
    assert!(e.contains("relation Child.spec: owns: an owning field belongs to the root of a composed model"), "{e}");
}

#[test]
fn a_write_rule_on_a_proxy_is_refused_and_the_root_keeps_its_rule() {
    let build = |model: &str| {
        let id = json!({"name": "id", "column": "id", "type": "int", "primary_key": true});
        let behavior = json!({"schema_contract": SCHEMA_CONTRACT,
            "model_views": [{"model": "PublicPost", "source": "Post", "root": "Post"}],
            "write_rules": [{"source": "ext-a", "model": model, "name": "protect", "operations": ["update"], "layer": "app"}]});
        let ir = json!({"models": [
            {"name": "Post", "table": "posts", "fields": [id]},
            {"name": "PublicPost", "table": "posts", "fields": [id]}], "behavior": behavior});
        Schema::from_ir(serde_json::from_value(ir).unwrap())
    };
    let error = build("PublicPost").err().expect("a proxy cannot declare a write rule");
    assert!(error.contains("PublicPost: a proxy cannot declare a write rule; protect the root model Post"), "{error}");
    assert!(build("Post").is_ok());
}
