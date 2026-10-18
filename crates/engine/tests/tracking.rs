//! Column roles of the updated-at, soft-delete and optimistic-locking extensions.
use orm_core::{dialect::{Dialect, Target}, ir::{Operation, ValueType}, schema::Schema};
use orm_engine::{exec, plan::Planner, Params, Result};
use sea_query::Value;
use serde_json::{json, Value as Json};

struct Ints(Vec<i32>);
impl Params for Ints {
    fn len(&self) -> usize { self.0.len() }
    fn value(&self, i: usize, _: Option<ValueType>) -> Result<Value> { Ok(Value::Int(Some(self.0[i]))) }
    fn text(&self, _: usize) -> Result<String> { unreachable!() }
    fn count(&self, _: usize) -> Result<u64> { unreachable!() }
}

fn schema(behavior: Json) -> std::result::Result<Schema, String> {
    let mut behavior = behavior;
    behavior["schema_contract"] = json!(2);
    Schema::from_ir(serde_json::from_value(json!({"models":[{"name":"Post","table":"posts","fields":[
        {"name":"id","column":"id","type":"int","primary_key":true},
        {"name":"views","column":"views","type":"int"},
        {"name":"changed","column":"changed_at","type":"date_time"},
        {"name":"deleted","column":"deleted_at","type":"date_time","nullable":true},
        {"name":"version","column":"version","type":"int"}
    ]}],"behavior":behavior})).unwrap())
}

fn sql(schema: &Schema, dialect: Dialect, op: Json) -> String {
    let op: Operation = serde_json::from_value(op).unwrap();
    let target = Target::new(dialect);
    exec::statement(target, &Planner::plan(schema, target, &op, &Ints(vec![7])).unwrap())
}

fn update(set: &str) -> Json {
    json!({"op":"update","model":"Post","set":[{"field":set,"value":{"t":"param","i":0}}],
        "filters":[{"t":"cmp","op":"eq","l":{"t":"col","path":[],"name":"id"},"r":{"t":"param","i":0}}]})
}

#[test]
fn a_disabled_artifact_rejects_each_role_at_definition() {
    for (key, value, feature) in [
        ("updated_at", json!([{"model":"Post","field":"changed"}]), "updated-at"),
        ("soft_delete", json!([{"model":"Post","field":"deleted"}]), "soft-delete"),
        ("versions", json!([{"model":"Post","field":"version"}]), "optimistic-locking"),
    ] {
        let enabled = match feature { "updated-at" => cfg!(feature = "updated-at"), "soft-delete" => cfg!(feature = "soft-delete"), _ => cfg!(feature = "optimistic-locking") };
        let result = schema(json!({key: value}));
        if enabled { assert!(result.is_ok()); } else {
            let error = result.err().unwrap();
            assert!(error.contains(feature) && error.contains("rebuild"), "{error}");
        }
    }
}

#[cfg(feature = "updated-at")]
#[test]
fn an_update_sets_updated_at_unless_it_sets_the_field() {
    let schema = schema(json!({"updated_at":[{"model":"Post","field":"changed","mode":"database"}]})).unwrap();
    for dialect in [Dialect::Postgres, Dialect::Sqlite] {
        let statement = sql(&schema, dialect, update("views"));
        assert!(statement.contains("\"views\" = ") && statement.contains("\"changed_at\" = "), "{statement}");
        let statement = sql(&schema, dialect, update("changed"));
        assert_eq!(statement.matches("\"changed_at\" = ").count(), 1, "{statement}");
    }
    let rows = vec![vec![Value::Int(Some(1)), Value::Int(Some(2))]];
    let (_, plan) = exec::plan_update_many(&schema, Target::new(Dialect::Postgres), "Post", &["id".into(), "views".into()], rows, &[], &Ints(vec![]), false, None, &Default::default()).unwrap();
    assert!(plan.statements[0].0.contains("\"changed_at\" = "), "{}", plan.statements[0].0);
}

