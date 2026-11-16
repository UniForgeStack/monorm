//! CSV export: the list state, the `queryset` filter, the cells, the keyset batches, the audit row, the idle timeout
//! and the request timeout, on SQLite in memory.
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use monorm_admin::server::api_with;
use monorm_admin::service::with_timeout;
use monorm_admin::{call_full, router, AuthConfig, BoxFuture, BridgeError, CallContext, HostBridge, Security, SecurityOptions, Site, User};
use monorm_core::{dsl, migrate, schema::Schema};
use monorm_engine::db::{self, Executor};
use serde_json::{json, Value};
use tower::ServiceExt;

const ADMIN: &str = include_str!("../../../apps/admin/schema.prisma");
const MODELS: &str = r#"
datasource db {
  provider = "sqlite"
}

model Owner {
  id    Int    @id
  name  String
  items Item[]
}

model Item {
  id       Int    @id
  owner_id Int
  title    String
  rank     Int
  big      BigInt
  owner    Owner  @relation(fields: [owner_id], references: [id])
  @@index([rank])
}
"#;

fn schema() -> Arc<Schema> {
    let source = format!("{MODELS}\n{ADMIN}");
    Arc::new(dsl::check(dsl::compile(&source, None).unwrap()).unwrap().1)
}

fn length() -> Value {
    json!({"t": "func", "name": "length", "args": [{"t": "col", "path": [], "name": "title"}]})
}

fn item_list() -> Value {
    json!({
        "columns": ["title", "rank", "big", "owner.name", "title_length"],
        "order": ["id"],
        "search": ["title"],
        "filters": ["rank"],
        "computed": [{"name": "title_length", "label": "Length", "type": "int", "nullable": false, "sortable": true, "expr": length()}]
    })
}

fn config(extra: Value) -> Value {
    let mut resource = json!({"model": "Item", "list": item_list()});
    resource.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    let owner = json!({"model": "Owner", "list": {"columns": ["name", "item_count"], "order": ["id"], "aggregates": [{"name": "item_count", "label": "Items", "relation": "items", "fn": "count"}]}});
    json!({"resources": [resource, owner]})
}

async fn database() -> Arc<dyn Executor> {
    let conn = db::connect("sqlite://:memory:", 1).await.unwrap();
    for step in migrate::plan(&schema(), &Default::default()).unwrap().up {
        conn.batch(step.sql).await.unwrap();
    }
    conn.batch("INSERT INTO \"Owner\" (id, name) VALUES (1, 'Ada'), (2, 'Bo')".into()).await.unwrap();
    conn
}

/// `count` rows owned by Ada (odd ids) and Bo (even ids), one statement.
async fn fill(conn: &Arc<dyn Executor>, from: u32, to: u32) {
    let sql = format!(
        "WITH RECURSIVE n(i) AS (SELECT {from} UNION ALL SELECT i + 1 FROM n WHERE i < {to}) \
         INSERT INTO \"Item\" (id, owner_id, title, rank, big) SELECT i, 2 - (i % 2), 'item ' || i, i % 7, i FROM n"
    );
    conn.batch(sql).await.unwrap();
}

fn site(config: Value) -> Arc<Site> {
    Arc::new(Site::build(schema(), &config.to_string()).unwrap())
}

async fn export(site: &Arc<Site>, conn: &Arc<dyn Executor>, query: &str) -> (u16, Vec<(String, String)>, String) {
    let uri = format!("/admin/api/export/Item{query}");
    let (status, headers, body) = call_full(site.clone(), conn.clone(), "GET", &uri, Some(json!({"key": 1, "is_superuser": true})), None, None).await;
    (status, headers, String::from_utf8(body).unwrap())
}

fn lines(csv: &str) -> Vec<&str> {
    csv.split("\r\n").filter(|l| !l.is_empty()).collect()
}

fn header_of<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

