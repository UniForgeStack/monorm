//! Detail pages, record keys, the UI metadata and the routes of the page.
use std::sync::Arc;

use axum::http::Request;
use monorm_admin::service::{service, Logger, Options};
use monorm_admin::{key, Site};
use monorm_core::{dsl, migrate, schema::Schema};
use monorm_engine::db::{self, Executor};
use monorm_engine::trace::{self, Trace};
use serde_json::{json, Value};
use tower::ServiceExt;

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
  at        DateTime
  author    Author?  @relation(fields: [author_id], references: [id])
  @@map("comments")
}

model OrderLine {
  order_id Int
  sku      String
  qty      Int
  @@id([order_id, sku])
  @@map("order_lines")
}

model Stamp {
  at DateTime @id
  @@map("stamps")
}

model Dotted {
  name String @id
  @@map("dotted")
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
         INSERT INTO comments (id, body, author_id, big, level, at) VALUES
           (1, 'one', 1, 9007199254740993, 10, '2026-01-02T03:04:05Z'),
           (2, 'two', NULL, 2, 1, '2026-01-02T03:04:06Z');
         INSERT INTO order_lines (order_id, sku, qty) VALUES (1, 'a,b/c d', 3), (1, 'plain', 4);"
            .into(),
    )
    .await
    .unwrap();
    conn
}

fn site(config: Value) -> Arc<Site> {
    Arc::new(Site::build(schema(), &config.to_string()).unwrap())
}

fn build_error(config: Value) -> String {
    Site::build(schema(), &config.to_string()).err().expect("the config is wrong").to_string()
}

async fn get(site: &Arc<Site>, conn: Arc<dyn Executor>, uri: &str) -> (u16, Value, usize) {
    let trace = Trace::default();
    let wrapped = trace::wrap(conn, Some(&trace));
    let (status, body) = monorm_admin::call(site.clone(), wrapped, "GET", uri, Some(json!({"is_superuser": true}))).await;
    (status, serde_json::from_str(&body).unwrap(), trace.take().len())
}

fn everything() -> Arc<Site> {
    site(json!({"resources": [{"model": "Author"}, {"model": "Comment", "list": {"columns": ["body", "author.name"]}}, {"model": "OrderLine"}]}))
}

#[tokio::test]
async fn a_detail_is_one_statement_with_every_field_and_a_link_to_the_to_one_target() {
    let conn = database().await;
    let (status, body, statements) = get(&everything(), conn, "/admin/api/resources/Comment/1").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(statements, 1);
    let names: Vec<&str> = body["fields"].as_array().unwrap().iter().map(|f| f["label"].as_str().unwrap()).collect();
    assert_eq!(names, ["id", "body", "author_id", "big", "level", "at"]);
    assert_eq!(body["values"], json!([1, "one", 1, "9007199254740993", "high", "2026-01-02T03:04:05+00:00"]));
    assert_eq!(body["links"], json!([{"name": "author", "title": "Author", "resource": "Author", "key": "1"}]));
}

#[tokio::test]
async fn a_null_foreign_key_has_no_link_and_a_missing_record_is_404() {
    let conn = database().await;
    let (status, body, _) = get(&everything(), conn.clone(), "/admin/api/resources/Comment/2").await;
    assert_eq!((status, &body["links"]), (200, &json!([])));
    let (status, body, _) = get(&everything(), conn, "/admin/api/resources/Comment/99").await;
    assert_eq!((status, body["error"]["kind"].as_str()), (404, Some("NotFound")));
}

#[tokio::test]
async fn a_bad_key_is_400_and_an_unknown_resource_is_404() {
    let conn = database().await;
    let site = everything();
    for uri in ["/admin/api/resources/Comment/abc", "/admin/api/resources/Comment/1,2", "/admin/api/resources/OrderLine/1"] {
        let (status, body, _) = get(&site, conn.clone(), uri).await;
        assert_eq!((status, body["error"]["kind"].as_str()), (400, Some("BadRequest")), "{uri}");
    }
    let (status, _, _) = get(&site, conn, "/admin/api/resources/Nope/1").await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn a_composite_key_names_a_row_from_all_its_fields_and_survives_awkward_text() {
    let conn = database().await;
    let site = everything();
    let (_, list, _) = get(&site, conn.clone(), "/admin/api/resources/OrderLine").await;
    let keys: Vec<&str> = list["keys"].as_array().unwrap().iter().map(|k| k.as_str().unwrap()).collect();
    assert_eq!(keys, ["1,a%2Cb%2Fc%20d", "1,plain"]);
    let (status, body, _) = get(&site, conn, &format!("/admin/api/resources/OrderLine/{}", keys[0])).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["values"], json!([1, "a,b/c d", 3]));
}

#[tokio::test]
async fn the_list_names_each_row_by_its_key() {
    let conn = database().await;
    let (_, list, _) = get(&everything(), conn, "/admin/api/resources/Comment").await;
    assert_eq!(list["keys"], json!(["1", "2"]));
}

