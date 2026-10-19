//! Subfield paths (`Expr::Col { sub }`) in filter, order, select, group_by and distinct,
//! for `json` and `columns` storage, on PostgreSQL and SQLite.
use std::sync::Arc;
use orm_core::{dialect::{Dialect, Target}, dsl, ir::{ColType, Operation, ValueType}, migrate, schema::Schema};
use orm_engine::{db::{self, Executor}, exec, introspect, plan::Planner, Params, Result};
use sea_query::Value;
use serde_json::{json, Value as J};

type Rows = Vec<Vec<Option<String>>>;

struct Lits(Vec<J>);
impl Params for Lits {
    fn len(&self) -> usize { self.0.len() }
    fn value(&self, i: usize, ty: Option<ValueType>) -> Result<Value> {
        let v = &self.0[i];
        Ok(match ty.map(|t| t.ty) {
            Some(ColType::Int) => Value::Int(Some(v.as_i64().unwrap() as i32)),
            Some(ColType::BigInt) => Value::BigInt(Some(v.as_i64().unwrap())),
            Some(ColType::Float) => Value::Double(Some(v.as_f64().unwrap())),
            Some(ColType::Bool) => Value::Bool(Some(v.as_bool().unwrap())),
            _ => Value::String(Some(v.as_str().unwrap().to_owned())),
        })
    }
    fn text(&self, i: usize) -> Result<String> { Ok(self.0[i].as_str().unwrap().to_owned()) }
    fn count(&self, i: usize) -> Result<u64> { Ok(self.0[i].as_u64().unwrap()) }
}

fn source(dialect: Dialect) -> String {
    let prefix = if dialect == Dialect::Sqlite { "datasource db {\n  provider = \"sqlite\"\n}\n" } else { "" };
    let rent = if dialect == Dialect::Sqlite { "" } else { "  rent Decimal\n" };
    format!("{prefix}type Geo {{\n  lat Float\n  lng Float?\n}}\n\n\
        type Address {{\n  city String\n  floor Int\n  big BigInt\n  open Boolean\n{rent}  opened DateTime\n  geo Geo\n}}\n\n\
        model JShop {{\n  id Int @id\n  address Address @storage(json)\n  @@map(\"jshop\")\n}}\n\n\
        model CShop {{\n  id Int @id\n  address Address\n  @@map(\"cshop\")\n}}\n")
}

pub fn schema(dialect: Dialect) -> Schema {
    dsl::check(dsl::compile(&source(dialect), None).unwrap()).unwrap().1
}

// id, city, floor, big, open, rent, lng, lat
const ROWS: [(i32, &str, i32, &str, bool, &str, Option<f64>, f64); 3] = [
    (1, "Oslo", 3, "9007199254740993", true, "10.50", Some(10.7), 59.9),
    (2, "Bergen", 12, "5", false, "2.25", None, 60.4),
    (3, "Rome", 7, "-1", true, "100", Some(12.5), 41.9),
];

fn inserts(dialect: Dialect) -> String {
    let pg = dialect == Dialect::Postgres;
    let mut sql = String::new();
    for (id, city, floor, big, open, rent, lng, lat) in ROWS {
        let geo = match lng { Some(l) => json!({"lat": lat, "lng": l}), None => json!({"lat": lat}) };
        let mut doc = json!({"city": city, "floor": floor, "big": big, "open": open, "opened": format!("2026-01-0{id}T00:00:00Z"), "geo": geo});
        let (rent_col, rent_val) = if pg { (", address_rent".to_owned(), format!(", {rent}")) } else { (String::new(), String::new()) };
        if pg { doc["rent"] = json!(rent); }
        sql += &format!("INSERT INTO jshop (id, address) VALUES ({id}, '{doc}');\n");
        sql += &format!(
            "INSERT INTO cshop (id, address_city, address_floor, address_big, address_open{rent_col}, address_opened, address_geo_lat, address_geo_lng) \
             VALUES ({id}, '{city}', {floor}, {big}, {open}{rent_val}, '2026-01-0{id}T00:00:00Z', {lat}, {});\n",
            lng.map_or("NULL".to_owned(), |l| l.to_string()));
    }
    sql
}

