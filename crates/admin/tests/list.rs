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
  @@index([name])
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

#[tokio::test]
async fn a_malformed_query_is_401_without_a_session_and_a_json_400_with_one() {
    let conn = database().await;
    let site = Arc::new(Site::build(schema(), "").unwrap());
    let call = |user: Option<Value>, uri: &'static str| monorm_admin::call(site.clone(), conn.clone(), "GET", uri, user);
    let su = || Some(json!({"is_superuser": true}));
    for uri in ["/admin/api/resources/Author?limit=abc", "/admin/api/resources/Author?page=-1", "/admin/api/counts/Author?exact=%ff&limit=x"] {
        assert_eq!(call(None, uri).await.0, 401, "{uri}");
        let (status, body) = call(su(), uri).await;
        let body: Value = serde_json::from_str(&body).unwrap_or_else(|_| panic!("{uri}: not JSON: {body}"));
        assert_eq!((status, body["ok"].as_bool()), (400, Some(false)), "{uri}");
    }
    assert_eq!(call(su(), "/admin/api/resources/Nope?limit=abc").await.0, 404);
}

#[tokio::test]
async fn a_page_number_that_overflows_the_offset_is_a_400() {
    let conn = database().await;
    let site = comments(json!({}));
    // The first two overflow usize; 2^57 * 100 fits usize but not the i64 of an SQL offset.
    for page in ["18446744073709551615", "1152921504606846976", "144115188075855872"] {
        let (status, body, _) = get_sql(&site, conn.clone(), &format!("{COMMENTS}?limit=100&page={page}")).await;
        assert_eq!(status, 400, "{page}: {body}");
    }
}

#[tokio::test]
async fn a_cursor_page_whose_rows_are_gone_keeps_the_way_back() {
    let conn = database().await;
    let site = comments(json!({}));
    let (_, first, _) = get_sql(&site, conn.clone(), COMMENTS).await;
    let next = first["next"].as_str().unwrap().to_owned();
    conn.batch("DELETE FROM comments WHERE id > 2".into()).await.unwrap();
    let (status, after, _) = get_sql(&site, conn.clone(), &format!("{COMMENTS}?cursor={next}")).await;
    assert_eq!((status, after["rows"].as_array().unwrap().len(), after["next"].is_null()), (200, 0, true), "{after}");
    let (_, back, _) = get_sql(&site, conn.clone(), &format!("{COMMENTS}?before={}", after["prev"].as_str().unwrap())).await;
    assert_eq!(bodies(&back), ["one"], "{back}");
    let (_, before, _) = get_sql(&site, conn, &format!("{COMMENTS}?before={}", first["prev"].as_str().unwrap_or("x"))).await;
    assert!(before.get("rows").is_none() || before["rows"].as_array().is_some(), "{before}");
}

#[tokio::test]
async fn above_50000_rows_a_sort_needs_an_index_unless_the_config_names_the_columns() {
    let conn = database().await;
    conn.batch(
        "INSERT INTO comments (id, body, big, level, rank, at)
         WITH RECURSIVE n(i) AS (SELECT 10 UNION ALL SELECT i + 1 FROM n WHERE i < 50020)
         SELECT i, 'row' || i, 1, 1, 1, '2026-01-02T03:04:05Z' FROM n;"
            .into(),
    )
    .await
    .unwrap();
    let site = comments(json!({"columns": ["body", "rank"]}));
    let (status, body, sql) = get_sql(&site, conn.clone(), &format!("{COMMENTS}?sort=rank")).await;
    assert_eq!(status, 400, "{body}");
    assert!(body["error"]["message"].as_str().unwrap_or_default().contains("no index"), "{body}");
    assert!(sql.iter().any(|s| s.to_lowercase().contains("count")), "the first request measures the table: {sql:?}");
    let (status, _, sql) = get_sql(&site, conn.clone(), &format!("{COMMENTS}?sort=rank")).await;
    assert_eq!(status, 400);
    assert!(!sql.iter().any(|s| s.to_lowercase().contains("count")), "the answer is cached: {sql:?}");
    assert_eq!(get_sql(&site, conn.clone(), &format!("{COMMENTS}?sort=-id")).await.0, 200, "the key leads an index");
    let explicit = comments(json!({"columns": ["body", "rank"], "sortable": ["rank"]}));
    assert_eq!(get_sql(&explicit, conn, &format!("{COMMENTS}?sort=rank")).await.0, 200, "an explicit sortable skips the rule");
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
    assert!(build(json!({"statement_timeout_ms": 2_147_483_648u64})).contains("statement_timeout_ms"));
    assert!(Site::build(schema(), &json!({"statement_timeout_ms": 2_147_483_647u64}).to_string()).is_ok());
}

