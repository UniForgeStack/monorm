//! Expression records (`AssignValue::Expr`, `ClientDefaultIr::Expr`) in the planner.
use std::collections::BTreeMap;
use orm_core::{dialect::{Dialect, Target}, ir::{Operation, ValueType}, schema::Schema};
use orm_engine::{db, exec, params::Scoped, plan::{Plan, Planner}, Params, Result};
use sea_query::Value;
use serde_json::{json, Value as Json};

struct Ints(Vec<i32>);
impl Params for Ints {
    fn len(&self) -> usize { self.0.len() }
    fn value(&self, i: usize, _: Option<ValueType>) -> Result<Value> { Ok(Value::Int(Some(self.0[i]))) }
    fn text(&self, _: usize) -> Result<String> { unreachable!() }
    fn count(&self, _: usize) -> Result<u64> { unreachable!() }
}

fn schema(client_default: Json, behavior: Json) -> std::result::Result<Schema, String> {
    let mut behavior = behavior;
    behavior["schema_contract"] = json!(2);
    let mut shop = json!({"name":"shop","column":"shop_id","type":"int"});
    if !client_default.is_null() { shop["client_default"] = client_default; }
    Schema::from_ir(serde_json::from_value(json!({"models":[{"name":"Post","table":"posts","fields":[
        {"name":"id","column":"id","type":"int","primary_key":true},
        {"name":"name","column":"name","type":"string"},
        {"name":"slug","column":"slug","type":"string","nullable":true},
        shop,
    ]}],"behavior":behavior})).unwrap())
}

fn expr_default() -> Json { json!({"expr":{"t":"scope","name":"shop"}}) }

fn lower_name() -> Json {
    json!({"source":"test.a","model":"Post","field":"slug","on":["update"],
        "value":{"expr":{"t":"func","name":"lower","args":[{"t":"col","path":[],"name":"name"}]}}})
}

fn update() -> Json {
    json!({"op":"update","model":"Post","set":[{"field":"name","value":{"t":"param","i":0}}],
        "filters":[{"t":"cmp","op":"eq","l":{"t":"col","path":[],"name":"id"},"r":{"t":"param","i":0}}]})
}

fn insert(schema: &Schema, dialect: Dialect, fields: &[&str], rows: Vec<Vec<Option<Value>>>, params: &dyn Params) -> (String, Vec<Value>) {
    let target = Target::new(dialect);
    let fields: Vec<String> = fields.iter().map(|f| f.to_string()).collect();
    match exec::plan_insert(schema, target, "Post", &fields, rows, None, params, &Default::default()).ok().unwrap() {
        Plan::Insert(stmt, _) => db::build(dialect, &stmt),
        _ => unreachable!(),
    }
}

#[test]
fn an_expression_default_fills_each_omitted_value_from_the_scope() {
    let schema = schema(expr_default(), json!({})).unwrap();
    let scope = BTreeMap::from([("shop".to_owned(), 0usize)]);
    let ints = Ints(vec![42]);
    let params = Scoped { params: &ints, scope: &scope };
    let text = |s: &str| Some(Value::String(Some(s.to_owned())));
    for dialect in [Dialect::Postgres, Dialect::Sqlite] {
        let (statement, values) = insert(&schema, dialect, &["name"], vec![vec![text("a")], vec![text("b")]], &params);
        assert!(statement.contains("\"shop_id\""), "{statement}");
        assert_eq!(values.iter().filter(|v| **v == Value::Int(Some(42))).count(), 2, "{values:?}");
        let (_, values) = insert(&schema, dialect, &["name", "shop"], vec![vec![text("a"), Some(Value::Int(Some(7)))]], &params);
        assert!(values.contains(&Value::Int(Some(7))) && !values.contains(&Value::Int(Some(42))), "{values:?}");
    }
}

#[test]
fn an_expression_default_without_its_scope_value_fails_closed() {
    let schema = schema(expr_default(), json!({})).unwrap();
    let target = Target::new(Dialect::Sqlite);
    let rows = vec![vec![Some(Value::String(Some("a".to_owned())))]];
    let error = exec::plan_insert(&schema, target, "Post", &["name".to_owned()], rows, None, &Ints(vec![]), &Default::default()).err().unwrap();
    assert!(error.to_string().contains("shop"), "{error}");
}

#[test]
fn an_expression_assignment_sets_its_expression_unless_the_update_sets_the_field() {
    let schema = schema(Json::Null, json!({"write_assignments":[lower_name()]})).unwrap();
    for dialect in [Dialect::Postgres, Dialect::Sqlite] {
        let target = Target::new(dialect);
        let op: Operation = serde_json::from_value(update()).unwrap();
        let statement = exec::statement(target, &Planner::plan(&schema, target, &op, &Ints(vec![7])).unwrap());
        assert!(statement.contains("\"slug\" = LOWER(\"posts\".\"name\")"), "{statement}");
        let op: Operation = serde_json::from_value(json!({"op":"update","model":"Post","set":[{"field":"slug","value":{"t":"param","i":0}}]})).unwrap();
        let statement = exec::statement(target, &Planner::plan(&schema, target, &op, &Ints(vec![7])).unwrap());
        assert_eq!(statement.matches("\"slug\" = ").count(), 1, "{statement}");
        assert!(!statement.contains("LOWER"), "{statement}");
    }
}

#[test]
fn a_model_without_expression_records_plans_the_same_sql() {
    let plain = schema(Json::Null, json!({})).unwrap();
    let text = |s: &str| Some(Value::String(Some(s.to_owned())));
    for dialect in [Dialect::Postgres, Dialect::Sqlite] {
        let (statement, _) = insert(&plain, dialect, &["name"], vec![vec![text("a")]], &Ints(vec![]));
        assert!(statement.starts_with("INSERT INTO \"posts\" (\"name\") VALUES (") && !statement.contains("shop_id\")"), "{statement}");
        let target = Target::new(dialect);
        let op: Operation = serde_json::from_value(update()).unwrap();
        let statement = exec::statement(target, &Planner::plan(&plain, target, &op, &Ints(vec![7])).unwrap());
        assert_eq!(statement.matches(" = ").count(), 2, "{statement}");
    }
}

#[test]
fn an_expression_record_rejects_nodes_that_a_statement_cannot_bind() {
    for tag in [json!({"t":"param","i":0}), json!({"t":"excluded","name":"name"}), json!({"t":"outer","depth":1,"name":"id"})] {
        let record = json!({"source":"test.a","model":"Post","field":"slug","on":["update"],"value":{"expr":tag}});
        let error = schema(Json::Null, json!({"write_assignments":[record]})).err().unwrap();
        assert!(error.contains("cannot use"), "{error}");
    }
    let col = json!({"t":"col","path":[],"name":"name"});
    let error = schema(json!({"expr":col}), json!({})).err().unwrap();
    assert!(error.contains("`col`"), "{error}");
}

#[test]
fn a_hook_default_without_its_binding_fails_define() {
    let error = schema(json!({"hook":{"export":"x.y"}}), json!({})).err().unwrap();
    assert!(error.contains("Post.shop") && error.contains("no point-7 binding"), "{error}");
}
