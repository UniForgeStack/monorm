use orm_core::{dialect::{Dialect, Target}, ir::Operation, schema::Schema};
use orm_engine::{exec, plan::Planner, NoParams};
use serde_json::json;

fn schema() -> Schema {
    Schema::from_ir(serde_json::from_value(json!({"models":[{"name":"Person","table":"person","fields":[
        {"name":"id","column":"id","type":"int","primary_key":true}
    ]}]})).unwrap()).unwrap()
}

fn render(dialect: Dialect, tag: &str, terms: i64) -> String {
    let items: Vec<_> = (0..terms).map(|i| json!({"t":"cmp","op":"eq","l":{"t":"col","path":[],"name":"id"},"r":{"t":"int","value":i}})).collect();
    let op: Operation = serde_json::from_value(json!({"op":"select","model":"Person","filters":[{"t":tag,"items":items}]})).unwrap();
    let target = Target::new(dialect);
    exec::statement(target, &Planner::plan(&schema(), target, &op, &NoParams).unwrap())
}

#[test]
fn thousands_of_and_or_terms_render_in_bounded_depth() {
    for dialect in [Dialect::Sqlite, Dialect::Postgres] {
        for tag in ["or", "and"] {
            let sql = render(dialect, tag, 5000);
            assert_eq!(sql.matches(&format!(" {} ", tag.to_uppercase())).count(), 4999, "{dialect:?} {tag}");
        }
    }
}

#[test]
fn a_short_chain_keeps_its_flat_sql() {
    let sql = render(Dialect::Sqlite, "or", 3);
    assert!(sql.contains("\"person\".\"id\" = (0) OR \"person\".\"id\" = (1) OR \"person\".\"id\" = (2)"), "{sql}");
}
