//! A big export streams: the peak of Rust memory over the baseline does not grow with the number of rows.
//! One test in its own file, because the allocator counts the whole process.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use monorm_admin::{router, Site, User};
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
  id    Int    @id
  title String
  note  String
  @@index([title])
}
"#;

fn schema() -> Arc<Schema> {
    let source = format!("{SOURCE}\n{}", include_str!("../../../apps/admin/schema.prisma"));
    Arc::new(dsl::check(dsl::compile(&source, None).unwrap()).unwrap().1)
}

async fn insert(conn: &Arc<dyn Executor>, from: u32, to: u32) {
    let sql = format!(
        "WITH RECURSIVE n(i) AS (SELECT {from} UNION ALL SELECT i + 1 FROM n WHERE i < {to}) \
         INSERT INTO \"Item\" (id, title, note) SELECT i, 'item ' || i, 'a note of some length for the row ' || i FROM n"
    );
    conn.batch(sql).await.unwrap();
}

/// Reads the whole export one frame at a time and drops each frame. Returns the bytes read and the peak over the baseline.
async fn export(app: axum::Router) -> (usize, usize) {
    let mut request = Request::builder().method("GET").uri("/admin/api/export/Item").body(Body::empty()).unwrap();
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
async fn the_peak_memory_does_not_grow_with_the_rows() {
    let conn: Arc<dyn Executor> = db::connect("sqlite://:memory:", 1).await.unwrap();
    for step in migrate::plan(&schema(), &Default::default()).unwrap().up {
        conn.batch(step.sql).await.unwrap();
    }
    let config = json!({"resources": [{"model": "Item", "list": {"columns": ["title", "note"], "order": ["id"]}}]});
    let site = Arc::new(Site::build(schema(), &config.to_string()).unwrap());
    let app = router(site, conn.clone());

    insert(&conn, 1, 10_000).await;
    let (small_bytes, small_peak) = export(app.clone()).await;
    insert(&conn, 10_001, 100_000).await;
    let (big_bytes, big_peak) = export(app).await;

    eprintln!("export memory: 10k rows {small_bytes} bytes, peak {small_peak}; 100k rows {big_bytes} bytes, peak {big_peak}");
    assert!(big_bytes > small_bytes * 9, "the big export holds ten times the rows");
    assert!(big_bytes > 5_000_000, "the big export is a few megabytes at least");
    // Ten times the rows: a buffered export would grow about ten times. A flat peak stays within a small factor.
    assert!(big_peak < small_peak * 2, "peak grew from {small_peak} to {big_peak}");
    assert!(big_peak < big_bytes / 5, "the peak {big_peak} is a small part of the {big_bytes} bytes sent");
}
