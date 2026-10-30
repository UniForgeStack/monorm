use monorm_core::{dialect::{Dialect, Target}, ir::Operation, schema::Schema};
use monorm_engine::{db, params::NoParams, plan::{Output, PrefetchPlan, plan_prefetch_only}};
use serde_json::{json, Value};

fn schema() -> Schema {
    let id = json!({"name":"id","column":"id","type":"int","primary_key":true});
    let raw = json!({"models":[
        {"name":"Post","table":"post","fields":[id, {"name":"title","column":"title","type":"text"}, {"name":"author_id","column":"author_id","type":"int"}],
         "relations":[{"name":"author","kind":"one","target":"Author","from":"author_id","to":"id","foreign_key":true}]},
        {"name":"Author","table":"author","fields":[id, {"name":"name","column":"name","type":"text"}],
         "relations":[{"name":"posts","kind":"many","target":"Post","from":"id","to":"author_id"}]}]});
    Schema::from_ir(serde_json::from_value(raw).unwrap()).unwrap()
}

fn plans(dialect: Dialect, query: Value) -> Result<Vec<PrefetchPlan>, String> {
    let item = json!({"op":"select","model":"Author","prefetch":[query]});
    let Operation::Select(q) = serde_json::from_value(item).unwrap() else { panic!("select") };
    plan_prefetch_only(&schema(), Target::new(dialect), &q, &NoParams).map_err(|e| e.to_string())
}

fn sql(p: &PrefetchPlan, dialect: Dialect) -> String {
    db::build(dialect, &p.statement(vec![vec![1.into()]])).0
}

fn col(name: &str) -> Value {
    json!({"t":"col","path":[],"name":name})
}

fn item(name: &str) -> Value {
    json!({"t":"expr","expr":col(name),"name":name})
}

#[test]
fn a_locked_prefetch_query_locks_the_related_rows() {
    let p = plans(Dialect::Postgres, json!({"relation":"posts","model":"Post","lock":{}})).unwrap();
    assert!(sql(&p[0], Dialect::Postgres).ends_with("FOR UPDATE OF \"post\""), "{}", sql(&p[0], Dialect::Postgres));
    let p = plans(Dialect::Postgres, json!({"relation":"posts","model":"Post","lock":{"exclusive":false,"skip_locked":true}})).unwrap();
    assert!(sql(&p[0], Dialect::Postgres).ends_with("FOR SHARE OF \"post\" SKIP LOCKED"), "{}", sql(&p[0], Dialect::Postgres));
}

#[test]
fn a_lock_cannot_follow_a_per_parent_slice() {
    let e = plans(Dialect::Postgres, json!({"relation":"posts","model":"Post","limit":2,"lock":{}})).err().expect("error");
    assert!(e.contains("lock() can't be used with a slice"), "{e}");
}

#[test]
fn a_selected_prefetch_query_returns_rows_and_the_parent_key() {
    for dialect in [Dialect::Postgres, Dialect::Sqlite] {
        let p = plans(dialect, json!({"relation":"posts","model":"Post","attr":"titles","columns":[item("id"), item("title")]})).unwrap();
        let Output::Rows { items, .. } = &p[0].output else { panic!("rows") };
        assert_eq!(items.len(), 2);
        assert_eq!(p[0].types.len(), 3);
        assert_eq!(p[0].child_key_pos, [2]);
        let sql = sql(&p[0], dialect);
        assert!(sql.contains("IN (") && sql.contains("author_id"), "{sql}");
    }
}

#[test]
fn a_grouped_prefetch_query_groups_by_the_parent_key() {
    let count = json!({"t":"func","name":"count","args":[]});
    let query = json!({"relation":"posts","model":"Post","attr":"n","columns":[{"t":"expr","expr":count,"name":"n"}],"group_by":[col("title")]});
    let p = plans(Dialect::Postgres, query).unwrap();
    let sql = sql(&p[0], Dialect::Postgres);
    assert!(sql.contains("GROUP BY \"post\".\"title\", \"post\".\"author_id\""), "{sql}");
    assert!(!sql.contains("ORDER BY"), "{sql}");
}

#[test]
fn a_selected_prefetch_query_rejects_a_lock() {
    let query = json!({"relation":"posts","model":"Post","attr":"t","columns":[item("id")],"lock":{}});
    let e = plans(Dialect::Postgres, query).err().expect("error");
    assert!(e.contains("select() can't lock rows"), "{e}");
}

#[test]
fn a_grouped_sliced_prefetch_query_orders_its_window_by_the_groups_only() {
    let count = json!({"t":"func","name":"count","args":[]});
    let query = json!({"relation":"posts","model":"Post","attr":"n","columns":[{"t":"expr","expr":count,"name":"n"}],"group_by":[col("title")],"limit":1});
    let p = plans(Dialect::Postgres, query).unwrap();
    let sql = sql(&p[0], Dialect::Postgres);
    assert!(sql.contains("ROW_NUMBER()") && !sql.contains("ORDER BY \"post\".\"id\""), "{sql}");
}

#[test]
fn an_aggregate_without_group_by_groups_by_the_parent_key() {
    let count = json!({"t":"func","name":"count","args":[]});
    let query = json!({"relation":"posts","model":"Post","attr":"n","columns":[{"t":"expr","expr":count,"name":"n"}]});
    let p = plans(Dialect::Postgres, query).unwrap();
    let sql = sql(&p[0], Dialect::Postgres);
    assert!(sql.contains("GROUP BY \"post\".\"author_id\"") && !sql.contains("ORDER BY"), "{sql}");
}

#[test]
fn a_selected_prefetch_query_rejects_distinct() {
    let query = json!({"relation":"posts","model":"Post","attr":"t","columns":[item("title")],"distinct":true});
    let e = plans(Dialect::Postgres, query).err().expect("error");
    assert!(e.contains("distinct() can't be used"), "{e}");
}
