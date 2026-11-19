//! `pull`, baseline and drift against a live PostgreSQL database. The tests run in a database of their own
//! (`<test database>_pull`) and drop and recreate its `public` schema.
#![cfg(feature = "postgres")]
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use monorm_core::{dsl, migrate as ddl};
use monorm_engine::{db::{self, Driver}, introspect, migrate};

const LOSSY: &str = r#"
DROP SCHEMA IF EXISTS other CASCADE; CREATE SCHEMA other;
CREATE TABLE other.users (id uuid PRIMARY KEY);
CREATE TABLE users (id bigint PRIMARY KEY);
CREATE TABLE profiles (id bigint PRIMARY KEY, user_id uuid REFERENCES other.users, owner_id bigint REFERENCES users);
ALTER TABLE profiles ENABLE ROW LEVEL SECURITY;
CREATE TABLE booking (id bigint PRIMARY KEY, a int, b int, CONSTRAINT booking_a_uniq UNIQUE (a) INCLUDE (b));
CREATE UNLOGGED TABLE cache (id bigint PRIMARY KEY) WITH (fillfactor = 80);
CREATE TABLE a (id bigint PRIMARY KEY, n int CONSTRAINT positive CHECK (n > 0), p bigint CONSTRAINT fk_p REFERENCES users);
CREATE TABLE b (id bigint PRIMARY KEY, n int CONSTRAINT positive CHECK (n > 0), p bigint CONSTRAINT fk_p REFERENCES users);
CREATE TABLE t (id bigint PRIMARY KEY, c int CONSTRAINT t_c_key UNIQUE, d int CONSTRAINT t_d_key UNIQUE, "class" int,
    price numeric(10, 2) NOT NULL DEFAULT 0.00);
