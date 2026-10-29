use monorm_core::{dialect::{Dialect, Target}, ir::Operation, schema::Schema};
use monorm_engine::{db, params::NoParams, plan::{Output, Plan, Planner, plan_prefetch_only}};
use serde_json::{json, Value};

fn schema() -> Schema {
    let id = json!({"name":"id","column":"id","type":"int","primary_key":true});
    let raw = json!({"models":[
        {"name":"Post","table":"post","fields":[id, {"name":"title","column":"title","type":"text"}, {"name":"author_id","column":"author_id","type":"int"}],
         "relations":[
            {"name":"author","kind":"one","target":"Author","from":"author_id","to":"id","foreign_key":true},
            {"name":"comments","kind":"many","target":"Comment","from":"id","to":"post_id"},
            {"name":"likes","kind":"many","target":"Like","from":"id","to":"post_id"}]},
        {"name":"Author","table":"author","fields":[id, {"name":"name","column":"name","type":"text"}],
         "relations":[{"name":"posts","kind":"many","target":"Post","from":"id","to":"author_id"}]},
        {"name":"Comment","table":"comment","fields":[id, {"name":"post_id","column":"post_id","type":"int"}]},
        {"name":"Like","table":"like_","fields":[id, {"name":"post_id","column":"post_id","type":"int"}]}]});
    Schema::from_ir(serde_json::from_value(raw).unwrap()).unwrap()
}

fn count(rel: &str, label: &str) -> Value {
    json!({"name":label,"expr":{"t":"func","name":"count","rel":[rel]}})
}

fn plan(op: Value, dialect: Dialect) -> Result<monorm_engine::plan::SelectPlan, String> {
    let operation: Operation = serde_json::from_value(op).unwrap();
    match Planner::plan(&schema(), Target::new(dialect), &operation, &NoParams).map_err(|e| e.to_string())? {
        Plan::Select(p) => Ok(p),
        _ => panic!("select"),
    }
}

fn sql(p: &monorm_engine::plan::SelectPlan, dialect: Dialect) -> String {
    db::build(dialect, &p.stmt).0
}

fn annotations(p: &monorm_engine::plan::SelectPlan) -> Vec<(String, usize)> {
    let Output::Instances { annotations, .. } = &p.output else { panic!("instances") };
    annotations.iter().map(|a| (a.name.clone(), a.pos)).collect()
}

#[test]
fn one_annotation_is_a_subquery_column_after_the_model_columns() {
    for dialect in [Dialect::Postgres, Dialect::Sqlite] {
        let p = plan(json!({"op":"select","model":"Post","annotations":[count("comments", "n_comments")]}), dialect).unwrap();
        assert_eq!(annotations(&p), [("n_comments".to_owned(), 3)]);
        assert_eq!(p.types.len(), 4);
        let sql = sql(&p, dialect);
        assert!(sql.contains("(SELECT COUNT(*) FROM") && sql.contains("comment"), "{sql}");
        assert!(!sql.contains("GROUP BY") && !sql.contains("LATERAL"), "{sql}");
    }
}

#[test]
fn annotation_columns_follow_the_joined_columns() {
    let p = plan(json!({"op":"select","model":"Post","select_related":[["author"]],"annotations":[count("comments", "n")]}), Dialect::Postgres).unwrap();
    // Post (3) + Author (2) columns, then the annotation.
    assert_eq!(annotations(&p), [("n".to_owned(), 5)]);
    let sql = sql(&p, Dialect::Postgres);
    assert!(sql.contains("LEFT JOIN \"author\"") && !sql.contains("GROUP BY"), "{sql}");
}

#[test]
fn two_annotations_on_one_relation_share_a_lateral_join() {
    let q = |dialect| plan(json!({"op":"select","model":"Post","annotations":[
        count("comments", "a"),
        {"name":"b","expr":{"t":"func","name":"count","rel":["comments"],"distinct":true,"args":[{"t":"col","path":["comments"],"name":"id"}]}}]}), dialect).unwrap();
    let pg = sql(&q(Dialect::Postgres), Dialect::Postgres);
    assert_eq!(pg.matches("LEFT JOIN LATERAL").count(), 1, "{pg}");
    assert_eq!(pg.matches("FROM \"comment\"").count(), 1, "{pg}");
    // SQLite has no lateral join: one subquery per annotation.
    let lite = sql(&q(Dialect::Sqlite), Dialect::Sqlite);
    assert!(!lite.contains("LATERAL") && lite.matches("FROM \"comment\"").count() == 2, "{lite}");
}

