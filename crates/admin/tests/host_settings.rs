//! A host write runs in the transaction of the request, so `db_settings` reach it, on PostgreSQL.
use std::sync::{Arc, Mutex};

use monorm_admin::service::Logger;
use monorm_admin::{call_full, AuthConfig, BoxFuture, BridgeError, CallContext, HostBridge, Security, SecurityOptions, Site};
use monorm_core::{dsl, migrate};
use monorm_engine::db;
use serde_json::{json, Value};

const ADMIN: &str = include_str!("../../../apps/admin/schema.prisma");
const MODELS: &str = "datasource db {\n  provider = \"postgresql\"\n}\n\nmodel HsLocked {\n  id Int @id @default(autoincrement())\n  x  Int\n  @@map(\"hs_locked\")\n}\n";

/// The host reads the setting inside the transaction it gets, then writes the row as the default writer does.
struct Host {
    setting: Arc<Mutex<Option<String>>>,
}

impl HostBridge for Host {
    fn call(&self, name: &str, payload: Value, ctx: CallContext) -> BoxFuture<Result<Value, BridgeError>> {
        assert_eq!(name, "admin.write");
        let tx = ctx.tx.expect("the default writer holds the transaction of the request");
        let x = payload["values"]["x"].clone();
        let seen = self.setting.clone();
        Box::pin(async move {
            let rows = tx.query_text("SELECT current_setting('app.tenant', true)".into()).await.unwrap();
            *seen.lock().unwrap() = rows[0][0].clone();
            tx.batch(format!("INSERT INTO hs_locked (id, x) VALUES (1, {x})")).await.unwrap();
            Ok(json!({"ok": true, "key": {"id": 1}, "audited": false}))
        })
    }
}

#[tokio::test]
async fn a_host_write_sees_the_db_settings_of_the_request() {
    let Ok(url) = std::env::var("ORM_RUST_TEST_POSTGRES_URL") else {
        eprintln!("skipped: ORM_RUST_TEST_POSTGRES_URL is not set");
        return;
    };
    let source = format!("{MODELS}\n{ADMIN}");
    let schema = Arc::new({
        let mut ir = dsl::compile(&source, None).unwrap();
        ir.behavior.write_rules.push(serde_json::from_value(serde_json::json!({
            "source": "test", "model": "HsLocked", "name": "protect",
            "operations": ["insert", "update", "delete"], "layer": "app"
        })).unwrap());
        ir.stamp_schema_contract();
        dsl::check(ir).unwrap().1
    });
    let conn = db::connect(&url, 2).await.unwrap();
    conn.batch("DROP TABLE IF EXISTS hs_locked".into()).await.unwrap();
    for step in migrate::plan(&schema, &Default::default()).unwrap().up {
        // The admin tables may exist from another test.
        let _ = conn.batch(step.sql).await;
    }
    let site = Arc::new(Site::build(schema, &json!({"resources": [{"model": "HsLocked", "allow_protected_writes": true}]}).to_string()).unwrap());
    let host = Arc::new(Host { setting: Arc::new(Mutex::new(None)) });
    let auth = AuthConfig { security: Security::new(&[7u8; 32], SecurityOptions::default()).unwrap(), bridge: host.clone() };
    let wrapped = monorm_admin::serve::with_settings(conn.clone(), &[("app.tenant".into(), "acme".into())], &Logger::new(|_| {}));
    let body = json!({"set": {"x": 5}}).to_string().into_bytes();
    let (status, _, out) = call_full(site, wrapped, "POST", "/admin/api/resources/HsLocked", Some(json!({"key": 7, "is_superuser": true})), Some(auth), Some(("application/json", body))).await;
    let seen = host.setting.lock().unwrap().clone();
    conn.batch("DROP TABLE IF EXISTS hs_locked".into()).await.unwrap();
    assert_eq!(status, 201, "{}", String::from_utf8_lossy(&out));
    assert_eq!(seen.as_deref(), Some("acme"));
}
