//! `count()` and `exists()` of a `select(...)`: the select itself is the subquery.
use monorm_core::{dialect::{Dialect, Target}, ir::Operation, schema::Schema};
use monorm_engine::{db, params::NoParams, plan::{Plan, Planner}};
use serde_json::{json, Value};

fn schema() -> Schema {
    let fields = vec![
        json!({"name":"id","column":"id","type":"big_int","primary_key":true}),
        json!({"name":"author","column":"author","type":"big_int"}),
        json!({"name":"views","column":"views","type":"int"}),
    ];
    Schema::from_ir(serde_json::from_value(json!({"models":[{"name":"Post","table":"posts","fields":fields}]})).unwrap()).unwrap()
}

fn col(name: &str) -> Value { json!({"t":"col","path":[],"name":name}) }

fn sql(dialect: Dialect, op: &str, extra: Value) -> String {
    let mut doc = json!({"op":op,"model":"Post","columns":[
        {"t":"expr","expr":col("author"),"name":"author"},
        {"t":"expr","expr":{"t":"func","name":"count","args":[]},"name":"n"},
    ]});
    for (k, v) in extra.as_object().unwrap() { doc[k] = v.clone(); }
    let op: Operation = serde_json::from_value(doc).unwrap();
    match Planner::plan(&schema(), Target::new(dialect), &op, &NoParams).unwrap() {
        Plan::Count(stmt) | Plan::Exists(stmt) => db::build(dialect, &stmt).0,
        _ => panic!("count or exists"),
    }
}

#[test]
fn count_counts_the_rows_of_the_grouped_select() {
    for dialect in [Dialect::Postgres, Dialect::Sqlite] {
        let s = sql(dialect, "count", json!({"group_by":[col("author")],"order":[{"expr":col("author"),"desc":false}]}));
        assert!(s.starts_with("SELECT COUNT(*) FROM (SELECT "), "{s}");
        assert!(s.contains("GROUP BY"), "{s}");
        assert!(!s.contains("ORDER BY"), "an order without a slice is dropped: {s}");
    }
}

#[test]
fn count_of_an_aggregate_without_groups_counts_its_one_row() {
    let s = sql(Dialect::Postgres, "count", json!({}));
    assert!(s.starts_with("SELECT COUNT(*) FROM (SELECT "), "{s}");
}

#[test]
fn a_slice_keeps_its_order_inside_the_count() {
    let s = sql(Dialect::Postgres, "count", json!({"group_by":[col("author")],"order":[{"expr":col("author"),"desc":false}],"limit":3}));
    assert!(s.contains("ORDER BY") && s.contains("LIMIT"), "{s}");
}

#[test]
fn exists_wraps_the_select_with_limit_one() {
    for dialect in [Dialect::Postgres, Dialect::Sqlite] {
        let s = sql(dialect, "exists", json!({"group_by":[col("author")]}));
        assert!(s.starts_with("SELECT EXISTS(SELECT "), "{s}");
        assert!(s.contains("GROUP BY") && s.contains("LIMIT"), "{s}");
    }
}
