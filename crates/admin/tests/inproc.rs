//! The in-memory router: a request gets its headers, its client address and the guard, with no socket.
use std::sync::Arc;

use monorm_admin::inproc::Routed;
use monorm_admin::{AuthConfig, BoxFuture, BridgeError, CallContext, HostBridge, Security, SecurityOptions, Site};
use monorm_core::{dsl, migrate, schema::Schema};
use monorm_engine::db;
use serde_json::{json, Value};

const APP: &str = include_str!("../../../apps/admin/schema.prisma");

struct NoHost;

impl HostBridge for NoHost {
    fn call(&self, name: &str, _payload: Value, _ctx: CallContext) -> BoxFuture<Result<Value, BridgeError>> {
        let name = name.to_string();
        Box::pin(async move { Err(BridgeError::Bridge(format!("no host function {name}"))) })
    }
}

async fn routed() -> Routed {
    let driver = db::connect("sqlite://:memory:", 1).await.unwrap();
    let source = format!("datasource db {{\n  provider = \"sqlite\"\n}}\n\nmodel Note {{\n  id Int @id\n}}\n{APP}");
    let schema: Arc<Schema> = Arc::new(dsl::check(dsl::compile(&source, None).unwrap()).unwrap().1);
    for step in migrate::plan(&schema, &Default::default()).unwrap().up {
        driver.batch(step.sql).await.unwrap();
    }
    let site = Arc::new(Site::build(schema, &json!({"resources": [{"model": "Note"}]}).to_string()).unwrap());
    let security = Security::new(b"0123456789abcdef0123456789abcdef", SecurityOptions::default()).unwrap();
    Routed::new(site, driver, AuthConfig { security, bridge: Arc::new(NoHost) }, None)
}

#[tokio::test]
async fn a_request_without_a_session_gets_401_with_the_security_headers() {
    let reply = routed().await.request("GET", "/admin/api/resources", &[], None, "10.0.0.1").await.unwrap();
    assert_eq!(reply.status, 401);
    assert!(reply.headers.iter().any(|(k, v)| k == "x-frame-options" && v == "DENY"));
}

#[tokio::test]
async fn an_ipv6_client_address_works_and_a_wrong_address_is_an_error() {
    let routed = routed().await;
    assert_eq!(routed.request("GET", "/admin/api/me", &[], None, "::1").await.unwrap().status, 401);
    assert!(routed.request("GET", "/admin/api/me", &[], None, "not an ip").await.unwrap_err().contains("not an IP address"));
}
