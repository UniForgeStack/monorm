use monorm_core::{dialect::{Dialect, Target}, ir::Operation, schema::Schema};
use monorm_engine::{db, exec, params::NoParams, plan::{Planner, PrefetchPlan, plan_prefetch_only}};
use serde_json::{json, Value};

fn schema() -> Schema {
    let id = json!({"name":"id","column":"id","type":"int","primary_key":true});
    let raw = json!({"models":[
        {"name":"Author","table":"author","fields":[id, {"name":"name","column":"name","type":"text"}],
         "relations":[
            {"name":"posts","kind":"many","target":"Post","from":"id","to":"author_id"},
            {"name":"commenters","kind":"many","target":"Comment","from":[],"to":[],"path":["posts","comments"]}]},
        {"name":"Post","table":"post","fields":[id, {"name":"author_id","column":"author_id","type":"int"}],
         "relations":[
            {"name":"author","kind":"one","target":"Author","from":"author_id","to":"id","foreign_key":true},
            {"name":"comments","kind":"many","target":"Comment","from":"id","to":"post_id"}]},
        {"name":"Comment","table":"comment","fields":[id, {"name":"post_id","column":"post_id","type":"int"}, {"name":"body","column":"body","type":"text"}],
         "relations":[
            {"name":"post","kind":"one","target":"Post","from":"post_id","to":"id","foreign_key":true},
            {"name":"writer","kind":"one","target":"Author","from":[],"to":[],"path":["post","author"]}]}]});
    Schema::from_ir(serde_json::from_value(raw).unwrap()).unwrap()
}

fn col(path: &[&str], name: &str) -> Value { json!({"t":"col","path":path,"name":name}) }

fn select(model: &str, extra: Value) -> Result<String, String> {
    let mut op = json!({"op":"select","model":model});
    op.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    let op: Operation = serde_json::from_value(op).unwrap();
    let target = Target::new(Dialect::Postgres);
    Planner::plan(&schema(), target, &op, &NoParams).map(|p| exec::statement(target, &p)).map_err(|e| e.to_string())
}

fn prefetch(query: Value) -> Result<Vec<std::sync::Arc<PrefetchPlan>>, String> {
    let model = query["parent"].as_str().unwrap().to_owned();
    let item = json!({"op":"select","model":model,"prefetch":[query]});
    let Operation::Select(q) = serde_json::from_value(item).unwrap() else { panic!("select") };
    plan_prefetch_only(&schema(), Target::new(Dialect::Postgres), &q, &NoParams).map_err(|e| e.to_string())
}

fn prefetch_sql(p: &PrefetchPlan) -> String { db::build(Dialect::Postgres, &p.statement(vec![vec![1.into()]])).0 }

#[test]
fn a_filter_on_a_chain_is_one_exists_over_the_flat_hops() {
    let sql = select("Author", json!({"filters":[{"t":"cmp","op":"eq","l":col(&["commenters"], "body"),"r":{"t":"text","value":"x"}}]})).unwrap();
    assert_eq!(sql.matches("EXISTS").count(), 1, "{sql}");
    assert!(sql.contains("\"post\"") && sql.contains("\"comment\""), "{sql}");
}

#[test]
fn a_to_many_chain_loads_through_a_distinct_pair_subquery() {
    let p = prefetch(json!({"parent":"Author","relation":"commenters","model":"Comment"})).unwrap();
    let sql = prefetch_sql(&p[0]);
    assert!(sql.contains("DISTINCT") && sql.contains("\"post\"") && sql.contains("\"comment\""), "{sql}");
}

#[test]
fn a_to_one_chain_loads_with_joins_and_orders() {
    let sql = select("Comment", json!({"select_related":[["writer"]],"order":[{"expr":col(&["writer"], "name")}]})).unwrap();
    assert!(sql.matches("LEFT JOIN").count() >= 2, "{sql}");
    assert!(sql.contains("ORDER BY"), "{sql}");
}

#[test]
fn order_by_a_to_many_chain_is_refused() {
    let e = select("Author", json!({"order":[{"expr":col(&["commenters"], "body")}]})).unwrap_err();
    assert!(e.contains("to-many"), "{e}");
}

#[test]
fn count_over_a_to_many_chain_counts_distinct_targets() {
    let sql = select("Author", json!({"annotations":[{"name":"n","expr":{"t":"func","name":"count","rel":["commenters"]}}]})).unwrap();
    assert!(sql.contains("COUNT(DISTINCT"), "{sql}");
}

#[test]
fn outer_over_a_linked_to_one_correlates_through_the_flat_hops() {
    let exists = json!({"t":"exists","select":{"model":"Author","filters":[
        {"t":"cmp","op":"eq","l":col(&[], "name"),"r":{"t":"outer","depth":1,"path":["writer"],"name":"name"}}]}});
    let sql = select("Comment", json!({"filters":[exists]})).unwrap();
    assert!(sql.contains(r#"FROM "post" AS "o1" INNER JOIN "author" AS "o2" ON "o2"."id" = "o1"."author_id" WHERE "o1"."id" = "comment"."post_id""#), "{sql}");
}
