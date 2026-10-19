//! Whole-field tests, patch and replace updates, result objects and insert nulls of a nested
//! field, for `json` and `columns` storage, on PostgreSQL and SQLite.
use std::sync::Arc;
use orm_core::{dialect::{Dialect, Target}, dsl, ir::{ColType, Operation, ValueType}, migrate, schema::Schema};
use orm_engine::{db::{self, Executor}, exec, plan::{plan_insert, Plan, Planner}, Params, Result};
use sea_query::Value;
use serde_json::{json, Value as J};

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
    format!("{prefix}type Geo {{\n  lat Float\n  lng Float?\n}}\n\n\
        type Address {{\n  city String\n  floor Int @default(7)\n  big BigInt\n  open Boolean\n  opened DateTime\n  geo Geo?\n}}\n\n\
        model JShop {{\n  id Int @id\n  address Address? @storage(json)\n  @@map(\"jshop\")\n}}\n\n\
        model CShop {{\n  id Int @id\n  address Address?\n  @@map(\"cshop\")\n}}\n")
}

fn schema(dialect: Dialect) -> Schema {
    dsl::check(dsl::compile(&source(dialect), None).unwrap()).unwrap().1
}

// id, city, floor, big, open, geo: lat, lng. Row 3 has no geo; row 4 has no address.
type Row = (i32, &'static str, i32, &'static str, bool, Option<(f64, Option<f64>)>);
const ROWS: [Row; 3] = [
    (1, "Oslo", 3, "9007199254740993", true, Some((59.9, Some(10.7)))),
    (2, "Bergen", 12, "5", false, Some((60.4, None))),
    (3, "Rome", 7, "-1", true, None),
];

fn inserts() -> String {
    let mut sql = String::new();
    for (id, city, floor, big, open, geo) in ROWS {
        let mut doc = json!({"city": city, "floor": floor, "big": big, "open": open, "opened": format!("2026-01-0{id}T00:00:00Z")});
        if let Some((lat, lng)) = geo {
            doc["geo"] = lng.map_or(json!({"lat": lat}), |l| json!({"lat": lat, "lng": l}));
        }
        sql += &format!("INSERT INTO jshop (id, address) VALUES ({id}, '{doc}');\n");
        let (lat, lng) = match geo {
            Some((lat, lng)) => (lat.to_string(), lng.map_or("NULL".into(), |l| l.to_string())),
            None => ("NULL".into(), "NULL".into()),
        };
        sql += &format!(
            "INSERT INTO cshop (id, address_city, address_floor, address_big, address_open, address_opened, address_geo_lat, address_geo_lng) \
             VALUES ({id}, '{city}', {floor}, {big}, {open}, '2026-01-0{id}T00:00:00Z', {lat}, {lng});\n");
    }
    sql += "INSERT INTO jshop (id, address) VALUES (4, NULL);\nINSERT INTO cshop (id, address_floor) VALUES (4, NULL);\n";
    sql
}

fn col(name: &str, sub: &[&str]) -> J { json!({"t":"col","path":[],"name":name,"sub":sub}) }
fn address(sub: &[&str]) -> J { col("address", sub) }
fn param(i: usize) -> J { json!({"t":"param","i":i}) }
fn cmp(op: &str, l: J, r: J) -> J { json!({"t":"cmp","op":op,"l":l,"r":r}) }
fn object(fields: Vec<(&str, J)>) -> J { json!({"t":"object","fields":fields.into_iter().map(|(n, v)| json!({"name":n,"value":v})).collect::<Vec<_>>()}) }
fn null() -> J { json!({"t":"null"}) }
fn is_null(e: J, neg: bool) -> J { json!({"t":"is_null","item":e,"neg":neg}) }
fn set(sub: &[&str], value: J) -> J { json!({"field":"address","sub":sub,"value":value}) }

/// Objects without null members, and DateTime without fraction digits: the two storages agree on the rest.
fn normal(v: J) -> J {
    match v {
        J::Object(m) => J::Object(m.into_iter().filter(|(_, v)| !v.is_null()).map(|(k, v)| (k, normal(v))).collect()),
        J::String(s) => J::String(s.replace(".000000Z", "Z").replace(".000Z", "Z")),
        v => v,
    }
}

