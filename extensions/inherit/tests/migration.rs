//! An override keeps the base table, so a migration adds or drops only the child's own columns.
use orm_contracts::{extension::capture_storage, ir::SchemaIr};
use orm_core::migrate::{self, DbSchema, Step};
use serde_json::{json, Value};

const OWN: &str = "nick";

fn user(extra: bool) -> Vec<Value> {
    let mut fields = vec![
        json!({"name": "id", "column": "id", "type": "int", "primary_key": true}),
        json!({"name": "name", "column": "name", "type": "string"}),
    ];
    if extra {
        fields.push(json!({"name": OWN, "column": OWN, "type": "string", "nullable": true}));
    }
    fields
}

/// `overriding` adds the own column in a `MyUser` model that overrides `User`; `map` gives it its own table.
fn plan(dialect: &str, overriding: bool, map: Option<&str>, previous: &DbSchema) -> (Vec<Step>, DbSchema) {
    let mut models = vec![json!({"name": "User", "table": "user", "fields": user(false)})];
    let mut declarations = vec![];
    if overriding {
        let table = map.unwrap_or("myuser");
        models.push(json!({"name": "MyUser", "table": table, "fields": [{"name": OWN, "column": OWN, "type": "string", "nullable": true}]}));
        declarations.push(json!({"attribute": "inherit.override", "model": "MyUser", "field": null, "arguments": {},
            "positional": ["User"], "location": {"file": "schema.prisma", "line": 9, "column": 1}}));
    }
    let mut ir: SchemaIr = serde_json::from_value(json!({"dialect": dialect, "models": models, "behavior": {"declarations": declarations}})).unwrap();
    orm_inherit::expand(&mut ir).unwrap();
    capture_storage(&mut ir).unwrap();
    let storage = json!({"dialect": ir.dialect, "models": ir.behavior.storage.as_ref().unwrap().models});
    let schema = orm_core::schema::Schema::from_ir(serde_json::from_value(storage).unwrap()).unwrap();
    let plan = migrate::plan(&schema, previous).unwrap();
    (plan.up, plan.snapshot)
}

fn sql(steps: &[Step]) -> Vec<&str> {
    steps.iter().map(|s| s.sql.as_str()).collect()
}

fn rows(db: &rusqlite::Connection, table: &str) -> Vec<String> {
    let mut statement = db.prepare(&format!("SELECT name FROM \"{table}\" ORDER BY id")).unwrap();
    statement.query_map([], |r| r.get::<_, String>(0)).unwrap().map(Result::unwrap).collect()
}

fn columns(db: &rusqlite::Connection, table: &str) -> Vec<String> {
    let mut statement = db.prepare(&format!("SELECT name FROM pragma_table_info('{table}') ORDER BY cid")).unwrap();
    statement.query_map([], |r| r.get::<_, String>(0)).unwrap().map(Result::unwrap).collect()
}

#[test]
fn sqlite_override_of_a_table_with_rows_keeps_the_rows_and_removing_it_drops_only_own_columns() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    let (up, v1) = plan("sqlite", false, None, &DbSchema::default());
    for step in &up { db.execute_batch(&step.sql).unwrap(); }
    db.execute_batch("INSERT INTO \"user\" (id, name) VALUES (1, 'a'), (2, 'b'), (3, 'c')").unwrap();

    let (up, v2) = plan("sqlite", true, None, &v1);
    assert!(up.iter().all(|s| !s.sql.contains("CREATE TABLE \"myuser\"")), "{:?}", sql(&up));
    for step in &up { db.execute_batch(&step.sql).unwrap(); }
    assert_eq!(columns(&db, "user"), ["id", "name", OWN]);
    assert_eq!(rows(&db, "user"), ["a", "b", "c"]);

    let (down, _) = plan("sqlite", false, None, &v2);
    for step in &down { db.execute_batch(&step.sql).unwrap(); }
    assert_eq!(columns(&db, "user"), ["id", "name"]);
    assert_eq!(rows(&db, "user"), ["a", "b", "c"]);
    assert!(down.iter().any(|s| s.warning.as_deref().is_some_and(|w| w.contains(OWN))), "{down:?}");
}

