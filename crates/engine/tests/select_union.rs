//! `UNION` / `UNION ALL` of `select(...)` queries: one statement, derived-table branches.
use monorm_core::{dialect::{Dialect, Target}, dsl, ir::{Operation, ValueType, ColType}, schema::Schema};
use monorm_engine::{db, params::{NoParams, Params}, plan::{Output, Plan, Planner}};
use serde_json::{json, Value};

fn schema() -> Schema {
    let post = vec![
        json!({"name":"id","column":"id","type":"big_int","primary_key":true}),
        json!({"name":"title","column":"title","type":"text"}),
        json!({"name":"views","column":"views","type":"int"}),
        json!({"name":"slug","column":"slug","type":"string"}),
        json!({"name":"stamp","column":"stamp","type":"int","read_sql":"({} + 0)"}),
        json!({"name":"status","column":"status","type":"string","enum":"Status"}),
        json!({"name":"kind","column":"kind","type":"string","enum":"Kind"}),
    ];
    let user = vec![
        json!({"name":"id","column":"id","type":"big_int","primary_key":true}),
        json!({"name":"name","column":"name","type":"text"}),
        json!({"name":"tags","column":"tags","type":"text","array":true}),
        json!({"name":"meta","column":"meta","type":"json"}),
    ];
    let enum_of = |name: &str| json!({"name":name,"db_name":name.to_lowercase(),"values":[{"name":"A","value":"a"},{"name":"B","value":"b"}]});
    let ir = json!({"enums":[enum_of("Status"), enum_of("Kind")],"models":[
        {"name":"Post","table":"posts","fields":post},
        {"name":"User","table":"users","fields":user},
    ]});
    Schema::from_ir(serde_json::from_value(ir).unwrap()).unwrap()
}

fn col(name: &str) -> Value { json!({"t":"col","path":[],"name":name}) }
fn item(e: Value, name: &str) -> Value { json!({"t":"expr","expr":e,"name":name}) }

fn leaf(model: &str, cols: &[(&str, &str)], extra: Value) -> Value {
    let columns: Vec<Value> = cols.iter().map(|(c, n)| item(col(c), n)).collect();
    let mut doc = json!({"t":"leaf","model":model,"columns":columns});
    for (k, v) in extra.as_object().unwrap() { doc[k] = v.clone(); }
    doc
}

fn set(operator: &str, left: Value, right: Value, extra: Value) -> Value {
    let mut doc = json!({"t":"set","operator":operator,"left":left,"right":right});
    for (k, v) in extra.as_object().unwrap() { doc[k] = v.clone(); }
    doc
}

fn plan(dialect: Dialect, terminal: &str, expr: Value) -> monorm_engine::Result<Plan> {
    let op: Operation = serde_json::from_value(json!({"op":"compound","terminal":terminal,"expr":expr})).unwrap();
    Planner::plan(&schema(), Target::new(dialect), &op, &NoParams)
}

fn sql(dialect: Dialect, terminal: &str, expr: Value) -> String {
    match plan(dialect, terminal, expr).unwrap() {
        Plan::Select(p) => db::build(dialect, &p.stmt).0,
        Plan::Count(s) | Plan::Exists(s) => db::build(dialect, &s).0,
        _ => panic!("a read"),
    }
}

fn error(dialect: Dialect, expr: Value) -> String {
    match plan(dialect, "read", expr) { Err(e) => e.to_string(), Ok(_) => panic!("expected an error") }
}

fn two() -> Value {
    set(
        "union",
        leaf("Post", &[("id", "a"), ("title", "b")], json!({"order":[{"expr":col("id"),"desc":false}],"limit":2})),
        leaf("Post", &[("id", "a"), ("title", "b")], json!({})),
        json!({"order":["-b"],"limit":5,"offset":1}),
    )
}