struct Run { dialect: Dialect, conn: Arc<dyn Executor>, schema: Schema, model: &'static str }

impl Run {
    fn plan(&self, mut q: J, op: &str, lits: Vec<J>) -> std::result::Result<Plan, String> {
        q["op"] = json!(op);
        q["model"] = json!(self.model);
        let op: Operation = serde_json::from_value(q).unwrap();
        Planner::plan(&self.schema, Target::new(self.dialect), &op, &Lits(lits)).map_err(|e| e.to_string())
    }

    async fn select(&self, q: J, lits: Vec<J>) -> std::result::Result<Vec<Vec<Option<String>>>, String> {
        let plan = self.plan(q, "select", lits)?;
        let sql = exec::sql(Target::new(self.dialect), &plan);
        self.conn.query_text(sql.clone()).await.map_err(|e| format!("{e}: {sql}"))
    }

    async fn ids(&self, filter: J, lits: Vec<J>) -> Vec<String> {
        let q = json!({"columns":[{"t":"expr","expr":col("id", &[])}], "filters":[filter], "order":[{"expr":col("id", &[])}]});
        self.select(q, lits).await.unwrap().into_iter().map(|r| r[0].clone().unwrap()).collect()
    }

    async fn update(&self, id: i32, set: Vec<J>, mut lits: Vec<J>) -> std::result::Result<(), String> {
        let q = json!({"filters":[cmp("eq", col("id", &[]), param(lits.len()))], "set": set});
        lits.push(json!(id));
        let plan = self.plan(q, "update", lits)?;
        let sql = exec::sql(Target::new(self.dialect), &plan);
        self.conn.batch(sql.clone()).await.map(|_| ()).map_err(|e| format!("{e}: {sql}"))
    }