#[cfg(feature = "soft-delete")]
#[test]
fn a_delete_of_a_soft_delete_model_is_an_update_of_live_rows() {
    let schema = schema(json!({"soft_delete":[{"model":"Post","field":"deleted"}]})).unwrap();
    let filter = json!([{"t":"cmp","op":"eq","l":{"t":"col","path":[],"name":"id"},"r":{"t":"param","i":0}}]);
    for dialect in [Dialect::Postgres, Dialect::Sqlite] {
        let statement = sql(&schema, dialect, json!({"op":"delete","model":"Post","filters":filter,"returning":true}));
        assert!(statement.starts_with("UPDATE \"posts\" SET \"deleted_at\" = "), "{statement}");
        assert!(statement.contains("AND \"posts\".\"deleted_at\" IS NULL RETURNING"), "{statement}");
        let statement = sql(&schema, dialect, json!({"op":"delete","model":"Post","filters":filter,"hard":true}));
        assert!(statement.starts_with("DELETE FROM \"posts\""), "{statement}");
    }
}

#[cfg(all(feature = "soft-delete", feature = "updated-at", feature = "optimistic-locking"))]
#[test]
fn a_soft_delete_also_maintains_updated_at_and_the_version() {
    let schema = schema(json!({"soft_delete":[{"model":"Post","field":"deleted"}],"updated_at":[{"model":"Post","field":"changed"}],"versions":[{"model":"Post","field":"version"}]})).unwrap();
    let statement = sql(&schema, Dialect::Postgres, json!({"op":"delete","model":"Post"}));
    assert!(statement.contains("\"changed_at\" = ") && statement.contains("\"version\" = \"posts\".\"version\" + "), "{statement}");
}

#[cfg(feature = "optimistic-locking")]
#[test]
fn an_update_increments_the_version_unless_it_sets_it() {
    let schema = schema(json!({"versions":[{"model":"Post","field":"version"}]})).unwrap();
    for dialect in [Dialect::Postgres, Dialect::Sqlite] {
        let statement = sql(&schema, dialect, update("views"));
        assert!(statement.contains("\"version\" = \"posts\".\"version\" + "), "{statement}");
        let statement = sql(&schema, dialect, update("version"));
        assert_eq!(statement.matches("\"version\" = ").count(), 1, "{statement}");
    }
    let rows = vec![vec![Value::Int(Some(1)), Value::Int(Some(2))]];
    let (_, plan) = exec::plan_update_many(&schema, Target::new(Dialect::Sqlite), "Post", &["id".into(), "views".into()], rows, &[], &Ints(vec![]), false, None, &Default::default()).unwrap();
    assert!(plan.statements[0].0.contains("\"version\" = \"posts\".\"version\" + "), "{}", plan.statements[0].0);
}

#[cfg(all(feature = "updated-at", feature = "optimistic-locking"))]
#[test]
fn an_upsert_maintains_the_version_and_updated_at() {
    let schema = schema(json!({"updated_at":[{"model":"Post","field":"changed"}],"versions":[{"model":"Post","field":"version"}]})).unwrap();
    let set = |field: &str| vec![serde_json::from_value(json!({"field":field,"value":{"t":"param","i":0}})).unwrap()];
    let upsert = |dialect, update: &[&str], set| {
        let target = Target::new(dialect);
        let conflict = exec::Conflict::Update { target: vec!["id".into()], filter: None, update: update.iter().map(|f| f.to_string()).collect(), set };
        let fields: Vec<String> = ["id", "views"].iter().map(|f| f.to_string()).collect();
        let rows = vec![vec![Some(Value::Int(Some(1))), Some(Value::Int(Some(2)))]];
        exec::statement(target, &exec::plan_insert(&schema, target, "Post", &fields, rows, Some(conflict), &Ints(vec![7])).unwrap())
    };
    for dialect in [Dialect::Postgres, Dialect::Sqlite] {
        for statement in [upsert(dialect, &["views"], vec![]), upsert(dialect, &[], set("views"))] {
            assert!(statement.contains("\"version\" = \"posts\".\"version\" + "), "{statement}");
            assert!(statement.contains("\"changed_at\" = "), "{statement}");
        }
        let statement = upsert(dialect, &["views", "changed", "version"], vec![]);
        assert_eq!(statement.matches("\"version\" = ").count(), 1, "{statement}");
        assert_eq!(statement.matches("\"changed_at\" = ").count(), 1, "{statement}");
        let statement = upsert(dialect, &[], set("version"));
        assert_eq!(statement.matches("\"version\" = ").count(), 1, "{statement}");
        assert!(!statement.contains("\"posts\".\"version\" + "), "{statement}");
    }
}

