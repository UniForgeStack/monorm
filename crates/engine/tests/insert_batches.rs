use orm_core::{dialect::{Dialect, Target}, dsl};
use orm_engine::{exec, params::NoParams, plan::Plan};
use sea_query::Value;

const SOURCE: &str = r#"
model Item {
 id BigInt @id @default(autoincrement())
 name String
 token String @client_default(uuid())
}
"#;

fn batches(target: Target, rows: usize, batch_size: Option<usize>) -> Vec<usize> {
    let (_, schema) = dsl::check(dsl::compile(SOURCE, None).unwrap()).unwrap();
    let rows = (0..rows).map(|i| vec![Some(Value::String(Some(format!("n{i}"))))]).collect();
    let plans = exec::plan_inserts(&schema, target, "Item", &["name".into()], rows, None, &NoParams, batch_size, true, &Default::default()).unwrap();
    plans
        .iter()
        .map(|p| match p {
            Plan::Insert(s, _) => orm_engine::db::build(target.dialect, s).1.len() / 2,
            _ => panic!("an insert plan"),
        })
        .collect()
}

#[test]
fn rows_split_by_the_parameter_limit_after_client_defaults() {
    // `token` is filled in by its client default: two parameters per row.
    let target = Target::new(Dialect::Postgres).without(&["max_params=5".into()]).unwrap();
    assert_eq!(batches(target, 5, None), vec![2, 2, 1]);
    assert_eq!(batches(Target::new(Dialect::Postgres), 40_000, None), vec![32_767, 7_233]);
}

#[test]
fn batch_size_caps_the_rows_per_statement() {
    assert_eq!(batches(Target::new(Dialect::Postgres), 5, Some(3)), vec![3, 2]);
    assert_eq!(batches(Target::new(Dialect::Postgres), 2, Some(3)), vec![2]);
    let (_, schema) = dsl::check(dsl::compile(SOURCE, None).unwrap()).unwrap();
    let err = exec::plan_inserts(&schema, Target::new(Dialect::Postgres), "Item", &[], vec![], None, &NoParams, Some(0), true, &Default::default());
    assert!(err.is_err());
}

const INDEXED: &str = r#"model Doc {
 id BigInt @id @default(autoincrement())
 slug String
 title String
 deleted_at DateTime?
 hidden Boolean @default(false)
 @@index([slug], unique: true, where: raw("deleted_at IS NULL"), map: "doc_slug_live")
 @@index([slug], unique: true, where: raw("hidden = false"), map: "doc_slug_visible")
 @@index([title], unique: true, map: "doc_title")
 @@index([title, slug], map: "doc_title_slug")
}
model Solo {
 id BigInt @id @default(autoincrement())
 slug String
 deleted_at DateTime?
 @@index([slug], unique: true, where: raw("deleted_at IS NULL"))
}
"#;

fn conflict_sql(dialect: Dialect, model: &str, field: &str, index: exec::OnConflictTarget) -> Result<String, String> {
    let (_, schema) = dsl::check(dsl::compile(INDEXED, None).unwrap()).unwrap();
    let target = Target::new(dialect);
    let conflict = exec::Conflict::Nothing { target: vec![field.into()], index };
    let rows = vec![vec![Some(Value::String(Some("a".into())))]];
    exec::plan_insert(&schema, target, model, &[field.into()], rows, Some(conflict), &NoParams, &Default::default())
        .map(|plan| exec::statement(target, &plan))
        .map_err(|e| e.to_string())
}

fn names(list: &[&str]) -> exec::OnConflictTarget {
    exec::OnConflictTarget::Indexes(list.iter().map(|n| n.to_string()).collect())
}