async fn get_sql(site: &Arc<Site>, conn: Arc<dyn Executor>, uri: &str) -> (u16, Value, Vec<String>) {
    let trace = Trace::default();
    let wrapped = trace::wrap(conn, Some(&trace));
    let user = json!({"is_superuser": true});
    let (status, body) = monorm_admin::call(site.clone(), wrapped, "GET", uri, Some(user)).await;
    (status, serde_json::from_str(&body).unwrap(), trace.take().into_iter().map(|e| e.sql).collect())
}

fn bodies(body: &Value) -> Vec<String> {
    body["rows"].as_array().unwrap().iter().map(|r| r[0].as_str().unwrap().to_owned()).collect()
}

const COMMENTS: &str = "/admin/api/resources/Comment";

fn comments(extra: Value) -> Arc<Site> {
    let mut list = json!({"columns": ["body"], "order": ["id"], "page_size": 2});
    for (k, v) in extra.as_object().unwrap() {
        list[k] = v.clone();
    }
    site(json!({"resources": [{"model": "Comment", "list": list}]}))
}

#[tokio::test]
async fn first_last_and_before_walk_the_keyset_both_ways() {
    let conn = database().await;
    let site = comments(json!({}));
    let (_, last, statements) = get(&site, conn.clone(), &format!("{COMMENTS}?last=1")).await;
    assert_eq!(statements, 1);
    assert_eq!(bodies(&last), ["two", "three"]);
    assert!(last["next"].is_null());
    let prev = last["prev"].as_str().unwrap();
    let (_, back, _) = get(&site, conn.clone(), &format!("{COMMENTS}?before={prev}")).await;
    assert_eq!(bodies(&back), ["one"]);
    assert!(back["prev"].is_null());
    let next = back["next"].as_str().unwrap();
    let (_, forth, _) = get(&site, conn, &format!("{COMMENTS}?cursor={next}")).await;
    assert_eq!(bodies(&forth), ["two", "three"]);
}

#[tokio::test]
async fn sort_orders_rows_and_a_cursor_belongs_to_its_sort() {
    let conn = database().await;
    let site = comments(json!({"columns": ["body", "rank"], "sortable": ["body", "rank"]}));
    let (status, body, _) = get(&site, conn.clone(), &format!("{COMMENTS}?sort=-body")).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(bodies(&body), ["two", "three"]);
    let next = body["next"].as_str().unwrap();
    let (_, rest, _) = get(&site, conn.clone(), &format!("{COMMENTS}?sort=-body&cursor={next}")).await;
    assert_eq!(bodies(&rest), ["one"]);
    assert_eq!(get(&site, conn.clone(), &format!("{COMMENTS}?sort=body&cursor={next}")).await.0, 400);
    assert_eq!(get(&site, conn.clone(), &format!("{COMMENTS}?sort=level")).await.0, 400);
    assert_eq!(get(&site, conn, &format!("{COMMENTS}?sort=nope")).await.0, 400);
}

#[tokio::test]
async fn the_page_size_stops_at_200() {
    let conn = database().await;
    let site = comments(json!({}));
    assert_eq!(get(&site, conn.clone(), &format!("{COMMENTS}?limit=200")).await.0, 200);
    assert_eq!(get(&site, conn, &format!("{COMMENTS}?limit=201")).await.0, 400);
}

#[tokio::test]
async fn the_count_is_its_own_request_and_a_capped_count_has_a_limit() {
    let conn = database().await;
    let site = comments(json!({}));
    let (_, rows, statements) = get(&site, conn.clone(), COMMENTS).await;
    assert!(rows.get("count").is_none());
    assert_eq!(statements, 1);
    let (status, body, sql) = get_sql(&site, conn.clone(), "/admin/api/counts/Comment").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!((body["count"].as_i64(), body["capped"].as_bool()), (Some(3), Some(false)));
    assert_eq!(sql.len(), 1);
    assert!(sql[0].contains("LIMIT ?"), "{}", sql[0]);
    let (_, exact, sql) = get_sql(&site, conn, "/admin/api/counts/Comment?exact=1").await;
    assert_eq!((exact["count"].as_i64(), exact["capped"].as_bool()), (Some(3), Some(false)));
    assert!(!sql[0].contains("LIMIT"), "{}", sql[0]);
}

