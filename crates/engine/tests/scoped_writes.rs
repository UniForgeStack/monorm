//! Scope columns: the insert fill and the write check of the engine, with a hand-built IR (no extension).
use std::collections::BTreeMap;

use monorm_core::{dialect::{Dialect, Target}, dsl, ir::ValueType, schema::Schema};
use monorm_engine::{
    db::{self, Cell},
    error::Error,
    exec::{self, Conflict, Outcome},
    params::Params,
    plan::Plan,
};
use sea_query::Value;

const SOURCE: &str = r#"
datasource db {
 provider = "sqlite"
}
model Order {
 id BigInt @id @default(autoincrement())
 shop_id Int
 total Int
 @@map("scoped_orders")
}
"#;

/// Positional values; `scope` maps a key to an index; `open` holds the keys of `unscoped`.
struct Ctx {
    values: Vec<Value>,
    scope: BTreeMap<String, usize>,
    open: Vec<String>,
}

impl Ctx {
    fn none() -> Self { Ctx { values: vec![], scope: BTreeMap::new(), open: vec![] } }
    fn shop(id: i32) -> Self { Ctx { values: vec![Value::Int(Some(id))], scope: BTreeMap::from([("shop".into(), 0)]), open: vec![] } }
    fn open(mut self) -> Self { self.open.push("shop".into()); self }
}

impl Params for Ctx {
    fn len(&self) -> usize { self.values.len() }
    fn value(&self, i: usize, _: Option<ValueType>) -> monorm_engine::error::Result<Value> { Ok(self.values[i].clone()) }
    fn text(&self, _: usize) -> monorm_engine::error::Result<String> { unreachable!() }
    fn count(&self, _: usize) -> monorm_engine::error::Result<u64> { unreachable!() }
    fn scope(&self, name: &str) -> Option<usize> { self.scope.get(name).copied() }
    fn unscoped(&self, key: &str) -> bool { self.open.iter().any(|k| k == key) }
}

fn schema() -> Schema {
    let mut ir = dsl::compile(SOURCE, None).unwrap();
    ir.behavior.scoped_fields.push(monorm::writes::ScopedField { source: "test".into(), model: "Order".into(), field: "shop_id".into(), key: "shop".into() });
    ir.stamp_schema_contract();
    dsl::check(ir).unwrap().1
}

fn cell(n: i32) -> Option<Value> { Some(Value::Int(Some(n))) }

fn insert(schema: &Schema, fields: &[&str], rows: Vec<Vec<Option<Value>>>, conflict: Option<Conflict>, ctx: &Ctx) -> Result<Plan, Error> {
    let fields: Vec<String> = fields.iter().map(|f| f.to_string()).collect();
    exec::plan_insert(schema, Target::new(Dialect::Sqlite), "Order", &fields, rows, conflict, ctx, &Default::default())
}

fn mismatch(e: Error) -> (String, String) {
    match e {
        Error::ScopeMismatch { key, expected, actual, .. } => { assert_eq!(key, "shop"); (expected, actual) }
        other => panic!("not a mismatch: {other:?}"),
    }
}

#[test]
fn insert_fills_checks_and_refuses() {
    let schema = schema();
    // An omitted column and an omitted cell take the scope value; an equal value passes.
    assert!(insert(&schema, &["total"], vec![vec![cell(5)]], None, &Ctx::shop(7)).is_ok());
    assert!(insert(&schema, &["shop_id", "total"], vec![vec![None, cell(5)], vec![cell(7), cell(6)]], None, &Ctx::shop(7)).is_ok());
    // One row with another value fails the whole insert, with the values named.
    let e = insert(&schema, &["shop_id", "total"], vec![vec![cell(7), cell(5)], vec![cell(8), cell(6)]], None, &Ctx::shop(7)).err().expect("the write is refused");
    assert_eq!(mismatch(e), ("7".into(), "8".into()));
    let e = insert(&schema, &["shop_id", "total"], vec![vec![Some(Value::Int(None)), cell(5)]], None, &Ctx::shop(7)).err().expect("the write is refused");
    assert_eq!(mismatch(e), ("7".into(), "NULL".into()));
    // No scope value: ScopeMissing, naming the model and the key.
    match insert(&schema, &["total"], vec![vec![cell(5)]], None, &Ctx::none()).err().expect("the write is refused") {
        Error::ScopeMissing { model, key, .. } => assert_eq!((model.as_str(), key.as_str()), ("Order", "shop")),
        other => panic!("{other:?}"),
    }
    // `unscoped("shop")` turns both off: another value is written as given, and no value is needed.
    assert!(insert(&schema, &["shop_id", "total"], vec![vec![cell(8), cell(5)]], None, &Ctx::shop(7).open()).is_ok());
    assert!(insert(&schema, &["shop_id", "total"], vec![vec![cell(8), cell(5)]], None, &Ctx::none().open()).is_ok());
}

