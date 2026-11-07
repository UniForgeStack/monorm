//! The list planner on a real database: statements, keyset pages, JSON types.
use std::sync::Arc;

use monorm_admin::Site;
use monorm_core::{dsl, migrate, schema::Schema};
use monorm_engine::db::{self, Executor};
use monorm_engine::trace::{self, Trace};
use serde_json::{json, Value};

const SOURCE: &str = r#"
datasource db {
  provider = "sqlite"
}

enum Level {
  low  @value(1)
  high @value(10)

  @@storage(int)
}

model Author {
  id       Int       @id
  name     String
  comments Comment[]
  @@map("authors")
}

model Comment {
  id        Int      @id
  body      String
  author_id Int?
  big       BigInt
  level     Level
  rank      Int
  at        DateTime
  author    Author?  @relation(fields: [author_id], references: [id])
  @@map("comments")
}
"#;

fn schema() -> Arc<Schema> {
    Arc::new(dsl::check(dsl::compile(SOURCE, None).unwrap()).unwrap().1)
}

async fn database() -> Arc<dyn Executor> {
    let conn = db::connect("sqlite://:memory:", 1).await.unwrap();
    for sql in migrate::create_statements(&migrate::snapshot(&schema()).unwrap()) {
        conn.batch(sql).await.unwrap();
    }
    conn.batch(
        "INSERT INTO authors (id, name) VALUES (1, 'Ada'), (2, 'Bo');
         INSERT INTO comments (id, body, author_id, big, level, rank, at) VALUES
           (1, 'one', 1, 9007199254740993, 10, 1, '2026-01-02T03:04:05Z'),
           (2, 'two', NULL, 2, 1, 1, '2026-01-02T03:04:06Z'),
           (3, 'three', 2, 3, 10, 2, '2026-01-02T03:04:07Z');"
            .into(),
    )
    .await
    .unwrap();
    conn
}

async fn get(site: &Arc<Site>, conn: Arc<dyn Executor>, uri: &str) -> (u16, Value, usize) {
    let trace = Trace::default();
    let wrapped = trace::wrap(conn, Some(&trace));
    let user = json!({"is_superuser": true});
    let (status, body) = monorm_admin::call(site.clone(), wrapped, "GET", uri, Some(user)).await;
    (status, serde_json::from_str(&body).unwrap(), trace.take().len())
}

#[tokio::test]
async fn a_to_one_column_through_a_nullable_key_is_one_statement_and_keeps_the_row() {
    let conn = database().await;
    let config = json!({"resources": [{"model": "Comment", "list": {"columns": ["body", "author.name"], "order": ["id"]}}]});
    let site = Arc::new(Site::build(schema(), &config.to_string()).unwrap());
    let (status, body, statements) = get(&site, conn, "/admin/api/resources/Comment").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(statements, 1);
    assert_eq!(body["rows"], json!([["one", "Ada", 1], ["two", null, 2], ["three", "Bo", 3]]));
}

fn site(config: Value) -> Arc<Site> {
    Arc::new(Site::build(schema(), &config.to_string()).unwrap())
}

#[tokio::test]
async fn a_keyset_page_walk_visits_every_row_once_with_ties_and_a_desc_order() {
    let conn = database().await;
    let site = site(json!({"resources": [{"model": "Comment", "list": {"columns": ["body"], "order": ["-rank"], "page_size": 1}}]}));
    let mut seen = Vec::new();
    let mut uri = "/admin/api/resources/Comment".to_owned();
    loop {
        let (status, body, statements) = get(&site, conn.clone(), &uri).await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(statements, 1);
        let rows = body["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        seen.push(rows[0][0].as_str().unwrap().to_owned());
        match body["next"].as_str() {
            Some(next) => uri = format!("/admin/api/resources/Comment?cursor={next}"),
            None => break,
        }
    }
    // rank desc, then the primary key: "three" (2), then "one" and "two" (1) by id
    assert_eq!(seen, ["three", "one", "two"]);
}

#[tokio::test]
async fn cells_are_json_in_the_documented_types() {
    let conn = database().await;
    let site = site(json!({"resources": [{"model": "Comment", "list": {"columns": ["id", "big", "level", "at"], "order": ["id"]}}]}));
    let (status, body, _) = get(&site, conn, "/admin/api/resources/Comment?limit=1").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["rows"][0], json!([1, "9007199254740993", "high", "2026-01-02T03:04:05+00:00"]));
}

#[tokio::test]
async fn with_no_config_every_concrete_model_is_a_resource() {
    let conn = database().await;
    let site = Arc::new(Site::build(schema(), "").unwrap());
    let (status, body, _) = get(&site, conn, "/admin/api/resources").await;
    assert_eq!(status, 200, "{body}");
    let names: Vec<&str> = body["resources"].as_array().unwrap().iter().map(|r| r["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Author", "Comment"]);
    assert_eq!(body["resources"][0]["permissions"], json!(["read"]));
}

#[tokio::test]
async fn the_prefix_is_a_config_value() {
    let conn = database().await;
    let site = site(json!({"prefix": "/staff/", "resources": [{"model": "Author"}]}));
    assert_eq!(get(&site, conn.clone(), "/staff/resources/Author").await.0, 200);
    assert_eq!(get(&site, conn, "/admin/api/resources/Author").await.0, 404);
}

#[tokio::test]
async fn a_request_needs_a_superuser() {
    let conn = database().await;
    let site = Arc::new(Site::build(schema(), "").unwrap());
    let call = |user: Option<Value>, uri: &'static str| monorm_admin::call(site.clone(), conn.clone(), "GET", uri, user);
    assert_eq!(call(None, "/admin/api/resources").await.0, 401);
    assert_eq!(call(Some(json!({"is_superuser": false})), "/admin/api/resources").await.0, 403);
    assert_eq!(call(Some(json!({"id": 1})), "/admin/api/resources/Author").await.0, 403);
    assert_eq!(call(Some(json!({"is_superuser": true})), "/admin/api/resources/Nope").await.0, 404);
    assert_eq!(call(Some(json!({"is_superuser": true})), "/admin/api/resources/Author?cursor=zz").await.0, 400);
    assert_eq!(call(Some(json!({"is_superuser": true})), "/admin/api/resources/Author?limit=0").await.0, 400);
}

#[test]
fn a_wrong_config_is_one_precise_error() {
    let build = |config: Value| Site::build(schema(), &config.to_string()).err().unwrap().to_string();
    let list = |columns: Value| json!({"resources": [{"model": "Author", "list": {"columns": columns}}]});
    assert!(build(list(json!(["nope"]))).contains("Author: unknown field \"nope\""), "unknown field");
    assert!(build(list(json!(["comments.body"]))).contains("to-many relation \"comments\""), "to-many");
    assert!(build(json!({"resources": [{"model": "Nope"}]})).contains("unknown model \"Nope\""));
    assert!(build(json!({"resources": [{"model": "Author"}, {"model": "Author"}]})).contains("registered twice"));
    assert!(build(json!({"resources": [{"model": "Comment", "list": {"order": ["author_id"]}}]})).contains("nullable"));
    assert!(build(json!({"resources": [{"model": "Comment", "list": {"order": ["level"]}}]})).contains("cannot order"));
    assert!(build(json!({"resources": [{"model": "Author", "list": {"page_size": 0}}]})).contains("page_size"));
    assert!(build(json!({"resources": [{"model": "Author", "list": {"colums": []}}]})).contains("colums"));
}