fn sub(path: &[&str]) -> J { json!({"t":"col","path":[],"name":"address","sub":path}) }
fn id() -> J { json!({"t":"col","path":[],"name":"id"}) }
fn cmp(op: &str, l: J, i: usize) -> J { json!({"t":"cmp","op":op,"l":l,"r":{"t":"param","i":i}}) }
fn item(e: J) -> J { json!({"t":"expr","expr":e}) }
fn asc(e: J) -> J { json!({"expr": e}) }
fn desc(e: J) -> J { json!({"expr": e, "desc": true}) }

struct Run { dialect: Dialect, conn: Arc<dyn Executor>, schema: Schema, model: &'static str }

impl Run {
    async fn run(&self, mut q: J, lits: Vec<J>) -> std::result::Result<Rows, String> {
        let target = Target::new(self.dialect);
        q["op"] = json!("select");
        q["model"] = json!(self.model);
        let op: Operation = serde_json::from_value(q).unwrap();
        let plan = Planner::plan(&self.schema, target, &op, &Lits(lits)).map_err(|e| e.to_string())?;
        let sql = exec::sql(target, &plan);
        self.conn.query_text(sql.clone()).await.map_err(|e| format!("{e}: {sql}"))
    }

    async fn ids(&self, q: J, lits: Vec<J>) -> Vec<String> {
        let mut q = q;
        q["columns"] = json!([item(id())]);
        self.run(q, lits).await.unwrap().into_iter().map(|r| r[0].clone().unwrap()).collect()
    }
}

fn pick<'a>(q: &'a mut J, key: &str, v: J) -> &'a mut J { q[key] = v; q }

async fn scenario(r: &Run) {
    let ids = |filters: Vec<J>, order: Vec<J>, lits: Vec<J>| {
        let mut q = json!({});
        pick(&mut q, "filters", J::Array(filters));
        pick(&mut q, "order", J::Array(order));
        r.ids(q, lits)
    };
    let by_id = vec![asc(id())];
    // Int compares as a number: text order would put 12 before 3.
    assert_eq!(ids(vec![cmp("gt", sub(&["floor"]), 0)], by_id.clone(), vec![json!(5)]).await, ["2", "3"]);
    assert_eq!(ids(vec![], vec![asc(sub(&["floor"]))], vec![]).await, ["1", "3", "2"]);
    // Two levels, Float.
    assert_eq!(ids(vec![cmp("lt", sub(&["geo", "lat"]), 0)], by_id.clone(), vec![json!(50.0)]).await, ["3"]);
    assert_eq!(ids(vec![], vec![desc(sub(&["geo", "lat"]))], vec![]).await, ["2", "1", "3"]);
    // BigInt above 2^53 and negative.
    assert_eq!(ids(vec![cmp("gt", sub(&["big"]), 0)], by_id.clone(), vec![json!(6)]).await, ["1"]);
    assert_eq!(ids(vec![], vec![asc(sub(&["big"]))], vec![]).await, ["3", "2", "1"]);
    // Bool, String.
    assert_eq!(ids(vec![cmp("eq", sub(&["open"]), 0)], by_id.clone(), vec![json!(true)]).await, ["1", "3"]);
    assert_eq!(ids(vec![cmp("eq", sub(&["city"]), 0)], by_id.clone(), vec![json!("Rome")]).await, ["3"]);
    // A missing optional subfield is NULL.
    let null = json!({"t":"is_null","item":sub(&["geo", "lng"])});
    assert_eq!(ids(vec![null], by_id.clone(), vec![]).await, ["2"]);
    // DateTime stays text: ISO order is time order.
    assert_eq!(ids(vec![], vec![desc(sub(&["opened"]))], vec![]).await, ["3", "2", "1"]);
    // select
    let mut q = json!({"columns":[item(id()), item(sub(&["city"])), item(sub(&["geo", "lat"]))], "order": [asc(id())]});
    let rows = r.run(q.take(), vec![]).await.unwrap();
    assert_eq!(rows.iter().map(|x| x[1].clone().unwrap()).collect::<Vec<_>>(), ["Oslo", "Bergen", "Rome"]);
    assert_eq!(rows[2][2].as_deref().map(|v| v.parse::<f64>().unwrap()), Some(41.9));
    // group_by + distinct
    let count = json!({"t":"func","name":"count"});
    let q = json!({"columns":[item(sub(&["open"])), item(count)], "group_by":[sub(&["open"])], "order":[asc(sub(&["open"]))]});
    assert_eq!(r.run(q, vec![]).await.unwrap().iter().map(|x| x[1].clone().unwrap()).collect::<Vec<_>>(), ["1", "2"]);
    let q = json!({"columns":[item(sub(&["open"]))], "distinct": true, "order":[asc(sub(&["open"]))]});
    assert_eq!(r.run(q, vec![]).await.unwrap().len(), 2);
    // errors
    let bad = |path: &[&str]| json!({"columns":[item(sub(path))]});
    for (path, message) in [(&["nope"][..], "nope"), (&["city", "x"], "city")] {
        let e = r.run(bad(path), vec![]).await.unwrap_err();
        assert!(e.contains(message), "{path:?}: {e}");
    }
    let e = r.run(json!({"columns":[item(id())], "distinct_on":[sub(&["city"])]}), vec![]).await.unwrap_err();
    assert!(e.contains("distinct"), "{e}");
}