ALTER TABLE t ADD CONSTRAINT uq2 UNIQUE NULLS NOT DISTINCT (c);
CREATE INDEX t_d_idx ON t (d);
CREATE FUNCTION g() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN -- """
RETURN NEW; END $$;
CREATE TRIGGER t_g BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION g();
"#;

/// The tests share one database.
static POSTGRES: Mutex<()> = Mutex::new(());

struct Dir(PathBuf);

impl Dir {
    fn new(label: &str) -> Dir {
        let path = std::env::temp_dir().join(format!("monorm-pull-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Dir(path)
    }
    fn join(&self, name: &str) -> PathBuf { self.0.join(name) }
}

impl Drop for Dir {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

/// The URL of the database the tests recreate, and the URL of the server's maintenance database.
fn urls() -> Option<(String, String, String)> {
    let url = std::env::var("ORM_RUST_TEST_POSTGRES_URL").ok()?;
    let (base, tail) = url.rsplit_once('/')?;
    let (name, query) = tail.split_once('?').map_or((tail, String::new()), |(n, q)| (n, format!("?{q}")));
    let pull = format!("{name}_pull");
    Some((format!("{base}/{pull}{query}"), format!("{base}/postgres{query}"), pull))
}

/// Runs `body` on a fresh `public` schema holding `sql`; skips when no PostgreSQL URL is set.
fn with_database<F, Fut>(sql: &str, body: F)
where
    F: FnOnce(Arc<dyn Driver>, String, String) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let Some((url, admin_url, name)) = urls() else { return };
    let _lock = POSTGRES.lock().unwrap_or_else(|e| e.into_inner());
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let admin = db::connect(&admin_url, 1).await.unwrap();
        if admin.query_text(format!("SELECT 1 FROM pg_database WHERE datname = '{name}'")).await.unwrap().is_empty() {
            admin.batch(format!("CREATE DATABASE \"{name}\"")).await.unwrap();
        }
        admin.batch(format!("ALTER DATABASE \"{name}\" RESET standard_conforming_strings")).await.unwrap();
        let conn = db::connect(&url, 2).await.unwrap();
        conn.batch(format!("DROP SCHEMA public CASCADE; CREATE SCHEMA public;{sql}")).await.unwrap();
        body(conn, url, name).await;
    });
}

fn pulled_schema(source: &str) -> monorm_core::schema::Schema { dsl::check(dsl::compile(source, None).unwrap()).unwrap().1 }

/// Writes the pulled schema to a file and makes the first migration from it.
fn make(dir: &Dir, schema: &str) -> PathBuf {
    let file = dir.join("schema.prisma");
    std::fs::write(&file, schema).unwrap();
    let migrations = dir.join("migrations");
    ddl::files::make(&migrations, &pulled_schema(schema), None, false, false).unwrap();
    migrations
}

async fn pull(conn: &dyn Driver) -> introspect::Pull { introspect::pull(conn, &dsl::Installed::new()).await.unwrap() }

#[test]
fn a_database_made_by_the_orm_pulls_back_without_differences() {
    with_database("", |conn, _, _| async move {
        let dir = Dir::new("blog");
        let blog = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/blog/schema.prisma")).unwrap();
        let migrations = dir.join("blog");
        ddl::files::make(&migrations, &pulled_schema(&blog), None, false, false).unwrap();
        assert!(!migrate::upgrade(&*conn, &migrations, None, true).await.unwrap().is_empty());
        let drift = introspect::drift(&*conn, &migrations).await.unwrap();
        assert!(drift.steps.is_empty(), "{:?}", drift.steps);
        let pulled = pull(&*conn).await;
        assert!(pulled.steps.is_empty(), "{:?}", pulled.steps);
        assert!(pulled.gaps.is_empty(), "{:?}", pulled.gaps);
        assert!(pulled.schema.contains("@@check(") && pulled.schema.contains("extensions = [pg_trgm]"));
    });
}

#[test]
fn add_identity_refuses_a_column_that_owns_a_sequence_but_defaults_to_another() {
    with_database(
        "CREATE SEQUENCE other_seq; CREATE TABLE t_owns (id serial PRIMARY KEY); ALTER TABLE t_owns ALTER COLUMN id SET DEFAULT nextval('other_seq')",
        |conn, _, _| async move {
            let dir = Dir::new("identity");
            let pulled = pull(&*conn).await;
            let default = "@default(dbgenerated(\"nextval('other_seq'::regclass)\"))";
            assert!(pulled.schema.contains(default), "{}", pulled.schema);
            let migrations = make(&dir, &pulled.schema.replace(default, "@default(autoincrement())"));
            migrate::baseline(&*conn, &migrations).await.unwrap();
            let err = introspect::drift(&*conn, &migrations).await.err().unwrap().to_string();
            assert!(err.contains("t_owns.id: the column takes its values from a sequence it does not own"), "{err}");
        },
    );
}

#[test]
fn an_older_migrations_table_gets_the_baseline_column() {
    with_database(
        "CREATE TABLE monorm_migrations (name text PRIMARY KEY, checksum text NOT NULL, applied_at timestamptz NOT NULL DEFAULT now()); CREATE TABLE keep (id bigint PRIMARY KEY)",
        |conn, _, _| async move {
            let dir = Dir::new("older");
            let migrations = make(&dir, &pull(&*conn).await.schema);
            migrate::baseline(&*conn, &migrations).await.unwrap();
            assert_eq!(conn.query_text("SELECT baseline FROM monorm_migrations".into()).await.unwrap(), vec![vec![Some("t".to_string())]]);
        },
    );
}

#[test]
fn comments_cannot_escape_the_shadow() {
    with_database("CREATE TABLE victim (id bigint PRIMARY KEY); CREATE TABLE t (id bigint PRIMARY KEY);", |conn, url, name| async move {
        conn.batch(format!("ALTER DATABASE \"{name}\" SET standard_conforming_strings = off")).await.unwrap();
        conn.batch("SET standard_conforming_strings = on; COMMENT ON TABLE t IS 'x\\''; COMMIT; DROP TABLE victim; --'".into()).await.unwrap();
        let legacy = db::connect(&url, 1).await.unwrap();
        let pulled = pull(&*legacy).await;
        conn.batch(format!("ALTER DATABASE \"{name}\" RESET standard_conforming_strings")).await.unwrap();
        let exists = conn.query_text("SELECT to_regclass('victim') IS NOT NULL".into()).await.unwrap();
        assert_eq!(exists, vec![vec![Some("t".to_string())]]);
        let shadows = conn.query_text("SELECT count(*) FROM pg_namespace WHERE nspname LIKE 'monorm_shadow%'".into()).await.unwrap();
        assert_eq!(shadows, vec![vec![Some("0".to_string())]]);
        assert!(pulled.steps.is_empty(), "{:?}", pulled.steps);
    });
}

#[test]
fn drift_after_an_enum_value_recreates_the_columns() {
    with_database(
        "CREATE TYPE order_state AS ENUM ('new', 'paid'); CREATE TABLE orders (id bigint PRIMARY KEY, state order_state);",
        |conn, _, _| async move {
            let dir = Dir::new("enum");
            let migrations = make(&dir, &pull(&*conn).await.schema);
            migrate::baseline(&*conn, &migrations).await.unwrap();
            conn.batch("ALTER TYPE order_state ADD VALUE 'refunded'".into()).await.unwrap();
            let drift = introspect::drift(&*conn, &migrations).await.unwrap();
            let sql: Vec<_> = drift.steps.iter().map(|s| s.sql.clone()).collect();
            assert!(sql.join("\n").contains("ALTER COLUMN \"state\" TYPE"), "{sql:?}");
        },
    );
}

#[test]
fn drift_after_pulling_an_interval_with_a_precision_is_a_step_not_a_refusal() {
    with_database("CREATE TABLE jobs (id bigint PRIMARY KEY, timeout interval(3))", |conn, _, _| async move {
        let dir = Dir::new("interval");
        let pulled = pull(&*conn).await;
        assert!(pulled.schema.contains("Duration"), "{}", pulled.schema);
        let migrations = make(&dir, &pulled.schema);
        migrate::baseline(&*conn, &migrations).await.unwrap();
        let drift = introspect::drift(&*conn, &migrations).await.unwrap();
        let sql: Vec<_> = drift.steps.iter().map(|s| s.sql.clone()).collect();
        assert!(sql.join("\n").contains("ALTER COLUMN \"timeout\" TYPE"), "{sql:?}");
    });
}

#[test]
fn pull_reports_what_it_cannot_write() {
    with_database(LOSSY, |conn, _, _| async move {
        let dir = Dir::new("lossy");
        let pulled = pull(&*conn).await;
        let (text, gaps) = (&pulled.schema, pulled.gaps.join("\n"));
        assert!(gaps.contains("references other.users, outside the schema; it is left out"), "{gaps}");
        assert!(!text.contains("fields: [user_id]"));
        assert!(gaps.contains("table profiles has row-level security"), "{gaps}");
        assert!(gaps.contains("booking.booking_a_uniq has INCLUDE columns"), "{gaps}");
        assert!(gaps.contains("table cache is UNLOGGED") && gaps.contains("table cache has storage options (fillfactor=80)"), "{gaps}");
        assert!(text.matches("name: \"positive\")").count() == 2 && text.matches("map: \"fk_p\")").count() == 2, "{text}");
        assert!(text.contains("map: \"uq2\", nulls_not_distinct: true"), "{text}");
        assert!(text.contains("@@index([d], name: \"t_d_idx\")"), "{text}");
        assert!(text.contains("class_ Int? @map(\"class\")"), "{text}");
        assert!(text.contains("@default(dbgenerated(\"0.00\"))"), "{text}");
        assert!(!text.contains("@@trigger(t_g") && gaps.contains("trigger t.t_g calls g, which is not pulled"), "{gaps}");
        let steps: Vec<_> = pulled.steps.iter().map(|s| s.summary.as_str()).collect();
        assert_eq!(steps, ["drop trigger t_g on t", "drop function g()"]);

        let migrations = make(&dir, text);
        let other = db::connect(&urls().unwrap().0, 1).await.unwrap();
        other.batch("DROP SCHEMA public CASCADE; CREATE SCHEMA public".into()).await.unwrap();
        migrate::upgrade(&*other, &migrations, None, true).await.unwrap();
    });
}