    /// The whole field of each row, normalised; `None` for a null field.
    async fn read(&self, id: i32) -> Option<J> {
        let q = json!({"columns":[{"t":"expr","expr":address(&[])}], "filters":[cmp("eq", col("id", &[]), param(0))]});
        let rows = self.select(q, vec![json!(id)]).await.unwrap();
        rows[0][0].as_deref().map(|t| normal(serde_json::from_str(t).unwrap()))
    }
}

async fn scenario(r: &Run) {
    let m = r.model;
    let ok = |res: std::result::Result<(), String>| res.unwrap();
    let err = |res: std::result::Result<(), String>| res.unwrap_err();
    let lat_lng = || object(vec![("lat", param(0)), ("lng", param(1))]);
    let whole = |g: J| object(vec![("city", param(0)), ("floor", param(1)), ("big", param(2)), ("open", param(3)), ("opened", param(4)), ("geo", g)]);
    // result objects
    assert_eq!(r.read(1).await, Some(json!({"city":"Oslo","floor":3,"big":"9007199254740993","open":true,"opened":"2026-01-01T00:00:00Z","geo":{"lat":59.9,"lng":10.7}})), "{m}");
    assert_eq!(r.read(2).await.unwrap()["geo"], json!({"lat":60.4}), "{m}");
    assert_eq!(r.read(3).await.unwrap().get("geo"), None, "{m}");
    assert_eq!(r.read(4).await, None, "{m}");
    // whole-field equality: a subfield left out is null
    assert_eq!(r.ids(cmp("eq", address(&["geo"]), lat_lng()), vec![json!(59.9), json!(10.7)]).await, ["1"], "{m}");
    assert_eq!(r.ids(cmp("eq", address(&["geo"]), object(vec![("lat", param(0))])), vec![json!(60.4)]).await, ["2"], "{m}");
    assert_eq!(r.ids(cmp("eq", object(vec![("lat", param(0))]), address(&["geo"])), vec![json!(60.4)]).await, ["2"], "{m}");
    // Ne is NOT(AND): rows with a null leaf are unknown, so they stay out.
    assert_eq!(r.ids(cmp("ne", address(&["geo"]), object(vec![("lat", param(0))])), vec![json!(60.4)]).await, ["1"], "{m}");
    let all = whole(object(vec![("lat", param(5)), ("lng", param(6))]));
    let lits = vec![json!("Oslo"), json!(3), json!(9007199254740993_i64), json!(true), json!("2026-01-01T00:00:00Z"), json!(59.9), json!(10.7)];
    assert_eq!(r.ids(cmp("eq", address(&[]), all.clone()), lits.clone()).await, ["1"], "{m}");
    assert_eq!(r.ids(cmp("ne", address(&[]), all), lits).await, ["2", "3"], "{m}");
    // null tests
    assert_eq!(r.ids(cmp("eq", address(&[]), null()), vec![]).await, ["4"], "{m}");
    assert_eq!(r.ids(cmp("ne", address(&[]), null()), vec![]).await, ["1", "2", "3"], "{m}");
    assert_eq!(r.ids(is_null(address(&[]), false), vec![]).await, ["4"], "{m}");
    assert_eq!(r.ids(is_null(address(&["geo"]), false), vec![]).await, ["3", "4"], "{m}");
    assert_eq!(r.ids(is_null(address(&["geo"]), true), vec![]).await, ["1", "2"], "{m}");
    // columns storage tests the one column that is null with the group, it builds no object
    if m == "CShop" {
        let q = json!({"columns":[{"t":"expr","expr":col("id", &[])}], "filters":[is_null(address(&[]), false)]});
        let sql = exec::sql(Target::new(r.dialect), &r.plan(q, "select", vec![]).unwrap());
        assert!(!sql.contains("json_object") && !sql.contains("build_object"), "{sql}");
    }
    // no order on a whole field
    let q = json!({"columns":[{"t":"expr","expr":col("id", &[])}], "order":[{"expr":address(&[])}]});
    assert!(r.select(q, vec![]).await.unwrap_err().contains("no order"), "{m}");
    // patch: a subfield and a deep subfield, the rest stays
    ok(r.update(1, vec![set(&["city"], param(0)), set(&["geo", "lat"], param(1))], vec![json!("Oslo2"), json!(1.5)]).await);
    assert_eq!(r.read(1).await.unwrap(), json!({"city":"Oslo2","floor":3,"big":"9007199254740993","open":true,"opened":"2026-01-01T00:00:00Z","geo":{"lat":1.5,"lng":10.7}}), "{m}");
    // patch below a missing object
    ok(r.update(3, vec![set(&["geo", "lat"], param(0))], vec![json!(7.5)]).await);
    assert_eq!(r.read(3).await.unwrap()["geo"], json!({"lat":7.5}), "{m}");
    assert_eq!(r.read(3).await.unwrap()["city"], json!("Rome"), "{m}");
    // a null value clears the subfield and keeps the rest
    ok(r.update(1, vec![set(&["geo", "lng"], null())], vec![]).await);
    assert_eq!(r.read(1).await.unwrap(), json!({"city":"Oslo2","floor":3,"big":"9007199254740993","open":true,"opened":"2026-01-01T00:00:00Z","geo":{"lat":1.5}}), "{m}");
    // replace a nested object: the old subfields do not survive
    ok(r.update(2, vec![set(&["geo"], lat_lng())], vec![json!(2.5), json!(3.5)]).await);
    assert_eq!(r.read(2).await.unwrap()["geo"], json!({"lat":2.5,"lng":3.5}), "{m}");
    ok(r.update(2, vec![set(&["geo"], object(vec![("lat", param(0))]))], vec![json!(4.5)]).await);
    assert_eq!(r.read(2).await.unwrap()["geo"], json!({"lat":4.5}), "{m}");
    // replace with a JSON parameter: the old subfields do not survive either
    if m == "JShop" {
        ok(r.update(2, vec![set(&["geo"], lat_lng())], vec![json!(2.5), json!(3.5)]).await);
        ok(r.update(2, vec![set(&["geo"], param(0))], vec![json!("{\"lat\": 8.5}")]).await);
        assert_eq!(r.read(2).await.unwrap()["geo"], json!({"lat":8.5}), "{m}");
    }
    // replace the whole field; geo is left out, so it is null
    let lits = vec![json!("Zed"), json!(1), json!(5), json!(false), json!("2026-01-04T00:00:00Z")];
    ok(r.update(3, vec![json!({"field":"address","value":object(vec![("city", param(0)), ("floor", param(1)), ("big", param(2)), ("open", param(3)), ("opened", param(4))])})], lits).await);
    assert_eq!(r.read(3).await.unwrap(), json!({"city":"Zed","floor":1,"big":"5","open":false,"opened":"2026-01-04T00:00:00Z"}), "{m}");
    // replace the whole field with null
    ok(r.update(2, vec![json!({"field":"address","value":{"t":"null"}})], vec![]).await);
    assert_eq!(r.read(2).await, None, "{m}");
    assert_eq!(r.ids(is_null(address(&[]), false), vec![]).await, ["2", "4"], "{m}");
    // errors
    let one = vec![json!("x")];
    assert!(err(r.update(1, vec![set(&["nope"], param(0))], one.clone()).await).contains("nope"), "{m}");
    assert!(err(r.update(1, vec![set(&["city", "x"], param(0))], one.clone()).await).contains("city"), "{m}");
    assert!(err(r.update(1, vec![set(&["city"], param(0)), set(&["city"], param(0))], one.clone()).await).contains("twice"), "{m}");
    assert!(err(r.update(1, vec![set(&["geo"], object(vec![("zzz", param(0))]))], one).await).contains("zzz"), "{m}");
}

async fn setup(dialect: Dialect, conn: Arc<dyn Executor>) {
    let s = schema(dialect);
    for sql in migrate::create_statements(&migrate::snapshot(&s).unwrap()) { conn.batch(sql).await.unwrap(); }
    conn.batch(inserts()).await.unwrap();
}

/// A null or omitted nested field writes NULL to its leaves, not the column default.
async fn insert_nulls(dialect: Dialect, conn: Arc<dyn Executor>) {
    let s = schema(dialect);
    let target = Target::new(dialect);
    let run = |fields: Vec<&str>, rows: Vec<Vec<Option<Value>>>| {
        let fields: Vec<String> = fields.into_iter().map(String::from).collect();
        let (stmt, types) = plan_insert(&s, target, "CShop", &fields, rows, None, &Lits(vec![]), false).unwrap();
        exec::sql(target, &Plan::Insert(stmt, Some((0, types))))
    };
    let sql = run(vec!["id"], vec![vec![Some(Value::Int(Some(10)))], vec![Some(Value::Int(Some(11)))]]);
    conn.batch(sql.clone()).await.map_err(|e| format!("{e}: {sql}")).unwrap();
    let rows = conn.query_text("SELECT id, address_city, address_floor FROM cshop WHERE id >= 10 ORDER BY id".into()).await.unwrap();
    assert_eq!(rows, [[Some("10".to_owned()), None, None], [Some("11".to_owned()), None, None]]);
    // the column has no database default under a nullable group: the engine writes it
    if dialect == Dialect::Postgres {
        let rows = conn
            .query_text("SELECT column_default FROM information_schema.columns WHERE table_schema = current_schema() AND table_name = 'cshop' AND column_name = 'address_floor'".into())
            .await
            .unwrap();
        assert_eq!(rows, [[None]]);
    }
    // a group that is set gets the default of a subfield left out
    let sql = run(
        vec!["id", "address_city", "address_big", "address_open", "address_opened"],
        vec![vec![Some(Value::Int(Some(12))), Some("Nes".into()), Some(Value::BigInt(Some(1))), Some(Value::Bool(Some(true))), Some("2026-02-01T00:00:00Z".into())]],
    );
    conn.batch(sql.clone()).await.map_err(|e| format!("{e}: {sql}")).unwrap();
    let rows = conn.query_text("SELECT address_city, address_floor FROM cshop WHERE id = 12".into()).await.unwrap();
    assert_eq!(rows, [[Some("Nes".to_owned()), Some("7".to_owned())]]);
}

#[test]
fn sqlite_nested_writes() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let conn: Arc<dyn Executor> = db::connect("sqlite://:memory:", 1).await.unwrap();
        setup(Dialect::Sqlite, conn.clone()).await;
        for model in ["JShop", "CShop"] {
            scenario(&Run { dialect: Dialect::Sqlite, conn: conn.clone(), schema: schema(Dialect::Sqlite), model }).await;
        }
        insert_nulls(Dialect::Sqlite, conn).await;
    });
}

#[test]
fn postgres_nested_writes() {
    let Ok(url) = std::env::var("ORM_RUST_TEST_POSTGRES_URL") else { return };
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let conn: Arc<dyn Executor> = db::connect(&url, 1).await.unwrap();
        conn.batch("DROP SCHEMA IF EXISTS nested_write CASCADE; CREATE SCHEMA nested_write; SET search_path TO nested_write, public".into()).await.unwrap();
        setup(Dialect::Postgres, conn.clone()).await;
        for model in ["JShop", "CShop"] {
            scenario(&Run { dialect: Dialect::Postgres, conn: conn.clone(), schema: schema(Dialect::Postgres), model }).await;
        }
        insert_nulls(Dialect::Postgres, conn.clone()).await;
        conn.batch("SET search_path TO public; DROP SCHEMA nested_write CASCADE".into()).await.unwrap();
    });
}