async fn setup(dialect: Dialect, conn: Arc<dyn Executor>) {
    let s = schema(dialect);
    for sql in migrate::create_statements(&migrate::snapshot(&s).unwrap()) { conn.batch(sql).await.unwrap(); }
    conn.batch(inserts(dialect)).await.unwrap();
}

#[test]
fn sqlite_subfield_paths() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let conn: Arc<dyn Executor> = db::connect("sqlite://:memory:", 1).await.unwrap();
        setup(Dialect::Sqlite, conn.clone()).await;
        for model in ["JShop", "CShop"] {
            scenario(&Run { dialect: Dialect::Sqlite, conn: conn.clone(), schema: schema(Dialect::Sqlite), model }).await;
        }
    });
}

#[test]
fn sqlite_columns_group_subfield_filter_plans_on_every_profile() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let conn: Arc<dyn Executor> = db::connect("sqlite://:memory:", 1).await.unwrap();
        setup(Dialect::Sqlite, conn.clone()).await;
        let r = Run { dialect: Dialect::Sqlite, conn, schema: schema(Dialect::Sqlite), model: "CShop" };
        let q = json!({"filters": [cmp("eq", sub(&["city"]), 0)], "columns": [item(id()), item(sub(&["city"]))]});
        let rows = r.run(q, vec![json!("Rome")]).await.unwrap();
        assert_eq!(rows.iter().map(|x| x[1].clone().unwrap()).collect::<Vec<_>>(), ["Rome"]);
    });
}

#[test]
fn postgres_subfield_paths() {
    let Ok(url) = std::env::var("ORM_RUST_TEST_POSTGRES_URL") else { return };
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let conn: Arc<dyn Executor> = db::connect(&url, 1).await.unwrap();
        conn.batch("DROP SCHEMA IF EXISTS nested_json_paths CASCADE; CREATE SCHEMA nested_json_paths; SET search_path TO nested_json_paths, public".into()).await.unwrap();
        setup(Dialect::Postgres, conn.clone()).await;
        for model in ["JShop", "CShop"] {
            scenario(&Run { dialect: Dialect::Postgres, conn: conn.clone(), schema: schema(Dialect::Postgres), model }).await;
        }
        // Decimal compares as a number.
        let r = Run { dialect: Dialect::Postgres, conn: conn.clone(), schema: schema(Dialect::Postgres), model: "JShop" };
        assert_eq!(r.ids(json!({"order":[desc(sub(&["rent"]))]}), vec![]).await, ["3", "1", "2"]);
        conn.batch("SET search_path TO public; DROP SCHEMA nested_json_paths CASCADE".into()).await.unwrap();
    });
}