#[test]
fn branches_and_the_final_slice_are_derived_tables() {
    let pg = sql(Dialect::Postgres, "read", two());
    assert_eq!(
        pg,
        "SELECT \"_u0\", \"_u1\" FROM (SELECT \"_u0\", \"_u1\" FROM (SELECT \"posts\".\"id\" AS \"_u0\", \"posts\".\"title\" AS \"_u1\" FROM \"posts\" ORDER BY \"posts\".\"id\" ASC LIMIT $1) AS \"_b0\" UNION (SELECT \"_u0\", \"_u1\" FROM (SELECT \"posts\".\"id\" AS \"_u0\", \"posts\".\"title\" AS \"_u1\" FROM \"posts\") AS \"_b1\")) AS \"_c0\" ORDER BY \"_u1\" DESC LIMIT $2 OFFSET $3"
    );
    let lite = sql(Dialect::Sqlite, "read", two());
    assert!(lite.contains("FROM (SELECT \"posts\".\"id\" AS \"_u0\"") && lite.contains("ORDER BY \"_u1\" DESC LIMIT ? OFFSET ?"), "{lite}");
}

#[test]
fn the_plan_is_one_row_select_with_value_columns() {
    let Plan::Select(p) = plan(Dialect::Postgres, "read", two()).unwrap() else { panic!("a select") };
    assert_eq!(p.types, vec![ValueType::scalar(ColType::BigInt), ValueType::scalar(ColType::Text)]);
    let Output::Rows { items, .. } = p.output else { panic!("rows") };
    assert_eq!(items, vec![None, None]);
    assert!(p.prefetch.is_empty() && p.computed.is_empty());
}

#[test]
fn union_all_and_nested_groups_keep_their_operators() {
    let inner = set("union_all", leaf("Post", &[("id", "a")], json!({})), leaf("Post", &[("id", "a")], json!({})), json!({}));
    let outer = set("union", inner, leaf("Post", &[("id", "a")], json!({})), json!({}));
    let s = sql(Dialect::Postgres, "read", outer);
    assert!(s.contains("UNION ALL") && s.contains(" UNION ("), "{s}");
    assert!(s.matches("\"_c").count() >= 2, "each set node is its own derived table: {s}");
}

#[test]
fn count_and_exists_wrap_the_whole_compound() {
    let c = sql(Dialect::Postgres, "count", two());
    assert!(c.starts_with("SELECT COUNT(*) FROM (SELECT \"_u0\""), "{c}");
    let e = sql(Dialect::Sqlite, "exists", two());
    assert!(e.starts_with("SELECT EXISTS(SELECT 1 FROM (SELECT \"_u0\""), "{e}");
}

#[test]
fn a_null_literal_takes_the_type_of_the_other_branch() {
    let null = json!({"t":"leaf","model":"Post","columns":[item(json!({"t":"null"}), "a")]});
    let ok = set("union_all", null.clone(), leaf("Post", &[("views", "a")], json!({})), json!({}));
    let Plan::Select(p) = plan(Dialect::Postgres, "read", ok).unwrap() else { panic!() };
    assert_eq!(p.types, vec![ValueType::scalar(ColType::Int)]);
    let both = set("union_all", null.clone(), null, json!({}));
    assert!(error(Dialect::Postgres, both).contains("no type"));
}

#[test]
fn mismatches_and_unsupported_outputs_are_rejected() {
    let a = leaf("Post", &[("id", "a"), ("title", "b")], json!({}));
    let cases: Vec<(Value, &str)> = vec![
        (set("union", a.clone(), leaf("Post", &[("id", "a")], json!({})), json!({})), "2 columns"),
        (set("union", a.clone(), leaf("Post", &[("id", "a"), ("title", "c")], json!({})), json!({})), "name"),
        (set("union", a.clone(), leaf("Post", &[("id", "a"), ("views", "b")], json!({})), json!({})), "type"),
        (set("union", a.clone(), leaf("User", &[("id", "a"), ("tags", "b")], json!({})), json!({})), "array"),
        (set("union", leaf("User", &[("id", "a"), ("meta", "b")], json!({})), a.clone(), json!({})), "json"),
        (set("union", a.clone(), a.clone(), json!({"order":["missing"]})), "missing"),
        (set("union", a.clone(), a.clone(), json!({"order":["-"]})), "order"),
        (set("union", a.clone(), leaf("Post", &[("id", "a"), ("title", "b")], json!({"lock":{"exclusive":true}})), json!({})), "lock"),
        (set("union", a.clone(), leaf("Post", &[("id", "a"), ("title", "b")], json!({"with":[{"name":"c","query":{"model":"Post"}}]})), json!({})), "CTE"),
        (set("union", a.clone(), json!({"t":"leaf","model":"Post"}), json!({})), "select"),
        (set("union", a.clone(), json!({"t":"leaf","model":"Post","columns":[{"t":"model"}]}), json!({})), "model"),
        (set("union", a.clone(), leaf("Post", &[("id", "a"), ("title", "b")], json!({"scope":{"x":0}})), json!({})), "scope"),
        (a.clone(), "set operation"),
    ];
    for (expr, needle) in cases {
        let e = error(Dialect::Postgres, expr.clone());
        assert!(e.to_lowercase().contains(&needle.to_lowercase()), "{needle}: {e} for {expr}");
    }
}

