//! A route stream reaches the client chunk by chunk: the peak of Rust memory over the baseline does not grow with the size of the stream.
//! One test in its own file, because the allocator counts the whole process.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::sync::Arc;

use axum::body::Body;
use axum::http::Request;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use http_body_util::BodyExt;
use monorm_admin::server::api_with;
use monorm_admin::{AuthConfig, BoxFuture, BridgeError, CallContext, HostBridge, Security, SecurityOptions, Site, User};
use monorm_core::{dsl, migrate, schema::Schema};
use monorm_engine::db::{self, Executor};
use serde_json::json;
use tower::ServiceExt;

static NOW: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = System.alloc(layout);
        if !p.is_null() {
            let now = NOW.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(now, Ordering::Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        System.dealloc(p, layout);
        NOW.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, p: *mut u8, layout: Layout, new: usize) -> *mut u8 {
        let q = System.realloc(p, layout, new);
        if !q.is_null() {
            if new >= layout.size() {
                let now = NOW.fetch_add(new - layout.size(), Ordering::Relaxed) + (new - layout.size());
                PEAK.fetch_max(now, Ordering::Relaxed);
            } else {
                NOW.fetch_sub(layout.size() - new, Ordering::Relaxed);
            }
        }
        q
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

const SOURCE: &str = r#"
datasource db {
  provider = "sqlite"
}

model Item {
  id Int @id
}
"#;

const CHUNK: usize = 16 * 1024;

fn schema() -> Arc<Schema> {
    let source = format!("{SOURCE}\n{}", include_str!("../../../apps/admin/schema.prisma"));
    Arc::new(dsl::check(dsl::compile(&source, None).unwrap()).unwrap().1)
}

/// A host that makes each chunk when asked, so that it holds no stream itself.
struct Host {
    left: Mutex<usize>,
}

impl HostBridge for Host {
    fn call(&self, name: &str, payload: serde_json::Value, _ctx: CallContext) -> BoxFuture<Result<serde_json::Value, BridgeError>> {
        let out = match name {
            "route.GET.big" => {
                *self.left.lock().unwrap() = payload["query"]["chunks"].as_str().unwrap().parse().unwrap();
                json!({"stream": "s", "content_type": "application/octet-stream"})
            }
            "route.__next" => {
                let mut left = self.left.lock().unwrap();
                if *left == 0 {
                    json!({"chunk": null})
                } else {
                    *left -= 1;
                    json!({"chunk": B64.encode(vec![b'x'; CHUNK])})
                }
            }
            _ => serde_json::Value::Null,
        };
        Box::pin(async move { Ok(out) })
    }
}

/// Reads the whole stream one frame at a time and drops each frame. Returns the bytes read and the peak over the baseline.
async fn stream(app: axum::Router, chunks: usize) -> (usize, usize) {
    let mut request = Request::builder().method("GET").uri(format!("/admin/api/routes/big?chunks={chunks}")).body(Body::empty()).unwrap();
    request.extensions_mut().insert(User(json!({"key": 1, "is_superuser": true})));
    let baseline = NOW.load(Ordering::Relaxed);
    PEAK.store(baseline, Ordering::Relaxed);
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), 200);
    let (mut body, mut bytes) = (response.into_body(), 0);
    while let Some(frame) = body.frame().await {
        bytes += frame.unwrap().into_data().map(|d| d.len()).unwrap_or(0);
    }
    (bytes, PEAK.load(Ordering::Relaxed).saturating_sub(baseline))
}

#[tokio::test]
async fn the_peak_memory_does_not_grow_with_the_stream() {
    let conn: Arc<dyn Executor> = db::connect("sqlite://:memory:", 1).await.unwrap();
    for step in migrate::plan(&schema(), &Default::default()).unwrap().up {
        conn.batch(step.sql).await.unwrap();
    }
    let config = json!({"resources": [{"model": "Item"}], "routes": [{"method": "GET", "path": "big"}]});
    let site = Arc::new(Site::build(schema(), &config.to_string()).unwrap());
    let auth = AuthConfig { security: Security::new(&[7u8; 32], SecurityOptions::default()).unwrap(), bridge: Arc::new(Host { left: Mutex::new(0) }) };
    let app = api_with(site, conn, Some(auth));

    let (small_bytes, small_peak) = stream(app.clone(), 100).await;
    let (big_bytes, big_peak) = stream(app, 1000).await;

    eprintln!("route stream memory: {small_bytes} bytes, peak {small_peak}; {big_bytes} bytes, peak {big_peak}");
    assert_eq!(small_bytes, 100 * CHUNK);
    assert_eq!(big_bytes, 1000 * CHUNK);
    // Ten times the bytes: a buffered stream would grow about ten times. A flat peak stays within a small factor.
    assert!(big_peak < small_peak * 2, "peak grew from {small_peak} to {big_peak}");
    assert!(big_peak < big_bytes / 5, "the peak {big_peak} is a small part of the {big_bytes} bytes sent");
}