#[cfg(feature = "model-composition")]
#[test]
fn a_role_on_a_composed_model_is_rejected_at_definition() {
    let source = |field: &str| format!(r#"
model Person {{
 id Int @id
 @@map("tracking_people")
}}
model Employee {{
 {field}
 @@composition.model(parent: "Person", parentRef: "person", childRef: "employee")
 @@map("tracking_employees")
}}
"#);
    let check = |field: &str| orm_core::dsl::compile(&source(field), None).and_then(orm_core::dsl::check).map(|_| ());
    assert!(check("salary Int").is_ok());
    #[cfg(feature = "updated-at")]
    assert!(check("changed DateTime @default(now()) @timestamps.updated_at").unwrap_err().to_string().contains("Employee: @timestamps.updated_at"));
    #[cfg(feature = "soft-delete")]
    assert!(check("deleted DateTime? @soft_delete.deleted_at").unwrap_err().to_string().contains("Employee: @soft_delete.deleted_at"));
    #[cfg(feature = "optimistic-locking")]
    assert!(check("version Int @default(0) @locking.version").unwrap_err().to_string().contains("Employee: @locking.version"));
}

#[test]
fn a_role_with_a_wrong_target_is_rejected_at_definition() {
    let mut cases: Vec<(&str, Json, &str)> = vec![];
    #[cfg(feature = "updated-at")]
    {
        cases.push(("updated_at", json!([{"model":"Post","field":"views"}]), "wrong type"));
        cases.push(("updated_at", json!([{"model":"Post","field":"tags"}]), "wrong type"));
        cases.push(("updated_at", json!([{"model":"Nope","field":"changed"}]), "unknown model"));
        cases.push(("updated_at", json!([{"model":"Post","field":"nope"}]), "nope"));
    }
    #[cfg(feature = "soft-delete")]
    {
        cases.push(("soft_delete", json!([{"model":"Post","field":"changed"}]), "wrong type"));
        cases.push(("soft_delete", json!([{"model":"Post","field":"tags"}]), "wrong type"));
        cases.push(("soft_delete", json!([{"model":"Post","field":"deleted"},{"model":"Post","field":"deleted"}]), "one soft-delete field only"));
        cases.push(("soft_delete", json!([{"model":"Nope","field":"deleted"}]), "unknown model"));
        cases.push(("soft_delete", json!([{"model":"Post","field":"nope"}]), "nope"));
    }
    #[cfg(feature = "optimistic-locking")]
    {
        cases.push(("versions", json!([{"model":"Post","field":"changed"}]), "wrong type"));
        cases.push(("versions", json!([{"model":"Post","field":"tags"}]), "wrong type"));
        cases.push(("versions", json!([{"model":"Post","field":"version"},{"model":"Post","field":"views"}]), "one version field only"));
        cases.push(("versions", json!([{"model":"Nope","field":"version"}]), "unknown model"));
        cases.push(("versions", json!([{"model":"Post","field":"nope"}]), "nope"));
    }
    for (key, value, expected) in cases {
        let ir = json!({"models":[{"name":"Post","table":"posts","fields":[
            {"name":"id","column":"id","type":"int","primary_key":true},
            {"name":"views","column":"views","type":"int"},
            {"name":"tags","column":"tags","type":"int","array":true},
            {"name":"changed","column":"changed_at","type":"date_time"},
            {"name":"deleted","column":"deleted_at","type":"date_time","nullable":true},
            {"name":"version","column":"version","type":"int"}
        ]}],"behavior":{"schema_contract":2, key: value.clone()}});
        let error = Schema::from_ir(serde_json::from_value(ir).unwrap()).err().unwrap_or_else(|| panic!("{key} {value} was accepted"));
        assert!(error.contains(expected), "{key} {value}: {error}");
    }
}
