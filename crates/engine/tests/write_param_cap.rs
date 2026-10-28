use monorm_core::{dialect::{Dialect, Target}, ir::{Operation, ValueType}, schema::Schema};
use monorm_engine::{plan::Planner, Params, Result};
use sea_query::Value;
use serde_json::json;

struct Ints(usize);
impl Params for Ints {
    fn len(&self) -> usize { self.0 }
    fn value(&self, i: usize, _: Option<ValueType>) -> Result<Value> { Ok(Value::Int(Some(i as i32))) }
    fn text(&self, _: usize) -> Result<String> { unreachable!() }
    fn count(&self, _: usize) -> Result<u64> { unreachable!() }
}

fn schema() -> Schema {
    Schema::from_ir(serde_json::from_value(json!({"models":[{"name":"Person","table":"person","fields":[
        {"name":"id","column":"id","type":"int","primary_key":true},
        {"name":"age","column":"age","type":"int"}
    ]}]})).unwrap()).unwrap()
}

fn write(kind: &str, terms: i64, cap: usize, dialect: Dialect) -> monorm_engine::Result<()> {
    let items: Vec<_> = (0..terms).map(|i| json!({"t":"cmp","op":"eq","l":{"t":"col","path":[],"name":"id"},"r":{"t":"param","i":i}})).collect();
    let mut op = json!({"op":kind,"model":"Person","filters":[{"t":"or","items":items}]});
    if kind == "update" {
        op["set"] = json!([{"field":"age","value":{"t":"param","i":0}}]);
    }
    let op: Operation = serde_json::from_value(op).unwrap();
    let target = Target::new(dialect).without(&[format!("max_params={cap}")]).unwrap();
    Planner::plan(&schema(), target, &op, &Ints(terms as usize)).map(|_| ())
}

#[test]
fn a_write_above_the_parameter_cap_is_a_query_error() {
    for dialect in [Dialect::Sqlite, Dialect::Postgres] {
        for kind in ["update", "delete"] {
            let err = write(kind, 10, 5, dialect).unwrap_err().to_string();
            assert!(err.contains("parameters") && err.contains('5'), "{dialect:?} {kind}: {err}");
        }
    }
}

#[test]
fn a_write_at_the_parameter_cap_plans() {
    for dialect in [Dialect::Sqlite, Dialect::Postgres] {
        assert!(write("delete", 5, 5, dialect).is_ok());
        assert!(write("update", 4, 5, dialect).is_ok());
    }
}