#[test]
fn a_string_column_and_a_text_column_are_one_family() {
    // `slug` is `String`, `title` is `Text`: the same family, in either order.
    let a = leaf("Post", &[("slug", "x")], json!({}));
    let b = leaf("Post", &[("title", "x")], json!({}));
    for dialect in [Dialect::Postgres, Dialect::Sqlite] {
        for expr in [set("union", a.clone(), b.clone(), json!({})), set("union_all", b.clone(), a.clone(), json!({}))] {
            let Plan::Select(p) = plan(dialect, "read", expr).unwrap() else { panic!("a select") };
            assert_eq!(p.types.len(), 1);
            assert!(matches!(p.types[0].ty, ColType::String | ColType::Text), "{:?}", p.types[0]);
        }
    }
    // Other families stay apart: text is not an integer.
    let views = leaf("Post", &[("views", "x")], json!({}));
    assert!(error(Dialect::Postgres, set("union", a, views, json!({}))).contains("does not convert"));
}

#[test]
fn an_error_names_the_branches() {
    let a = leaf("Post", &[("id", "a")], json!({}));
    let b = leaf("Post", &[("id", "a")], json!({}));
    let odd = leaf("Post", &[("title", "a")], json!({}));
    let group = set("union", a.clone(), b.clone(), json!({}));
    // Branch numbers count the leaves in reading order, so a nested tree names the right one.
    let e = error(Dialect::Postgres, set("union", group.clone(), odd.clone(), json!({})));
    assert!(e.contains("branches 0 to 1") && e.contains("branch 2"), "{e}");
    let e = error(Dialect::Postgres, set("union", a.clone(), set("union", b, odd, json!({})), json!({})));
    assert!(e.contains("branch 1") && e.contains("branch 2") && !e.contains("left"), "{e}");
    let wide = leaf("Post", &[("id", "a"), ("title", "b")], json!({}));
    let e = error(Dialect::Sqlite, set("union", group, wide, json!({})));
    assert!(e.contains("branches 0 to 1 has 1 columns") && e.contains("branch 2 has 2"), "{e}");
    let named = leaf("Post", &[("id", "z")], json!({}));
    assert!(error(Dialect::Postgres, set("union", a, named, json!({}))).contains("in branch 0 and \"z\" in branch 1"));
}

#[test]
fn an_enum_column_needs_the_same_enum_and_a_typed_value() {
    let status = leaf("Post", &[("status", "x")], json!({}));
    let kind = leaf("Post", &[("kind", "x")], json!({}));
    let e = error(Dialect::Postgres, set("union", status.clone(), kind, json!({})));
    assert!(e.contains("does not convert"), "{e}");
    assert!(plan(Dialect::Postgres, "read", set("union", status.clone(), status.clone(), json!({}))).is_ok());
    // A NULL takes the type of its sibling, but an enum has no `CAST` here: both databases reject it alike.
    let null = json!({"t":"leaf","model":"Post","columns":[item(json!({"t":"null"}), "x")]});
    for dialect in [Dialect::Postgres, Dialect::Sqlite] {
        let e = error(dialect, set("union_all", null.clone(), status.clone(), json!({})));
        assert!(e.contains("NULL can't stand for the enum column"), "{dialect:?}: {e}");
    }
}

