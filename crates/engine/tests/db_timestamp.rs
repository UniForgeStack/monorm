use chrono::{TimeZone, Utc};
use monorm_core::{dialect::{Dialect, Target}, dsl, ir::{Operation, ValueType}};
use monorm_engine::{db::{self, Cell}, exec::{self, Outcome}, params::NoParams, plan::Planner, Params, Result};
use sea_query::{ArrayType, Value};
use serde_json::json;

const SOURCE: &str = r#"
model Stamp {
 id Int @id
 at DateTime @db.Timestamp(6)
 ats DateTime[] @db.Timestamp(6)
 @@map("stamp_rows")
}
"#;

fn compile() -> monorm_core::schema::Schema {
    dsl::check(dsl::compile(SOURCE, None).unwrap()).unwrap().1
}

fn when() -> Value {
    Value::ChronoDateTimeWithTimeZone(Some(Utc.with_ymd_and_hms(2024, 1, 2, 7, 8, 9).unwrap().with_timezone(&Utc).fixed_offset() + chrono::Duration::microseconds(123_456)))
}

fn list() -> Value { Value::Array(ArrayType::ChronoDateTimeWithTimeZone, Some(Box::new(vec![when()]))) }

struct Stamp(Value);
impl Params for Stamp {
    fn len(&self) -> usize { 1 }
    fn value(&self, _: usize, _: Option<ValueType>) -> Result<Value> { Ok(self.0.clone()) }
    fn text(&self, _: usize) -> Result<String> { unreachable!() }
    fn count(&self, _: usize) -> Result<u64> { unreachable!() }
}

async fn keeps_the_utc_wall_clock(url: &str, zone: &str) {
    let dialect = Dialect::Postgres;
    let schema = compile();
    let target = Target::new(dialect);
    let db = db::connect(url, 1).await.unwrap();
    let conn = db.begin().await.unwrap();
    for statement in monorm_core::migrate::create_all(&schema).unwrap() { conn.batch(statement).await.unwrap(); }
    conn.batch(format!("SET LOCAL TIME ZONE '{zone}'")).await.unwrap();
    let fields = ["id", "at", "ats"].map(str::to_owned);
    let row = vec![Some(Value::Int(Some(1))), Some(when()), Some(list())];
    let plan = exec::plan_insert(&schema, target, "Stamp", &fields, vec![row], None, &NoParams, &Default::default()).unwrap();
    let Outcome::Rows { rows, .. } = exec::run(conn.as_ref(), target, plan).await.unwrap() else { panic!() };
    let types: Vec<ValueType> = schema.model(schema.model_idx("Stamp").unwrap()).fields().iter().map(|f| f.value_type()).collect();
    let expected = Utc.with_ymd_and_hms(2024, 1, 2, 7, 8, 9).unwrap() + chrono::Duration::microseconds(123_456);
    assert_eq!(rows.cell(0, 1, types[1]).unwrap(), Cell::DateTime(expected));
    let stored = conn.query_text("SELECT at::text, ats::text FROM stamp_rows".into()).await.unwrap();
    assert_eq!(stored, vec![vec![Some("2024-01-02 07:08:09.123456".to_owned()), Some("{\"2024-01-02 07:08:09.123456\"}".to_owned())]]);
    for (field, value) in [("at", when()), ("ats", list())] {
        let op: Operation = serde_json::from_value(json!({"op":"count","model":"Stamp","filters":[{"t":"cmp","op":"eq","l":{"t":"col","path":[],"name":field},"r":{"t":"param","i":0}}]})).unwrap();
        let plan = Planner::plan(&schema, target, &op, &Stamp(value)).unwrap();
        let Outcome::Count(n) = exec::run(conn.as_ref(), target, plan).await.unwrap() else { panic!() };
        assert_eq!(n, 1, "an equality filter on {field}");
    }
    conn.rollback().await.unwrap();
    db.close().await;
}

fn postgres_url() -> String {
    std::env::var("ORM_RUST_TEST_POSTGRES_URL").or_else(|_| std::env::var("ORM_TEST_DATABASE_URL")).unwrap_or_else(|_| "postgres://postgres:postgres@localhost/orm_test".into())
}

#[test]
fn postgres_timestamp_keeps_the_utc_wall_clock_under_a_session_zone() {
    let url = postgres_url();
    for zone in ["Asia/Tehran", "Pacific/Honolulu"] {
        tokio::runtime::Runtime::new().unwrap().block_on(keeps_the_utc_wall_clock(&url, zone));
    }
}