#[tokio::test]
async fn the_export_is_the_list_as_csv_with_a_header_and_the_computed_and_to_one_columns() {
    let conn = database().await;
    fill(&conn, 1, 4).await;
    let (status, headers, csv) = export(&site(config(json!({}))), &conn, "").await;
    assert_eq!(status, 200, "{csv}");
    assert_eq!(header_of(&headers, "content-type"), Some("text/csv; charset=utf-8"));
    assert_eq!(header_of(&headers, "content-disposition"), Some("attachment; filename=\"Item.csv\""));
    let all = lines(&csv);
    assert_eq!(all[0], "Title,Rank,Big,Owner name,Length,Id");
    assert_eq!(all.len(), 5, "{csv}");
        assert_eq!(all[1], "item 1,1,1,Ada,6,1", "to-one, computed and key cells");
}

#[tokio::test]
async fn the_aggregate_column_is_exported() {
    let conn = database().await;
    fill(&conn, 1, 5).await;
    let site = site(config(json!({})));
    let (status, _, body) = call_full(site, conn, "GET", "/admin/api/export/Owner", Some(json!({"key": 1, "is_superuser": true})), None, None).await;
    let csv = String::from_utf8(body).unwrap();
    assert_eq!(status, 200, "{csv}");
    assert_eq!(lines(&csv), ["Name,Items,Id", "Ada,3,1", "Bo,2,2"]);
}

#[tokio::test]
async fn the_export_keeps_the_search_the_filters_and_the_sort_of_the_list() {
    let conn = database().await;
    fill(&conn, 1, 20).await;
    let site = site(config(json!({})));
    let (_, _, csv) = export(&site, &conn, "?q=item%201").await;
    let titles: Vec<&str> = lines(&csv)[1..].iter().map(|l| l.split(',').next().unwrap()).collect();
    assert_eq!(titles, ["item 1", "item 10", "item 11", "item 12", "item 13", "item 14", "item 15", "item 16", "item 17", "item 18", "item 19"]);
    let (_, _, csv) = export(&site, &conn, "?f.rank=3&sort=-id").await;
    let ids: Vec<&str> = lines(&csv)[1..].iter().map(|l| l.split(',').next().unwrap()).collect();
    assert_eq!(ids, ["item 17", "item 10", "item 3"], "{csv}");
}

#[tokio::test]
async fn the_queryset_filter_limits_the_export() {
    let conn = database().await;
    fill(&conn, 1, 6).await;
    let mine = json!({"filter": {"t": "cmp", "op": "eq", "l": {"t": "col", "path": [], "name": "owner_id"}, "r": {"t": "param", "i": 0}}, "slots": ["user"]});
    let site = site(config(json!({"queryset": mine})));
    let (status, _, csv) = export(&site, &conn, "").await;
    assert_eq!(status, 200, "{csv}");
    assert_eq!(lines(&csv).len(), 1 + 3, "only the rows of owner 1: {csv}");
}

#[tokio::test]
async fn a_text_cell_that_starts_with_a_formula_character_is_escaped() {
    let conn = database().await;
    conn.batch(
        "INSERT INTO \"Item\" (id, owner_id, title, rank, big) VALUES
           (1, 1, '=1+1', -5, -5), (2, 1, '+x', 0, 0), (3, 1, '-5', 0, 0), (4, 1, '@a', 0, 0),
           (5, 1, char(9) || 'tab', 0, 0), (6, 1, char(13) || 'cr', 0, 0), (7, 1, 'say \"hi\", ok', 0, 0),
           (8, 1, 'two' || char(10) || 'lines', 0, 0), (9, 1, 'plain', 0, 0)"
            .into(),
    )
    .await
    .unwrap();
    let (_, _, csv) = export(&site(config(json!({}))), &conn, "").await;
    let cells: Vec<&str> = csv.split("\r\n").skip(1).filter(|l| !l.is_empty()).collect();
    assert!(cells[0].starts_with("'=1+1,-5,-5,"), "a number and a BigInt keep their minus sign, the text gets a quote: {}", cells[0]);
    assert!(cells[1].starts_with("'+x,"), "{}", cells[1]);
    assert!(cells[2].starts_with("'-5,"), "a text that reads as a number is still text: {}", cells[2]);
    assert!(cells[3].starts_with("'@a,"), "{}", cells[3]);
    assert!(cells[4].starts_with("'\ttab,"), "{:?}", cells[4]);
    assert!(csv.contains("\"'\rcr\""), "the escaped cell with a CR is quoted too: {csv:?}");
    assert!(csv.contains("\"say \"\"hi\"\", ok\""), "{csv}");
    assert!(csv.contains("\"two\nlines\""), "{csv}");
}

