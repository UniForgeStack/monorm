//! An expression as the value of a nested subfield in an update, for `json` and `columns`
//! storage, on PostgreSQL and SQLite.
use std::sync::Arc;
use monorm_core::{dialect::{Dialect, Target}, dsl, ir::{ColType, Operation, ValueType}, migrate, schema::Schema};
use monorm_engine::{db::{self, Executor}, exec, plan::{Plan, Planner}, Params, Result};
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
    let sqlite = dialect == Dialect::Sqlite;
    let prefix = if sqlite { "datasource db {\n  provider = \"sqlite\"\n}\n" } else { "" };
    let rent = if sqlite { "" } else { "  rent Decimal\n" };
    let src_rent = if sqlite { "" } else { "  src_rent Decimal\n" };
    let model = |name: &str, storage: &str, table: &str| format!(
        "model {name} {{\n  id Int @id\n  address Address {storage}\n  src_big BigInt\n  src_open Boolean\n  src_opened DateTime\n{src_rent}  src_meta Json\n  @@map(\"{table}\")\n}}\n\n");
    format!("{prefix}type Address {{\n  city String\n  country String\n  floor Int\n  big BigInt\n  open Boolean\n  opened DateTime\n{rent}  meta Json\n}}\n\n{}{}",
        model("JShop", "@storage(json)", "jshop"), model("CShop", "", "cshop"))
}

fn schema(dialect: Dialect) -> Schema {
    dsl::check(dsl::compile(&source(dialect), None).unwrap()).unwrap().1
}

fn inserts(dialect: Dialect) -> String {
    let pg = dialect == Dialect::Postgres;
    let rent = if pg { "12.50" } else { "" };
    let doc = |rent: &str| {
        let mut d = json!({"city": "Oslo", "country": "Norway", "floor": 1, "big": "1", "open": false, "opened": "2026-01-01T00:00:00Z", "meta": {"old": true}});
        if pg { d["rent"] = json!(rent); }
        d
    };
    let src_rent = if pg { ", src_rent" } else { "" };
    let src_rent_val = if pg { format!(", {rent}") } else { String::new() };
    let src = format!("9007199254740993, true, '2026-03-04T05:06:07Z'{src_rent_val}, '[1, 2]'");
    let cols = format!("src_big, src_open, src_opened{src_rent}, src_meta");
    let mut sql = format!("INSERT INTO jshop (id, address, {cols}) VALUES (1, '{}', {src});\n", doc("1"));
    let (rent_col, rent_v) = if pg { (", address_rent", ", 1") } else { ("", "") };
    sql += &format!(
        "INSERT INTO cshop (id, address_city, address_country, address_floor, address_big, address_open, address_opened{rent_col}, address_meta, {cols}) \
         VALUES (1, 'Oslo', 'Norway', 1, 1, false, '2026-01-01T00:00:00Z'{rent_v}, '{{\"old\": true}}', {src});\n");
    sql
}

