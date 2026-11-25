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
    assert_eq!(key::url("/admin", "user", "7"), "/admin/user/7");
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

#[test]
fn a_theme_is_the_short_form_of_the_brand_and_a_value_in_both_is_a_config_error() {
    let r = json!([{"model": "Author"}]);
    let themed = site(json!({"theme": {"title": "Acme", "logo": "/static/acme.svg", "color": "#123456"}, "resources": r}));
    assert_eq!(themed.brand["name"], "Acme");
    assert_eq!(themed.brand["logo"], "/static/acme.svg");
    assert_eq!(themed.brand["colors"]["primary"], "#123456");
    let mixed = site(json!({"theme": {"color": "#123456"}, "brand": {"name": "Acme", "colors": {"accent": "#abcdef"}}, "resources": r}));
    assert_eq!((mixed.brand["name"].as_str(), mixed.brand["colors"]["primary"].as_str(), mixed.brand["colors"]["accent"].as_str()), (Some("Acme"), Some("#123456"), Some("#abcdef")));
    assert!(build_error(json!({"theme": {"color": "red"}, "resources": r})).contains("theme.color"));
    assert!(build_error(json!({"theme": {"logo": "http://example.com/a.png"}, "resources": r})).contains("theme.logo"));
    assert!(build_error(json!({"theme": {"title": " "}, "resources": r})).contains("theme.title"));
    assert!(build_error(json!({"theme": {"shade": "x"}, "resources": r})).contains("shade"));
    for (theme, brand, both) in [
        (json!({"title": "A"}), json!({"name": "B"}), ("theme.title", "brand.name")),
        (json!({"logo": "/a.svg"}), json!({"logo": "/b.svg"}), ("theme.logo", "brand.logo")),
        (json!({"color": "#111111"}), json!({"colors": {"primary": "#222222"}}), ("theme.color", "brand.colors.primary")),
    ] {
        let e = build_error(json!({"theme": theme, "brand": brand, "resources": r}));
        assert!(e.contains(both.0) && e.contains(both.1), "{e}");
    }
}

#[test]
fn a_switcher_merges_code_and_file_keys_and_sets_the_primary_colour() {
    let r = json!([{"model": "Author"}]);
    let services = json!([
        {"key": "admin", "title": "Admin", "icon": "shield", "color": "#336699"},
        {"key": "ops", "title": "Ops", "url": "https://ops.example.com", "icon": "server"}
    ]);
    let built = site(json!({"switcher": {"current": "admin", "services": services, "build": {"deployed_at": "2026-11-24", "tree": "abcdef0123456"}}, "resources": r}));
    let s = &built.brand["switcher"];
    assert_eq!(s["current"], "admin");
    assert_eq!(s["services"][0]["url"], Value::Null);
    assert_eq!(s["services"][1]["url"], "https://ops.example.com");
    assert_eq!(s["subheader"], "2026-11-24 · abcdef0");
    assert_eq!(s["rows"], json!(["deployed 2026-11-24", "tree abcdef0"]));
    assert_eq!(built.brand["colors"]["primary"], "#336699");
    let both = site(json!({"switcher": {"current": "admin", "services": services}, "switcher_file": {"build": {"commit": "0123456789", "built_at": "t1"}}, "resources": r}));
    assert_eq!(both.brand["switcher"]["rows"], json!(["built t1 · commit 0123456"]));
    assert_eq!(both.brand["switcher"]["subheader"], "dev");
    assert_eq!(site(json!({"resources": r})).brand["switcher"], Value::Null);
    let empty_build = site(json!({"switcher_file": {"build": {"tree": ""}}, "resources": r}));
    assert_eq!(empty_build.brand["switcher"]["subheader"], "dev");
}