#[test]
fn an_open_index_marks_the_key_open_and_gives_no_value() {
    use monorm_engine::params::{Scoped, OPEN_INDEX};
    let base = Ctx::shop(7);
    let scope = BTreeMap::from([("shop".to_string(), OPEN_INDEX), ("user".to_string(), 0)]);
    let scoped = Scoped { params: &base, scope: &scope };
    assert!(scoped.unscoped("shop") && !scoped.unscoped("user"));
    assert_eq!(scoped.scope("shop"), None);
    assert_eq!(scoped.scope("user"), Some(0));
    assert_eq!(scoped.scope_values(), vec![("user".to_string(), 0)]);
}

#[test]
fn upsert_checks_each_row_and_keeps_the_stored_scope() {
    let schema = schema();
    let conflict = |update: &[&str]| Some(Conflict::Update { target: vec!["id".into()], index: Default::default(), update: update.iter().map(|u| u.to_string()).collect(), set: vec![] });
    let e = insert(&schema, &["id", "shop_id", "total"], vec![vec![cell(1), cell(8), cell(5)]], conflict(&["total"]), &Ctx::shop(7)).err().expect("the write is refused");
    assert_eq!(mismatch(e), ("7".into(), "8".into()));
    // The `DO UPDATE` list drops the scope column, so a conflict cannot move a row to the current scope.
    let plan = insert(&schema, &["id", "shop_id", "total"], vec![vec![cell(1), cell(7), cell(5)]], conflict(&["shop_id", "total"]), &Ctx::shop(7)).unwrap();
    let Plan::Insert(statement, _) = plan else { panic!("an insert plan") };
    let sql = db::build(Dialect::Sqlite, &statement).0;
    let update = sql.split("DO UPDATE").nth(1).unwrap();
    assert!(!update.contains("\"shop_id\" ="), "{sql}");
    assert!(update.contains("\"total\" ="), "{sql}");
}

#[test]
fn update_many_rows_must_keep_the_scope() {
    let schema = schema();
    let plan = |rows: Vec<Vec<Value>>, ctx: &Ctx| {
        let fields = ["id", "shop_id"].map(String::from);
        exec::plan_update_many(&schema, Target::new(Dialect::Sqlite), "Order", &fields, rows, &[], ctx, false, None, None, &Default::default())
    };
    let row = |shop: i32| vec![Value::BigInt(Some(1)), Value::Int(Some(shop))];
    assert!(plan(vec![row(7)], &Ctx::shop(7)).is_ok());
    assert_eq!(mismatch(plan(vec![row(7), row(8)], &Ctx::shop(7)).err().unwrap()), ("7".into(), "8".into()));
    assert!(plan(vec![row(8)], &Ctx::shop(7).open()).is_ok());
}

#[test]
fn update_many_refuses_an_expression_for_the_scope_column() {
    let schema = schema();
    let expr: monorm_core::ir::Expr = serde_json::from_value(serde_json::json!({"t":"col","path":[],"name":"total"})).unwrap();
    let extras = exec::ManyExtras { rows: vec![(vec![None, Some(expr)], vec![])], update_values: vec![] };
    let fields = ["id", "shop_id"].map(String::from);
    let plan = |ctx: &Ctx| exec::plan_update_many_with(&schema, Target::new(Dialect::Sqlite), "Order", &fields, vec![vec![Value::BigInt(Some(1)), Value::Int(None)]], &extras, &[], ctx, false, None, None, &Default::default());
    let error = plan(&Ctx::shop(7)).err().expect("the write is refused").to_string();
    assert!(error.contains("shop_id"), "{error}");
    assert!(plan(&Ctx::shop(7).open()).is_ok());
}

#[tokio::test]
async fn a_filled_insert_stores_the_scope_value() {
    let schema = schema();
    let target = Target::new(Dialect::Sqlite);
    let db = db::connect("sqlite://:memory:", 1).await.unwrap();
    let conn = db.begin().await.unwrap();
    for statement in monorm_core::migrate::create_all(&schema).unwrap() { conn.batch(statement).await.unwrap(); }
    let plan = insert(&schema, &["total"], vec![vec![cell(5)], vec![cell(6)]], None, &Ctx::shop(7)).unwrap();
    let Outcome::Rows { rows, .. } = exec::run(conn.as_ref(), target, plan).await.unwrap() else { panic!() };
    let types: Vec<ValueType> = schema.model(schema.model_idx("Order").unwrap()).fields().iter().map(|f| f.value_type()).collect();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows.cell(0, 1, types[1]).unwrap(), Cell::Int(7));
    assert_eq!(rows.cell(1, 1, types[1]).unwrap(), Cell::Int(7));
    conn.rollback().await.unwrap();
    db.close().await;
}
