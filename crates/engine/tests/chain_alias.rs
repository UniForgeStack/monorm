use monorm_core::{dialect::{Dialect, Target}, ir::Operation, schema::Schema};
use monorm_engine::{exec, plan::Planner, NoParams};
use serde_json::{json, Value as J};

fn schema() -> Schema {
    Schema::from_ir(serde_json::from_value(json!({"models":[
        {"name":"User","table":"user","fields":[
            {"name":"id","column":"id","type":"int","primary_key":true},
            {"name":"active","column":"active","type":"bool"}],
         "relations":[{"name":"posts","kind":"many","target":"Post","from":"id","to":"user_id"}]},
        {"name":"Post","table":"post","fields":[
            {"name":"id","column":"id","type":"int","primary_key":true},
            {"name":"user_id","column":"user_id","type":"int"},
            {"name":"title","column":"title","type":"text"},
            {"name":"views","column":"views","type":"int"}],
         "relations":[{"name":"comments","kind":"many","target":"Comment","from":"id","to":"post_id"}]},
        {"name":"Comment","table":"comment","fields":[
            {"name":"id","column":"id","type":"int","primary_key":true},
            {"name":"post_id","column":"post_id","type":"int"},
            {"name":"body","column":"body","type":"text"}]}
    ]})).unwrap()).unwrap()
}

fn plan(filters: J, extra: J) -> Result<String, String> {
    let mut op = json!({"op":"select","model":"User","filters":filters});
    op.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    let op: Operation = serde_json::from_value(op).unwrap();
    let target = Target::new(Dialect::Postgres);
    Planner::plan(&schema(), target, &op, &NoParams).map(|p| exec::statement(target, &p)).map_err(|e| e.to_string())
}

fn sql(filters: J) -> String { plan(filters, json!({})).unwrap() }
fn col(path: &[&str], alias: &[u32], name: &str) -> J { json!({"t":"col","path":path,"name":name,"alias":alias}) }
fn title(alias: u32, v: &str) -> J { eq(col(&["posts"], &[alias], "title"), json!({"t":"text","value":v})) }
fn eq(l: J, r: J) -> J { json!({"t":"cmp","op":"eq","l":l,"r":r}) }
fn and(items: J) -> J { json!({"t":"and","items":items}) }
fn exists(sql: &str) -> usize { sql.matches("EXISTS").count() }

#[test]
fn filters_on_one_path_share_one_exists_in_one_call_or_across_calls() {
    let views = eq(col(&["posts"], &[], "views"), json!({"t":"int","value":3}));
    let across = sql(json!([title(0, "a"), views.clone()]));
    let one_call = sql(json!([and(json!([title(0, "a"), views]))]));
    assert_eq!(across, one_call);
    assert_eq!(exists(&across), 1, "{across}");
}

#[test]
fn an_alias_is_another_related_row_and_the_same_alias_is_one_row() {
    let two = sql(json!([title(0, "a"), title(1, "b")]));
    assert_eq!(exists(&two), 2, "{two}");
    let same = sql(json!([title(1, "a"), title(1, "b")]));
    assert_eq!(exists(&same), 1, "{same}");
    let two_aliases = sql(json!([title(1, "a"), title(2, "b")]));
    assert_eq!(exists(&two_aliases), 2, "{two_aliases}");
}

#[test]
fn or_and_not_are_barriers() {
    let or = json!({"t":"or","items":[title(0, "a"), title(0, "b")]});
    let s = sql(json!([title(0, "c"), or]));
    assert_eq!(exists(&s), 2, "{s}");
    let not = json!({"t":"not","item":title(0, "b")});
    let s = sql(json!([title(0, "a"), not]));
    assert_eq!(exists(&s), 2, "{s}");
    assert!(s.contains("NOT EXISTS"), "{s}");
}

#[test]
fn a_deeper_path_shares_the_hop_and_recurses() {
    let body = eq(col(&["posts", "comments"], &[], "body"), json!({"t":"text","value":"x"}));
    let s = sql(json!([title(0, "a"), body]));
    assert_eq!(exists(&s), 2, "one for posts, one for comments inside it: {s}");
    assert_eq!(s.matches("FROM \"post\"").count(), 1, "{s}");
}

#[test]
fn a_condition_cannot_read_two_aliases() {
    let both = eq(col(&["posts"], &[1], "title"), col(&["posts"], &[2], "title"));
    let err = plan(json!([both]), json!({})).unwrap_err();
    assert!(err.contains("two aliases"), "{err}");
}

#[test]
fn an_alias_outside_a_filter_condition_is_an_error() {
    let sum = json!({"t":"func","name":"sum","rel":["posts"],"args":[col(&["posts"], &[1], "views")]});
    let err = plan(json!([]), json!({"annotations":[{"name":"n","expr":sum}]})).unwrap_err();
    assert!(err.contains("alias()"), "{err}");
    assert!(plan(json!([title(1, "a")]), json!({})).is_ok());
}

#[test]
fn the_alias_message_names_the_only_places_it_works() {
    let sum = json!({"t":"func","name":"sum","rel":["posts"],"args":[col(&["posts"], &[1], "views")]});
    let err = plan(json!([]), json!({"annotations":[{"name":"n","expr":sum}]})).unwrap_err();
    assert!(err.contains("it works only in filter() and exclude()"), "{err}");
}
