//! Arithmetic on a DateTime and a Duration (the D7 table) on both databases: the same microseconds from each.
use chrono::SecondsFormat;
use monorm_core::{dialect::{Dialect, Target}, dsl, ir::{ColType, Operation, ValueType}};
use monorm_engine::{db::{self, Cell, Executor}, exec::{self, Outcome}, params::NoParams, plan::Planner, prepared::PreparedRead, Params, Result};
use sea_query::Value;
use serde_json::{json, Value as Json};

const SOURCE: &str = r#"
model Event {
 id Int @id
 at DateTime
 until DateTime
 span Duration
 late DateTime?
 rest Duration?
 day DateTime @db.Date
 @@map("duration_arith_events")
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

fn micros(v: i64) -> Value { monorm_engine::params::duration(Some(v)) }
fn col(name: &str) -> Json { json!({"t":"col","path":[],"name":name}) }
fn param(i: usize) -> Json { json!({"t":"param","i":i}) }
fn arith(op: &str, l: Json, r: Json) -> Json { json!({"t":"arith","op":op,"l":l,"r":r}) }
fn stamp(s: &str) -> Value { Value::ChronoDateTimeWithTimeZone(Some(chrono::DateTime::parse_from_rfc3339(s).unwrap())) }

fn read(expr: Json) -> Operation {
    serde_json::from_value(json!({"op":"select","model":"Event","columns":[{"t":"expr","expr":expr}],"order":[{"expr":col("id")}]})).unwrap()
}

/// The first column of every row, ordered by id: a Duration as its microseconds, a DateTime as RFC 3339 to the microsecond.
async fn calc(conn: &dyn Executor, schema: &monorm_core::schema::Schema, target: Target, expr: Json, args: Vec<Value>, ty: ColType) -> Vec<String> {
    let plan = Planner::plan(schema, target, &read(expr), &Args(args)).unwrap();
    let Outcome::Select(s) = exec::run(conn, target, plan).await.unwrap() else { panic!("select") };
    (0..s.rows.len()).map(|r| match s.rows.cell(r, 0, ValueType::scalar(ty)).unwrap() {
        Cell::Duration(v) => v.to_string(),
        Cell::DateTime(t) => t.to_rfc3339_opts(SecondsFormat::Micros, true),
        Cell::Null => "null".to_owned(),
        other => panic!("{other:?}"),
    }).collect()
}

fn day() -> Value { Value::ChronoDate(Some(chrono::NaiveDate::from_ymd_opt(2024, 1, 1).unwrap())) }

fn strings(v: &[&str]) -> Vec<String> { v.iter().map(|s| s.to_string()).collect() }

async fn exercise(conn: &dyn Executor, dialect: Dialect) {
    let schema = schema_for(dialect);
    let target = Target::new(dialect);
    for statement in monorm_core::migrate::create_all(&schema).unwrap() { conn.batch(statement).await.unwrap(); }
    let rows = vec![
        vec![Some(Value::Int(Some(1))), Some(stamp("2024-01-01T00:00:00Z")), Some(stamp("2024-01-01T00:00:00.000001Z")), Some(micros(DAY)), Some(day())],
        vec![Some(Value::Int(Some(2))), Some(stamp("2024-03-01T12:00:00.5Z")), Some(stamp("2024-03-01T12:00:00Z")), Some(micros(10)), Some(day())],
        vec![Some(Value::Int(Some(3))), Some(stamp("2024-01-01T00:00:00Z")), Some(stamp("2024-01-01T00:00:00Z")), Some(micros(11)), Some(day())],
    ];
    let fields = ["id", "at", "until", "span", "day"].map(str::to_owned);
    let plan = exec::plan_insert(&schema, target, "Event", &fields, rows, None, &NoParams, &Default::default()).unwrap();
    exec::run(conn, target, plan).await.unwrap();

    let (at, until, span) = (col("at"), col("until"), col("span"));
    let dt = |e: Json, args: Vec<Value>| { let schema = &schema; async move { calc(conn, schema, target, e, args, ColType::DateTime).await } };
    let du = |e: Json, args: Vec<Value>| { let schema = &schema; async move { calc(conn, schema, target, e, args, ColType::Duration).await } };
    let tag = format!("{dialect:?}");

    // DateTime +/- Duration and Duration + DateTime, to the microsecond.
    let plus = strings(&["2024-01-02T00:00:00.000000Z", "2024-03-01T12:00:00.500010Z", "2024-01-01T00:00:00.000011Z"]);
    assert_eq!(dt(arith("add", at.clone(), span.clone()), vec![]).await, plus, "{tag}");
    assert_eq!(dt(arith("add", span.clone(), at.clone()), vec![]).await, plus, "{tag}");
    let minus = strings(&["2023-12-31T00:00:00.000000Z", "2024-03-01T12:00:00.499990Z", "2023-12-31T23:59:59.999989Z"]);
    assert_eq!(dt(arith("sub", at.clone(), span.clone()), vec![]).await, minus, "{tag}");

    // A Duration parameter: half a second reads back as .500, one microsecond as .000001.
    let half = dt(arith("add", at.clone(), param(0)), vec![micros(500_000)]).await;
    assert_eq!(half, strings(&["2024-01-01T00:00:00.500000Z", "2024-03-01T12:00:01.000000Z", "2024-01-01T00:00:00.500000Z"]), "{tag}");
    let one = dt(arith("add", at.clone(), param(0)), vec![micros(1)]).await;
    assert_eq!(one[0], "2024-01-01T00:00:00.000001Z", "{tag}");

    // DateTime - DateTime is a Duration; the sign follows the operands.
    assert_eq!(du(arith("sub", until.clone(), at.clone()), vec![]).await, strings(&["1", "-500000", "0"]), "{tag}");
    assert_eq!(du(arith("sub", at.clone(), until.clone()), vec![]).await, strings(&["-1", "500000", "0"]), "{tag}");

    // Duration +/- Duration.
    assert_eq!(du(arith("add", span.clone(), span.clone()), vec![]).await, strings(&["172800000000", "20", "22"]), "{tag}");
    assert_eq!(du(arith("sub", span.clone(), arith("mul", span.clone(), json!({"t":"int","value":3}))), vec![]).await, strings(&["-172800000000", "-20", "-22"]), "{tag}");

    // Duration * n, n * Duration and Duration / n, with a number parameter of each kind.
    let three = vec![Value::Int(Some(3))];
    assert_eq!(du(arith("mul", span.clone(), param(0)), three.clone()).await, strings(&["259200000000", "30", "33"]), "{tag}");
    assert_eq!(du(arith("mul", param(0), span.clone()), three.clone()).await, strings(&["259200000000", "30", "33"]), "{tag}");
    assert_eq!(du(arith("mul", param(0), span.clone()), vec![Value::BigInt(Some(3))]).await, strings(&["259200000000", "30", "33"]), "{tag}");
    // Rounding to the nearest microsecond: 10 / 3 = 3.33 -> 3, 11 / 3 = 3.67 -> 4.
    assert_eq!(du(arith("div", span.clone(), param(0)), three).await, strings(&["28800000000", "3", "4"]), "{tag}");
    assert_eq!(du(arith("mul", span.clone(), param(0)), vec![Value::Double(Some(0.3))]).await, strings(&["25920000000", "3", "3"]), "{tag}");

    // NULL in, NULL out, for every step.
    let nulls = strings(&["null", "null", "null"]);
    assert_eq!(dt(arith("add", at.clone(), col("rest")), vec![]).await, nulls, "{tag}");
    assert_eq!(dt(arith("sub", col("late"), span.clone()), vec![]).await, nulls, "{tag}");
    assert_eq!(du(arith("sub", col("late"), at.clone()), vec![]).await, nulls, "{tag}");
    assert_eq!(du(arith("add", col("rest"), span.clone()), vec![]).await, nulls, "{tag}");
    assert_eq!(du(arith("mul", col("rest"), param(0)), vec![Value::Int(Some(2))]).await, nulls, "{tag}");
    assert_eq!(du(arith("div", col("rest"), param(0)), vec![Value::Int(Some(2))]).await, nulls, "{tag}");

    // A difference over a day and over a month: PostgreSQL reports days, and the decode counts them as 24 hours.
    let month = (60 * DAY + 43_200_500_000).to_string();
    let day_before = vec![stamp("2023-12-31T00:00:00Z")];
    assert_eq!(du(arith("sub", at.clone(), param(0)), day_before).await, strings(&["86400000000", &(61 * DAY + 43_200_500_000).to_string(), "86400000000"]), "{tag}");
    let first = vec![stamp("2024-01-01T00:00:00Z")];
    assert_eq!(du(arith("sub", at.clone(), param(0)), first).await, strings(&["0", &month, "0"]), "{tag}");

    // A parameter on the left of a subtraction is a DateTime.
    let next = vec![stamp("2024-01-02T00:00:00Z")];
    assert_eq!(du(arith("sub", param(0), at.clone()), next).await, strings(&["86400000000", &(-(59 * DAY + 43_200_500_000)).to_string(), "86400000000"]), "{tag}");

    // A float parameter scales from either side, to the nearest microsecond.
    assert_eq!(du(arith("mul", param(0), span.clone()), vec![Value::Double(Some(0.3))]).await, strings(&["25920000000", "3", "3"]), "{tag}");

    // Errors run last. Each one is rolled back to a savepoint, because an error aborts a PostgreSQL transaction.
    let fails = |e: Json, args: Vec<Value>, what: &'static str| { let schema = &schema; let tag = tag.clone(); async move {
        conn.batch("SAVEPOINT arith".into()).await.unwrap();
        let plan = Planner::plan(schema, target, &read(e), &Args(args)).unwrap();
        let err = match exec::run(conn, target, plan).await { Ok(_) => panic!("{tag}: {what} must fail"), Err(e) => e.to_string() };
        conn.batch("ROLLBACK TO SAVEPOINT arith".into()).await.unwrap();
        err
    } };
    let int = |n: i64| vec![Value::BigInt(Some(n))];
    for zero in [Value::Int(Some(0)), Value::Double(Some(0.0))] {
        let err = fails(arith("div", span.clone(), param(0)), vec![zero], "a division by zero").await;
        assert!(err.contains("division by zero"), "{tag}: {err}");
    }
    for (e, args, what) in [
        (arith("mul", span.clone(), param(0)), int(i64::MAX), "a product out of range"),
        (arith("mul", param(0), span.clone()), int(i64::MAX), "a product out of range"),
        (arith("add", span.clone(), param(0)), vec![micros(i64::MAX)], "a sum out of range"),
        (arith("sub", span.clone(), param(0)), vec![micros(-i64::MAX)], "a difference out of range"),
        (arith("add", at.clone(), param(0)), vec![micros(i64::MAX)], "a DateTime above the range"),
        (arith("sub", at.clone(), param(0)), vec![micros(i64::MAX)], "a DateTime below the range"),
    ] {
        let err = fails(e, args, what).await;
        assert!(err.contains("out of"), "{tag}: {what}: {err}");
    }
}

