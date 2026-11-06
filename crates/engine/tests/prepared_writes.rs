//! A prepared write holds its statement shapes: a call must change what the unprepared
//! write of the same values changes.
use std::sync::Arc;

use monorm_core::{dialect::{Dialect, Target}, ir::{ColType, Operation, ValueType}, schema::Schema};
use monorm_engine::{
    db::{self, Executor},
    exec::{self, Conflict, Outcome},
    params::{NoParams, Params},
    plan::OnConflictTarget,
    prepared_write::{PreparedRows, PreparedWrite, RowsKind, RowsSpec},
    Result,
};
use sea_query::Value;
use serde_json::{json, Value as J};

fn schema(behavior: J) -> Arc<Schema> {
    let mut behavior = behavior;
    behavior["schema_contract"] = json!(5);
    Arc::new(
        Schema::from_ir(
            serde_json::from_value(json!({"models":[{"name":"Post","table":"posts","fields":[
                {"name":"id","column":"id","type":"big_int","primary_key":true},
                {"name":"title","column":"title","type":"text","nullable":true},
                {"name":"views","column":"views","type":"int"},
                {"name":"changed","column":"changed_at","type":"date_time","nullable":true}
            ]}],"behavior":behavior}))
            .unwrap(),
        )
        .unwrap(),
    )
}

const TABLE: &str = "CREATE TABLE posts (id INTEGER PRIMARY KEY, title TEXT, views INTEGER NOT NULL, changed_at TEXT)";

struct Vals(Vec<Value>);

impl Params for Vals {
    fn len(&self) -> usize {
        self.0.len()
    }
    fn value(&self, i: usize, _: Option<ValueType>) -> Result<Value> {
        Ok(self.0[i].clone())
    }
    fn text(&self, _: usize) -> Result<String> {
        unreachable!()
    }
    fn count(&self, _: usize) -> Result<u64> {
        unreachable!()
    }
}

fn big(n: i64) -> Value {
    Value::BigInt(Some(n))
}
fn int(n: i32) -> Value {
    Value::Int(Some(n))
}
fn text(s: &str) -> Value {
    Value::String(Some(s.into()))
}
fn ty(t: ColType) -> Option<ValueType> {
    Some(ValueType { ty: t, array: false, enum_idx: None })
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().unwrap()
}

async fn fresh(d: Dialect) -> Arc<dyn Driver> {
    if d == Dialect::Postgres {
        let url = std::env::var("ORM_RUST_TEST_POSTGRES_URL").or_else(|_| std::env::var("ORM_TEST_DATABASE_URL")).unwrap_or_else(|_| "postgres://postgres:postgres@localhost/orm_test".into());
        let conn = db::connect(&url, 1).await.unwrap();
        conn.batch("DROP TABLE IF EXISTS posts; CREATE TABLE posts (id BIGINT PRIMARY KEY, title TEXT, views INTEGER NOT NULL, changed_at TIMESTAMPTZ)".into()).await.unwrap();
        return conn;
    }
    let conn = db::connect("sqlite://:memory:", 1).await.unwrap();
    conn.batch(TABLE.into()).await.unwrap();
    conn
}
use db::Driver;

async fn table(conn: &dyn Executor) -> Vec<Vec<Option<String>>> {
    conn.query_text("SELECT id, title, views FROM posts ORDER BY id".into()).await.unwrap()
}

fn rows_spec(kind: RowsKind, fields: &[&str]) -> RowsSpec {
    RowsSpec {
        kind,
        model: "Post".into(),
        fields: fields.iter().map(|f| f.to_string()).collect(),
        returning: false,
        model_fields: None,
        without_defaults: Default::default(),
        scope: Default::default(),
        batch_size: None,
        len: 0,
        declared: vec![],
    }
}

fn target(d: Dialect, cap: Option<usize>) -> Target {
    match cap {
        Some(n) => Target::new(d).without(&[format!("max_params={n}")]).unwrap(),
        None => Target::new(d),
    }
}

fn insert_rows(n: usize, offset: usize) -> Vec<Vec<Option<Value>>> {
    (0..n).map(|i| vec![Some(big((offset + i) as i64)), Some(text(&format!("t{i}"))), Some(int(i as i32))]).collect()
}