const INDEXED: &str = "type Geo {\n  lat Float\n  lng Float?\n}\n\n\
    type Address {\n  city String\n  floor Int\n  geo Geo\n}\n\n\
    model Shop {\n  id Int @id\n  address Address @storage(json)\n  @@index([address.geo.lat], name: \"shop_lat\")\n  @@unique([address.city], name: \"shop_city\")\n  @@index([address.floor(sort: Desc)], name: \"shop_floor\")\n}\n";

#[test]
fn postgres_expression_indexes_serve_subfield_filters_and_round_trip() {
    let Ok(url) = std::env::var("ORM_RUST_TEST_POSTGRES_URL") else { return };
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let conn: Arc<dyn Executor> = db::connect(&url, 1).await.unwrap();
        conn.batch("DROP SCHEMA IF EXISTS nested_json_index CASCADE; CREATE SCHEMA nested_json_index; SET search_path TO nested_json_index, public".into()).await.unwrap();
        let s = dsl::check(dsl::compile(INDEXED, None).unwrap()).unwrap().1;
        let snapshot = migrate::snapshot(&s).unwrap();
        for sql in migrate::create_statements(&snapshot) { conn.batch(sql).await.unwrap(); }
        let rows: Vec<String> = (1..=2000).map(|i| format!("({i}, '{{\"city\": \"c{i}\", \"floor\": {}, \"geo\": {{\"lat\": {i}.5}}}}')", i % 50)).collect();
        conn.batch(format!("INSERT INTO shop (id, address) VALUES {}; ANALYZE shop", rows.join(", "))).await.unwrap();
        let r = Run { dialect: Dialect::Postgres, conn: conn.clone(), schema: dsl::check(dsl::compile(INDEXED, None).unwrap()).unwrap().1, model: "Shop" };
        let target = Target::new(Dialect::Postgres);
        let explain = |filters: Vec<J>, lits: Vec<J>| {
            let op: Operation = serde_json::from_value(json!({"op":"select","model":"Shop","columns":[item(id())],"filters":filters})).unwrap();
            let sql = exec::sql(target, &Planner::plan(&s, target, &op, &Lits(lits)).unwrap());
            let conn = conn.clone();
            async move {
                conn.batch("SET enable_seqscan = off".into()).await.unwrap();
                let plan = conn.query_text(format!("EXPLAIN {sql}")).await.unwrap();
                plan.iter().map(|r| r[0].clone().unwrap()).collect::<Vec<_>>().join("\n")
            }
        };
        assert!(explain(vec![cmp("eq", sub(&["geo", "lat"]), 0)], vec![json!(7.5)]).await.contains("shop_lat"));
        assert!(explain(vec![cmp("eq", sub(&["city"]), 0)], vec![json!("c9")]).await.contains("shop_city"));
        assert!(explain(vec![cmp("eq", sub(&["floor"]), 0)], vec![json!(3)]).await.contains("shop_floor"));
        // a filter on a subfield without an index scans the table
        assert!(!explain(vec![cmp("eq", sub(&["geo", "lng"]), 0)], vec![json!(1.0)]).await.contains("Index"));
        assert_eq!(r.ids(json!({"filters":[cmp("eq", sub(&["geo", "lat"]), 0)]}), vec![json!(7.5)]).await, ["7"]);
        // the unique index rejects a second row with the same city
        assert!(conn.batch("INSERT INTO shop (id, address) VALUES (5000, '{\"city\": \"c9\", \"floor\": 1, \"geo\": {\"lat\": 1}}')".into()).await.is_err());
        // the expression text reads back without a difference
        let live = introspect::introspect(&*conn).await.unwrap();
        let shadow = introspect::materialize(&*conn, &snapshot).await.unwrap();
        let drift = introspect::differences(&live.schema, &shadow.schema);
        assert!(drift.is_empty(), "{drift:?}");
        assert!(migrate::plan(&s, &snapshot).unwrap().up.is_empty());
        conn.batch("SET search_path TO public; DROP SCHEMA nested_json_index CASCADE".into()).await.unwrap();
    });
}