#[test]
fn key_encode_and_decode_round_trip() {
    use monorm_core::ir::ColType;
    let types = [ColType::Int, ColType::String];
    let raw = key::encode(&[json!(7), json!("a,b%c/ü")]);
    assert_eq!(raw, "7,a%2Cb%25c%2F%C3%BC");
    assert_eq!(key::decode(&types, &raw).unwrap(), [json!(7), json!("a,b%c/ü")]);
    assert!(key::decode(&types, "7").is_err());
    assert!(key::decode(&types, "x,y").is_err());
    assert_eq!(key::url("user", "7"), "/admin/user/7");
}

#[tokio::test]
async fn metadata_has_titles_widgets_choices_groups_and_the_brand() {
    let conn = database().await;
    let site = site(json!({
        "brand": {"name": "Acme", "colors": {"primary": "#123456"}, "links": [{"label": "Docs", "url": "https://example.com"}], "environment": "staging"},
        "groups": ["People", "Sales"],
        "resources": [
            {"model": "Author", "group": "People"},
            {"model": "Comment", "title": "Remarks", "group": "People"},
            {"model": "OrderLine", "group": "Sales"}
        ]
    }));
    let (status, body, _) = get(&site, conn, "/admin/api/resources").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["brand"]["name"], "Acme");
    assert_eq!(body["brand"]["colors"]["primary"], "#123456");
    assert_eq!(body["brand"]["environment"], "staging");
    assert_eq!(body["nav"], json!([{"group": "People", "resources": ["Author", "Comment"]}, {"group": "Sales", "resources": ["OrderLine"]}]));
    let comment = &body["resources"][1];
    assert_eq!(comment["title"], "Remarks");
    let fields = comment["fields"].as_array().unwrap();
    let level = fields.iter().find(|f| f["label"] == "level").unwrap();
    assert_eq!((level["widget"].as_str(), &level["choices"]), (Some("select"), &json!(["low", "high"])));
    assert_eq!(fields.iter().find(|f| f["label"] == "at").unwrap()["widget"], "datetime");
    assert_eq!(fields.iter().find(|f| f["label"] == "big").unwrap()["widget"], "number");
    assert_eq!(body["resources"][2]["title"], "Order line");
    assert_eq!(body["resources"][2]["key"], json!(["order_id", "sku"]));
}

#[tokio::test]
async fn without_a_brand_the_name_is_admin_and_the_nav_has_one_ungrouped_block() {
    let conn = database().await;
    let (_, body, _) = get(&everything(), conn, "/admin/api/resources").await;
    assert_eq!(body["brand"]["name"], "Admin");
    assert_eq!(body["nav"], json!([{"group": null, "resources": ["Author", "Comment", "OrderLine"]}]));
}

#[tokio::test]
async fn metadata_is_built_once_per_process_not_per_request() {
    let conn = database().await;
    let site = everything();
    let built = site.meta_builds();
    assert_eq!(built, 3);
    for _ in 0..3 {
        get(&site, conn.clone(), "/admin/api/resources").await;
        get(&site, conn.clone(), "/admin/api/resources/Comment/1").await;
    }
    assert_eq!(site.meta_builds(), built);
}

#[test]
fn a_wrong_brand_or_group_is_a_config_error_that_names_the_setting() {
    let r = json!([{"model": "Author"}]);
    assert!(build_error(json!({"brand": {"colors": {"primary": "red"}}, "resources": r})).contains("brand.colors.primary"));
    assert!(build_error(json!({"brand": {"colors": {"danger": "#fff"}}, "resources": r})).contains("danger"));
    assert!(build_error(json!({"brand": {"logo": "javascript:alert(1)"}, "resources": r})).contains("brand.logo"));
    assert!(build_error(json!({"brand": {"logo": "http://example.com/a.png"}, "resources": r})).contains("brand.logo"));
    assert!(site(json!({"brand": {"logo": "https://example.com/a.png"}, "resources": r})).brand["logo"].is_string());
    assert!(build_error(json!({"brand": {"links": [{"label": "x", "url": "//evil"}]}, "resources": r})).contains("brand.links"));
    assert!(build_error(json!({"groups": ["A"], "resources": [{"model": "Author", "group": "B"}]})).contains("group \"B\""));
    assert!(build_error(json!({"resources": [{"model": "Author", "title": ""}]})).contains("title"));
}

fn served(site: Arc<Site>, conn: Arc<dyn Executor>) -> axum::Router {
    let mut options = Options::new(None);
    options.log = Logger::new(|_| {});
    service(site, conn, options)
}

