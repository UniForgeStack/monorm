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

/// An override of `User` by `MyUser`: its own `@@map` and its `@@renamed_from`.
type Override<'a> = Option<(Option<&'a str>, Option<&'a str>)>;

/// `Role` carries a generated `{model}` foreign key and `Post.author` points at `User`, the table the override takes over.
/// `MyUser` extends the abstract base and overrides `User`, so both keys must follow the table.
fn plan_with_keys(dialect: &str, overriding: Override, previous: &DbSchema) -> (Vec<Step>, DbSchema) {
    let key = json!({"name": "id", "column": "id", "type": "int", "primary_key": true, "auto_increment": true});
    let roles = json!({"name": "roles", "kind": "many", "target": "Role", "from": "", "to": "", "foreign_key": false, "label": "{model}_roles"});
    let mut base = json!({"name": "AbstractUser", "table": "abstractuser", "fields": [{"name": "name", "column": "name", "type": "string"}]});
    base["relations"] = json!([roles]);
    let author = json!({"name": "author", "kind": "one", "target": "User", "from": "author_id", "to": "id", "foreign_key": true});
    let mut post = json!({"name": "Post", "table": "post", "fields": [key.clone(), {"name": "author_id", "column": "author_id", "type": "int"}]});
    post["relations"] = json!([author]);
    let mut models = vec![
        base,
        json!({"name": "User", "table": "user", "fields": [key.clone()]}),
        json!({"name": "Role", "table": "role", "fields": [key.clone()]}),
        post,
    ];
    let mut declarations = vec![declare("inherit.abstract", "AbstractUser", &[]), declare("inherit.extends", "User", &["AbstractUser"])];
    if let Some((map, renamed_from)) = overriding {
        let mut my = json!({"name": "MyUser", "table": map.unwrap_or("myuser"),
            "fields": [key, {"name": OWN, "column": OWN, "type": "string", "nullable": true}]});
        my["renamed_from"] = json!(renamed_from);
        models.push(my);
        declarations.push(declare("inherit.extends", "MyUser", &["AbstractUser"]));
        declarations.push(declare("inherit.override", "MyUser", &["User"]));
    }
    expand_and_plan(dialect, models, declarations, previous)
}

fn declare(attribute: &str, model: &str, positional: &[&str]) -> Value {
    json!({"attribute": attribute, "model": model, "field": null, "arguments": {}, "positional": positional,
        "location": {"file": "schema.prisma", "line": 9, "column": 1}})
}

fn expand_and_plan(dialect: &str, models: Vec<Value>, declarations: Vec<Value>, previous: &DbSchema) -> (Vec<Step>, DbSchema) {
    let mut ir: SchemaIr = serde_json::from_value(json!({"dialect": dialect, "models": models, "behavior": {"declarations": declarations}})).unwrap();
    orm_inherit::expand(&mut ir).unwrap();
    capture_storage(&mut ir).unwrap();
    let storage = json!({"dialect": ir.dialect, "models": ir.behavior.storage.as_ref().unwrap().models});
    let schema = orm_core::schema::Schema::from_ir(serde_json::from_value(storage).unwrap()).unwrap();
    let plan = migrate::plan(&schema, previous).unwrap();
    (plan.up, plan.snapshot)
}

const SEED: &str = "INSERT INTO \"user\" (id, name) VALUES (1, 'a'), (2, 'b');
    INSERT INTO role (id, user_roles_id) VALUES (1, 1), (2, 2), (3, 2);
    INSERT INTO post (id, author_id) VALUES (1, 2), (2, 1)";

/// A table rebuild copies and drops the old table, so enforcement is off while it runs; `foreign_key_check` verifies the end state.
fn sqlite_db() -> rusqlite::Connection {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch("PRAGMA foreign_keys = OFF").unwrap();
    db
}

fn sqlite_apply(db: &rusqlite::Connection, steps: &[Step]) {
    for step in steps { db.execute_batch(&step.sql).unwrap_or_else(|e| panic!("{e}: {}", step.sql)); }
}

fn sqlite_count(db: &rusqlite::Connection, query: &str) -> Vec<i64> {
    let mut statement = db.prepare(query).unwrap();
    statement.query_map([], |r| r.get::<_, i64>(0)).unwrap().map(Result::unwrap).collect()
}