#[test]
fn conflict_target_takes_the_predicate_of_the_named_index() {
    let sql = conflict_sql(Dialect::Postgres, "Doc", "slug", names(&["doc_slug_live"])).unwrap();
    assert!(sql.contains(r#"ON CONFLICT ("slug") WHERE deleted_at IS NULL DO NOTHING"#), "{sql}");
}

#[test]
fn a_list_of_indexes_sends_the_and_of_their_predicates() {
    let sql = conflict_sql(Dialect::Postgres, "Doc", "slug", names(&["doc_slug_live", "doc_slug_visible"])).unwrap();
    assert!(sql.contains(r#"ON CONFLICT ("slug") WHERE (deleted_at IS NULL) AND (hidden = false) DO NOTHING"#), "{sql}");
    assert!(!sql.contains("ON CONSTRAINT"), "{sql}");
}

#[test]
fn a_list_of_indexes_is_an_error_on_sqlite() {
    let err = conflict_sql(Dialect::Sqlite, "Doc", "slug", names(&["doc_slug_live", "doc_slug_visible"])).unwrap_err();
    assert!(err.contains("one index"), "{err}");
}

#[test]
fn a_named_index_must_be_a_unique_index_on_the_conflict_columns() {
    let unknown = conflict_sql(Dialect::Postgres, "Doc", "slug", names(&["nope"])).unwrap_err();
    assert!(unknown.contains("not an index of Doc") && unknown.contains("doc_slug_live"), "{unknown}");
    let other_model = conflict_sql(Dialect::Postgres, "Solo", "slug", names(&["doc_slug_live"])).unwrap_err();
    assert!(other_model.contains("not an index of Solo"), "{other_model}");
    let not_unique = conflict_sql(Dialect::Postgres, "Doc", "title", names(&["doc_title_slug"])).unwrap_err();
    assert!(not_unique.contains("not unique"), "{not_unique}");
    let other_columns = conflict_sql(Dialect::Postgres, "Doc", "title", names(&["doc_slug_live"])).unwrap_err();
    assert!(other_columns.contains("exactly the conflict columns"), "{other_columns}");
    let empty = conflict_sql(Dialect::Postgres, "Doc", "slug", names(&[])).unwrap_err();
    assert!(empty.contains("at least one"), "{empty}");
    let twice = conflict_sql(Dialect::Postgres, "Doc", "slug", names(&["doc_slug_live", "doc_slug_live"])).unwrap_err();
    assert!(twice.contains("twice"), "{twice}");
}

#[test]
fn inference_takes_the_only_matching_index_and_refuses_two_partial_ones() {
    let one = conflict_sql(Dialect::Postgres, "Solo", "slug", exec::OnConflictTarget::Infer).unwrap();
    assert!(one.contains("WHERE deleted_at IS NULL DO NOTHING"), "{one}");
    let two = conflict_sql(Dialect::Postgres, "Doc", "slug", exec::OnConflictTarget::Infer).unwrap_err();
    assert!(two.contains("doc_slug_live") && two.contains("doc_slug_visible") && two.contains("index="), "{two}");
    let plain = conflict_sql(Dialect::Postgres, "Doc", "title", exec::OnConflictTarget::Infer).unwrap();
    assert!(plain.contains(r#"ON CONFLICT ("title") DO NOTHING"#), "{plain}");
}

#[test]
fn a_count_insert_has_no_returning_clause() {
    let (_, schema) = dsl::check(dsl::compile(SOURCE, None).unwrap()).unwrap();
    let target = Target::new(Dialect::Postgres);
    let plan = |returning| {
        let rows = vec![vec![Some(Value::String(Some("a".into())))]];
        let plans = exec::plan_inserts(&schema, target, "Item", &["name".into()], rows, None, &NoParams, None, returning, &Default::default()).unwrap();
        exec::statement(target, &plans[0])
    };
    assert!(!plan(false).contains("RETURNING"), "{}", plan(false));
    assert!(plan(true).contains("RETURNING"), "{}", plan(true));
    // SQLite before 3.35 has no RETURNING; a count insert does not need it.
    let old = Target::new(Dialect::Sqlite).without(&["returning".into()]).unwrap();
    let rows = vec![vec![Some(Value::String(Some("a".into())))]];
    assert!(exec::plan_inserts(&schema, old, "Item", &["name".into()], rows.clone(), None, &NoParams, None, false, &Default::default()).is_ok());
    assert!(exec::plan_inserts(&schema, old, "Item", &["name".into()], rows, None, &NoParams, None, true, &Default::default()).is_err());
}

/// One parameter that the statement does not use: it still counts toward the limit.
struct OneParam;

impl orm_engine::params::Params for OneParam {
    fn len(&self) -> usize {
        1
    }
    fn value(&self, _: usize, _: Option<orm_core::ir::ValueType>) -> orm_engine::error::Result<Value> {
        Ok(Value::String(Some("p".into())))
    }
    fn text(&self, _: usize) -> orm_engine::error::Result<String> {
        Ok("p".into())
    }
    fn count(&self, _: usize) -> orm_engine::error::Result<u64> {
        Ok(1)
    }
}

fn statements(schema: &orm_core::schema::Schema, target: Target, model: &str, fields: &[String], rows: Vec<Vec<Option<Value>>>, params: &dyn orm_engine::params::Params) -> Vec<usize> {
    exec::plan_inserts(schema, target, model, fields, rows, None, params, None, true, &Default::default())
        .unwrap()
        .iter()
        .map(|p| match p {
            Plan::Insert(s, _) => orm_engine::db::build(target.dialect, s).1.len(),
            _ => panic!("an insert plan"),
        })
        .collect()
}

#[test]
fn the_statement_parameters_count_toward_the_limit() {
    let (_, schema) = dsl::check(dsl::compile(SOURCE, None).unwrap()).unwrap();
    let target = Target::new(Dialect::Postgres).without(&["max_params=6".into()]).unwrap();
    let rows = (0..5).map(|i| vec![Some(Value::String(Some(format!("n{i}"))))]).collect();
    // (6 - 1) / 2 = 2 rows per statement.
    assert_eq!(statements(&schema, target, "Item", &["name".into()], rows, &OneParam), vec![4, 4, 2]);
}

#[test]
fn rows_without_values_go_one_per_statement() {
    let (_, schema) = dsl::check(dsl::compile("model Blank {\n id BigInt @id @default(autoincrement())\n}\n", None).unwrap()).unwrap();
    let target = Target::new(Dialect::Postgres);
    assert_eq!(statements(&schema, target, "Blank", &[], vec![vec![], vec![], vec![]], &NoParams), vec![0, 0, 0]);
}

#[test]
fn a_write_template_counts_each_placeholder() {
    let mut ir = dsl::compile(SOURCE, None).unwrap();
    let name = ir.models[0].fields.iter_mut().find(|f| f.name == "name").unwrap();
    name.write_sql = Some("concat({}, {})".into());
    let (_, schema) = dsl::check(ir).unwrap();
    let target = Target::new(Dialect::Postgres).without(&["max_params=7".into()]).unwrap();
    let rows = (0..5).map(|i| vec![Some(Value::String(Some(format!("n{i}"))))]).collect();
    // `name` binds twice and `token` once: 3 parameters per row, 2 rows per statement.
    assert_eq!(statements(&schema, target, "Item", &["name".into()], rows, &NoParams), vec![6, 6, 3]);
}
