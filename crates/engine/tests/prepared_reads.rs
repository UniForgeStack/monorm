//! A prepared read holds its SQL text and slot map: a call must send what planning that
//! call's values from scratch sends.
use std::collections::BTreeMap;

use monorm_core::{dialect::{Dialect, Target}, ir::{ColType, Operation, ValueType}, schema::Schema};
use monorm_engine::{db, params::Params, plan::{Plan, Planner}, prepared::PreparedRead, Result};
use sea_query::Value;
use serde_json::{json, Value as J};

fn schema() -> Schema {
    let fields = vec![
        json!({"name":"id","column":"id","type":"big_int","primary_key":true}),
        json!({"name":"title","column":"title","type":"text"}),
        json!({"name":"views","column":"views","type":"int"}),
        json!({"name":"live","column":"live","type":"bool"}),
        json!({"name":"shop","column":"shop","type":"big_int"}),
    ];
    Schema::from_ir(serde_json::from_value(json!({"models":[{"name":"Post","table":"posts","fields":fields}]})).unwrap()).unwrap()
}

/// Call values as the bindings hand them over.
struct Vals(Vec<Value>);

impl Params for Vals {
    fn len(&self) -> usize {
        self.0.len()
    }
    fn value(&self, i: usize, _: Option<ValueType>) -> Result<Value> {
        Ok(self.0[i].clone())
    }
    fn text(&self, i: usize) -> Result<String> {
        match &self.0[i] {
            Value::String(Some(s)) => Ok(s.to_string()),
            other => panic!("not text: {other:?}"),
        }
    }
    fn count(&self, i: usize) -> Result<u64> {
        match &self.0[i] {
            Value::BigInt(Some(n)) => Ok(*n as u64),
            other => panic!("not a count: {other:?}"),
        }
    }
}

fn col(name: &str) -> J {
    json!({"t":"col","path":[],"name":name})
}
fn param(i: usize) -> J {
    json!({"t":"param","i":i})
}
fn cmp(op: &str, l: J, r: J) -> J {
    json!({"t":"cmp","op":op,"l":l,"r":r})
}
fn text(s: &str) -> Value {
    Value::String(Some(s.into()))
}
fn big(n: i64) -> Value {
    Value::BigInt(Some(n))
}

fn op(extra: J) -> Operation {
    let mut doc = json!({"op":"select","model":"Post"});
    for (k, v) in extra.as_object().unwrap() {
        doc[k] = v.clone();
    }
    serde_json::from_value(doc).unwrap()
}

fn built(plan: &Plan, d: Dialect) -> (String, Vec<Value>) {
    match plan {
        Plan::Select(p) => db::build(d, &p.stmt),
        Plan::Count(s) | Plan::Exists(s) => db::build(d, s),
        _ => panic!("a read"),
    }
}

/// The prepared call and the unprepared plan send the same statement.
fn same(d: Dialect, op: &Operation, declared: &[Option<ValueType>], calls: &[Vec<Value>]) {
    let target = Target::new(d);
    let first = &calls[0];
    let prepared = PreparedRead::prepare(&schema(), target, op, first.len(), declared).unwrap();
    for values in calls {
        let vals = Vals(values.clone());
        let call = prepared.call(&vals).unwrap();
        let expect = built(&Planner::plan(&schema(), target, op, &vals).unwrap(), d);
        assert_eq!(prepared.statement(&call), expect, "{d:?} {values:?}");
    }
}

fn both(f: impl Fn(Dialect)) {
    f(Dialect::Postgres);
    f(Dialect::Sqlite);
}

#[test]
fn a_filter_param_is_a_slot() {
    let q = op(json!({"filters":[cmp("eq", col("id"), param(0)), cmp("gt", col("views"), param(1))]}));
    both(|d| same(d, &q, &[], &[vec![big(1), Value::Int(Some(5))], vec![big(77), Value::Int(Some(0))]]));
}

#[test]
fn one_param_used_twice_binds_the_same_value_twice() {
    let q = op(json!({"filters":[cmp("eq", col("id"), param(0)), cmp("eq", col("shop"), param(0))]}));
    both(|d| same(d, &q, &[], &[vec![big(1)], vec![big(9)]]));
}