fn sqlite_keys(db: &rusqlite::Connection) -> Vec<String> {
    let mut statement = db.prepare("SELECT sql FROM sqlite_schema WHERE name IN ('role', 'post') ORDER BY name").unwrap();
    statement.query_map([], |r| r.get::<_, String>(0)).unwrap().map(Result::unwrap).collect()
}

#[test]
fn sqlite_override_plus_extends_keeps_the_rows_and_the_foreign_keys_into_the_table() {
    let db = sqlite_db();
    let (up, v1) = plan_with_keys("sqlite", None, &DbSchema::default());
    sqlite_apply(&db, &up);
    db.execute_batch(SEED).unwrap();
    let before = sqlite_keys(&db);
    assert!(before[0].contains("post_author_id_fkey") && before[1].contains("role_user_roles_id_fkey"), "{before:?}");

    let (up, _) = plan_with_keys("sqlite", Some((None, None)), &v1);
    assert!(up.iter().all(|s| !s.sql.contains("CREATE TABLE \"__orm_new_myuser\"")), "{:?}", sql(&up));
    sqlite_apply(&db, &up);
    assert_eq!(columns(&db, "user"), ["name", "id", OWN]);
    assert_eq!(rows(&db, "user"), ["a", "b"]);
    assert_eq!(sqlite_count(&db, "SELECT author_id FROM post ORDER BY id"), [2, 1]);
    assert_eq!(sqlite_count(&db, "SELECT user_roles_id FROM role ORDER BY id"), [1, 2, 2]);
    assert_eq!(sqlite_keys(&db), before);
    assert!(sqlite_count(&db, "SELECT 1 FROM pragma_foreign_key_check").is_empty());
}

#[test]
fn sqlite_removing_an_override_with_its_own_map_keeps_the_rows_in_two_steps() {
    let db = sqlite_db();
    let (up, v1) = plan_with_keys("sqlite", None, &DbSchema::default());
    sqlite_apply(&db, &up);
    db.execute_batch(SEED).unwrap();
    let (up, v2) = plan_with_keys("sqlite", Some((Some("people"), None)), &v1);
    sqlite_apply(&db, &up);
    assert_eq!(rows(&db, "people"), ["a", "b"]);

    // One step: `user` comes back empty and `people` goes, so the plan has to say so.
    let (one, _) = plan_with_keys("sqlite", None, &v2);
    assert!(one.iter().any(|s| s.sql.contains("DROP TABLE \"people\"")), "{:?}", sql(&one));
    assert!(one.iter().any(|s| s.warning.is_some()), "{one:?}");

    // Two steps: the override gives up its `@@map` and names it in `@@renamed_from`, then it goes.
    let (up, v3) = plan_with_keys("sqlite", Some((None, Some("people"))), &v2);
    sqlite_apply(&db, &up);
    assert_eq!(rows(&db, "user"), ["a", "b"]);
    assert_eq!(sqlite_count(&db, "SELECT author_id FROM post ORDER BY id"), [2, 1]);
    let (up, _) = plan_with_keys("sqlite", None, &v3);
    sqlite_apply(&db, &up);
    assert_eq!(columns(&db, "user"), ["name", "id"]);
    assert_eq!(rows(&db, "user"), ["a", "b"]);
    assert_eq!(sqlite_count(&db, "SELECT user_roles_id FROM role ORDER BY id"), [1, 2, 2]);
    assert!(sqlite_count(&db, "SELECT 1 FROM pragma_foreign_key_check").is_empty());
}

fn names(rows: Vec<Vec<Option<String>>>) -> Vec<String> {
    rows.into_iter().map(|r| r[0].clone().unwrap()).collect()
}

const PG_KEYS: &str = "SELECT conname FROM pg_constraint WHERE contype = 'f' AND connamespace = current_schema()::regnamespace ORDER BY conname";