async fn fetch(router: &axum::Router, method: &str, uri: &str) -> (u16, axum::http::HeaderMap, String) {
    let req = Request::builder().method(method).uri(uri).body(axum::body::Body::empty()).unwrap();
    let response = router.clone().oneshot(req).await.unwrap();
    let (parts, body) = response.into_parts();
    let bytes = http_body_util::BodyExt::collect(body).await.unwrap().to_bytes();
    (parts.status.as_u16(), parts.headers, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn the_page_is_served_at_admin_with_its_own_headers_and_the_api_stays_json() {
    let router = served(everything(), database().await);
    for path in ["/admin", "/admin/", "/admin/Comment", "/admin/Comment/1", "/admin/Comment/a.b%2Cc", "/admin/-/login"] {
        let (status, headers, body) = fetch(&router, "GET", path).await;
        assert_eq!(status, 200, "{path}");
        assert!(body.contains("<div id=\"root\">") || body.contains("id=\"root\""), "{path}");
        assert_eq!(headers["cache-control"], "no-store", "{path}");
        assert!(headers["content-security-policy"].to_str().unwrap().contains("script-src 'self'"), "{path}");
        assert_eq!(headers["x-content-type-options"], "nosniff");
    }
    let (status, headers, body) = fetch(&router, "GET", "/admin/api/nothing").await;
    assert_eq!(status, 404);
    assert!(body.contains("\"NotFound\""));
    assert!(headers["content-security-policy"].to_str().unwrap().starts_with("default-src 'none'"));
    let (status, _, _) = fetch(&router, "POST", "/admin/Comment").await;
    assert_eq!(status, 404);
    let (status, _, _) = fetch(&router, "GET", "/elsewhere").await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn the_index_names_the_api_prefix_escaped_and_assets_are_immutable() {
    let site = site(json!({"prefix": "/admin/a\"pi", "resources": [{"model": "Author"}]}));
    let router = served(site, database().await);
    let (_, _, body) = fetch(&router, "GET", "/admin").await;
    assert!(body.contains("content=\"/admin/a&quot;pi\""), "{body}");
    assert!(!body.contains("__MONORM_API__"));
    let page = body.split("src=\"").nth(1).and_then(|s| s.split('"').next()).expect("a script tag");
    let (status, headers, js) = fetch(&router, "GET", page).await;
    assert_eq!(status, 200);
    assert_eq!(headers["cache-control"], "public, max-age=31536000, immutable");
    assert!(headers["content-type"].to_str().unwrap().starts_with("text/javascript"));
    assert!(!js.is_empty());
    let (status, _, _) = fetch(&router, "GET", "/admin/assets/missing.js").await;
    assert_eq!(status, 404);
    let (status, _, _) = fetch(&router, "GET", "/admin/missing.png").await;
    assert_eq!(status, 404);
}

#[test]
fn the_api_prefix_cannot_be_the_path_of_the_ui() {
    assert!(build_error(json!({"prefix": "/admin", "resources": [{"model": "Author"}]})).contains("path of the UI"));
    assert!(build_error(json!({"prefix": "/admin/", "resources": [{"model": "Author"}]})).contains("path of the UI"));
}

#[test]
fn a_key_that_the_detail_route_refuses_gets_no_url() {
    // A Decimal key needs PostgreSQL: the site builds from the schema alone.
    let prices = r#"datasource db {
  provider = "postgresql"
}

model Price {
  amount Decimal @id
  @@map("prices")
}
"#;
    let prices = Site::build(Arc::new(dsl::check(dsl::compile(prices, None).unwrap()).unwrap().1), r#"{"resources": [{"model": "Price"}]}"#).unwrap();
    assert!(prices.url("Price", &json!("1.5")).unwrap_err().contains("page URL can name"));
    let site = site(json!({"resources": [{"model": "Stamp"}, {"model": "Dotted"}]}));
    assert_eq!(site.url("Stamp", &json!("2026-01-02T03:04:05+00:00")).unwrap(), "/admin/Stamp/2026-01-02T03%3A04%3A05%2B00%3A00");
    assert!(site.url("Stamp", &json!("2026-01-02 03:04:05")).unwrap_err().contains("page URL can name"));
    assert!(site.url("Dotted", &json!("..")).unwrap_err().contains("no page URL"));
    assert!(site.url("Dotted", &json!(".")).unwrap_err().contains("no page URL"));
    assert_eq!(site.url("Dotted", &json!("a.b")).unwrap(), "/admin/Dotted/a.b");
}

#[test]
fn a_resource_cannot_take_the_name_of_the_api_prefix() {
    assert!(build_error(json!({"prefix": "/admin/Author", "resources": [{"model": "Author"}]})).contains("API prefix"));
}

#[test]
fn site_url_takes_a_scalar_a_list_or_an_object_and_rejects_a_wrong_key() {
    let site = everything();
    assert_eq!(site.url("Author", &json!(7)).unwrap(), "/admin/Author/7");
    assert_eq!(site.url("Author", &json!([7])).unwrap(), "/admin/Author/7");
    assert_eq!(site.url("Author", &json!({"id": 7})).unwrap(), "/admin/Author/7");
    assert_eq!(site.url("OrderLine", &json!([1, "a,b/c d"])).unwrap(), "/admin/OrderLine/1,a%2Cb%2Fc%20d");
    assert_eq!(site.url("OrderLine", &json!({"sku": "x", "order_id": 2})).unwrap(), "/admin/OrderLine/2,x");
    assert!(site.url("OrderLine", &json!(1)).unwrap_err().contains("takes order_id, sku"));
    assert!(site.url("Author", &json!("seven")).is_err());
    assert!(site.url("Author", &json!({"id": 1, "x": 2})).is_err());
    assert!(site.url("Author", &json!(null)).is_err());
    assert!(site.url("Nope", &json!(1)).unwrap_err().contains("not an admin resource"));
}