#[tokio::test]
async fn above_the_cap_the_count_says_so_and_a_page_jump_is_refused() {
    let conn = database().await;
    conn.batch(
        "INSERT INTO comments (id, body, big, level, rank, at)
         WITH RECURSIVE n(i) AS (SELECT 10 UNION ALL SELECT i + 1 FROM n WHERE i < 10020)
         SELECT i, 'row' || i, 1, 1, 1, '2026-01-02T03:04:05Z' FROM n;"
            .into(),
    )
    .await
    .unwrap();
    let site = comments(json!({}));
    let (_, body, _) = get(&site, conn.clone(), "/admin/api/counts/Comment").await;
    assert_eq!((body["count"].as_i64(), body["capped"].as_bool()), (Some(10_000), Some(true)));
    let (_, exact, _) = get(&site, conn.clone(), "/admin/api/counts/Comment?exact=1").await;
    assert_eq!((exact["count"].as_i64(), exact["capped"].as_bool()), (Some(10_014), Some(false)), "exact on request goes past the cap");
    assert_eq!(get(&site, conn.clone(), &format!("{COMMENTS}?page=3")).await.0, 400);
    let small = comments(json!({"search": ["body"]}));
    let (status, body, _) = get(&small, conn, &format!("{COMMENTS}?q=%3Dtwo&page=1")).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(bodies(&body), ["two"]);
}

#[tokio::test]
async fn a_page_jump_uses_an_offset_below_the_cap() {
    let conn = database().await;
    let site = comments(json!({}));
    let (status, body, sql) = get_sql(&site, conn, &format!("{COMMENTS}?page=2")).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(bodies(&body), ["three"]);
    assert!(sql.iter().any(|s| s.contains("OFFSET")), "{sql:?}");
}

#[tokio::test]
async fn search_matches_per_field_with_prefixes_and_a_number_tries_the_key() {
    let conn = database().await;
    let site = comments(json!({"search": ["body"]}));
    let search = |q: &str| format!("{COMMENTS}?q={q}");
    assert_eq!(bodies(&get(&site, conn.clone(), &search("hre")).await.1), ["three"], "contains");
    assert_eq!(bodies(&get(&site, conn.clone(), &search("%5Eth")).await.1), ["three"], "prefix");
    assert!(bodies(&get(&site, conn.clone(), &search("%5Ehre")).await.1).is_empty(), "prefix is not contains");
    assert_eq!(bodies(&get(&site, conn.clone(), &search("%3Done")).await.1), ["one"], "exact");
    assert!(bodies(&get(&site, conn.clone(), &search("%25")).await.1).is_empty(), "a wildcard is text");
    assert_eq!(bodies(&get(&site, conn.clone(), &search("2")).await.1), ["two"], "a number tries the key and the text");
    let (status, _, _) = get(&site, conn, &search("%40word")).await;
    assert_eq!(status, 400, "no full-text on SQLite");
}

#[tokio::test]
async fn an_indexed_text_field_defaults_to_a_prefix_and_an_unindexed_one_warns() {
    let conn = database().await;
    let built = Site::build(schema(), &json!({"resources": [{"model": "Author", "list": {"search": ["name"]}}, {"model": "Comment", "list": {"search": ["body"]}}]}).to_string()).unwrap();
    assert_eq!(built.warnings.len(), 1, "{:?}", built.warnings);
    assert!(built.warnings[0].contains("Comment") && built.warnings[0].contains("body"));
    let site = Arc::new(built);
    let (_, body, _) = get(&site, conn.clone(), "/admin/api/resources/Author?q=A").await;
    assert_eq!(body["rows"].as_array().unwrap().len(), 1, "a prefix matches");
    let (_, body, _) = get(&site, conn, "/admin/api/resources/Author?q=d").await;
    assert_eq!(body["rows"].as_array().unwrap().len(), 0, "a prefix is not contains");
}

#[test]
fn a_search_that_no_strategy_serves_is_a_build_error() {
    let build = |list: Value| Site::build(schema(), &json!({"resources": [{"model": "Comment", "list": list}]}).to_string()).err().unwrap().to_string();
    assert!(build(json!({"search": ["at"]})).contains("no search strategy"));
    assert!(build(json!({"search": ["level"]})).contains("no search strategy"));
    assert!(build(json!({"search": ["author.name"]})).contains("related model"));
    assert!(build(json!({"search": ["body", "body"]})).contains("twice"));
    assert!(build(json!({"search": ["body"], "search_with": "f"})).contains("search_with"));
    assert!(build(json!({"sortable": ["level"]})).contains("cannot order"));
}

#[tokio::test]
async fn the_meta_names_the_sort_and_search_choices() {
    let conn = database().await;
    let site = comments(json!({"search": ["body"], "sortable": ["body"]}));
    let (_, body, _) = get(&site, conn, "/admin/api/resources").await;
    let r = &body["resources"][0];
    assert_eq!(r["max_page_size"], 200);
    assert_eq!(r["search"], json!({"fields": ["body"], "custom": false}));
    assert_eq!(r["sortable"][0]["column"], "body");
    assert_eq!(r["sortable_explicit"], true);
}
