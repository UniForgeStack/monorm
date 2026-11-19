#![cfg(feature = "sqlite")]
use std::sync::Arc;
use monorm_core::{dsl, migrate as ddl};
use monorm_engine::{db::{self, Driver, Executor, ErrorKind}, exec, migrate, Error};

fn runtime() -> tokio::runtime::Runtime { tokio::runtime::Runtime::new().unwrap() }

#[test]
fn worker_transactions_and_integrity() {
    runtime().block_on(async {
        let db = db::connect("sqlite://:memory:", 4).await.unwrap();
        db.batch("CREATE TABLE probe (id INTEGER PRIMARY KEY, name TEXT UNIQUE)".into()).await.unwrap();
        let tx = db.begin().await.unwrap();
        tx.execute("INSERT INTO probe VALUES (?, ?)".into(), vec![1i64.into(), "outer".into()]).await.unwrap();
        let nested = tx.begin().await.unwrap();
        nested.execute("INSERT INTO probe VALUES (?, ?)".into(), vec![2i64.into(), "inner".into()]).await.unwrap();
        nested.rollback().await.unwrap();
        tx.commit().await.unwrap();
        assert_eq!(db.query("SELECT count(*) FROM probe".into(), vec![]).await.unwrap().get_i64(0,0).unwrap(), 1);
        assert!(tx.batch("SELECT 1".into()).await.is_err());
        let tx = db.begin().await.unwrap();
        tx.batch("INSERT INTO probe VALUES (3, 'abandoned')".into()).await.unwrap();
        drop(tx);
        assert_eq!(db.query("SELECT count(*) FROM probe".into(), vec![]).await.unwrap().get_i64(0,0).unwrap(), 1);
        let error = db.batch("INSERT INTO probe VALUES (4, 'outer')".into()).await.err().unwrap();
        assert_eq!(error.kind, ErrorKind::Integrity);
        db.close().await;
        assert!(db.batch("SELECT 1".into()).await.is_err());
    });
}

/// Apply the plan back from `second` to `first`; the title of the one book.
async fn tx_down(db: &Arc<dyn Driver>, first: &monorm_core::schema::Schema, second: &monorm_core::schema::Schema) -> String {
    let plan = ddl::plan(second, &ddl::snapshot(first).unwrap()).unwrap();
    let tx = db.begin_migration().await.unwrap();
    for step in &plan.down { tx.batch(step.sql.clone()).await.unwrap(); }
    tx.commit().await.unwrap();
    db.query_text("SELECT title FROM book".into()).await.unwrap()[0][0].clone().unwrap()
}

#[test]
fn rebuilds_preserve_cascading_relations_and_autoincrement() {
    runtime().block_on(async {
        let source = include_str!("../../../examples/sqlite/schema.prisma");
        let schema = |source: &str| dsl::check(dsl::compile(source, None).unwrap()).unwrap().1;
        let first = schema(source);
        let second = schema(&source.replace("  title     String", "  name      String @renamed_from(\"title\")").replace("  pages     Int", "  added     Int @default(42)\n  pages     Int").replace("@@index([title]", "@@index([name]"));
        let db = db::connect("sqlite://:memory:", 1).await.unwrap();
        let conn: Arc<dyn Executor> = db.clone();
        exec::run_schema_script(conn, ddl::create_all(&first).unwrap()).await.unwrap();
        db.batch("INSERT INTO author (email, name) VALUES ('a', 'A'); INSERT INTO book (author_id, title) VALUES (1, 'kept'); INSERT INTO author (id, email, name) VALUES (100, 'gone', 'gone'); DELETE FROM author WHERE id=100".into()).await.unwrap();
        let plan = ddl::plan(&second, &ddl::snapshot(&first).unwrap()).unwrap();
        let tx = db.begin_migration().await.unwrap();
        for step in &plan.up { tx.batch(step.sql.clone()).await.unwrap(); }
        tx.commit().await.unwrap();
        assert_eq!(db.query_text("SELECT name, added FROM book".into()).await.unwrap()[0], [Some("kept".into()), Some("42".into())]);
        assert!(db.query_text("PRAGMA foreign_key_check".into()).await.unwrap().is_empty());
        db.batch("INSERT INTO author (email, name) VALUES ('next', 'Next')".into()).await.unwrap();
        assert_eq!(db.query("SELECT max(id) FROM author".into(), vec![]).await.unwrap().get_i64(0,0).unwrap(), 101);
        let tx = db.begin_migration().await.unwrap();
        tx.batch("DELETE FROM author WHERE id=1".into()).await.unwrap();
        assert_eq!(tx.commit().await.err().unwrap().kind, ErrorKind::Integrity);
        assert_eq!(db.query("SELECT count(*) FROM author".into(), vec![]).await.unwrap().get_i64(0,0).unwrap(), 2);
        let error = db.batch("INSERT INTO book (author_id, name) VALUES (999, 'orphan')".into()).await.err().unwrap();
        assert_eq!(error.kind, ErrorKind::Integrity);
        let down = tx_down(&db, &first, &second).await;
        assert_eq!(down, "kept", "a downgrade keeps the rows under the old column name");
        let conn: Arc<dyn Executor> = db.clone();
        exec::run_schema_script(conn, ddl::drop_all(&first).unwrap()).await.unwrap();
        db.close().await;
    });
}

