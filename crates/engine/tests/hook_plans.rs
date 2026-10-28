//! Hook plans at the write seams: a hook 4 on a value that the caller sets, copy loads and the request context.
use orm_contracts::abi::{Arg, OrmArg, OrmContext, OrmValue, Value as Abi, STATUS_OK};
use orm_core::{
    dialect::{Dialect, Target},
    hooks::{HookPlan, HookRef},
    ir::{Operation, ValueType},
    schema::Schema,
    writes::{AssignValue, PreparedAssignment, UpsertDefault, WriteKind},
};
use orm_engine::{exec, plan::Planner, Params, Result};
use sea_query::Value;
use serde_json::json;
use std::{cell::Cell, ffi::c_void};

const TEXT: usize = 1;
const UPDATED_BY: usize = 2;
const HOOK_VALUE: &str = "from the hook";

struct Args { values: Vec<Value>, context_builds: Cell<usize> }

impl Args {
    fn of(values: Vec<Value>) -> Self { Self { values, context_builds: Cell::new(0) } }
}

fn string(value: &str) -> Value { Value::String(Some(value.to_owned())) }

impl Params for Args {
    fn len(&self) -> usize { self.values.len() }
    fn value(&self, i: usize, _: Option<ValueType>) -> Result<Value> { Ok(self.values[i].clone()) }
    fn text(&self, _: usize) -> Result<String> { unreachable!() }
    fn count(&self, _: usize) -> Result<u64> { unreachable!() }
    fn scope_values(&self) -> Vec<(String, usize)> { self.context_builds.set(self.context_builds.get() + 1); vec![] }
}

unsafe extern "C" fn stamp(_: *const c_void, _: *const OrmArg, _: usize, _: *const OrmContext, out: *mut OrmValue) -> i32 {
    // SAFETY: the host passes a writable `out`.
    unsafe { *out = Abi::Text(HOOK_VALUE.to_owned()).leak() };
    STATUS_OK
}

unsafe extern "C" fn trim(_: *const c_void, args: *const OrmArg, n: usize, _: *const OrmContext, out: *mut OrmValue) -> i32 {
    // SAFETY: the host passes `n` readable arguments and a writable `out`.
    let first = unsafe { Arg::from_raw(&std::slice::from_raw_parts(args, n)[0]) }.expect("an argument");
    let Abi::Text(text) = first.value else { panic!("a text value") };
    // SAFETY: see above.
    unsafe { *out = Abi::Text(text.trim().to_owned()).leak() };
    STATUS_OK
}

fn note() -> Schema {
    Schema::from_ir(serde_json::from_value(json!({"models":[{"name":"Note","table":"notes","fields":[
        {"name":"id","column":"id","type":"int","primary_key":true},
        {"name":"text","column":"text","type":"string"},
        {"name":"updated_by","column":"updated_by","type":"string","nullable":true}
    ]}]})).unwrap()).unwrap()
}

/// `Note.updated_by` takes its value from a hook 4 on update; the hook replaces any value.
fn with_hook_4() -> Schema {
    let mut schema = note();
    let plan = HookPlan { assign: vec![(UPDATED_BY, HookRef::in_process("test.stamp", stamp, false))], ..Default::default() };
    schema.models[0].hooks = plan.finish();
    schema.models[0].assignments.push(PreparedAssignment {
        source: "test.a".into(),
        field: UPDATED_BY,
        on: vec![WriteKind::Update],
        value: AssignValue::Hook { export: "stamp".into() },
        expr: None,
        upsert: UpsertDefault::Drop,
    });
    schema
}

/// The model of `with_hook_4` with a transform of `Note.text` (hook 1).
fn with_change_set() -> Schema {
    let mut schema = with_hook_4();
    let mut plan = *schema.models[0].hooks.take().unwrap();
    plan.transforms.push((TEXT, HookRef::in_process("test.trim", trim, false)));
    schema.models[0].hooks = plan.finish();
    schema
}

/// `UPDATE notes SET <field> = <value> ... WHERE id = $<key>`; the values are parameters `0..` in order.
fn update(sets: &[(&str, serde_json::Value)], key: usize) -> Operation {
    let set: Vec<_> = sets.iter().map(|(field, value)| json!({"field":field,"value":value})).collect();
    serde_json::from_value(json!({"op":"update","model":"Note","set":set,
        "filters":[{"t":"cmp","op":"eq","l":{"t":"col","path":[],"name":"id"},"r":{"t":"param","i":key}}]})).unwrap()
}

fn param(i: usize) -> serde_json::Value { json!({"t":"param","i":i}) }