async fn same_insert(d: Dialect, rows: &dyn Fn() -> Vec<Vec<Option<Value>>>, cap: Option<usize>, conflict: Option<Conflict>) {
    let schema = schema(json!({}));
    let t = target(d, cap);
    let fields = ["id", "title", "views"];
    let prepared = PreparedRows::prepare(schema.clone(), t, rows_spec(RowsKind::Insert(conflict.clone()), &fields)).unwrap();
    let a = fresh(d).await;
    let n = rows().len();
    let got = prepared.run(&*a, &NoParams, rows(), true).await.unwrap_or_else(|e| panic!("{n} rows, cap {cap:?}: {e}"));
    let from_a = table(&*a).await;
    if n == 0 {
        assert!(matches!(got, Outcome::Affected(0)) && from_a.is_empty());
        return;
    }
    let b = fresh(d).await;
    let plans = exec::plan_inserts(&schema, t, "Post", &fields.map(String::from), rows(), conflict, &NoParams, None, false, None, &Default::default()).unwrap();
    let expect = exec::run_inserts(&*b, t, plans, true, false).await.unwrap();
    match (got, expect) {
        (Outcome::Affected(x), Outcome::Affected(y)) => assert_eq!(x, y),
        _ => panic!("affected counts"),
    }
    assert_eq!(from_a, table(&*b).await);
}

#[test]
fn insert_many_writes_what_the_unprepared_write_writes() {
    rt().block_on(async {
        for n in [0, 1, 2, 31, 32, 33, 100] {
            same_insert(Dialect::Sqlite, &|| insert_rows(n, 0), None, None).await;
        }
        // 3 columns and 7 parameters: 2 rows per statement.
        for n in [1, 2, 3, 7] {
            same_insert(Dialect::Sqlite, &|| insert_rows(n, 0), Some(7), None).await;
        }
    });
}

#[test]
fn a_null_cell_and_an_omitted_cell_differ() {
    rt().block_on(async {
        let rows = || {
            vec![
                vec![Some(big(1)), Some(Value::String(None)), Some(int(1))],
                vec![Some(big(2)), None, Some(int(2))],
                vec![Some(big(3)), Some(text("x")), Some(int(3))],
            ]
        };
        same_insert(Dialect::Sqlite, &rows, None, None).await;
        same_insert(Dialect::Sqlite, &rows, Some(7), None).await;
    });
}

#[test]
fn an_upsert_that_does_nothing_skips_the_duplicate() {
    rt().block_on(async {
        let conflict = Conflict::Nothing { target: vec!["id".into()], index: OnConflictTarget::Infer };
        let rows = || {
            let mut r = insert_rows(3, 0);
            r.extend(insert_rows(3, 2));
            r
        };
        same_insert(Dialect::Sqlite, &rows, None, Some(conflict)).await;
    });
}

#[test]
fn a_call_with_other_cells_set_uses_another_shape() {
    rt().block_on(async {
        let schema = schema(json!({}));
        let t = target(Dialect::Sqlite, None);
        let prepared = PreparedRows::prepare(schema, t, rows_spec(RowsKind::Insert(None), &["id", "title", "views"])).unwrap();
        let conn = fresh(Dialect::Sqlite).await;
        prepared.run(&*conn, &NoParams, insert_rows(2, 0), true).await.unwrap();
        prepared.run(&*conn, &NoParams, vec![vec![Some(big(10)), None, Some(int(1))]], true).await.unwrap();
        prepared.run(&*conn, &NoParams, vec![vec![Some(big(11)), Some(text("z")), Some(int(1))]], true).await.unwrap();
        let rows = table(&*conn).await;
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[2][1], None);
        assert_eq!(rows[3][1].as_deref(), Some("z"));
    });
}

fn update_many_rows(n: usize) -> Vec<Vec<Value>> {
    (0..n).map(|i| vec![big(i as i64), int(1000 + i as i32)]).collect()
}

async fn same_update_many(d: Dialect, n: usize, cap: Option<usize>) {
    let schema = schema(json!({}));
    let t = target(d, cap);
    let seed = || async {
        let c = fresh(d).await;
        let rows: Vec<_> = (0..n.max(1)).map(|i| vec![Some(big(i as i64)), Some(text("old")), Some(int(0))]).collect();
        let plans = exec::plan_inserts(&schema, t, "Post", &["id".into(), "title".into(), "views".into()], rows, None, &NoParams, None, false, None, &Default::default()).unwrap();
        exec::run_inserts(&*c, t, plans, true, false).await.unwrap();
        c
    };
    let fields = ["id", "views"];
    let prepared = PreparedRows::prepare(schema.clone(), t, rows_spec(RowsKind::UpdateMany(vec![]), &fields)).unwrap();
    let a = seed().await;
    prepared.run(&*a, &NoParams, update_many_rows(n).into_iter().map(|r| r.into_iter().map(Some).collect()).collect(), true).await.unwrap();
    let from_a = table(&*a).await;
    let b = seed().await;
    let (stmts, um) = exec::plan_update_many(&schema, t, "Post", &fields.map(String::from), update_many_rows(n), &[], &NoParams, false, None, None, &Default::default()).unwrap();
    let _ = stmts;
    exec::run_update_many(&*b, um, true).await.unwrap();
    assert_eq!(from_a, table(&*b).await, "{n} rows, cap {cap:?}");
}

