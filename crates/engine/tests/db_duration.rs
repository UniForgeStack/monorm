//! A Duration column on both databases: whole microseconds through the interval carrier of `monorm_engine::params`.
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
    schema_of(SOURCE, dialect)
}

fn schema_of(source: &str, dialect: Dialect) -> monorm_core::schema::Schema {
    let mut ir = dsl::compile(source, None).unwrap();
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

fn micros(v: i64) -> Value { monorm_engine::params::duration(Some(v)) }
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
    let rows = values.iter().enumerate().map(|(i, v)| vec![Some(Value::Int(Some(i as i32 + 1))), Some(monorm_engine::params::duration(*v))]).collect();
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

    // in_(param()) takes a list of Durations as one parameter.
    let one = json!({"t":"in_param","item":col(),"i":0});
    let array = |v: &[i64]| Value::Array(monorm_engine::params::array_type(ColType::Duration), Some(Box::new(v.iter().map(|v| micros(*v)).collect())));
    let found = select(conn, &schema, target, read(vec![col()], vec![one.clone()], true), vec![array(&[999 * DAY, -1, i64::MIN])]).await;
    assert_eq!(found, vec![Some(i64::MIN), Some(-1), Some(999 * DAY)], "{dialect:?}");
    let none = json!({"t":"in_param","item":col(),"i":0,"neg":true});
    let rest = select(conn, &schema, target, read(vec![col()], vec![none, cmp("lt", 1)], true), vec![array(&[0, 1]), micros(3)]).await;
    assert_eq!(rest, vec![Some(i64::MIN), Some(-1), Some(2)], "{dialect:?}");

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
    let (sql, values) = db::build(Dialect::Postgres, &p.stmt);
    assert!(sql.contains("$1::\"interval\""), "{sql}");
    assert_eq!(values.len(), 1);
    assert_eq!(values[0], Value::String(Some("1 microseconds".into())));
}

#[test]
fn postgres_copy_loads_a_duration_column() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let db = db::connect(&postgres_url(), 1).await.unwrap();
        let schema = schema_of(&SOURCE.replace("duration_jobs", "duration_copy_jobs"), Dialect::Postgres);
        let target = Target::new(Dialect::Postgres);
        db.batch("DROP TABLE IF EXISTS duration_copy_jobs".into()).await.unwrap();
        for statement in monorm_core::migrate::create_all(&schema).unwrap() { db.batch(statement).await.unwrap(); }
        let values = [Some(0), Some(-1), Some(i64::MAX), Some(i64::MIN), None, Some(36 * 3_600_000_000)];
        let rows = values.iter().enumerate().map(|(i, v)| vec![Some(Value::Int(Some(i as i32 + 1))), Some(monorm_engine::params::duration(*v))]).collect();
        let fields = ["id", "timeout"].map(str::to_owned);
        let copy = exec::plan_copy(&schema, target, "Job", &fields, rows).unwrap();
        assert_eq!(exec::run_copy(db.as_ref(), copy).await.unwrap(), 6);
        let all = select(db.as_ref(), &schema, target, read(vec![col()], vec![], false), vec![]).await;
        let mut got = all.clone();
        got.sort();
        let mut want = values.to_vec();
        want.sort();
        assert_eq!(got, want);
        db.batch("DROP TABLE duration_copy_jobs".into()).await.unwrap();
        db.close().await;
    });
}