#[test]
fn a_hostile_output_name_never_reaches_the_sql() {
    // Output names are labels: the statement aliases every column `_u<i>`.
    let name = "a\" FROM \"posts\"; DROP TABLE posts; --";
    let one = || leaf("Post", &[("id", name)], json!({}));
    let expr = set("union", one(), one(), json!({"order":[format!("-{name}")]}));
    for dialect in [Dialect::Postgres, Dialect::Sqlite] {
        let s = sql(dialect, "read", expr.clone());
        assert!(!s.contains("DROP") && s.contains("ORDER BY \"_u0\" DESC"), "{s}");
    }
    // A name that no column has stays an error, even when it looks like an alias.
    assert!(error(Dialect::Postgres, set("union", one(), one(), json!({"order":["_u0"]}))).contains("_u0"));
}

struct Vals(Vec<sea_query::Value>);

impl Params for Vals {
    fn len(&self) -> usize { self.0.len() }
    fn value(&self, i: usize, _: Option<ValueType>) -> monorm_engine::Result<sea_query::Value> { Ok(self.0[i].clone()) }
    fn text(&self, _: usize) -> monorm_engine::Result<String> { unreachable!("no pattern") }
    fn count(&self, _: usize) -> monorm_engine::Result<u64> { unreachable!("no slice") }
}

#[test]
fn the_root_scope_reaches_every_branch() {
    let by_scope = json!({"filters":[{"t":"cmp","op":"eq","l":col("id"),"r":{"t":"scope","name":"shop"}}]});
    let expr = || set("union_all", leaf("Post", &[("id", "a")], by_scope.clone()), leaf("Post", &[("id", "a")], by_scope.clone()), json!({}));
    let with = |scope: Value, params: &dyn Params| {
        let op: Operation = serde_json::from_value(json!({"op":"compound","terminal":"read","scope":scope,"expr":expr()})).unwrap();
        Planner::plan(&schema(), Target::new(Dialect::Postgres), &op, params).map(|p| match p {
            Plan::Select(p) => db::build(Dialect::Postgres, &p.stmt),
            _ => panic!("a select"),
        })
    };
    let params = Vals(vec![sea_query::Value::BigInt(Some(7))]);
    let (text, values) = with(json!({"shop":0}), &params).unwrap();
    assert_eq!(text.matches("= $").count(), 2, "{text}");
    assert_eq!(values.len(), 2, "each branch binds the scope value once");
    let e = with(json!({}), &params).err().expect("closed by default").to_string();
    assert!(e.contains("scope.shop") && e.contains("not set"), "{e}");
}

#[test]
fn a_read_conversion_is_rejected_for_a_plain_column_only() {
    // The check mirrors `select_columns`: a plain column reads through `read_sql`, an expression over one does not.
    let plain = leaf("Post", &[("stamp", "x")], json!({}));
    let e = error(Dialect::Postgres, set("union", plain, leaf("Post", &[("views", "x")], json!({})), json!({})));
    assert!(e.contains("read conversion"), "{e}");
    let calc = json!({"t":"leaf","model":"Post","columns":[item(json!({"t":"arith","op":"add","l":col("stamp"),"r":col("views")}), "x")]});
    let ok = set("union", calc, leaf("Post", &[("views", "x")], json!({})), json!({}));
    assert!(plan(Dialect::Postgres, "read", ok).is_ok());
}

#[test]
fn the_parameter_cap_covers_the_whole_statement() {
    let op: Operation = serde_json::from_value(json!({"op":"compound","terminal":"read","expr":two()})).unwrap();
    let mut target = Target::new(Dialect::Sqlite);
    target.caps.max_params = 3;
    assert!(Planner::plan(&schema(), target, &op, &NoParams).is_ok());
    target.caps.max_params = 2;
    let e = Planner::plan(&schema(), target, &op, &NoParams).err().expect("over the cap").to_string();
    assert!(e.contains("3 parameters") && e.contains("above the 2"), "{e}");
}

mod backends {
    use super::*;
    use monorm_engine::{db::Cell, exec::{self, Outcome}};

