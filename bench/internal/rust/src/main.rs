//! Engine-only reference row of the internal benchmark: the JSON IR path of `bench/rust-compare`
//! (parse, plan, run, read the cells) for the cases that `bench/internal/python/bench.py --dump` wrote.
//! Run only against the database that the Python runner seeded.
#[path = "../../../rust-compare/src/environment.rs"]
mod environment;

use monorm_core::{
    dialect::Target,
    ir::{Bypass, ColType, ValueType},
    schema::Schema,
};
use monorm_engine::{
    db::Driver,
    exec::{self, Outcome},
    plan::Planner,
    Params,
};
use sea_query::Value;
use serde_json::{json, Value as Json};
use std::{hint::black_box, path::PathBuf, time::Instant};

const SINK: i64 = 11; // the user that the insert case writes to

struct Bound(Vec<Json>);

impl Params for Bound {
    fn len(&self) -> usize {
        self.0.len()
    }
    fn value(&self, i: usize, ty: Option<ValueType>) -> monorm_engine::Result<Value> {
        let v = &self.0[i];
        Ok(match (ty.map(|t| t.ty), v) {
            (Some(ColType::Int), Json::Number(n)) => Value::from(n.as_i64().expect("int") as i32),
            (Some(ColType::Bool), Json::Bool(b)) => Value::from(*b),
            (_, Json::Number(n)) => Value::from(n.as_i64().expect("integer param")),
            (_, Json::String(s)) => Value::from(s.clone()),
            (_, Json::Bool(b)) => Value::from(*b),
            _ => panic!("unsupported param {v}"),
        })
    }
    fn text(&self, i: usize) -> monorm_engine::Result<String> {
        Ok(self.0[i].as_str().expect("text param").to_owned())
    }
    fn count(&self, i: usize) -> monorm_engine::Result<u64> {
        Ok(self.0[i].as_u64().expect("count param"))
    }
}

/// The model, its fields and the rows of an insert.
type InsertSpec = (String, Vec<String>, Vec<Vec<Option<Value>>>);

struct Case {
    name: String,
    inner: usize,
    ir: Option<String>,
    params: Vec<Bound>,
    insert: Option<InsertSpec>,
}

fn load(dump: &Json) -> Vec<Case> {
    let mut out: Vec<Case> = dump
        .as_object()
        .expect("dump object")
        .iter()
        .map(|(name, c)| Case {
            name: name.clone(),
            inner: c["inner"].as_u64().expect("inner") as usize,
            ir: c["ir"].as_str().map(str::to_owned),
            params: c["params"].as_array().into_iter().flatten().map(|p| Bound(p.as_array().expect("params").clone())).collect(),
            insert: c.get("insert").map(|i| {
                let rows = i["rows"].as_array().expect("rows").iter().map(|r| {
                    r.as_array().expect("row").iter().map(|x| match x {
                        Json::Null => None,
                        Json::Number(n) => Some(Value::from(n.as_i64().expect("integer"))),
                        Json::String(s) => Some(Value::from(s.clone())),
                        _ => panic!("unsupported insert value {x}"),
                    }).collect()
                });
                let fields = i["fields"].as_array().expect("fields").iter().map(|f| f.as_str().expect("field").to_owned());
                (i["model"].as_str().expect("model").to_owned(), fields.collect(), rows.collect())
            }),
        })
        .collect();
    // Python writes the dump in the case order; serde_json sorts keys, so restore the order of the shared spec.
    let order = ["pk_get", "filter_order_50", "read_1000", "filtered_count", "insert_one", "prefetch_posts"];
    out.sort_by_key(|c| order.iter().position(|n| *n == c.name).unwrap_or(usize::MAX));
    out
}

/// Reads every cell of the prefetched rows and of their children, like the bindings do.
fn read_fetched(fetched: &[exec::Fetched]) {
    for f in fetched {
        for row in 0..f.rows.len() {
            for (col, ty) in f.plan.types.iter().enumerate() {
                black_box(f.rows.cell(row, col, *ty).unwrap());
            }
        }
        read_fetched(&f.children);
    }
}