#[test]
fn postgres_override_plus_extends_keeps_the_rows_and_the_foreign_keys_into_the_table() {
    let Ok(url) = std::env::var("ORM_RUST_TEST_POSTGRES_URL") else { return };
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let conn = orm_engine::db::connect(&url, 1).await.unwrap();
        conn.batch("DROP SCHEMA IF EXISTS inherit_keys CASCADE; CREATE SCHEMA inherit_keys; SET search_path TO inherit_keys, public".into()).await.unwrap();
        let (up, v1) = plan_with_keys("postgres", None, &DbSchema::default());
        for step in &up { conn.batch(step.sql.clone()).await.unwrap(); }
        conn.batch(SEED.into()).await.unwrap();
        let before = names(conn.query_text(PG_KEYS.into()).await.unwrap());
        assert_eq!(before, ["post_author_id_fkey", "role_user_roles_id_fkey"]);

        let (up, _) = plan_with_keys("postgres", Some((None, None)), &v1);
        assert!(up.iter().all(|s| !s.sql.contains("DROP TABLE") && !s.sql.contains("CREATE TABLE") && !s.sql.contains("DROP CONSTRAINT")), "{:?}", sql(&up));
        for step in &up { conn.batch(step.sql.clone()).await.unwrap(); }
        assert_eq!(names(conn.query_text(PG_KEYS.into()).await.unwrap()), before);
        assert_eq!(names(conn.query_text("SELECT name FROM \"user\" ORDER BY id".into()).await.unwrap()), ["a", "b"]);
        assert_eq!(names(conn.query_text("SELECT author_id::text FROM post ORDER BY id".into()).await.unwrap()), ["2", "1"]);
        assert_eq!(names(conn.query_text("SELECT user_roles_id::text FROM role ORDER BY id".into()).await.unwrap()), ["1", "2", "2"]);
        conn.batch("DROP SCHEMA inherit_keys CASCADE".into()).await.unwrap();
    });
}

#[test]
fn postgres_removing_an_override_with_its_own_map_keeps_the_rows_in_two_steps() {
    let Ok(url) = std::env::var("ORM_RUST_TEST_POSTGRES_URL") else { return };
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let conn = orm_engine::db::connect(&url, 1).await.unwrap();
        conn.batch("DROP SCHEMA IF EXISTS inherit_two_step CASCADE; CREATE SCHEMA inherit_two_step; SET search_path TO inherit_two_step, public".into()).await.unwrap();
        let (up, v1) = plan_with_keys("postgres", None, &DbSchema::default());
        for step in &up { conn.batch(step.sql.clone()).await.unwrap(); }
        conn.batch(SEED.into()).await.unwrap();
        let (up, v2) = plan_with_keys("postgres", Some((Some("people"), None)), &v1);
        for step in &up { conn.batch(step.sql.clone()).await.unwrap(); }
        assert_eq!(names(conn.query_text("SELECT name FROM people ORDER BY id".into()).await.unwrap()), ["a", "b"]);

        let (one, _) = plan_with_keys("postgres", None, &v2);
        assert!(one.iter().any(|s| s.sql.contains("DROP TABLE")), "{:?}", sql(&one));
        assert!(one.iter().any(|s| s.warning.is_some()), "{one:?}");

        let (up, v3) = plan_with_keys("postgres", Some((None, Some("people"))), &v2);
        assert!(up.iter().all(|s| !s.sql.contains("DROP TABLE")), "{:?}", sql(&up));
        for step in &up { conn.batch(step.sql.clone()).await.unwrap(); }
        assert_eq!(names(conn.query_text("SELECT name FROM \"user\" ORDER BY id".into()).await.unwrap()), ["a", "b"]);
        let (up, _) = plan_with_keys("postgres", None, &v3);
        assert!(up.iter().all(|s| !s.sql.contains("DROP TABLE")), "{:?}", sql(&up));
        for step in &up { conn.batch(step.sql.clone()).await.unwrap(); }
        assert_eq!(names(conn.query_text("SELECT name FROM \"user\" ORDER BY id".into()).await.unwrap()), ["a", "b"]);
        assert_eq!(names(conn.query_text(PG_KEYS.into()).await.unwrap()), ["post_author_id_fkey", "role_user_roles_id_fkey"]);
        conn.batch("DROP SCHEMA inherit_two_step CASCADE".into()).await.unwrap();
    });
}