#[test]
fn a_wrong_switcher_is_a_config_error_that_names_the_setting() {
    let r = json!([{"model": "Author"}]);
    let svc = |extra: Value| {
        let mut one = json!({"key": "admin", "title": "Admin", "url": "/admin"});
        one.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        one
    };
    let other = json!({"key": "ops", "title": "Ops", "url": "/ops"});
    let err = |switcher: Value| build_error(json!({"switcher": switcher, "resources": r}));
    assert!(err(json!({"current": "nope", "services": [svc(json!({}))]})).contains("switcher.current"));
    assert!(err(json!({"services": [svc(json!({}))]})).contains("switcher.current is needed"));
    assert!(err(json!({"current": "ops", "services": [svc(json!({})), other.clone(), other.clone()]})).contains("twice"));
    assert!(err(json!({"current": "ops", "services": [svc(json!({}))]})).contains("not the key"));
    assert!(err(json!({"current": "admin", "services": [svc(json!({"icon": "Shield!"}))]})).contains("icon"));
    assert!(err(json!({"current": "admin", "services": [svc(json!({"color": "red"}))]})).contains("color"));
    assert!(err(json!({"current": "admin", "services": [svc(json!({"title": ""}))]})).contains("title"));
    assert!(err(json!({"current": "admin", "services": [svc(json!({})), {"key": "ops", "title": "Ops"}]})).contains("url is needed"));
    assert!(err(json!({"current": "admin", "services": [svc(json!({})), {"key": "ops", "title": "Ops", "url": "http://ops.example.com"}]})).contains("https"));
    assert!(err(json!({"current": "admin", "services": [svc(json!({})), {"key": "Ops", "title": "Ops", "url": "/ops"}]})).contains("key"));
    assert!(err(json!({"build": {"hash": "x"}})).contains("hash"));
    let in_both = build_error(json!({"switcher": {"current": "admin"}, "switcher_file": {"current": "admin"}, "resources": r}));
    assert!(in_both.contains("switcher.current") && in_both.contains("monorm.toml"), "{in_both}");
    let build_both = build_error(json!({"switcher": {"build": {"tree": "a"}}, "switcher_file": {"build": {"tree": "b"}}, "resources": r}));
    assert!(build_both.contains("switcher.build.tree"), "{build_both}");
    let colours = build_error(json!({"brand": {"colors": {"primary": "#111111"}}, "switcher": {"current": "admin", "services": [svc(json!({"color": "#222222"}))]}, "resources": r}));
    assert!(colours.contains("primary"), "{colours}");
    let themed = build_error(json!({"theme": {"color": "#111111"}, "switcher": {"current": "admin", "services": [svc(json!({"color": "#222222"}))]}, "resources": r}));
    assert!(themed.contains("theme.color"), "{themed}");
}