#[tokio::test]
async fn a_big_export_is_read_in_keyset_batches_with_no_row_lost_or_doubled() {
    let conn = database().await;
    fill(&conn, 1, 2_500).await;
    let site = site(config(json!({})));
    let (status, _, csv) = export(&site, &conn, "").await;
    assert_eq!(status, 200);
    let rows = &lines(&csv)[1..];
    assert_eq!(rows.len(), 2_500);
    let ids: Vec<u32> = rows.iter().map(|l| l.split(',').next().unwrap().trim_start_matches("item ").parse().unwrap()).collect();
    assert_eq!(ids, (1..=2_500).collect::<Vec<_>>(), "in the keyset order, across the batch edges");
    let (_, _, csv) = export(&site, &conn, "?sort=-id&cursor=junk&limit=3&page=2").await;
    let ids: Vec<u32> = lines(&csv)[1..].iter().map(|l| l.split(',').next().unwrap().trim_start_matches("item ").parse().unwrap()).collect();
    assert_eq!(ids, (1..=2_500).rev().collect::<Vec<_>>(), "the cursor, page and limit of the query string do not cut the export");
}

#[tokio::test]
async fn an_unknown_sort_is_a_plain_error_before_any_byte() {
    let conn = database().await;
    fill(&conn, 1, 3).await;
    let (status, _, body) = export(&site(config(json!({}))), &conn, "?sort=nope").await;
    assert_eq!(status, 400, "{body}");
    assert!(body.starts_with('{'), "a JSON error, not a cut CSV: {body}");
}

#[tokio::test]
async fn each_export_writes_one_audit_row_with_the_query() {
    let conn = database().await;
    fill(&conn, 1, 3).await;
    let site = site(config(json!({})));
    export(&site, &conn, "?q=item&sort=-id").await;
    let rows = conn.query_text("SELECT user_key || '|' || action || '|' || resource || '|' || record_key || '|' || changes FROM \"AdminLogEntry\"".into()).await.unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    let row = format!("{rows:?}");
    assert!(row.contains("export") && row.contains("Item") && row.contains("q=item&sort=-id"), "{row}");
    export(&site, &conn, "?sort=nope").await;
    let after = conn.query_text("SELECT count(*) FROM \"AdminLogEntry\"".into()).await.unwrap();
    assert!(format!("{after:?}").contains('1'), "a refused export writes no row: {after:?}");
}

fn request(uri: &str) -> Request<Body> {
    let mut request = Request::builder().method("GET").uri(uri).body(Body::empty()).unwrap();
    request.extensions_mut().insert(User(json!({"key": 1, "is_superuser": true})));
    request
}

/// Frames until the body ends or fails; `pause` runs after the first frame. Returns the text and the error, if any.
async fn read(body: Body, pause: Duration) -> (String, Option<String>) {
    let mut body = body;
    let (mut out, mut first) = (Vec::new(), true);
    loop {
        match body.frame().await {
            None => return (String::from_utf8(out).unwrap(), None),
            Some(Err(e)) => return (String::from_utf8_lossy(&out).into_owned(), Some(e.to_string())),
            Some(Ok(frame)) => {
                if let Ok(data) = frame.into_data() {
                    out.extend_from_slice(&data);
                }
                if std::mem::take(&mut first) {
                    tokio::time::sleep(pause).await;
                }
            }
        }
    }
}

#[tokio::test]
async fn the_request_timeout_bounds_the_head_and_not_the_stream() {
    let conn = database().await;
    fill(&conn, 1, 5_000).await;
    let app = with_timeout(router(site(config(json!({}))), conn.clone()), Duration::from_millis(50));
    let response = app.oneshot(request("/admin/api/export/Item")).await.unwrap();
    assert_eq!(response.status(), 200);
    let (csv, error) = read(response.into_body(), Duration::from_millis(300)).await;
    assert_eq!(error, None, "a slow but live reader is not cut by the request timeout");
    assert_eq!(lines(&csv).len(), 5_001);
}

