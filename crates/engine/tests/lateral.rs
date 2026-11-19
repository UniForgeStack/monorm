//! Two aggregates over one relation share a lateral join; a statement shape that cannot take it keeps scalar subqueries.
use monorm_core::{dialect::{Dialect, Target}, ir::Operation, schema::Schema};
use monorm_engine::{db, params::NoParams, plan::{Plan, Planner}};
use serde_json::{json, Value};

fn schema() -> Schema {
    Schema::from_ir(serde_json::from_value(json!({"dialect":"postgres","models":[
        {"name":"User","table":"users","fields":[
            {"name":"id","column":"id","type":"int","primary_key":true},
            {"name":"name","column":"name","type":"text"}],
         "relations":[{"name":"posts","kind":"many","target":"Post","from":"id","to":"user_id"}]},
        {"name":"Post","table":"posts","fields":[
            {"name":"id","column":"id","type":"int","primary_key":true},
            {"name":"user_id","column":"user_id","type":"int"},
            {"name":"views","column":"views","type":"int"}]}
    ]})).unwrap()).unwrap()
}

fn user_id() -> Value { json!({"t":"col","path":[],"name":"id"}) }
fn count_posts() -> Value { json!({"t":"func","name":"count","rel":["posts"]}) }
fn max_views() -> Value { json!({"t":"func","name":"max","args":[{"t":"col","path":["posts"],"name":"views"}]}) }

/// `User.id`, `count(User.posts)` and `max(User.posts.views)`, then the `extra` columns.
fn two_aggregates(extra: Vec<Value>) -> Vec<Value> {
    [user_id(), count_posts(), max_views()].into_iter().chain(extra)
        .map(|expr| json!({"t":"expr","expr":expr})).collect()
}

fn sql(columns: Vec<Value>, extra: Value) -> String {
    let mut op = json!({"op":"select","model":"User","columns":columns});
    op.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    let op: Operation = serde_json::from_value(op).unwrap();
    match Planner::plan(&schema(), Target::new(Dialect::Postgres), &op, &NoParams).unwrap() {
        Plan::Select(p) => db::build(Dialect::Postgres, &p.stmt).0,
        _ => panic!("select"),
    }
}

fn keeps_scalar_subqueries(sql: &str, case: &str) {
    assert!(!sql.contains("LATERAL"), "{case}: {sql}");
    assert!(sql.contains(r#"(SELECT COUNT(*) FROM "posts""#) && sql.contains("(SELECT MAX("), "{case}: {sql}");
}

#[test]
fn two_aggregates_over_one_relation_share_a_lateral_join() {
    let plain = sql(two_aggregates(vec![]), json!({"order_by":[{"expr":user_id()}]}));
    assert!(plain.contains("LATERAL"), "{plain}");
}

#[test]
fn lateral_guards_keep_scalar_subqueries() {
    let window = json!({"t":"window","func":{"t":"func","name":"row_number"},"order_by":[{"expr":user_id()}]});
    let named = ["id", "n", "m"].iter().zip([user_id(), count_posts(), max_views()]).map(|(name, expr)| json!({"t":"expr","expr":expr,"name":name})).collect::<Vec<_>>();
    let cte_body = json!({"model":"User","columns":named});
    let cases: Vec<(&str, String)> = vec![
        ("having", sql(two_aggregates(vec![]), json!({"having":[{"t":"cmp","op":"gt","l":count_posts(),"r":{"t":"int","value":0}}]}))),
        ("distinct", sql(two_aggregates(vec![]), json!({"distinct":true}))),
        ("distinct_on", sql(two_aggregates(vec![]), json!({"distinct":true,"distinct_on":[user_id()]}))),
        ("window", sql(two_aggregates(vec![window]), json!({}))),
        ("local_aggregate", sql(two_aggregates(vec![json!({"t":"func","name":"count"})]), json!({}))),
        ("cte", sql(vec![json!({"t":"expr","expr":{"t":"cte_col","cte":"c","name":"id"}})], json!({"model":"c","with":[{"name":"c","query":cte_body}]}))),
    ];
    for (case, sql) in cases {
        keeps_scalar_subqueries(&sql, case);
    }
}

#[test]
fn a_locked_select_with_relation_aggregates_keeps_for_update() {
    let locked = sql(two_aggregates(vec![]), json!({"lock":{}}));
    assert!(locked.ends_with(r#"FOR UPDATE OF "users""#), "{locked}");
    keeps_scalar_subqueries(&locked, "lock");
}