#[test]
fn update_many_writes_what_the_unprepared_write_writes() {
    rt().block_on(async {
        for n in [0, 1, 2, 31, 32, 33, 100] {
            same_update_many(Dialect::Sqlite, n, None).await;
        }
        for n in [1, 2, 3, 7] {
            same_update_many(Dialect::Sqlite, n, Some(9)).await;
        }
    });
}

fn op(doc: J) -> Operation {
    serde_json::from_value(doc).unwrap()
}

fn update_op() -> Operation {
    op(json!({"op":"update","model":"Post","set":[{"field":"title","value":{"t":"param","i":0}}],
        "filters":[{"t":"cmp","op":"eq","l":{"t":"col","path":[],"name":"id"},"r":{"t":"param","i":1}}]}))
}

#[test]
fn an_update_sets_null_from_a_set_param_and_refuses_null_in_a_filter() {
    rt().block_on(async {
        let schema = schema(json!({}));
        let t = target(Dialect::Sqlite, None);
        let w = PreparedWrite::prepare(&schema, t, &update_op(), 2, &[ty(ColType::Text), ty(ColType::BigInt)]).unwrap();
        let conn = fresh(Dialect::Sqlite).await;
        conn.batch("INSERT INTO posts VALUES (1, 'a', 0, NULL), (2, 'b', 0, NULL)".into()).await.unwrap();
        let call = w.call(&Vals(vec![Value::String(None), big(1)])).unwrap();
        let out = w.run(&*conn, call).await.unwrap();
        assert!(matches!(out, Outcome::Affected(1)));
        assert_eq!(table(&*conn).await[0][1], None);
        let err = w.call(&Vals(vec![text("x"), Value::BigInt(None)])).err().expect("an error").to_string();
        assert!(err.contains("NULL filter value"), "{err}");
    });
}

#[test]
fn a_prepared_update_stamps_the_clock_on_each_call() {
    let record = json!({"source":"test.a","model":"Post","field":"changed","on":["update"],"value":"now"});
    let schema = schema(json!({"write_assignments":[record]}));
    let t = target(Dialect::Sqlite, None);
    let w = PreparedWrite::prepare(&schema, t, &update_op(), 2, &[ty(ColType::Text), ty(ColType::BigInt)]).unwrap();
    let vals = Vals(vec![text("x"), big(1)]);
    let first = w.statement(&w.call(&vals).unwrap());
    std::thread::sleep(std::time::Duration::from_millis(20));
    let second = w.statement(&w.call(&vals).unwrap());
    assert_eq!(first.0, second.0);
    assert_ne!(first.1, second.1, "the time of the second call is the time of the first");
}

#[test]
fn a_prepared_delete_removes_the_rows_of_the_call() {
    rt().block_on(async {
        let schema = schema(json!({}));
        let delete = op(json!({"op":"delete","model":"Post","filters":[{"t":"cmp","op":"eq","l":{"t":"col","path":[],"name":"id"},"r":{"t":"param","i":0}}]}));
        let w = PreparedWrite::prepare(&schema, target(Dialect::Sqlite, None), &delete, 1, &[ty(ColType::BigInt)]).unwrap();
        let conn = fresh(Dialect::Sqlite).await;
        conn.batch("INSERT INTO posts VALUES (1, 'a', 0, NULL), (2, 'b', 0, NULL)".into()).await.unwrap();
        let call = w.call(&Vals(vec![big(2)])).unwrap();
        assert!(matches!(w.run(&*conn, call).await.unwrap(), Outcome::Affected(1)));
        assert_eq!(table(&*conn).await.len(), 1);
    });
}

#[test]
fn postgres_rows_write_what_the_unprepared_write_writes() {
    rt().block_on(async {
        let d = Dialect::Postgres;
        for n in [1, 2, 31, 32, 33, 100] {
            same_insert(d, &|| insert_rows(n, 0), None, None).await;
            same_update_many(d, n, None).await;
        }
        for n in [1, 3, 7] {
            same_insert(d, &|| insert_rows(n, 0), Some(7), None).await;
            same_update_many(d, n, Some(9)).await;
        }
        let nulls = || vec![vec![Some(big(1)), Some(Value::String(None)), Some(int(1))], vec![Some(big(2)), None, Some(int(2))]];
        same_insert(d, &nulls, None, None).await;
    });
}