#[test]
fn sqlite_override_with_its_own_table_renames_and_keeps_the_rows() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    let (up, v1) = plan("sqlite", false, None, &DbSchema::default());
    for step in &up { db.execute_batch(&step.sql).unwrap(); }
    db.execute_batch("INSERT INTO \"user\" (id, name) VALUES (1, 'a'), (2, 'b')").unwrap();
    let (up, _) = plan("sqlite", true, Some("people"), &v1);
    for step in &up { db.execute_batch(&step.sql).unwrap(); }
    assert_eq!(columns(&db, "people"), ["id", "name", OWN]);
    assert_eq!(rows(&db, "people"), ["a", "b"]);
}

#[test]
fn postgres_override_of_a_table_with_rows_keeps_the_rows_and_removing_it_drops_only_own_columns() {
    let Ok(url) = std::env::var("ORM_RUST_TEST_POSTGRES_URL") else { return };
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let conn = orm_engine::db::connect(&url, 1).await.unwrap();
        conn.batch("DROP SCHEMA IF EXISTS inherit_acceptance CASCADE; CREATE SCHEMA inherit_acceptance; SET search_path TO inherit_acceptance, public".into()).await.unwrap();
        let columns = |table: &'static str| format!("SELECT column_name FROM information_schema.columns WHERE table_schema = 'inherit_acceptance' AND table_name = '{table}' ORDER BY ordinal_position");
        let names = |rows: Vec<Vec<Option<String>>>| rows.into_iter().map(|r| r[0].clone().unwrap()).collect::<Vec<_>>();

        let (up, v1) = plan("postgres", false, None, &DbSchema::default());
        for step in &up { conn.batch(step.sql.clone()).await.unwrap(); }
        conn.batch("INSERT INTO \"user\" (id, name) VALUES (1, 'a'), (2, 'b'), (3, 'c')".into()).await.unwrap();

        let (up, v2) = plan("postgres", true, None, &v1);
        assert!(up.iter().all(|s| !s.sql.contains("DROP TABLE") && !s.sql.contains("CREATE TABLE")), "{:?}", sql(&up));
        for step in &up { conn.batch(step.sql.clone()).await.unwrap(); }
        assert_eq!(names(conn.query_text(columns("user")).await.unwrap()), ["id", "name", OWN]);
        assert_eq!(names(conn.query_text("SELECT name FROM \"user\" ORDER BY id".into()).await.unwrap()), ["a", "b", "c"]);

        let (down, _) = plan("postgres", false, None, &v2);
        assert!(down.iter().all(|s| !s.sql.contains("DROP TABLE")), "{:?}", sql(&down));
        assert!(down.iter().any(|s| s.warning.as_deref().is_some_and(|w| w.contains(OWN))), "{down:?}");
        for step in &down { conn.batch(step.sql.clone()).await.unwrap(); }
        assert_eq!(names(conn.query_text(columns("user")).await.unwrap()), ["id", "name"]);
        assert_eq!(names(conn.query_text("SELECT name FROM \"user\" ORDER BY id".into()).await.unwrap()), ["a", "b", "c"]);
        conn.batch("DROP SCHEMA inherit_acceptance CASCADE".into()).await.unwrap();
    });
}

#[test]
fn postgres_override_with_its_own_table_renames_and_keeps_the_rows() {
    let Ok(url) = std::env::var("ORM_RUST_TEST_POSTGRES_URL") else { return };
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let conn = orm_engine::db::connect(&url, 1).await.unwrap();
        conn.batch("DROP SCHEMA IF EXISTS inherit_rename CASCADE; CREATE SCHEMA inherit_rename; SET search_path TO inherit_rename, public".into()).await.unwrap();
        let (up, v1) = plan("postgres", false, None, &DbSchema::default());
        for step in &up { conn.batch(step.sql.clone()).await.unwrap(); }
        conn.batch("INSERT INTO \"user\" (id, name) VALUES (1, 'a'), (2, 'b')".into()).await.unwrap();
        let (up, _) = plan("postgres", true, Some("people"), &v1);
        assert!(up.iter().any(|s| s.sql.contains("RENAME")), "{:?}", sql(&up));
        for step in &up { conn.batch(step.sql.clone()).await.unwrap(); }
        let rows = conn.query_text("SELECT name FROM people ORDER BY id".into()).await.unwrap();
        assert_eq!(rows.into_iter().map(|r| r[0].clone().unwrap()).collect::<Vec<_>>(), ["a", "b"]);
        conn.batch("DROP SCHEMA inherit_rename CASCADE".into()).await.unwrap();
    });
}