fn texts(values: &[Value]) -> Vec<String> {
    values.iter().filter_map(|v| if let Value::String(Some(s)) = v { Some(s.clone()) } else { None }).collect()
}

fn fields(names: &[&str]) -> Vec<String> { names.iter().map(|n| (*n).to_owned()).collect() }

#[test]
fn update_runs_the_hook_4_on_the_value_that_the_caller_sets() {
    let schema = with_hook_4();
    let target = Target::new(Dialect::Postgres);
    let op = update(&[("updated_by", param(0))], 1);
    let plan = Planner::plan(&schema, target, &op, &Args::of(vec![string("caller"), Value::Int(Some(7))])).unwrap();
    let orm_engine::plan::Plan::Update(stmt, _) = plan else { panic!("an update plan") };
    let (sql, values) = orm_engine::db::build(Dialect::Postgres, &stmt);
    assert!(sql.contains("\"updated_by\""), "{sql}");
    assert_eq!(texts(&values), [HOOK_VALUE]);
}

#[test]
fn update_refuses_a_database_expression_for_a_field_that_a_hook_assigns() {
    let schema = with_hook_4();
    let op = update(&[("updated_by", json!({"t":"col","path":[],"name":"text"}))], 0);
    let error = Planner::plan(&schema, Target::new(Dialect::Postgres), &op, &Args::of(vec![Value::Int(Some(7))])).err().expect("a refusal").to_string();
    assert!(error.contains("Note.updated_by: a hook assigns this field; supply a value, not a database expression"), "{error}");
}

#[test]
fn update_many_runs_the_hook_4_on_the_value_that_the_caller_sets() {
    let schema = with_hook_4();
    let rows = vec![
        vec![Value::Int(Some(1)), string("first")],
        vec![Value::Int(Some(2)), string("second")],
    ];
    let (_, plan) = exec::plan_update_many(&schema, Target::new(Dialect::Postgres), "Note", &fields(&["id", "updated_by"]), rows, &[], &Args::of(vec![]), false, None, &Default::default()).unwrap();
    let sent: Vec<String> = plan.statements.iter().flat_map(|(_, values)| texts(values)).collect();
    assert_eq!(sent, [HOOK_VALUE, HOOK_VALUE]);
}

#[test]
fn insert_keeps_the_value_that_the_caller_supplies_for_a_hook_4_field() {
    let schema = with_hook_4();
    let rows = vec![vec![Some(Value::Int(Some(1))), Some(string("note")), Some(string("caller"))]];
    let plan = exec::plan_insert(&schema, Target::new(Dialect::Postgres), "Note", &fields(&["id", "text", "updated_by"]), rows, None, &Args::of(vec![]), &Default::default()).unwrap();
    let orm_engine::plan::Plan::Insert(stmt, _) = plan else { panic!("an insert plan") };
    let values = orm_engine::db::build(Dialect::Postgres, &stmt).1;
    assert_eq!(texts(&values), ["note", "caller"]);
}

#[test]
fn copy_is_refused_for_a_model_with_a_change_set_hook() {
    let rows = vec![vec![Some(Value::Int(Some(1))), Some(string("note"))]];
    let error = exec::plan_copy(&with_change_set(), Target::new(Dialect::Postgres), "Note", &fields(&["id", "text"]), rows).err().expect("a refusal").to_string();
    assert!(error.contains("Note has change-set hooks"), "{error}");
}

#[test]
fn copy_is_allowed_for_a_model_with_only_a_hook_4() {
    let rows = vec![vec![Some(Value::Int(Some(1))), Some(string("note"))]];
    let planned = exec::plan_copy(&with_hook_4(), Target::new(Dialect::Postgres), "Note", &fields(&["id", "text"]), rows);
    assert!(planned.is_ok(), "{:?}", planned.err().map(|e| e.to_string()));
}

#[test]
fn a_plan_of_hooks_that_read_no_context_does_not_build_it() {
    let schema = with_change_set();
    let args = Args::of(vec![string("  padded  "), string("caller"), Value::Int(Some(7))]);
    let op = update(&[("text", param(0)), ("updated_by", param(1))], 2);
    let plan = Planner::plan(&schema, Target::new(Dialect::Postgres), &op, &args).unwrap();
    let orm_engine::plan::Plan::Update(stmt, _) = plan else { panic!("an update plan") };
    let values = orm_engine::db::build(Dialect::Postgres, &stmt).1;
    assert_eq!(texts(&values), ["padded", HOOK_VALUE]);
    assert_eq!(args.context_builds.get(), 0);
}