/// `Date + Duration` with a Duration parameter: PostgreSQL would give a timestamp and SQLite an integer, so both refuse it.
#[test]
fn a_date_column_and_a_duration_parameter_are_a_plan_error() {
    for dialect in [Dialect::Sqlite, Dialect::Postgres] {
        let schema = schema_for(dialect);
        for expr in [arith("add", col("day"), param(0)), arith("add", param(0), col("day")), arith("sub", col("day"), param(0))] {
            let err = Planner::plan(&schema, Target::new(dialect), &read(expr), &Args(vec![micros(DAY / 2)])).err().expect("a plan error").to_string();
            assert!(err.contains("Date") && err.contains("Duration") && err.contains("is not defined"), "{dialect:?}: {err}");
        }
    }
}

async fn plan_error(dialect: Dialect, expr: Json) -> String {
    let schema = schema_for(dialect);
    match Planner::plan(&schema, Target::new(dialect), &read(expr), &Args(vec![Value::Int(Some(2))])) {
        Ok(_) => panic!("{dialect:?}: the pair must be a plan error"),
        Err(e) => e.to_string(),
    }
}

#[test]
fn an_unlisted_pair_is_a_plan_error() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        for dialect in [Dialect::Sqlite, Dialect::Postgres] {
            for (expr, pair) in [
                (arith("add", col("at"), col("until")), "DateTime + DateTime"),
                (arith("mul", col("at"), param(0)), "DateTime * "),
                (arith("mul", col("span"), col("span")), "Duration * Duration"),
                (arith("div", col("span"), col("span")), "Duration / Duration"),
                (arith("sub", col("span"), col("at")), "Duration - DateTime"),
                (arith("div", param(0), col("span")), " / Duration"),
                (arith("add", col("day"), col("span")), "Date + Duration"),
            ] {
                let msg = plan_error(dialect, expr).await;
                assert!(msg.contains(pair) && msg.contains("is not defined"), "{dialect:?}: {msg}");
            }
        }
    });
}