#[test]
fn annotations_on_different_relations_stay_subqueries() {
    let p = plan(json!({"op":"select","model":"Post","annotations":[count("comments", "c"), count("likes", "l")]}), Dialect::Postgres).unwrap();
    let sql = sql(&p, Dialect::Postgres);
    assert!(!sql.contains("LATERAL") && sql.contains("comment") && sql.contains("like_"), "{sql}");
    assert_eq!(annotations(&p), [("c".to_owned(), 3), ("l".to_owned(), 4)]);
}

#[test]
fn a_slice_keeps_the_annotations_out_of_the_lateral_join() {
    let p = plan(json!({"op":"select","model":"Post","limit":5,"annotations":[count("comments", "a"), count("comments", "b")]}), Dialect::Postgres).unwrap();
    let sql = sql(&p, Dialect::Postgres);
    assert!(!sql.contains("LATERAL") && sql.contains("LIMIT"), "{sql}");
}

#[test]
fn a_prefetch_annotation_is_inside_the_row_number_query() {
    let q: monorm_core::ir::Select = {
        let Operation::Select(q) = serde_json::from_value(json!({"op":"select","model":"Author","prefetch":[{
            "relation":"posts","model":"Post","limit":2,"annotations":[count("comments", "n")]}]})).unwrap() else { panic!("select") };
        q
    };
    let plans = plan_prefetch_only(&schema(), Target::new(Dialect::Postgres), &q, &NoParams).unwrap();
    let Output::Instances { annotations, .. } = &plans[0].output else { panic!("instances") };
    assert_eq!(annotations.iter().map(|a| (a.name.as_str(), a.pos)).collect::<Vec<_>>(), [("n", 3)]);
    let (sql, _) = db::build(Dialect::Postgres, &plans[0].statement(vec![vec![1.into()]]));
    let (inner, outer) = sql.split_once(") AS \"p\"").unwrap_or_else(|| panic!("{sql}"));
    assert!(inner.contains("COUNT(*)") && inner.contains("ROW_NUMBER()"), "{sql}");
    assert!(outer.contains("\"_rn\""), "{sql}");
}

#[test]
fn an_annotation_may_not_merge_rows() {
    let expr = json!({"t":"func","name":"count","args":[{"t":"col","path":[],"name":"id"}]});
    let e = plan(json!({"op":"select","model":"Post","annotations":[{"name":"x","expr":expr}]}), Dialect::Postgres).err().expect("error");
    assert!(e.contains("annotation x is an aggregate"), "{e}");
}

#[test]
fn an_annotation_may_be_any_row_expression() {
    let title = json!({"t":"col","path":[],"name":"title"});
    let p = plan(json!({"op":"select","model":"Post","annotations":[{"name":"x","expr":title}]}), Dialect::Postgres).unwrap();
    assert_eq!(annotations(&p), [("x".to_owned(), 3)]);
}

#[test]
fn a_window_annotation_keeps_one_row_per_instance() {
    let window = json!({"t":"window","func":{"t":"func","name":"count","args":[]},"partition_by":[{"t":"col","path":[],"name":"author_id"}]});
    let p = plan(json!({"op":"select","model":"Post","annotations":[{"name":"x","expr":window}]}), Dialect::Postgres).unwrap();
    assert!(sql(&p, Dialect::Postgres).contains("OVER (PARTITION BY"), "{}", sql(&p, Dialect::Postgres));
    let locked = plan(json!({"op":"select","model":"Post","lock":{"mode":"update"},"annotations":[{"name":"x","expr":window}]}), Dialect::Postgres);
    assert!(locked.err().expect("error").contains("lock() can't be used with window functions"));
}

#[test]
fn annotations_are_not_select_columns() {
    let e = plan(json!({"op":"select","model":"Post","columns":[{"t":"model"}],"annotations":[count("comments", "n")]}), Dialect::Postgres).err().expect("error");
    assert!(e.contains("relation aggregates"), "{e}");
}