#[test]
fn the_environment_level_the_resource_icon_and_the_group_icons_are_checked() {
    let r = json!([{"model": "Author", "group": "People", "icon": "users"}]);
    let level = |env: &str, level: Option<&str>| {
        let mut brand = json!({"environment": env});
        if let Some(l) = level {
            brand["environment_level"] = json!(l);
        }
        site(json!({"brand": brand, "resources": [{"model": "Author"}]})).brand["environment_level"].clone()
    };
    assert_eq!((level("Production", None), level("prod", None), level("staging", None), level("demo", None), level("demo", Some("danger"))), (json!("danger"), json!("danger"), json!("warning"), json!("info"), json!("danger")));
    assert!(build_error(json!({"brand": {"environment": "x", "environment_level": "loud"}, "resources": [{"model": "Author"}]})).contains("environment_level"));
    assert!(build_error(json!({"brand": {"environment_level": "info"}, "resources": [{"model": "Author"}]})).contains("needs brand.environment"));
    assert_eq!(site(json!({"resources": [{"model": "Author"}]})).brand["environment_level"], Value::Null);
    let built = site(json!({"groups": ["People"], "group_icons": {"People": "users"}, "resources": r}));
    assert_eq!(built.nav(&["Author"]), json!([{"group": "People", "icon": "users", "resources": ["Author"]}]));
    assert_eq!(built.resource("Author").unwrap().meta["icon"], "users");
    assert!(build_error(json!({"groups": ["People"], "group_icons": {"Sales": "users"}, "resources": r})).contains("group_icons"));
    assert!(build_error(json!({"groups": ["People"], "group_icons": {"People": "Users!"}, "resources": r})).contains("group_icons"));
    assert!(build_error(json!({"groups": ["People"], "resources": [{"model": "Author", "group": "People", "icon": "Users!"}]})).contains("icon"));
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

/// A package directory with a widget, a form module and a style, in a fresh temp directory.
fn package_dir(tag: &str) -> String {
    let dir = std::env::temp_dir().join(format!("monorm-admin-pkg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    std::fs::write(dir.join("stars.js"), "import React from \"react\";\nexport const adminMajor = 1;\nexport default () => null;\n").unwrap();
    std::fs::write(dir.join("lib/form.mjs"), "export const adminMajor = 1;\nexport default () => null;\n").unwrap();
    std::fs::write(dir.join("lib/reviews.js"), "export const adminMajor = 1;\nexport default () => null;\n").unwrap();
    std::fs::write(dir.join("stars.css"), ".stars { color: gold }\n").unwrap();
    std::fs::write(dir.join("notes.txt"), "not served").unwrap();
    dir.to_string_lossy().into_owned()
}

fn with_package(dir: &str, major: u64) -> Value {
    json!({"packages": [{"name": "rating", "admin_major": major, "dir": dir, "widgets": {"stars": "stars.js"}, "forms": {"edit": "lib/form.mjs"}, "tabs": {"reviews": "lib/reviews.js"}, "styles": ["stars.css"]}],
           "widgets": {"Comment.body": "rating.stars"}, "resources": [{"model": "Comment", "form_component": "rating.edit", "detail_tabs": ["rating.reviews"]}, {"model": "Author"}]})
}

#[tokio::test]
async fn a_package_site_lists_its_package_and_serves_only_the_loaded_files() {
    let dir = package_dir("serve");
    let site = site(with_package(&dir, 1));
    let conn = database().await;
    let (status, body, _) = get(&site, conn.clone(), "/admin/api/resources").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["admin_major"], 1);
    assert_eq!(body["packages"][0]["name"], "rating");
    assert_eq!(body["packages"][0]["widgets"], json!({"stars": "packages/rating/stars.js"}));
    assert_eq!(body["packages"][0]["styles"], json!(["packages/rating/stars.css"]));
    let comment = body["resources"].as_array().unwrap().iter().find(|r| r["name"] == "Comment").unwrap();
    assert_eq!(comment["form_module"], "packages/rating/lib/form.mjs");
    assert_eq!(comment["package_tabs"], json!([{"name": "rating.reviews", "title": "Reviews", "module": "packages/rating/lib/reviews.js"}]));
    assert_eq!(body["packages"][0]["tabs"], json!({"reviews": "packages/rating/lib/reviews.js"}));
    let user = || Some(json!({"is_superuser": true}));
    for (path, kind, text) in [("stars.js", "text/javascript", "import React"), ("lib/form.mjs", "text/javascript", "adminMajor"), ("stars.css", "text/css", ".stars")] {
        let (status, headers, bytes) = monorm_admin::server::call_full(site.clone(), conn.clone(), "GET", &format!("/admin/api/packages/rating/{path}"), user(), None, None).await;
        assert_eq!(status, 200, "{path}");
        assert!(headers.iter().any(|(k, v)| k == "content-type" && v.starts_with(kind)), "{path}: {headers:?}");
        assert!(String::from_utf8_lossy(&bytes).contains(text), "{path}");
    }
    // A file that the build did not load is not served, and a path cannot leave the package.
    for path in ["notes.txt", "../notes.txt", "nothing.js", "%2e%2e/notes.txt", "lib"] {
        let (status, _, _) = monorm_admin::server::call_full(site.clone(), conn.clone(), "GET", &format!("/admin/api/packages/rating/{path}"), user(), None, None).await;
        assert_eq!(status, 404, "{path}");
    }
    let (status, _, _) = monorm_admin::server::call_full(site.clone(), conn.clone(), "GET", "/admin/api/packages/other/stars.js", user(), None, None).await;
    assert_eq!(status, 404);
    // No user, no file.
    let (status, _, _) = monorm_admin::server::call_full(site, conn, "GET", "/admin/api/packages/rating/stars.js", None, None, None).await;
    assert_eq!(status, 401);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_package_for_another_major_or_with_a_bad_name_or_file_is_a_build_error() {
    let dir = package_dir("errors");
    assert!(build_error(with_package(&dir, 2)).contains("built for admin major 2; this admin is major 1"));
    let mut config = with_package(&dir, 1);
    config["packages"][0]["widgets"] = json!({"stars": "missing.js"});
    assert!(build_error(config).contains("missing.js"));
    let mut config = with_package(&dir, 1);
    config["packages"][0]["widgets"] = json!({"stars": "../stars.js"});
    assert!(build_error(config).contains("not a .js, .mjs or .css file"));
    let mut config = with_package(&dir, 1);
    config["packages"][0]["styles"] = json!(["stars.js"]);
    assert!(build_error(config).contains("not a .css file"));
    let mut config = with_package(&dir, 1);
    let first = config["packages"][0].clone();
    config["packages"].as_array_mut().unwrap().push(first);
    assert!(build_error(config).contains("registered twice"));
    let mut config = with_package(&dir, 1);
    config["widgets"] = json!({"Comment.body": "rating.nothing"});
    assert!(build_error(config).contains("rating.nothing"));
    let mut config = with_package(&dir, 1);
    config["resources"][0]["detail_tabs"] = json!(["rating.nothing"]);
    assert!(build_error(config).contains("detail_tabs names \"rating.nothing\""));
    let mut config = with_package(&dir, 1);
    config["packages"][0].as_object_mut().unwrap().remove("admin_major");
    assert!(build_error(config).contains("admin_major"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_package_over_the_file_limits_is_a_build_error_and_the_limit_itself_passes() {
    let dir = package_dir("limits");
    // stars.js, lib/form.mjs, lib/reviews.js and stars.css are 4 files; 252 more make 256, the limit.
    for i in 0..252 {
        std::fs::write(std::path::Path::new(&dir).join(format!("extra{i}.js")), "export {};\n").unwrap();
    }
    Site::build(schema(), &with_package(&dir, 1).to_string()).expect("256 files pass");
    std::fs::write(std::path::Path::new(&dir).join("extra252.js"), "export {};\n").unwrap();
    assert!(build_error(with_package(&dir, 1)).contains("too large: at most 256 files"));
    std::fs::remove_dir_all(&dir).unwrap();
    // A sparse file reports its size without bytes on disk; the build refuses it before the read.
    let dir = package_dir("big");
    let big = std::fs::File::create(std::path::Path::new(&dir).join("big.js")).unwrap();
    big.set_len(1024 * 1024 + 1).unwrap();
    assert!(build_error(with_package(&dir, 1)).contains("1048576 bytes each"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn the_import_map_and_its_csp_hash_appear_only_on_a_site_with_packages() {
    let dir = package_dir("index");
    let conn = database().await;
    let with = served(site(with_package(&dir, 1)), conn.clone());
    let (status, headers, body) = fetch(&with, "GET", "/admin").await;
    assert_eq!(status, 200);
    let map = body.split("<script type=\"importmap\">").nth(1).and_then(|s| s.split("</script>").next()).expect("an import map");
    assert!(map.contains("\"react\"") && map.contains("/admin/shared/react.js"), "{map}");
    assert!(map.contains("\"monorm-admin/ui\"") && map.contains("/admin/shared/ui.js"), "{map}");
    assert!(body.find("importmap").unwrap() < body.find("type=\"module\"").unwrap(), "the map comes before the module script");
    let csp = headers["content-security-policy"].to_str().unwrap();
    let hash = {
        use base64::Engine;
        use sha2::Digest;
        base64::engine::general_purpose::STANDARD.encode(sha2::Sha256::digest(map.as_bytes()))
    };
    assert!(csp.contains(&format!("script-src 'self' 'sha256-{hash}'")), "{csp}");
    assert!(csp.contains("style-src 'self'") && !csp.contains("unsafe-inline"), "{csp}");
    let (status, headers, js) = fetch(&with, "GET", "/admin/shared/react.js").await;
    assert_eq!(status, 200);
    assert_eq!(headers["cache-control"], "no-cache");
    assert!(js.contains("__monormAdmin"));
    let without = served(everything(), conn);
    let (_, headers, body) = fetch(&without, "GET", "/admin").await;
    assert!(!body.contains("importmap"));
    assert!(!headers["content-security-policy"].to_str().unwrap().contains("sha256-"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn a_site_at_its_own_base_path_serves_the_page_assets_and_import_map_there() {
    let dir = package_dir("base");
    let mut config = with_package(&dir, 1);
    config["base_path"] = json!("/bundler/admin/");
    let site = site(config);
    assert_eq!((site.base_path.as_str(), site.prefix.as_str()), ("/bundler/admin", "/bundler/admin/api"));
    assert_eq!(site.url("Author", &json!(7)).unwrap(), "/bundler/admin/Author/7");
    let router = served(site, database().await);
    for path in ["/bundler/admin", "/bundler/admin/", "/bundler/admin/Comment/1", "/bundler/admin/-/login"] {
        let (status, headers, body) = fetch(&router, "GET", path).await;
        assert_eq!(status, 200, "{path}");
        assert!(body.contains("content=\"/bundler/admin\""), "the base in the page: {body}");
        assert!(body.contains("content=\"/bundler/admin/api\""), "{body}");
        assert!(body.contains("src=\"/bundler/admin/assets/") && !body.contains("=\"./") && !body.contains("=\"/admin"), "absolute assets, so a deep link loads them: {body}");
        let map = body.split("<script type=\"importmap\">").nth(1).and_then(|s| s.split("</script>").next()).expect("an import map");
        assert!(map.contains("/bundler/admin/shared/react.js") && map.contains("/bundler/admin/shared/ui.js") && !map.contains("\"/admin/"), "{map}");
        let hash = {
            use base64::Engine;
            use sha2::Digest;
            base64::engine::general_purpose::STANDARD.encode(sha2::Sha256::digest(map.as_bytes()))
        };
        assert!(headers["content-security-policy"].to_str().unwrap().contains(&format!("'sha256-{hash}'")), "the hash follows the map of this base");
    }
    let (_, _, body) = fetch(&router, "GET", "/bundler/admin").await;
    let script = body.split("src=\"").nth(1).and_then(|s| s.split('"').next()).unwrap();
    assert_eq!(fetch(&router, "GET", script).await.0, 200, "{script}");
    assert_eq!(fetch(&router, "GET", "/bundler/admin/shared/react.js").await.0, 200);
    for outside in ["/admin", "/admin/Comment", "/admin/shared/react.js", "/bundler/adminx", "/bundler/api/resources", "/bundler"] {
        assert_eq!(fetch(&router, "GET", outside).await.0, 404, "{outside}");
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_bad_base_path_or_a_prefix_outside_it_is_a_build_error() {
    let r = json!([{"model": "Author"}]);
    let with = |extra: Value| {
        let mut config = json!({"resources": r});
        config.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        build_error(config)
    };
    assert!(with(json!({"base_path": ""})).contains("base_path"));
    assert!(with(json!({"base_path": "/"})).contains("base_path"));
    assert!(with(json!({"base_path": "admin"})).contains("must start with /"));
    assert!(with(json!({"base_path": "/a b"})).contains("use letters, digits"));
    assert!(with(json!({"base_path": "/a\"b"})).contains("use letters, digits"));
    assert!(with(json!({"base_path": "/a//b"})).contains("empty or dot segment"));
    assert!(with(json!({"base_path": "/a/../b"})).contains("empty or dot segment"));
    assert!(with(json!({"base_path": "/bundler/admin", "prefix": "/bundler/admin"})).contains("path of the UI"));
    assert!(with(json!({"base_path": "/bundler/admin", "prefix": "/api"})).contains("must sit under the base path"));
    assert!(with(json!({"base_path": "/bundler/admin", "prefix": "/bundler/administrator"})).contains("must sit under the base path"));
    assert!(with(json!({"base_path": "/bundler/admin", "prefix": "/bundler/admin/Author"})).contains("API prefix"));
    assert!(with(json!({"base_path": "/bundler/admin", "prefix": "/bundler/admin/Author/v1"})).contains("API prefix"));
    for taken in ["assets", "shared", "index.html", "-", "-/login"] {
        let error = with(json!({"base_path": "/bundler/admin", "prefix": format!("/bundler/admin/{taken}")}));
        assert!(error.contains("for its pages and files"), "{taken}: {error}");
    }
}

#[test]
fn a_base_path_is_normalized_and_a_deeper_prefix_is_kept() {
    for (given, base) in [("/bundler/admin/", "/bundler/admin"), ("/a-b_c.d~e", "/a-b_c.d~e"), ("/x", "/x")] {
        assert_eq!(site(json!({"base_path": given, "resources": [{"model": "Author"}]})).base_path, base);
    }
    let kept = site(json!({"base_path": "/bundler/admin", "prefix": "/bundler/admin/v1/", "resources": [{"model": "Author"}]}));
    assert_eq!(kept.prefix, "/bundler/admin/v1");
}