/// One call: parse the IR, plan, run, and read every cell of the rows. Gives the row or count total.
async fn call(db: &dyn Driver, schema: &Schema, c: &Case, k: usize) -> u64 {
    let target = Target::new(db.dialect());
    if let Some((model, fields, rows)) = &c.insert {
        let plans = exec::plan_inserts(schema, target, model, fields, rows.clone(), None, &Bound(vec![]), None, true, &Bypass::default()).unwrap();
        return match exec::run_inserts(db, target, plans, true, true).await.unwrap() {
            Outcome::Rows { rows, .. } => rows.len() as u64,
            Outcome::Affected(n) => n,
            _ => unreachable!(),
        };
    }
    let params = &c.params[k % c.params.len()];
    let op = monorm_engine::parse_op(c.ir.as_deref().expect("ir")).unwrap();
    let plan = Planner::plan(schema, target, &op, params).unwrap();
    match exec::run(db, target, plan).await.unwrap() {
        Outcome::Select(s) => {
            for row in 0..s.rows.len() {
                for (col, ty) in s.plan.types.iter().enumerate() {
                    black_box(s.rows.cell(row, col, *ty).unwrap());
                }
            }
            read_fetched(&s.prefetched);
            s.rows.len() as u64
        }
        Outcome::Count(n) => n as u64,
        _ => unreachable!(),
    }
}

fn arg(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

#[tokio::main]
async fn main() {
    let url = arg("--url").expect("--url");
    let out = PathBuf::from(arg("--out").expect("--out"));
    let rounds: usize = arg("--rounds").map_or(15, |r| r.parse().expect("--rounds"));
    let backend = if url.starts_with("sqlite") { "sqlite" } else { "postgresql" };
    let dump: Json = serde_json::from_str(&std::fs::read_to_string(arg("--dump").expect("--dump")).unwrap()).unwrap();
    let source = std::fs::read_to_string(arg("--schema").expect("--schema")).unwrap().replace("BACKEND", backend);
    let schema = Schema::from_ir(monorm_core::dsl::compile(&source, None).unwrap()).unwrap();
    let db = monorm_engine::db::connect(&url, 1).await.unwrap();
    let mut environment = environment::collect(db.as_ref()).await;
    environment["language"] = json!("rust");
    environment["backend"] = json!(backend);
    environment["date"] = json!(command_date());

    let mut cases = serde_json::Map::new();
    for c in load(&dump) {
        let clean = || db.batch(format!("DELETE FROM posts WHERE author_id = {SINK}"));
        // The check: the call returns rows (or one inserted row, or a count above zero).
        let first = call(db.as_ref(), &schema, &c, 0).await;
        assert!(first > 0, "{}: the check result is empty", c.name);
        clean().await.unwrap();
        for k in 0..(c.inner / 4).max(3) {
            black_box(call(db.as_ref(), &schema, &c, k).await);
        }
        clean().await.unwrap();
        let mut samples = vec![];
        for _ in 0..rounds {
            let start = Instant::now();
            for k in 0..c.inner {
                black_box(call(db.as_ref(), &schema, &c, k).await);
            }
            samples.push(start.elapsed().as_secs_f64() * 1e6 / c.inner as f64);
            clean().await.unwrap();
        }
        samples.sort_by(f64::total_cmp);
        let mid = samples.len() / 2;
        let median = if samples.len() % 2 == 1 { samples[mid] } else { (samples[mid - 1] + samples[mid]) / 2.0 };
        println!("rust {backend} {}: engine {median:.1}us", c.name);
        cases.insert(c.name.clone(), json!({"checked": true, "check": "the call returns rows or a count above zero", "inner": c.inner, "rounds": rounds, "paths": {"engine": {"median_us": median, "min_us": samples[0], "samples_us": samples}}}));
    }
    std::fs::create_dir_all(&out).unwrap();
    let result = json!({"environment": environment, "quick": false, "cases": cases});
    std::fs::write(out.join(format!("rust-{backend}.json")), serde_json::to_string_pretty(&result).unwrap() + "\n").unwrap();
    db.close().await;
}

fn command_date() -> String {
    std::process::Command::new("date").args(["-u", "+%Y-%m-%dT%H:%M:%SZ"]).output().ok().and_then(|o| String::from_utf8(o.stdout).ok()).map(|s| s.trim().to_owned()).unwrap_or_default()
}