#[test]
fn sqlite_does_duration_arithmetic() {
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
fn postgres_does_duration_arithmetic() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let db = db::connect(&postgres_url(), 1).await.unwrap();
        let conn = db.begin().await.unwrap();
        exercise(conn.as_ref(), Dialect::Postgres).await;
        conn.rollback().await.unwrap();
        db.close().await;
    });
}

#[test]
fn the_sqlite_functions_run_only_from_a_statement_monorm_built() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let db = db::connect("sqlite://:memory:", 1).await.unwrap();
        db.batch("CREATE VIEW unsafe_view AS SELECT monorm_dt_add('2024-01-01 00:00:00', 1) AS t".into()).await.unwrap();
        let err = match db.query("SELECT t FROM unsafe_view".into(), vec![]).await { Ok(_) => panic!("a view must not call a DIRECTONLY function"), Err(e) => e.to_string() };
        assert!(err.contains("unsafe use of"), "{err}");
        db.close().await;
    });
}

#[test]
fn a_prepared_arithmetic_param_without_a_declared_type_names_the_arithmetic() {
    for dialect in [Dialect::Sqlite, Dialect::Postgres] {
        let schema = schema_for(dialect);
        let err = PreparedRead::prepare(&schema, Target::new(dialect), &read(arith("sub", col("at"), param(0))), 1, &[]).err().expect("an error").to_string();
        assert!(err.contains("arithmetic operand") && err.contains("declare its type"), "{dialect:?}: {err}");
    }
}