#[tokio::test]
async fn a_reader_that_stops_hits_the_idle_timeout_and_the_body_fails() {
    let conn = database().await;
    fill(&conn, 1, 5_000).await;
    let mut with_idle = config(json!({}));
    with_idle["export_idle_timeout_ms"] = json!(100);
    let app = router(site(with_idle), conn.clone());
    let response = app.oneshot(request("/admin/api/export/Item")).await.unwrap();
    assert_eq!(response.status(), 200);
    let (csv, error) = read(response.into_body(), Duration::from_millis(600)).await;
    let error = error.expect("the cut export fails instead of ending like a whole file");
    assert!(error.contains("no data for 100 ms"), "{error}");
    assert!(lines(&csv).len() < 5_001, "the file is not whole");
}

/// The function `price` of a Decimal computed column: the text for each key comes from `texts` by the order of the keys, and a call that holds a key above `stall_after` never answers.
struct Price {
    texts: Vec<&'static str>,
    stall_after: i64,
}

impl HostBridge for Price {
    fn call(&self, _: &str, payload: Value, _: CallContext) -> BoxFuture<Result<Value, BridgeError>> {
        let keys = payload["keys"].as_array().unwrap();
        let stalled = keys.iter().any(|k| k[0].as_i64().unwrap() > self.stall_after);
        let answer: Vec<Value> = keys.iter().map(|k| json!(self.texts[(k[0].as_i64().unwrap() as usize - 1) % self.texts.len()])).collect();
        Box::pin(async move {
            if stalled {
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
            Ok(Value::Array(answer))
        })
    }
}

fn price_site(idle_ms: Option<u64>) -> Arc<Site> {
    let mut config = config(json!({}));
    if let Some(ms) = idle_ms {
        config["export_idle_timeout_ms"] = json!(ms);
    }
    config["resources"][0]["list"]["columns"] = json!(["title", "price"]);
    config["resources"][0]["list"]["computed"] = json!([{"name": "price", "label": "Price", "type": "decimal", "nullable": false, "function": "price"}]);
    site(config)
}

fn auth(host: Price) -> AuthConfig {
    AuthConfig { security: Security::new(&[7u8; 32], SecurityOptions::default()).unwrap(), bridge: Arc::new(host) }
}

#[tokio::test]
async fn a_decimal_column_from_a_host_function_cannot_carry_a_formula() {
    let conn = database().await;
    fill(&conn, 1, 6).await;
    let host = Price { texts: vec!["-5.50", "=HYPERLINK(\"http://x\")", "+1", "@SUM(A1)", "1e3", "12.5"], stall_after: i64::MAX };
    let uri = "/admin/api/export/Item";
    let (status, _, body) = call_full(price_site(None), conn, "GET", uri, Some(json!({"key": 1, "is_superuser": true})), Some(auth(host)), None).await;
    let csv = String::from_utf8(body).unwrap();
    assert_eq!(status, 200, "{csv}");
    let prices: Vec<&str> = csv.split("\r\n").skip(1).filter(|l| !l.is_empty()).map(|l| l.split_once(',').unwrap().1.rsplit_once(',').unwrap().0).collect();
    assert_eq!(prices[0], "-5.50", "a plain number keeps its minus sign");
    assert_eq!(prices[1], "\"'=HYPERLINK(\"\"http://x\"\")\"", "a formula is quoted: {csv}");
    assert_eq!(prices[2], "'+1");
    assert_eq!(prices[3], "'@SUM(A1)");
    assert_eq!(prices[4], "1e3");
    assert_eq!(prices[5], "12.5");
}

#[tokio::test]
async fn a_later_batch_that_never_answers_hits_the_idle_timeout_and_the_body_fails() {
    let conn = database().await;
    fill(&conn, 1, 2_500).await;
    let host = Price { texts: vec!["1.00"], stall_after: 1_000 };
    let app = api_with(price_site(Some(100)), conn.clone(), Some(auth(host)));
    let response = app.oneshot(request("/admin/api/export/Item")).await.unwrap();
    assert_eq!(response.status(), 200, "the first batch is read before the head");
    let started = std::time::Instant::now();
    let (csv, error) = read(response.into_body(), Duration::ZERO).await;
    let error = error.expect("the cut export fails instead of ending like a whole file");
    assert!(error.contains("more than 100 ms"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(5), "the stalled call does not hold the body");
    assert_eq!(lines(&csv).len(), 1_001, "the header and the first batch");
}