fn col(name: &str, sub: &[&str]) -> J { json!({"t":"col","path":[],"name":name,"sub":sub}) }
fn address(sub: &[&str]) -> J { col("address", sub) }
fn param(i: usize) -> J { json!({"t":"param","i":i}) }
fn set(sub: &[&str], value: J) -> J { json!({"field":"address","sub":sub,"value":value}) }
fn normal(v: J) -> J {
    match v {
        J::Object(m) => J::Object(m.into_iter().map(|(k, v)| (k, normal(v))).collect()),
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

    async fn update(&self, set: Vec<J>) -> std::result::Result<(), String> {
        let q = json!({"filters":[{"t":"cmp","op":"eq","l":col("id", &[]),"r":param(0)}], "set": set});
        let plan = self.plan(q, "update", vec![json!(1)])?;
        let sql = exec::sql(Target::new(self.dialect), &plan);
        self.conn.batch(sql.clone()).await.map(|_| ()).map_err(|e| format!("{e}: {sql}"))
    }

    async fn read(&self) -> J {
        let q = json!({"columns":[{"t":"expr","expr":address(&[])}], "filters":[{"t":"cmp","op":"eq","l":col("id", &[]),"r":param(0)}]});
        let plan = self.plan(q, "select", vec![json!(1)]).unwrap();
        let sql = exec::sql(Target::new(self.dialect), &plan);
        let rows = self.conn.query_text(sql.clone()).await.map_err(|e| format!("{e}: {sql}")).unwrap();
        normal(serde_json::from_str(rows[0][0].as_deref().unwrap()).unwrap())
    }
}

async fn scenario(r: &Run) {
    let m = r.model;
    let pg = r.dialect == Dialect::Postgres;
    // a leaf of each type takes a column of the same row; the other leaves stay
    let mut set_all = vec![
        set(&["big"], col("src_big", &[])),
        set(&["open"], col("src_open", &[])),
        set(&["opened"], col("src_opened", &[])),
        set(&["meta"], col("src_meta", &[])),
    ];
    if pg { set_all.push(set(&["rent"], col("src_rent", &[]))); }
    r.update(set_all).await.unwrap();
    let mut want = json!({"city":"Oslo","country":"Norway","floor":1,"big":"9007199254740993","open":true,"opened":"2026-03-04T05:06:07Z","meta":[1,2]});
    if pg { want["rent"] = json!("12.50"); }
    assert_eq!(r.read().await, want, "{m}");
    // a subfield of the stored value, and an expression over a leaf
    r.update(vec![set(&["city"], address(&["country"])), set(&["floor"], json!({"t":"arith","op":"add","l":address(&["floor"]),"r":{"t":"int","value":41}}))]).await.unwrap();
    let now = r.read().await;
    assert_eq!((&now["city"], &now["floor"]), (&json!("Norway"), &json!(42)), "{m}");
    // two leaves swap: each reads the old row
    r.update(vec![set(&["city"], address(&["country"])), set(&["country"], address(&["city"]))]).await.unwrap();
    let swapped = r.read().await;
    assert_eq!((&swapped["city"], &swapped["country"]), (&json!("Norway"), &json!("Norway")), "{m}");
    r.update(vec![set(&["city"], json!({"t":"text","value":"Oslo"}))]).await.unwrap();
    r.update(vec![set(&["city"], address(&["country"])), set(&["country"], address(&["city"]))]).await.unwrap();
    let swapped = r.read().await;
    assert_eq!((&swapped["city"], &swapped["country"]), (&json!("Norway"), &json!("Oslo")), "{m}");
}

async fn setup(dialect: Dialect, conn: Arc<dyn Executor>) {
    let s = schema(dialect);
    for sql in migrate::create_statements(&migrate::snapshot(&s).unwrap()) { conn.batch(sql).await.unwrap(); }
    conn.batch(inserts(dialect)).await.unwrap();
}

#[test]
fn sqlite_json_patch_takes_an_expression() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let conn: Arc<dyn Executor> = db::connect("sqlite://:memory:", 1).await.unwrap();
        setup(Dialect::Sqlite, conn.clone()).await;
        for model in ["JShop", "CShop"] {
            scenario(&Run { dialect: Dialect::Sqlite, conn: conn.clone(), schema: schema(Dialect::Sqlite), model }).await;
        }
    });
}

#[test]
fn postgres_json_patch_takes_an_expression() {
    let Ok(url) = std::env::var("ORM_RUST_TEST_POSTGRES_URL") else { return };
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let conn: Arc<dyn Executor> = db::connect(&url, 1).await.unwrap();
        conn.batch("DROP SCHEMA IF EXISTS nested_patch_expr CASCADE; CREATE SCHEMA nested_patch_expr; SET search_path TO nested_patch_expr, public".into()).await.unwrap();
        setup(Dialect::Postgres, conn.clone()).await;
        for model in ["JShop", "CShop"] {
            scenario(&Run { dialect: Dialect::Postgres, conn: conn.clone(), schema: schema(Dialect::Postgres), model }).await;
        }
        conn.batch("SET search_path TO public; DROP SCHEMA nested_patch_expr CASCADE".into()).await.unwrap();
    });
}