#[test]
fn finished_nested_transaction_releases_abandoned_parent() {
    runtime().block_on(async {
        let db = db::connect("sqlite://:memory:", 1).await.unwrap();
        db.batch("CREATE TABLE probe (id INTEGER)".into()).await.unwrap();
        let tx = db.begin().await.unwrap();
        let nested = tx.begin().await.unwrap();
        nested.commit().await.unwrap();
        tx.batch("INSERT INTO probe VALUES (1)".into()).await.unwrap();
        drop(tx);
        let rows = tokio::time::timeout(std::time::Duration::from_secs(3), db.query("SELECT count(*) FROM probe".into(), vec![])).await.unwrap().unwrap();
        assert_eq!(rows.get_i64(0, 0).unwrap(), 0);
        assert!(nested.batch("SELECT 1".into()).await.is_err());
        db.close().await;
    });
}

const SOURCE: &str = include_str!("../../../examples/sqlite/schema.prisma");

fn schema(source: &str) -> monorm_core::schema::Schema { dsl::check(dsl::compile(source, None).unwrap()).unwrap().1 }

/// A migrations directory with one migration per source, and an in-memory database.
async fn migrations(label: &str, sources: &[String]) -> (std::path::PathBuf, Arc<dyn Driver>) {
    let dir = std::env::temp_dir().join(format!("monorm-sqlite-{}-{label}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (i, source) in sources.iter().enumerate() {
        let name = if i == 0 { "initial" } else { "change" };
        assert!(!ddl::files::make(&dir, &schema(source), Some(name), false, false).unwrap().is_empty());
    }
    (dir, db::connect("sqlite://:memory:", 1).await.unwrap())
}

async fn applied(db: &Arc<dyn Driver>, dir: &std::path::Path) -> Vec<bool> {
    migrate::status(&**db, dir).await.unwrap().iter().map(|s| s.applied).collect()
}

#[test]
fn cyclic_fks_table_rename_and_empty_sequence() {
    runtime().block_on(async {
        let source = "datasource db { provider = \"sqlite\" }\nmodel A {\n  id BigInt @id @default(autoincrement())\n  b_id BigInt?\n  b B? @relation(\"AB\", fields: [b_id], references: [id])\n  bs B[] @relation(\"BA\")\n}\nmodel B {\n  id BigInt @id @default(autoincrement())\n  a_id BigInt?\n  a A? @relation(\"BA\", fields: [a_id], references: [id])\n  as A[] @relation(\"AB\")\n}\nmodel Empty {\n  id BigInt @id @default(autoincrement())\n}\n".to_string();
        let renamed = source.replace("  bs B[] @relation(\"BA\")", "  bs B[] @relation(\"BA\")\n  @@map(\"renamed_a\")\n  @@renamed_from(\"a\")");
        let (dir, db) = migrations("cyclic", &[source, renamed]).await;
        migrate::upgrade(&*db, &dir, Some("1"), true).await.unwrap();
        db.batch("INSERT INTO a DEFAULT VALUES; INSERT INTO b (a_id) VALUES (1); UPDATE a SET b_id=1; INSERT INTO empty (id) VALUES (200); DELETE FROM empty".into()).await.unwrap();
        migrate::upgrade(&*db, &dir, None, true).await.unwrap();
        assert_eq!(db.query_text("SELECT b_id FROM renamed_a".into()).await.unwrap()[0][0].as_deref(), Some("1"));
        assert!(db.query_text("PRAGMA foreign_key_check".into()).await.unwrap().is_empty());
        db.batch("INSERT INTO empty DEFAULT VALUES".into()).await.unwrap();
        assert_eq!(db.query_text("SELECT id FROM empty".into()).await.unwrap()[0][0].as_deref(), Some("201"));
        migrate::downgrade(&*db, &dir, migrate::Down::Steps(1), false, true).await.unwrap();
        assert_eq!(db.query_text("SELECT b_id FROM a".into()).await.unwrap()[0][0].as_deref(), Some("1"));
        assert!(db.query_text("PRAGMA foreign_key_check".into()).await.unwrap().is_empty());
        db.batch("INSERT INTO empty DEFAULT VALUES".into()).await.unwrap();
        assert_eq!(db.query_text("SELECT max(id) FROM empty".into()).await.unwrap()[0][0].as_deref(), Some("202"));
        db.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    });
}

#[test]
fn failed_rebuild_rolls_back_schema_rows_and_history() {
    runtime().block_on(async {
        let strict = SOURCE.replace("@check(raw(\"pages >= 0\"))", "@check(raw(\"pages >= 10\"))");
        assert_ne!(strict, SOURCE);
        let (dir, db) = migrations("rollback", &[SOURCE.to_string(), strict]).await;
        migrate::upgrade(&*db, &dir, Some("1"), true).await.unwrap();
        db.batch("INSERT INTO author (email, name) VALUES ('a', 'A'); INSERT INTO book (author_id, title) VALUES (1, 'kept')".into()).await.unwrap();
        let error = migrate::upgrade(&*db, &dir, None, true).await.err().unwrap();
        assert!(matches!(&error, Error::Db(e) if e.kind == ErrorKind::Integrity), "{error}");
        assert_eq!(db.query_text("SELECT title, pages FROM book".into()).await.unwrap()[0], [Some("kept".into()), Some("0".into())]);
        assert_eq!(applied(&db, &dir).await, [true, false]);
        assert!(db.query_text("SELECT name FROM sqlite_schema WHERE name LIKE '__orm_%'".into()).await.unwrap().is_empty());
        assert_eq!(db.query_text("PRAGMA foreign_keys".into()).await.unwrap()[0][0].as_deref(), Some("1"));
        db.batch("UPDATE book SET pages=10".into()).await.unwrap();
        migrate::upgrade(&*db, &dir, None, true).await.unwrap();
        assert_eq!(applied(&db, &dir).await, [true, true]);
        migrate::downgrade(&*db, &dir, migrate::Down::Steps(1), false, true).await.unwrap();
        db.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    });
}

#[test]
fn rebuild_refuses_unmanaged_objects() {
    runtime().block_on(async {
        let extra = SOURCE.replace("  name       String", "  name       String\n  extra      Int @default(1)");
        assert_ne!(extra, SOURCE);
        for (object, drop) in [
            ("CREATE INDEX custom_index ON author(name)", "DROP INDEX custom_index"),
            ("CREATE TRIGGER custom_trigger AFTER INSERT ON author BEGIN SELECT 1; END", "DROP TRIGGER custom_trigger"),
        ] {
            let (dir, db) = migrations("unmanaged", &[SOURCE.to_string(), extra.clone()]).await;
            migrate::upgrade(&*db, &dir, Some("1"), true).await.unwrap();
            db.batch("INSERT INTO author (email, name) VALUES ('a', 'A')".into()).await.unwrap();
            db.batch(object.into()).await.unwrap();
            let error = migrate::upgrade(&*db, &dir, None, true).await.err().unwrap();
            assert!(error.to_string().contains("monorm_unmanaged_index_or_trigger"), "{error}");
            assert_eq!(db.query_text("SELECT name FROM author".into()).await.unwrap()[0][0].as_deref(), Some("A"));
            assert_eq!(applied(&db, &dir).await, [true, false]);
            assert!(!db.query_text("SELECT name FROM sqlite_schema WHERE name LIKE 'custom_%'".into()).await.unwrap().is_empty());
            db.batch(drop.into()).await.unwrap();
            migrate::upgrade(&*db, &dir, None, true).await.unwrap();
            migrate::downgrade(&*db, &dir, migrate::Down::Steps(1), false, true).await.unwrap();
            db.close().await;
            let _ = std::fs::remove_dir_all(&dir);
        }
    });
}
