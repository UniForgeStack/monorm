//! A field that an adapter marks `null_only` (the bytes mean nothing to the database) takes no expression but a null test.
use monorm_core::{dialect::{Dialect, Target}, ir::Operation, schema::Schema};
use monorm_engine::{db, params::NoParams, plan::{Plan, Planner}};
use serde_json::{json, Value};

fn schema() -> Schema {
    let ir = json!({"models":[{"name":"Doc","table":"docs","fields":[
        {"name":"id","column":"id","type":"big_int","primary_key":true},
        {"name":"blob","column":"blob","type":"bytes","nullable":true},
        {"name":"msg","column":"msg","type":"bytes","nullable":true}]}],
        "behavior":{"schema_contract":5,"schema_contract_minor":3,"field_adapters":[
            {"model":"Doc","field":"msg","adapter":"acme.proto.v1","null_only":true}]}});
    Schema::from_ir(serde_json::from_value(ir).unwrap()).unwrap()
}

fn plan(doc: Value) -> Result<String, String> {
    let op: Operation = serde_json::from_value(doc).unwrap();
    match Planner::plan(&schema(), Target::new(Dialect::Postgres), &op, &NoParams).map_err(|e| e.to_string())? {
        Plan::Select(p) => Ok(db::build(Dialect::Postgres, &p.stmt).0),
        _ => panic!("select"),
    }
}

fn col(name: &str) -> Value { json!({"t":"col","path":[],"name":name}) }
fn filter(f: Value) -> Value { json!({"op":"select","model":"Doc","filters":[f]}) }

#[test]
fn a_null_test_and_a_plain_read_pass() {
    assert!(plan(filter(json!({"t":"is_null","item":col("msg")}))).unwrap().contains("IS NULL"));
    assert!(plan(filter(json!({"t":"is_null","item":col("msg"),"neg":true}))).unwrap().contains("IS NOT NULL"));
    assert!(plan(json!({"op":"select","model":"Doc"})).is_ok());
    let whole = json!({"op":"select","model":"Doc","columns":[{"t":"expr","expr":col("msg")}]});
    assert!(plan(whole).is_ok());
}

#[test]
fn every_other_use_is_refused() {
    let eq = json!({"t":"cmp","op":"eq","l":col("msg"),"r":{"t":"param","i":0}});
    let ordered = json!({"op":"select","model":"Doc","order":[{"expr":col("msg")}]});
    for doc in [filter(eq), ordered, filter(json!({"t":"is_null","item":{"t":"func","name":"length","args":[col("msg")]}}))] {
        let error = plan(doc).unwrap_err();
        assert!(error.contains("IS NULL or IS NOT NULL") || error.contains("Bytes"), "{error}");
    }
}

#[test]
fn a_plain_bytes_field_still_compares() {
    let eq = json!({"t":"cmp","op":"eq","l":col("blob"),"r":{"t":"param","i":0}});
    struct One;
    impl monorm_engine::Params for One {
        fn len(&self) -> usize { 1 }
        fn value(&self, _: usize, _: Option<monorm_core::ir::ValueType>) -> monorm_engine::Result<sea_query::Value> { Ok(sea_query::Value::Bytes(Some(vec![1]))) }
        fn text(&self, _: usize) -> monorm_engine::Result<String> { unreachable!() }
        fn count(&self, _: usize) -> monorm_engine::Result<u64> { unreachable!() }
    }
    let op: Operation = serde_json::from_value(filter(eq)).unwrap();
    assert!(Planner::plan(&schema(), Target::new(Dialect::Postgres), &op, &One).is_ok());
}
