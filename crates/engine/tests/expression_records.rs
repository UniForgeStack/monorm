//! Expression records (`AssignValue::Expr`, `ClientDefaultIr::Expr`) in the planner.
use std::{collections::BTreeMap, ffi::c_void};
use monorm::abi::{OrmArg, OrmContext, OrmValue, Value as Abi, STATUS_ERROR};
use monorm_core::{client_default::ClientDefault, dialect::{Dialect, Target}, hooks::{HookPlan, HookRef}, ir::{Operation, ValueType}, schema::Schema};
use monorm_engine::{db, exec, params::Scoped, plan::{Plan, Planner}, Params, Result};
use sea_query::Value;
use serde_json::{json, Value as Json};

struct Ints(Vec<i32>);
impl Params for Ints {
    fn len(&self) -> usize { self.0.len() }
    fn value(&self, i: usize, _: Option<ValueType>) -> Result<Value> { Ok(Value::Int(Some(self.0[i]))) }
    fn text(&self, _: usize) -> Result<String> { unreachable!() }
    fn count(&self, _: usize) -> Result<u64> { unreachable!() }
}

const NAME: usize = 1;
const SLUG: usize = 2;
const SHOP: usize = 3;

fn schema(client_default: Json, behavior: Json) -> std::result::Result<Schema, String> {
    let mut behavior = behavior;
    behavior["schema_contract"] = json!(monorm_core::behavior::SCHEMA_CONTRACT);
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

fn text(value: &str) -> Option<Value> { Some(Value::String(Some(value.to_owned()))) }

fn int(value: i32) -> Option<Value> { Some(Value::Int(Some(value))) }

unsafe extern "C" fn stand_in(_: *const c_void, _: *const OrmArg, _: usize, _: *const OrmContext, out: *mut OrmValue) -> i32 {
    // SAFETY: the host passes a writable `out`.
    unsafe { *out = Abi::Text("the test hook cannot give an expression".to_owned()).leak() };
    STATUS_ERROR
}

/// `Post.shop` takes its default from a point-7 hook. The test hook fails when it is called, so a call fails the statement.
fn with_hook_default() -> Schema {
    let mut schema = schema(Json::Null, json!({})).unwrap();
    schema.models[0].hooks = HookPlan { expr_default: vec![(SHOP, HookRef::in_process("test.stand_in", stand_in, false))], ..Default::default() }.finish();
    schema.models[0].client_defaults.push((SHOP, ClientDefault::Hook("x.y".into())));
    schema
}

/// `Post.slug` is a computed field (point 5) of `name`. The test hook is never called by these tests.
fn with_computed_field() -> Schema {
    let mut schema = schema(Json::Null, json!({})).unwrap();
    schema.models[0].hooks = HookPlan { computed: vec![(SLUG, HookRef::in_process("test.stand_in", stand_in, false), vec![NAME])], ..Default::default() }.finish();
    schema
}

#[test]
fn an_expression_default_fills_each_omitted_value_from_the_scope() {
    let schema = schema(expr_default(), json!({})).unwrap();
    let scope = BTreeMap::from([("shop".to_owned(), 0usize)]);
    let ints = Ints(vec![42]);
    let params = Scoped { params: &ints, scope: &scope };
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
fn an_expression_default_is_planned_only_for_a_row_that_omits_the_value() {
    let schema = schema(expr_default(), json!({})).unwrap();
    let scope = BTreeMap::from([("shop".to_owned(), 0usize)]);
    let ints = Ints(vec![42]);
    let scoped = Scoped { params: &ints, scope: &scope };
    for dialect in [Dialect::Postgres, Dialect::Sqlite] {
        // Every row sets `shop`: the request needs no scope value.
        let (_, values) = insert(&schema, dialect, &["name", "shop"], vec![vec![text("a"), int(7)], vec![text("b"), int(8)]], &Ints(vec![]));
        assert!(values.contains(&Value::Int(Some(7))) && values.contains(&Value::Int(Some(8))), "{values:?}");
        // One row omits it: that row takes the scope value and the other keeps its own.
        let (_, values) = insert(&schema, dialect, &["name", "shop"], vec![vec![text("a"), int(7)], vec![text("b"), None]], &scoped);
        assert_eq!(values.iter().filter(|v| matches!(v, Value::Int(Some(7 | 42)))).count(), 2, "{values:?}");
        assert!(values.contains(&Value::Int(Some(7))) && values.contains(&Value::Int(Some(42))), "{values:?}");
        // Without the scope value, the omitting row still fails closed.
        let rows = vec![vec![text("a"), int(7)], vec![text("b"), None]];
        let error = exec::plan_insert(&schema, Target::new(dialect), "Post", &["name".to_owned(), "shop".to_owned()], rows, None, &Ints(vec![]), &Default::default()).err().unwrap();
        assert!(error.to_string().contains("shop"), "{error}");
    }
}

#[test]
fn a_hook_default_runs_only_for_a_row_that_omits_the_value() {
    let schema = with_hook_default();
    for dialect in [Dialect::Postgres, Dialect::Sqlite] {
        // The test hook fails when it is called: every row sets `shop`, so no call happens.
        let (statement, values) = insert(&schema, dialect, &["name", "shop"], vec![vec![text("a"), int(7)], vec![text("b"), int(8)]], &Ints(vec![]));
        assert!(statement.contains("\"shop_id\""), "{statement}");
        assert!(values.contains(&Value::Int(Some(7))) && values.contains(&Value::Int(Some(8))), "{values:?}");
        for rows in [vec![vec![text("a")]], vec![vec![text("a"), int(7)], vec![text("b"), None]]] {
            let fields: Vec<String> = ["name", "shop"].iter().take(rows[0].len()).map(|f| f.to_string()).collect();
            let error = exec::plan_insert(&schema, Target::new(dialect), "Post", &fields, rows, None, &Ints(vec![]), &Default::default()).err().unwrap();
            assert!(error.to_string().contains("cannot give an expression"), "{error}");
        }
    }
}

#[test]
fn a_copy_load_rejects_an_omitted_value_that_has_an_expression_or_hook_default() {
    let postgres = Target::new(Dialect::Postgres);
    let hook = with_hook_default();
    let expr = schema(expr_default(), json!({})).unwrap();
    for (label, schema) in [("expression", &expr), ("hook", &hook)] {
        for (fields, rows) in [(vec!["name"], vec![vec![text("a")]]), (vec!["name", "shop"], vec![vec![text("a"), int(7)], vec![text("b"), None]])] {
            let fields: Vec<String> = fields.into_iter().map(str::to_owned).collect();
            let error = exec::plan_copy(schema, postgres, "Post", &fields, rows).err().unwrap_or_else(|| panic!("the {label} default was dropped"));
            assert!(error.to_string().contains("has an expression default; use insert_many()"), "{error}");
        }
        // Every row sets the value: the default is not needed.
        let fields = ["name".to_owned(), "shop".to_owned()];
        assert!(exec::plan_copy(schema, postgres, "Post", &fields, vec![vec![text("a"), int(7)]]).is_ok());
    }
}

#[test]
fn update_many_rejects_a_computed_field_in_every_build() {
    let schema = with_computed_field();
    let fields = ["id".to_owned(), "slug".to_owned()];
    for dialect in [Dialect::Postgres, Dialect::Sqlite] {
        let rows = vec![vec![Value::Int(Some(1)), Value::String(Some("x".to_owned()))]];
        let error = exec::plan_update_many(&schema, Target::new(dialect), "Post", &fields, rows, &[], &Ints(vec![]), false, None, &Default::default()).err().unwrap();
        assert!(error.to_string().contains("computed fields are read-only"), "{error}");
    }
}

#[test]
fn an_update_and_an_insert_reject_a_computed_field_in_every_build() {
    let schema = with_computed_field();
    for dialect in [Dialect::Postgres, Dialect::Sqlite] {
        let target = Target::new(dialect);
        let op: Operation = serde_json::from_value(json!({"op":"update","model":"Post","set":[{"field":"slug","value":{"t":"param","i":0}}]})).unwrap();
        let error = Planner::plan(&schema, target, &op, &Ints(vec![7])).err().unwrap();
        assert!(error.to_string().contains("computed fields are read-only"), "{error}");
        let fields = ["id".to_owned(), "slug".to_owned()];
        let error = exec::plan_insert(&schema, target, "Post", &fields, vec![vec![int(1), text("x")]], None, &Ints(vec![]), &Default::default()).err().unwrap();
        assert!(error.to_string().contains("computed fields are read-only"), "{error}");
    }
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

/// The SQL and the number of hook-5 computations of a read of `Post`; `fields` is `null` for the whole row.
fn select(schema: &Schema, dialect: Dialect, fields: Json) -> (String, usize) {
    let mut op = json!({"op":"select","model":"Post"});
    if !fields.is_null() { op["model_fields"] = fields; }
    let op: Operation = serde_json::from_value(op).unwrap();
    match Planner::plan(schema, Target::new(dialect), &op, &Ints(vec![])).unwrap() {
        Plan::Select(plan) => (db::build(dialect, &plan.stmt).0, plan.computed.len()),
        _ => unreachable!(),
    }
}

fn update_sql(schema: &Schema, dialect: Dialect) -> String {
    let target = Target::new(dialect);
    let op: Operation = serde_json::from_value(update()).unwrap();
    exec::statement(target, &Planner::plan(schema, target, &op, &Ints(vec![7])).unwrap())
}

#[test]
fn a_model_without_expression_records_plans_the_same_sql() {
    let plain = schema(Json::Null, json!({})).unwrap();
    for (dialect, first, second) in [(Dialect::Postgres, "$1", "$2"), (Dialect::Sqlite, "?", "?")] {
        let (statement, values) = insert(&plain, dialect, &["name"], vec![vec![text("a")]], &Ints(vec![]));
        assert_eq!(statement, format!("INSERT INTO \"posts\" (\"name\") VALUES ({first}) RETURNING \"id\", \"name\", \"slug\", \"shop_id\""));
        assert_eq!(values, vec![Value::String(Some("a".to_owned()))]);
        assert_eq!(update_sql(&plain, dialect), format!("UPDATE \"posts\" SET \"name\" = {first} WHERE \"posts\".\"id\" = {second}"));
        assert_eq!(select(&plain, dialect, Json::Null), ("SELECT \"posts\".\"id\", \"posts\".\"name\", \"posts\".\"slug\", \"posts\".\"shop_id\" FROM \"posts\"".to_owned(), 0));
    }
}

#[test]
fn a_read_selects_the_inputs_of_a_computed_field_only_when_it_asks_for_the_field() {
    let plain = schema(Json::Null, json!({})).unwrap();
    let computed = with_computed_field();
    for dialect in [Dialect::Postgres, Dialect::Sqlite] {
        // The computed column is a NULL placeholder that the hook fills after the read; its input follows the asked fields.
        assert_eq!(select(&computed, dialect, Json::Null), ("SELECT \"posts\".\"id\", \"posts\".\"name\", NULL, \"posts\".\"shop_id\" FROM \"posts\"".to_owned(), 1));
        assert_eq!(select(&computed, dialect, json!(["id", "slug"])), ("SELECT \"posts\".\"id\", NULL, \"posts\".\"name\" FROM \"posts\"".to_owned(), 1));
        assert_eq!(select(&computed, dialect, json!(["id", "name"])), ("SELECT \"posts\".\"id\", \"posts\".\"name\" FROM \"posts\"".to_owned(), 0));
        // A hook of point 5 adds nothing to a write.
        assert_eq!(update_sql(&computed, dialect), update_sql(&plain, dialect));
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
