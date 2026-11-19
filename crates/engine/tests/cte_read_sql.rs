//! A column with a read conversion is converted once, where a CTE selects it, not again where the outer query reads the CTE.
use monorm_core::{dialect::{Dialect, Target}, ir::Operation, schema::Schema};
use monorm_engine::{db, params::NoParams, plan::{Plan, Planner}};
use serde_json::json;

#[test]
fn a_cte_column_applies_read_sql_once() {
    let schema = Schema::from_ir(serde_json::from_value(json!({"dialect":"postgres","models":[
        {"name":"Doc","table":"docs","fields":[
            {"name":"id","column":"id","type":"int","primary_key":true},
            {"name":"word","column":"word","type":"text","read_sql":"({} || '!')"}]}
    ]})).unwrap()).unwrap();
    let recent = json!({"name":"recent","query":{"model":"Doc","filters":[{"t":"cmp","op":"gt","l":{"t":"col","path":[],"name":"id"},"r":{"t":"int","value":0}}]}});
    let op: Operation = serde_json::from_value(json!({"op":"select","model":"Doc","with":[recent],"from":"recent"})).unwrap();
    let Plan::Select(p) = Planner::plan(&schema, Target::new(Dialect::Postgres), &op, &NoParams).unwrap() else { panic!("select") };
    let sql = db::build(Dialect::Postgres, &p.stmt).0;
    assert_eq!(sql.matches("|| '!'").count(), 1, "{sql}");
}
