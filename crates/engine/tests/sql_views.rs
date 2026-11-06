use monorm_core::dialect::Dialect;
use monorm_engine::{db, introspect};

fn postgres_url() -> String {
    std::env::var("ORM_RUST_TEST_POSTGRES_URL").or_else(|_| std::env::var("ORM_TEST_DATABASE_URL")).unwrap_or_else(|_| "postgres://postgres:postgres@localhost/orm_test".into())
}

/// Creates `statements`, pulls the schema, and checks that the pulled schema compiles and reproduces the database.
async fn pulls_back_and_matches(url: &str, dialect: Dialect, statements: &[&str], expected: &[&str], bodies: &[(&str, &str)]) -> introspect::Pull {
    let db = db::connect(url, 1).await.unwrap();
    let conn = db.begin().await.unwrap();
    for s in statements { conn.batch((*s).to_owned()).await.unwrap(); }
    let pulled = introspect::pull(conn.as_ref()).await.unwrap();
    for want in expected {
        assert!(pulled.schema.contains(want), "{dialect:?}: missing {want} in\n{}", pulled.schema);
    }
    for (path, text) in bodies {
        let body = pulled.views.get(std::path::Path::new(path)).unwrap_or_else(|| panic!("{dialect:?}: no sidecar {path} in {:?}", pulled.views.keys()));
        assert!(body.contains(text), "{dialect:?}: {path} is {body}");
    }
    assert!(pulled.steps.is_empty(), "{dialect:?}: {:?}", pulled.steps);
    conn.rollback().await.unwrap();
    db.close().await;
    pulled
}

#[test]
fn postgres_pull_reads_views_and_materialized_views_and_matches() {
    let statements = [
        "CREATE TABLE pv_orders (id bigint PRIMARY KEY, shop text NOT NULL, total integer NOT NULL)",
        "CREATE VIEW pv_big AS SELECT id, shop, total FROM pv_orders WHERE total > 100",
        "CREATE MATERIALIZED VIEW pv_by_shop AS SELECT shop AS id, sum(total) AS spent FROM pv_big GROUP BY shop",
        "CREATE UNIQUE INDEX pv_by_shop_id ON pv_by_shop (id)",
    ];
    let expected = ["view PvBig {", "view PvByShop {", "@@materialized", "@@depends_on([PvBig])", "@@index([id], name: \"pv_by_shop_id\", unique: true)"];
    let bodies = [("views/public/PvBig.sql", "total > 100"), ("views/public/PvByShop.sql", "sum(")];
    let pulled = tokio::runtime::Runtime::new().unwrap().block_on(pulls_back_and_matches(&postgres_url(), Dialect::Postgres, &statements, &expected, &bodies));
    assert!(!pulled.gaps.iter().any(|g| g.contains("view")), "{:?}", pulled.gaps);
}

#[test]
fn sqlite_pull_reads_a_view_and_matches() {
    let statements = [
        "CREATE TABLE pv_items (id INTEGER PRIMARY KEY, name TEXT NOT NULL, qty INTEGER NOT NULL)",
        "CREATE VIEW pv_stocked AS SELECT id, name FROM pv_items WHERE qty > 0",
    ];
    let bodies = [("views/main/PvStocked.sql", "qty > 0")];
    tokio::runtime::Runtime::new().unwrap().block_on(pulls_back_and_matches("sqlite://:memory:", Dialect::Sqlite, &statements, &["view PvStocked {"], &bodies));
}

#[test]
fn a_pulled_view_without_an_id_column_gets_the_first_column_as_key_and_a_gap() {
    let statements = ["CREATE TABLE pv_a (code text PRIMARY KEY, n integer NOT NULL)", "CREATE VIEW pv_a_view AS SELECT code, n FROM pv_a"];
    let pulled = tokio::runtime::Runtime::new().unwrap().block_on(pulls_back_and_matches(&postgres_url(), Dialect::Postgres, &statements, &["view PvAView {", "@id"], &[]));
    assert!(pulled.gaps.iter().any(|g| g.contains("pv_a_view") && g.contains("@id")), "{:?}", pulled.gaps);
}

#[test]
fn a_materialized_view_created_with_no_data_pulls_with_a_gap_line() {
    let statements = [
        "CREATE TABLE pv_n (id bigint PRIMARY KEY)",
        "CREATE MATERIALIZED VIEW pv_n_empty AS SELECT id FROM pv_n WITH NO DATA",
        "CREATE MATERIALIZED VIEW pv_n_full AS SELECT id FROM pv_n",
    ];
    let pulled = tokio::runtime::Runtime::new().unwrap().block_on(pulls_back_and_matches(&postgres_url(), Dialect::Postgres, &statements, &["view PvNEmpty {", "view PvNFull {"], &[]));
    let gaps: Vec<_> = pulled.gaps.iter().filter(|g| g.contains("WITH NO DATA")).collect();
    assert!(gaps.len() == 1 && gaps[0].contains("pv_n_empty"), "{:?}", pulled.gaps);
}
