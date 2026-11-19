//! A Duration column on both databases: whole microseconds through a `Value::BigInt` carrier.
use monorm_core::{dialect::{Dialect, Target}, dsl, ir::{ColType, Operation, ValueType}};
use monorm_engine::{db::{self, Cell, Executor}, exec::{self, Outcome}, params::NoParams, plan::Planner, Params, Result};
use sea_query::Value;
use serde_json::{json, Value as Json};

const SOURCE: &str = r#"
model Job {
 id Int @id
 timeout Duration?
 @@map("duration_jobs")
}
"#;

const DAY: i64 = 86_400_000_000;

fn schema_for(dialect: Dialect) -> monorm_core::schema::Schema {
    let mut ir = dsl::compile(SOURCE, None).unwrap();
    ir.dialect = dialect;
    dsl::check(ir).unwrap().1
}

struct Args(Vec<Value>);
impl Params for Args {
    fn len(&self) -> usize { self.0.len() }
    fn value(&self, i: usize, _: Option<ValueType>) -> Result<Value> { Ok(self.0[i].clone()) }
    fn text(&self, _: usize) -> Result<String> { unreachable!() }
    fn count(&self, _: usize) -> Result<u64> { unreachable!() }
}

fn micros(v: i64) -> Value { Value::BigInt(Some(v)) }
fn col() -> Json { json!({"t":"col","path":[],"name":"timeout"}) }
fn param(i: usize) -> Json { json!({"t":"param","i":i}) }
fn cmp(op: &str, i: usize) -> Json { json!({"t":"cmp","op":op,"l":col(),"r":param(i)}) }
fn func(name: &str) -> Json { json!({"t":"func","name":name,"args":[col()]}) }

/// The `timeout` values of a select, in order, as microseconds.
async fn select(conn: &dyn Executor, schema: &monorm_core::schema::Schema, target: Target, op: Json, args: Vec<Value>) -> Vec<Option<i64>> {
    let op: Operation = serde_json::from_value(op).unwrap();
    let plan = Planner::plan(schema, target, &op, &Args(args)).unwrap();
    let Outcome::Select(s) = exec::run(conn, target, plan).await.unwrap() else { panic!("select") };
    (0..s.rows.len()).map(|r| match s.rows.cell(r, 0, ValueType::scalar(ColType::Duration)).unwrap() {
        Cell::Null => None,
        Cell::Duration(v) => Some(v),
        other => panic!("{other:?}"),
    }).collect()
}

fn read(columns: Vec<Json>, filters: Vec<Json>, order: bool) -> Json {
    let columns: Vec<Json> = columns.into_iter().map(|e| json!({"t":"expr","expr":e})).collect();
    let mut op = json!({"op":"select","model":"Job","columns":columns,"filters":filters});
    if order { op["order"] = json!([{"expr":col()}]); }
    op
}

