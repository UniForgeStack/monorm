use orm_core::{dialect::{Dialect, Target}, ir::Operation, schema::Schema};
use orm_engine::{exec, plan::Planner, NoParams};
use serde_json::{json, Value as J};

fn schema() -> Schema {
    Schema::from_ir(serde_json::from_value(json!({"models":[
        {"name":"Note","table":"note","fields":[
            {"name":"id","column":"id","type":"int","primary_key":true},
            {"name":"writer_id","column":"writer_id","type":"int","nullable":true}],
         "relations":[{"name":"writer","kind":"one","target":"Writer","from":"writer_id","to":"id","foreign_key":true}]},
        {"name":"Writer","table":"writer","fields":[
            {"name":"id","column":"id","type":"int","primary_key":true},
            {"name":"email","column":"email","type":"text"}],
         "relations":[{"name":"notes","kind":"many","target":"Note","from":"id","to":"writer_id"}]}
    ]})).unwrap()).unwrap()
}

fn render(model: &str, filter: J) -> String {
    let op: Operation = serde_json::from_value(json!({"op":"select","model":model,"filters":[filter]})).unwrap();
    let target = Target::new(Dialect::Postgres);
    exec::statement(target, &Planner::plan(&schema(), target, &op, &NoParams).unwrap())
}

fn email(path: &[&str]) -> J { json!({"t":"col","path":path,"name":"email"}) }
fn text(v: &str) -> J { json!({"t":"text","value":v}) }
fn cmp(op: &str, l: J, r: J) -> J { json!({"t":"cmp","op":op,"l":l,"r":r}) }
fn where_of(sql: &str) -> &str { sql.split_once(" WHERE ").unwrap().1 }

#[test]
fn a_to_one_ne_is_not_exists_of_the_eq() {
    let ne = render("Note", cmp("ne", email(&["writer"]), text("a")));
    let negated_eq = render("Note", json!({"t":"not","item":cmp("eq", email(&["writer"]), text("a"))}));
    assert_eq!(where_of(&ne), where_of(&negated_eq), "{ne}");
    assert!(where_of(&ne).starts_with("NOT EXISTS"), "{ne}");
    assert!(!where_of(&ne).contains("<>"), "{ne}");
}

#[test]
fn a_to_one_not_in_is_not_exists_of_the_in() {
    let items = json!([text("a"), text("b")]);
    let not_in = render("Note", json!({"t":"in","item":email(&["writer"]),"values":items,"neg":true}));
    let negated = render("Note", json!({"t":"not","item":{"t":"in","item":email(&["writer"]),"values":items,"neg":false}}));
    assert_eq!(where_of(&not_in), where_of(&negated), "{not_in}");
}

#[test]
fn a_to_many_ne_stays_inside_the_exists() {
    let sql = render("Writer", cmp("ne", json!({"t":"col","path":["notes"],"name":"id"}), json!({"t":"int","value":1})));
    assert!(where_of(&sql).starts_with("EXISTS"), "{sql}");
    assert!(where_of(&sql).contains("<>"), "{sql}");
}

#[test]
fn a_column_of_the_root_and_a_null_test_do_not_flip() {
    let sql = render("Note", cmp("ne", json!({"t":"col","path":[],"name":"writer_id"}), json!({"t":"int","value":1})));
    assert!(where_of(&sql).contains("<>") && !where_of(&sql).contains("EXISTS"), "{sql}");
    let sql = render("Note", json!({"t":"is_null","item":email(&["writer"]),"neg":true}));
    assert!(where_of(&sql).starts_with("EXISTS"), "{sql}");
}
