use monorm_engine::{db, introspect};

fn url() -> Option<String> {
    std::env::var("ORM_RUST_TEST_POSTGRES_URL").or_else(|_| std::env::var("ORM_TEST_DATABASE_URL")).ok()
}

const CREATE: &str = "CREATE TABLE rls_probe (id bigint PRIMARY KEY, shop_id bigint NOT NULL); \
    ALTER TABLE rls_probe ENABLE ROW LEVEL SECURITY; ALTER TABLE rls_probe FORCE ROW LEVEL SECURITY; \
    CREATE POLICY probe_shop ON rls_probe AS PERMISSIVE FOR ALL \
    USING (shop_id = (SELECT NULLIF(current_setting('scope.shop', true), '')::bigint)) \
    WITH CHECK (shop_id = (SELECT NULLIF(current_setting('scope.shop', true), '')::bigint))";

async fn reads_flags_and_policies(url: &str) {
    let db = db::connect(url, 1).await.unwrap();
    let conn = db.begin().await.unwrap();
    conn.batch(CREATE.to_owned()).await.unwrap();
    let live = introspect::introspect(conn.as_ref()).await.unwrap();
    let table = live.schema.tables.iter().find(|t| t.name == "rls_probe").unwrap();
    assert!(table.rls.enabled && table.rls.forced);
    assert_eq!(table.policies.len(), 1);
    let p = &table.policies[0];
    assert_eq!((p.name.as_str(), p.permissive), ("probe_shop", true));
    assert!(p.using.as_deref().unwrap().contains("scope.shop") && p.with_check.is_some());
    assert!(!live.gaps.iter().any(|g| g.contains("policy") || g.contains("row-level")), "{:?}", live.gaps);
    conn.rollback().await.unwrap();
    db.close().await;
}

#[test]
fn postgres_introspection_reads_the_flags_and_the_policy() {
    let Some(url) = url() else { return };
    tokio::runtime::Runtime::new().unwrap().block_on(reads_flags_and_policies(&url));
}

async fn pull_keeps_policies_as_gaps(url: &str) {
    let db = db::connect(url, 1).await.unwrap();
    let conn = db.begin().await.unwrap();
    conn.batch(CREATE.to_owned()).await.unwrap();
    let pulled = introspect::pull(conn.as_ref()).await.unwrap();
    assert!(pulled.gaps.iter().any(|g| g.contains("rls_probe has row-level security (forced)")), "{:?}", pulled.gaps);
    assert!(pulled.gaps.iter().any(|g| g.contains("policy probe_shop on rls_probe")), "{:?}", pulled.gaps);
    assert!(pulled.steps.is_empty(), "{:?}", pulled.steps);
    conn.rollback().await.unwrap();
    db.close().await;
}

#[test]
fn postgres_pull_names_a_policy_as_a_gap() {
    let Some(url) = url() else { return };
    tokio::runtime::Runtime::new().unwrap().block_on(pull_keeps_policies_as_gaps(&url));
}