async fn exercise(conn: &dyn Executor, dialect: Dialect) {
    let schema = schema_for(dialect);
    let target = Target::new(dialect);
    for statement in monorm_core::migrate::create_all(&schema).unwrap() { conn.batch(statement).await.unwrap(); }
    let values = [Some(0), Some(1), Some(-1), Some(999 * DAY), Some(i64::MAX), Some(i64::MIN), None, Some(2), Some(3_600_000_000)];
    let rows = values.iter().enumerate().map(|(i, v)| vec![Some(Value::Int(Some(i as i32 + 1))), Some(Value::BigInt(*v))]).collect();
    let fields = ["id", "timeout"].map(str::to_owned);
    let plan = exec::plan_insert(&schema, target, "Job", &fields, rows, None, &NoParams, &Default::default()).unwrap();
    exec::run(conn, target, plan).await.unwrap();

    // Every value comes back as it went in, and an order sorts by the duration.
    let all = select(conn, &schema, target, read(vec![col()], vec![], true), vec![]).await;
    let mut sorted: Vec<Option<i64>> = values.iter().flatten().map(|v| Some(*v)).collect();
    sorted.sort();
    let non_null: Vec<_> = all.iter().filter(|v| v.is_some()).cloned().collect();
    assert_eq!(non_null, sorted, "{dialect:?}");
    assert_eq!(all.iter().filter(|v| v.is_none()).count(), 1);

    // Comparison, between and in.
    let gt = select(conn, &schema, target, read(vec![col()], vec![cmp("gt", 0)], true), vec![micros(1)]).await;
    assert_eq!(gt, vec![Some(2), Some(3_600_000_000), Some(999 * DAY), Some(i64::MAX)]);
    let le = select(conn, &schema, target, read(vec![col()], vec![cmp("le", 0)], true), vec![micros(0)]).await;
    assert_eq!(le, vec![Some(i64::MIN), Some(-1), Some(0)]);
    let eq = select(conn, &schema, target, read(vec![col()], vec![cmp("eq", 0)], false), vec![micros(-1)]).await;
    assert_eq!(eq, vec![Some(-1)]);
    let between = json!({"t":"and","items":[cmp("ge", 0), cmp("le", 1)]});
    let range = select(conn, &schema, target, read(vec![col()], vec![between], true), vec![micros(1), micros(2)]).await;
    assert_eq!(range, vec![Some(1), Some(2)]);
    let list = json!({"t":"in","item":col(),"values":[param(0), param(1)]});
    let found = select(conn, &schema, target, read(vec![col()], vec![list], true), vec![micros(-1), micros(999 * DAY)]).await;
    assert_eq!(found, vec![Some(-1), Some(999 * DAY)]);

    // Aggregates return a Duration; an empty sum is NULL.
    let small = json!({"t":"and","items":[cmp("ge", 0), cmp("le", 1)]});
    for (name, expected) in [("sum", Some(3)), ("min", Some(1)), ("max", Some(2)), ("avg", Some(2))] {
        let got = select(conn, &schema, target, read(vec![func(name)], vec![small.clone()], false), vec![micros(1), micros(2)]).await;
        assert_eq!(got, vec![expected], "{name} on {dialect:?}");
    }
    let none = select(conn, &schema, target, read(vec![func("sum")], vec![cmp("gt", 0)], false), vec![micros(i64::MAX)]).await;
    assert_eq!(none, vec![None]);
    let hours = select(conn, &schema, target, read(vec![func("sum")], vec![cmp("eq", 0)], false), vec![micros(3_600_000_000)]).await;
    assert_eq!(hours, vec![Some(3_600_000_000)]);
}

#[test]
fn sqlite_keeps_whole_microseconds() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let db = db::connect("sqlite://:memory:", 1).await.unwrap();
        exercise(db.as_ref(), Dialect::Sqlite).await;
        db.close().await;
    });
}

fn postgres_url() -> String {
    std::env::var("ORM_RUST_TEST_POSTGRES_URL").or_else(|_| std::env::var("ORM_TEST_DATABASE_URL")).unwrap_or_else(|_| "postgres://postgres:postgres@localhost/orm_test".into())
}

#[test]
fn postgres_keeps_whole_microseconds() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let db = db::connect(&postgres_url(), 1).await.unwrap();
        let conn = db.begin().await.unwrap();
        exercise(conn.as_ref(), Dialect::Postgres).await;
        conn.rollback().await.unwrap();
        db.close().await;
    });
}

#[test]
fn postgres_reads_a_month_as_30_days_and_a_year_as_360() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let db = db::connect(&postgres_url(), 1).await.unwrap();
        let rows = db.query("SELECT CAST('1 month 2 days' AS interval), CAST('1 year' AS interval), CAST('-1 microsecond' AS interval)".into(), vec![]).await.unwrap();
        let read = |c| match rows.cell(0, c, ValueType::scalar(ColType::Duration)).unwrap() { Cell::Duration(v) => v, other => panic!("{other:?}") };
        assert_eq!(read(0), 32 * DAY);
        assert_eq!(read(1), 360 * DAY);
        assert_eq!(read(2), -1);
        db.close().await;
    });
}

#[test]
fn the_postgres_column_is_an_interval_and_the_sqlite_column_an_integer() {
    let sql = |d| monorm_core::migrate::create_all(&schema_for(d)).unwrap().join(" ");
    assert!(sql(Dialect::Postgres).contains("\"timeout\" interval"), "{}", sql(Dialect::Postgres));
    assert!(sql(Dialect::Sqlite).contains("\"timeout\" INTEGER"), "{}", sql(Dialect::Sqlite));
}

#[test]
fn a_postgres_parameter_builds_the_interval_from_its_microseconds() {
    let schema = schema_for(Dialect::Postgres);
    let op: Operation = serde_json::from_value(read(vec![col()], vec![cmp("gt", 0)], false)).unwrap();
    let plan = Planner::plan(&schema, Target::new(Dialect::Postgres), &op, &Args(vec![micros(1)])).unwrap();
    let monorm_engine::plan::Plan::Select(p) = plan else { panic!() };
    let sql = db::build(Dialect::Postgres, &p.stmt).0;
    assert!(sql.contains("CAST(CAST($1 AS text) || ' microseconds' AS interval)"), "{sql}");
}
