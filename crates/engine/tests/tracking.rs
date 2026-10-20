//! Neutral write records (assignments, delete rewrite, instance guards) in the planner.
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

fn now(field: &str) -> Json {
    json!({"source":"test.a","model":"Post","field":field,"on":["update","upsert","delete_rewrite"],"value":"now"})
}
fn increment(field: &str) -> Json {
    json!({"source":"test.b","model":"Post","field":field,"on":["update","upsert","delete_rewrite"],"value":"increment"})
}
fn rewrite() -> Json {
    json!({"source":"test.c","model":"Post","field":"deleted","guard":{"t":"is_null","item":{"t":"col","path":[],"name":"deleted"},"neg":false}})
}

#[test]
fn an_update_sets_a_now_assignment_unless_it_sets_the_field() {
    let schema = schema(json!({"write_assignments":[now("changed")]})).unwrap();
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

#[test]
fn an_assignment_applies_only_to_its_statement_kinds() {
    let record = json!({"source":"test.a","model":"Post","field":"changed","on":["upsert"],"value":"now"});
    let schema = schema(json!({"write_assignments":[record]})).unwrap();
    let statement = sql(&schema, Dialect::Postgres, update("views"));
    assert!(!statement.contains("changed_at"), "{statement}");
}

#[test]
fn a_delete_rewrite_makes_a_delete_an_update_of_guarded_rows() {
    let schema = schema(json!({"delete_rewrites":[rewrite()]})).unwrap();
    let filter = json!([{"t":"cmp","op":"eq","l":{"t":"col","path":[],"name":"id"},"r":{"t":"param","i":0}}]);
    for dialect in [Dialect::Postgres, Dialect::Sqlite] {
        let statement = sql(&schema, dialect, json!({"op":"delete","model":"Post","filters":filter,"returning":true}));
        assert!(statement.starts_with("UPDATE \"posts\" SET \"deleted_at\" = "), "{statement}");
        assert!(statement.contains("AND \"posts\".\"deleted_at\" IS NULL RETURNING"), "{statement}");
        let statement = sql(&schema, dialect, json!({"op":"delete","model":"Post","filters":filter,"hard":true}));
        assert!(statement.starts_with("DELETE FROM \"posts\""), "{statement}");
    }
}

#[test]
fn a_delete_without_a_rewrite_deletes_rows() {
    let schema = schema(json!({"write_assignments":[now("changed")]})).unwrap();
    let statement = sql(&schema, Dialect::Postgres, json!({"op":"delete","model":"Post"}));
    assert!(statement.starts_with("DELETE FROM \"posts\""), "{statement}");
}

#[test]
fn a_delete_rewrite_runs_the_assignments_of_its_kind() {
    let schema = schema(json!({"delete_rewrites":[rewrite()],"write_assignments":[now("changed"), increment("version")]})).unwrap();
    let statement = sql(&schema, Dialect::Postgres, json!({"op":"delete","model":"Post"}));
    assert!(statement.contains("\"changed_at\" = ") && statement.contains("\"version\" = \"posts\".\"version\" + "), "{statement}");
}

#[test]
fn an_update_increments_an_increment_assignment_unless_it_sets_it() {
    let schema = schema(json!({"write_assignments":[increment("version")]})).unwrap();
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

#[test]
fn an_upsert_runs_the_assignments_of_its_kind() {
    let schema = schema(json!({"write_assignments":[now("changed"), increment("version")]})).unwrap();
    let set = |field: &str| vec![serde_json::from_value(json!({"field":field,"value":{"t":"param","i":0}})).unwrap()];
    let upsert = |dialect, update: &[&str], set| {
        let target = Target::new(dialect);
        let conflict = exec::Conflict::Update { target: vec!["id".into()], filter: None, update: update.iter().map(|f| f.to_string()).collect(), set };
        let fields: Vec<String> = ["id", "views"].iter().map(|f| f.to_string()).collect();
        let rows = vec![vec![Some(Value::Int(Some(1))), Some(Value::Int(Some(2)))]];
        exec::statement(target, &exec::plan_insert(&schema, target, "Post", &fields, rows, Some(conflict), &Ints(vec![7]), &Default::default()).unwrap())
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