#[test]
fn limit_and_offset_params_are_slots() {
    let q = op(json!({"limit":{"t":"param","i":0},"offset":{"t":"param","i":1},"order":[{"expr":col("id"),"desc":false}]}));
    both(|d| same(d, &q, &[], &[vec![big(5), big(10)], vec![big(1), big(0)]]));
}

#[test]
fn a_like_param_is_text_and_lowercase_on_sqlite() {
    let q = op(json!({"filters":[{"t":"like","item":col("title"),"pattern":param(0),"ci":true}]}));
    both(|d| same(d, &q, &[], &[vec![text("Ab%")], vec![text("ZZ_")]]));
}

#[test]
fn a_bool_param_is_a_slot() {
    let q = op(json!({"filters":[cmp("eq", col("live"), param(0))]}));
    let bool_ty = Some(ValueType { ty: ColType::Bool, array: false, enum_idx: None });
    both(|d| same(d, &q, &[bool_ty], &[vec![Value::Bool(Some(true))], vec![Value::Bool(Some(false))]]));
}

#[test]
fn scope_values_follow_the_params() {
    let q = op(json!({"scope":{"shop":1},"filters":[cmp("eq", col("id"), param(0)), cmp("eq", col("shop"), json!({"t":"scope","name":"shop"}))]}));
    both(|d| same(d, &q, &[], &[vec![big(1), big(10)], vec![big(2), big(20)]]));
}

#[test]
fn a_null_scope_value_fails_the_call() {
    let q = op(json!({"scope":{"shop":1},"filters":[cmp("eq", col("shop"), json!({"t":"scope","name":"shop"}))]}));
    let prepared = PreparedRead::prepare(&schema(), Target::new(Dialect::Postgres), &q, 2, &[]).unwrap();
    let err = prepared.call(&Vals(vec![big(0), Value::BigInt(None)])).err().expect("an error").to_string();
    assert!(err.contains("scope.shop is None/null"), "{err}");
}

#[test]
fn in_a_param_list_is_one_slot() {
    let list = |v: Vec<i64>| Value::Array(sea_query::ArrayType::BigInt, Some(Box::new(v.into_iter().map(big).collect())));
    let q = op(json!({"filters":[{"t":"in_param","item":col("id"),"i":0,"neg":false}]}));
    let ty = Some(ValueType { ty: ColType::BigInt, array: true, enum_idx: None });
    both(|d| {
        let target = Target::new(d);
        let prepared = PreparedRead::prepare(&schema(), target, &q, 1, &[ty]).unwrap();
        for v in [vec![1, 2, 3], vec![], vec![9]] {
            let call = prepared.call(&Vals(vec![list(v.clone())])).unwrap();
            let expect = built(&Planner::plan(&schema(), target, &q, &Vals(vec![list(v.clone())])).unwrap(), d);
            let (sql, _) = prepared.statement(&call);
            assert_eq!(sql, expect.0, "{d:?} {v:?}");
        }
    });
}

#[test]
fn a_case_of_params_needs_a_declared_type() {
    let case = json!({"t":"case","whens":[{"cond":cmp("gt", col("views"), json!({"t":"int","value":1})),"value":param(0)}],"default":param(1)});
    let q = op(json!({"columns":[{"t":"expr","expr":case,"name":"x"}]}));
    let err = PreparedRead::prepare(&schema(), Target::new(Dialect::Postgres), &q, 2, &[]).err().expect("an error").to_string();
    assert!(err.contains("declare its type"), "{err}");
    let ty = Some(ValueType { ty: ColType::Text, array: false, enum_idx: None });
    assert!(PreparedRead::prepare(&schema(), Target::new(Dialect::Postgres), &q, 2, &[ty, ty]).is_ok());
}

#[test]
fn sql_inlines_the_call_values() {
    let q = op(json!({"filters":[cmp("eq", col("id"), param(0))]}));
    let prepared = PreparedRead::prepare(&schema(), Target::new(Dialect::Postgres), &q, 1, &[]).unwrap();
    let call = prepared.call(&Vals(vec![big(42)])).unwrap();
    assert!(prepared.sql(&call).contains("= 42"), "{}", prepared.sql(&call));
    let _: BTreeMap<String, usize> = BTreeMap::new();
}