    fn column(rows: &dyn monorm_engine::db::RowSet, ty: ColType) -> Vec<Cell<'_>> {
        (0..rows.len()).map(|i| rows.cell(i, 0, ValueType::scalar(ty)).unwrap()).collect()
    }

    fn run_schema(dialect: Dialect) -> Schema {
        let head = if dialect == Dialect::Sqlite { "datasource db {\n provider = \"sqlite\"\n}\n" } else { "" };
        let source = format!("{head}model Post {{\n id BigInt @id\n title String\n views Int\n @@map(\"posts\")\n}}\n");
        dsl::check(dsl::compile(&source, None).unwrap()).unwrap().1
    }

    async fn exercise(url: &str, dialect: Dialect) {
        let schema = run_schema(dialect);
        let target = Target::new(dialect);
        let db = db::connect(url, 1).await.unwrap();
        let conn = db.begin().await.unwrap();
        for s in monorm_core::migrate::create_all(&schema).unwrap() { conn.batch(s).await.unwrap(); }
        conn.batch("INSERT INTO posts (id, title, views) VALUES (1,'a',10),(2,'b',20),(3,'c',30),(4,'d',40)".into()).await.unwrap();
        let go = |terminal: &str, expr: Value| {
            let op: Operation = serde_json::from_value(json!({"op":"compound","terminal":terminal,"expr":expr})).unwrap();
            Planner::plan(&schema, target, &op, &NoParams).unwrap()
        };
        let one = |extra: Value| leaf("Post", &[("id", "a"), ("title", "b")], extra);
        // Each branch keeps its own order and limit; the final order and slice cover the union.
        let expr = set(
            "union",
            one(json!({"order":[{"expr":col("id"),"desc":false}],"limit":2})),
            one(json!({"order":[{"expr":col("id"),"desc":true}],"limit":2})),
            json!({"order":["-a"],"limit":3,"offset":1}),
        );
        let Outcome::Select(sel) = exec::run(conn.as_ref(), target, go("read", expr.clone())).await.unwrap() else { panic!() };
        assert_eq!(column(sel.rows.as_ref(), ColType::BigInt), vec![Cell::BigInt(3), Cell::BigInt(2), Cell::BigInt(1)]);
        // A nested group runs as its own operand.
        let nested = set("union_all", set("union_all", one(json!({"limit":1})), one(json!({"limit":1})), json!({"limit":1})), one(json!({"limit":2})), json!({}));
        let Outcome::Select(sel) = exec::run(conn.as_ref(), target, go("read", nested)).await.unwrap() else { panic!() };
        assert_eq!(sel.rows.len(), 3);
        // A NULL literal takes the other operand's type.
        let null = json!({"t":"leaf","model":"Post","columns":[item(json!({"t":"null"}), "a")],"limit":1});
        let with_null = set("union_all", null, leaf("Post", &[("views", "a")], json!({"order":[{"expr":col("id"),"desc":false}],"limit":1})), json!({}));
        let Outcome::Select(sel) = exec::run(conn.as_ref(), target, go("read", with_null)).await.unwrap() else { panic!() };
        let values = column(sel.rows.as_ref(), ColType::Int);
        assert_eq!(values.len(), 2);
        assert!(values.contains(&Cell::Null) && values.contains(&Cell::Int(10)), "{values:?}");
        // count and exists see the whole compound.
        let Outcome::Count(n) = exec::run(conn.as_ref(), target, go("count", expr.clone())).await.unwrap() else { panic!("count") };
        assert_eq!(n, 3);
        let Outcome::Exists(found) = exec::run(conn.as_ref(), target, go("exists", expr)).await.unwrap() else { panic!("exists") };
        assert!(found);
        conn.rollback().await.unwrap();
        db.close().await;
    }

    #[test]
    fn sqlite_runs_sliced_and_nested_operands() {
        tokio::runtime::Runtime::new().unwrap().block_on(exercise("sqlite://:memory:", Dialect::Sqlite));
    }

    #[test]
    fn postgres_runs_sliced_and_nested_operands() {
        let Ok(url) = std::env::var("ORM_TEST_DATABASE_URL") else { return };
        tokio::runtime::Runtime::new().unwrap().block_on(exercise(&url, Dialect::Postgres));
    }
}
