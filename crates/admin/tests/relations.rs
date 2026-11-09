//! Relation fields and files: the keyset autocomplete, `choices`, many-to-many chips in one save, upload and download, on SQLite in memory.
use std::sync::{Arc, Mutex};

use monorm_admin::{call_body, call_full, AuthConfig, BoxFuture, BridgeError, CallContext, HostBridge, Security, SecurityOptions, Site};
use monorm_core::{dsl, migrate, schema::Schema};
use monorm_engine::db::{self, Executor};
use serde_json::{json, Value};

const ADMIN: &str = include_str!("../../../apps/admin/schema.prisma");
const MODELS: &str = r#"
datasource db {
  provider = "sqlite"
}

model Author {
  id     Int     @id @default(autoincrement())
  name   String
  active Boolean @default(true)
  books  Book[]
}

model Tag {
  id    Int       @id @default(autoincrement())
  name  String
  books Book[]    @relation(through: BookTag)
  links BookTag[]
}

model Book {
  id         Int       @id @default(autoincrement())
  author_id  Int
  title      String
  cover      Json?
  attachment Json?
  author     Author    @relation(fields: [author_id], references: [id])
  tags       Tag[]     @relation(through: BookTag)
  links      BookTag[]
}

model BookTag {
  id      Int  @id @default(autoincrement())
  book_id Int
  tag_id  Int
  book    Book @relation(fields: [book_id], references: [id], onDelete: Cascade)
  tag     Tag  @relation(fields: [tag_id], references: [id], onDelete: Cascade)
}
"#;

fn schema() -> Arc<Schema> {
    let source = format!("{MODELS}\n{ADMIN}");
    Arc::new(dsl::check(dsl::compile(&source, None).unwrap()).unwrap().1)
}

async fn database(schema: &Schema) -> Arc<dyn Executor> {
    let conn = db::connect("sqlite://:memory:", 1).await.unwrap();
    for step in migrate::plan(schema, &Default::default()).unwrap().up {
        conn.batch(step.sql).await.unwrap();
    }
    let mut authors = String::from("INSERT INTO \"Author\" (id, name, active) VALUES (1, 'Ada', 1), (2, 'Bo', 0)");
    for i in 3..=30 {
        authors.push_str(&format!(", ({i}, 'Author {i:02}', 1)"));
    }
    conn.batch(authors).await.unwrap();
    conn.batch("INSERT INTO \"Tag\" (id, name) VALUES (1, 'red'), (2, 'green'), (3, 'blue'), (4, 'grey'); INSERT INTO \"Book\" (id, author_id, title) VALUES (1, 1, 'First'); INSERT INTO \"BookTag\" (book_id, tag_id) VALUES (1, 1), (1, 2);".into()).await.unwrap();
    conn
}

fn resources(author: Value, book: Value) -> Value {
    let mut author_resource = json!({"model": "Author", "list": {"columns": ["name"], "order": ["id"], "search": ["name"]}});
    author_resource.as_object_mut().unwrap().extend(author.as_object().unwrap().clone());
    let mut book_resource = json!({"model": "Book", "list": {"columns": ["title"], "order": ["id"]}, "relations": {"author": {}, "tags": {}}, "files": ["cover", "attachment"]});
    book_resource.as_object_mut().unwrap().extend(book.as_object().unwrap().clone());
    json!({"resources": [author_resource, {"model": "Tag", "list": {"columns": ["name"], "order": ["id"], "search": ["name"]}}, book_resource]})
}

fn config() -> Value {
    resources(json!({}), json!({}))
}

struct World {
    site: Arc<Site>,
    conn: Arc<dyn Executor>,
}

async fn world(config: Value) -> World {
    let schema = schema();
    let conn = database(&schema).await;
    let site = Arc::new(Site::build(schema, &config.to_string()).unwrap());
    World { site, conn }
}

fn build_error(config: Value) -> String {
    match Site::build(schema(), &config.to_string()) {
        Ok(_) => String::from("built"),
        Err(e) => e.to_string(),
    }
}

impl World {
    async fn send(&self, method: &str, uri: &str, body: Option<Value>) -> (u16, Value) {
        let text = body.map(|b| b.to_string());
        let (status, out) = call_body(self.site.clone(), self.conn.clone(), method, &format!("/admin/api{uri}"), Some(json!({"key": 1, "is_superuser": true})), None, text.as_deref()).await;
        (status, serde_json::from_str(&out).unwrap_or(Value::Null))
    }

    async fn rows(&self, sql: &str) -> Vec<Vec<Option<String>>> {
        self.conn.query_text(sql.to_string()).await.unwrap()
    }
}

fn text(v: &Option<String>) -> &str {
    v.as_deref().unwrap_or_default()
}

fn labels(body: &Value) -> Vec<String> {
    body["items"].as_array().unwrap().iter().map(|i| i["label"].as_str().unwrap().to_string()).collect()
}

#[test]
fn a_picker_target_without_a_label_or_a_search_is_a_build_error() {
    let no_search = json!({"resources": [{"model": "Author", "list": {"columns": ["name"]}}, {"model": "Book", "list": {"columns": ["title"]}, "relations": {"author": {}}}]});
    assert!(build_error(no_search).contains("has no search"));
    let no_text = json!({"resources": [{"model": "Author", "list": {"columns": ["id"], "search": ["name"]}}, {"model": "Book", "list": {"columns": ["title"]}, "relations": {"author": {}}}]});
    assert!(build_error(no_text).contains("label"));
    let bad_label = resources(json!({}), json!({"relations": {"author": {"label": "active"}}}));
    assert!(build_error(bad_label).contains("not a text column"));
    let unknown = resources(json!({}), json!({"relations": {"nope": {}}}));
    assert!(build_error(unknown).contains("not a relation"));
    let not_resource = json!({"resources": [{"model": "Book", "list": {"columns": ["title"]}, "relations": {"author": {}}}]});
    assert!(build_error(not_resource).contains("not a resource"));
    let plain = resources(json!({}), json!({"relations": {"links": {}}}));
    assert!(build_error(plain).contains("not a resource"));
    assert_eq!(build_error(config()), "built");
}

#[test]
fn a_file_field_must_be_a_nullable_json_field() {
    assert!(build_error(resources(json!({}), json!({"files": ["title"]}))).contains("title"));
    assert!(build_error(resources(json!({}), json!({"files": ["cover", "cover"]}))).contains("cover"));
}

#[tokio::test]
async fn the_autocomplete_pages_by_keyset_and_stays_inside_the_queryset() {
    let w = world(config()).await;
    let (status, first) = w.send("GET", "/autocomplete/Book/author?limit=10", None).await;
    assert_eq!(status, 200, "{first}");
    assert_eq!(labels(&first).len(), 10);
    assert_eq!(first["items"][0], json!({"value": 1, "label": "Ada"}));
    let cursor = first["next"].as_str().expect("a next cursor");
    let (_, second) = w.send("GET", &format!("/autocomplete/Book/author?limit=10&cursor={cursor}"), None).await;
    assert_eq!(second["items"][0]["value"], 11);
    let mut all = labels(&first);
    let mut next = second["next"].as_str().map(str::to_owned);
    all.extend(labels(&second));
    while let Some(c) = next {
        let (_, page) = w.send("GET", &format!("/autocomplete/Book/author?limit=10&cursor={c}"), None).await;
        all.extend(labels(&page));
        next = page["next"].as_str().map(str::to_owned);
    }
    assert_eq!(all.len(), 30, "every row once");
    let (_, found) = w.send("GET", "/autocomplete/Book/author?q=Author%2007", None).await;
    assert_eq!(labels(&found), ["Author 07"]);
    let (status, _) = w.send("GET", "/autocomplete/Book/title", None).await;
    assert_eq!(status, 404, "a column that is not a relation field");

    let active = json!({"filter": {"t": "cmp", "op": "eq", "l": {"t": "col", "path": [], "name": "active"}, "r": {"t": "const", "value": true}}});
    let w = world(resources(json!({"queryset": active}), json!({}))).await;
    let (_, body) = w.send("GET", "/autocomplete/Book/author?limit=3", None).await;
    assert_eq!(labels(&body), ["Ada", "Author 03", "Author 04"], "the queryset of the target hides Bo");
}

#[tokio::test]
async fn choices_limit_the_picker_and_a_write_outside_them_is_a_422() {
    let active = json!({"filter": {"t": "cmp", "op": "eq", "l": {"t": "col", "path": [], "name": "active"}, "r": {"t": "const", "value": true}}});
    let w = world(resources(json!({}), json!({"relations": {"author": {"choices": active}, "tags": {}}}))).await;
    let (_, body) = w.send("GET", "/autocomplete/Book/author?limit=3", None).await;
    assert_eq!(labels(&body), ["Ada", "Author 03", "Author 04"]);
    let (status, body) = w.send("POST", "/resources/Book", Some(json!({"set": {"author_id": 2, "title": "Bad"}}))).await;
    assert_eq!(status, 422, "{body}");
    assert!(body["fields"]["author_id"][0].as_str().unwrap().contains("Choose"), "{body}");
    let (status, body) = w.send("POST", "/resources/Book", Some(json!({"set": {"author_id": 3, "title": "Good"}}))).await;
    assert_eq!(status, 201, "{body}");
    let (status, _) = w.send("PATCH", "/resources/Book/2", Some(json!({"set": {"author_id": 2}, "loaded": {"author_id": 3}}))).await;
    assert_eq!(status, 422, "a changed value is checked too");
    let (status, _) = w.send("PATCH", "/resources/Book/2", Some(json!({"set": {"title": "Renamed"}, "loaded": {"title": "Good"}}))).await;
    assert_eq!(status, 200, "an unchanged value stays");
}

#[tokio::test]
async fn the_chips_add_and_remove_links_in_the_save_with_one_audit_row() {
    let w = world(config()).await;
    let (status, links) = w.send("GET", "/links/Book/1/tags", None).await;
    assert_eq!(status, 200, "{links}");
    let mut names = labels(&links);
    names.sort();
    assert_eq!(names, ["green", "red"]);
    let (status, body) = w.send("PATCH", "/resources/Book/1", Some(json!({"set": {"title": "Renamed"}, "loaded": {"title": "First"}, "links": {"tags": {"add": [3, 4], "remove": [1]}}}))).await;
    assert_eq!(status, 200, "{body}");
    let rows = w.rows("SELECT tag_id FROM \"BookTag\" WHERE book_id = 1 ORDER BY tag_id").await;
    assert_eq!(rows.iter().map(|r| text(&r[0])).collect::<Vec<_>>(), ["2", "3", "4"]);
    let audit = w.rows("SELECT action, changes FROM \"AdminLogEntry\"").await;
    assert_eq!(audit.len(), 1, "one audit row for the field and the links");
    let changes: Value = serde_json::from_str(text(&audit[0][1])).unwrap();
    assert_eq!(changes["diff"]["links.tags"]["removed"], json!([1]));
    // A body with the links only is a change too, and adding a link that exists changes nothing.
    let (status, _) = w.send("PATCH", "/resources/Book/1", Some(json!({"links": {"tags": {"add": [3], "remove": [2]}}}))).await;
    assert_eq!(status, 200);
    let rows = w.rows("SELECT tag_id FROM \"BookTag\" WHERE book_id = 1 ORDER BY tag_id").await;
    assert_eq!(rows.iter().map(|r| text(&r[0])).collect::<Vec<_>>(), ["3", "4"]);
    let (status, body) = w.send("PATCH", "/resources/Book/1", Some(json!({"links": {"tags": {"add": [99]}}}))).await;
    assert_eq!(status, 422, "{body}");
    assert_eq!(w.rows("SELECT COUNT(*) FROM \"BookTag\"").await[0][0].as_deref(), Some("2"), "the failed save left the links as they were");
}

#[tokio::test]
async fn a_create_takes_chips_in_the_same_transaction() {
    let w = world(config()).await;
    let (status, body) = w.send("POST", "/resources/Book", Some(json!({"set": {"author_id": 1, "title": "New"}, "links": {"tags": {"add": [1, 3]}}}))).await;
    assert_eq!(status, 201, "{body}");
    let rows = w.rows("SELECT tag_id FROM \"BookTag\" WHERE book_id = 2 ORDER BY tag_id").await;
    assert_eq!(rows.iter().map(|r| text(&r[0])).collect::<Vec<_>>(), ["1", "3"]);
    let (status, _) = w.send("POST", "/resources/Book", Some(json!({"set": {"author_id": 1, "title": "Bad"}, "links": {"tags": {"add": [99]}}}))).await;
    assert_eq!(status, 422);
    assert_eq!(w.rows("SELECT COUNT(*) FROM \"Book\"").await[0][0].as_deref(), Some("2"), "the book of the failed save is not stored");
    assert_eq!(w.rows("SELECT COUNT(*) FROM \"AdminLogEntry\"").await[0][0].as_deref(), Some("1"));
}

struct Host {
    seen: Mutex<Vec<(String, Value)>>,
}

impl HostBridge for Host {
    fn call(&self, name: &str, payload: Value, ctx: CallContext) -> BoxFuture<Result<Value, BridgeError>> {
        self.seen.lock().unwrap().push((name.to_owned(), payload.clone()));
        assert!(ctx.tx.is_none(), "a file call holds no database transaction");
        let name = name.to_owned();
        Box::pin(async move {
            match name.as_str() {
                "file.store" => {
                    let size = std::fs::read(payload["path"].as_str().unwrap()).unwrap().len();
                    Ok(json!({"storage": "media", "key": "k1", "version": null, "filename": payload["filename"], "content_type": payload["content_type"], "size": size}))
                }
                "file.fetch" => {
                    std::fs::write(payload["path"].as_str().unwrap(), b"stored bytes").unwrap();
                    Ok(Value::Null)
                }
                other => Err(BridgeError::Bridge(format!("unknown host function {other}"))),
            }
        })
    }
}

fn auth(host: Arc<impl HostBridge + 'static>) -> AuthConfig {
    AuthConfig { security: Security::new(&[7u8; 32], SecurityOptions::default()).unwrap(), bridge: host }
}

const BOUNDARY: &str = "----monorm";

fn multipart(filename: &str, kind: &str, data: &[u8]) -> (String, Vec<u8>) {
    let mut body = format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: {kind}\r\n\r\n").into_bytes();
    body.extend_from_slice(data);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={BOUNDARY}"), body)
}

impl World {
    async fn upload(&self, uri: &str, host: &Arc<Host>, filename: &str, data: &[u8]) -> (u16, Vec<(String, String)>, Value) {
        let (kind, body) = multipart(filename, "image/png", data);
        let (status, headers, out) = call_full(self.site.clone(), self.conn.clone(), "POST", &format!("/admin/api{uri}"), Some(json!({"key": 1, "is_superuser": true})), Some(auth(host.clone())), Some((&kind, body))).await;
        (status, headers, serde_json::from_slice(&out).unwrap_or(Value::Null))
    }
}

#[tokio::test]
async fn an_upload_stores_the_reference_and_a_download_sends_it_as_an_attachment() {
    let w = world(config()).await;
    let host = Arc::new(Host { seen: Mutex::new(Vec::new()) });
    let (status, _, body) = w.upload("/files/Book/1/cover", &host, "dir/cover 1.png", b"\x89PNG data").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["value"]["key"], "k1");
    assert_eq!(body["value"]["filename"], "cover 1.png", "the quotes of the name are cleaned");
    assert_eq!(body["value"]["size"], 9);
    {
        let seen = host.seen.lock().unwrap();
        assert_eq!(seen[0].0, "file.store");
        assert_eq!((seen[0].1["model"].as_str(), seen[0].1["field"].as_str()), (Some("Book"), Some("cover")));
        assert!(seen[0].1["max_size"].as_u64().unwrap() > 0);
    }
    let stored = w.rows("SELECT cover FROM \"Book\" WHERE id = 1").await;
    assert!(text(&stored[0][0]).contains("\"k1\""));
    let audit = w.rows("SELECT action, changes FROM \"AdminLogEntry\"").await;
    assert_eq!(audit.len(), 1);
    assert_eq!(text(&audit[0][0]), "update");

    let user = Some(json!({"key": 1, "is_superuser": true}));
    let (status, headers, bytes) = call_full(w.site.clone(), w.conn.clone(), "GET", "/admin/api/files/Book/1/cover", user.clone(), Some(auth(host.clone())), None).await;
    assert_eq!(status, 200);
    assert_eq!(bytes, b"stored bytes");
    let header = |name: &str| headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.clone()).unwrap_or_default();
    assert_eq!(header("content-type"), "image/png", "the stored content type");
    assert!(header("content-disposition").starts_with("attachment;"), "{}", header("content-disposition"));
    assert_eq!(header("x-content-type-options"), "nosniff");
    assert!(headers.iter().all(|(_, v)| !v.contains("http")), "no storage URL in a header");

    let (status, ..) = call_full(w.site.clone(), w.conn.clone(), "GET", "/admin/api/files/Book/1/attachment", user.clone(), Some(auth(host.clone())), None).await;
    assert_eq!(status, 404, "a field with no file");
    let (status, ..) = call_full(w.site.clone(), w.conn.clone(), "GET", "/admin/api/files/Book/1/title", user, Some(auth(host.clone())), None).await;
    assert_eq!(status, 404, "a field that is not a file field");
}

#[tokio::test]
async fn a_plain_set_of_a_file_field_is_a_422_and_a_bad_upload_stores_nothing() {
    let w = world(config()).await;
    let (status, body) = w.send("PATCH", "/resources/Book/1", Some(json!({"set": {"cover": {"key": "x"}}, "loaded": {"cover": null}}))).await;
    assert_eq!(status, 422, "{body}");
    assert!(body["fields"]["cover"][0].as_str().unwrap().contains("upload"));
    let host = Arc::new(Host { seen: Mutex::new(Vec::new()) });
    let (status, ..) = w.upload("/files/Book/9/cover", &host, "a.png", b"x").await;
    assert_eq!(status, 404, "a record that does not exist");
    let (status, ..) = w.upload("/files/Book/1/title", &host, "a.png", b"x").await;
    assert_eq!(status, 404);
    let user = Some(json!({"key": 1, "is_superuser": true}));
    let (status, ..) = call_full(w.site.clone(), w.conn.clone(), "POST", "/admin/api/files/Book/1/cover", user.clone(), Some(auth(host.clone())), Some(("multipart/form-data", b"x".to_vec()))).await;
    assert_eq!(status, 400, "no boundary");
    let (status, ..) = call_full(w.site.clone(), w.conn.clone(), "POST", "/admin/api/files/Book/1/cover", user, Some(auth(host.clone())), Some((&format!("multipart/form-data; boundary={BOUNDARY}"), b"not multipart".to_vec()))).await;
    assert_eq!(status, 400);
    assert!(host.seen.lock().unwrap().is_empty(), "the host stored nothing");
    assert_eq!(w.rows("SELECT COUNT(*) FROM \"AdminLogEntry\"").await[0][0].as_deref(), Some("0"));
}

#[tokio::test]
async fn an_upload_over_the_cap_is_a_413() {
    let w = world(config()).await;
    let host = Arc::new(Host { seen: Mutex::new(Vec::new()) });
    let big = vec![b'a'; 26 * 1024 * 1024];
    let (status, _, _) = w.upload("/files/Book/1/cover", &host, "big.bin", &big).await;
    assert_eq!(status, 413);
    assert!(host.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_read_only_file_field_takes_no_upload() {
    let w = world(resources(json!({}), json!({"readonly": ["cover"]}))).await;
    let host = Arc::new(Host { seen: Mutex::new(Vec::new()) });
    let (status, _, body) = w.upload("/files/Book/1/cover", &host, "a.png", b"x").await;
    assert_eq!(status, 422, "{body}");
    assert!(body["fields"]["cover"][0].as_str().unwrap().contains("read-only"), "{body}");
    let (status, ..) = w.upload("/files/Book/1/attachment", &host, "a.png", b"x").await;
    assert_eq!(status, 200, "a field that is not read-only still takes an upload");
    let seen = host.seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "the host stored only the second file");
    assert_eq!(seen[0].1["field"], "attachment");
}

/// The host runs `file.store` and queries the one connection of the app. A transaction held over the call would block the query.
struct Probe {
    conn: Arc<dyn Executor>,
    queried: Arc<Mutex<Vec<bool>>>,
}

impl HostBridge for Probe {
    fn call(&self, name: &str, payload: Value, _: CallContext) -> BoxFuture<Result<Value, BridgeError>> {
        let (name, conn, queried) = (name.to_owned(), self.conn.clone(), self.queried.clone());
        Box::pin(async move {
            if name != "file.store" {
                return Err(BridgeError::Bridge(format!("unknown host function {name}")));
            }
            let answer = tokio::time::timeout(std::time::Duration::from_secs(2), conn.query_text("SELECT COUNT(*) FROM \"Book\"".into())).await;
            queried.lock().unwrap().push(matches!(answer, Ok(Ok(_))));
            Ok(json!({"storage": "media", "key": "k2", "filename": payload["filename"], "content_type": payload["content_type"], "size": 1}))
        })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_upload_holds_no_transaction_while_the_host_stores_the_file() {
    let w = world(config()).await;
    let probe = Arc::new(Probe { conn: w.conn.clone(), queried: Arc::default() });
    let (kind, body) = multipart("a.png", "image/png", b"x");
    let user = Some(json!({"key": 1, "is_superuser": true}));
    let call = call_full(w.site.clone(), w.conn.clone(), "POST", "/admin/api/files/Book/1/cover", user, Some(auth(probe.clone())), Some((&kind, body)));
    let (status, _, out) = tokio::time::timeout(std::time::Duration::from_secs(10), call).await.expect("the upload hangs: a transaction blocks the one connection");
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&out));
    assert_eq!(*probe.queried.lock().unwrap(), vec![true], "the host query ran while file.store ran");
    assert!(text(&w.rows("SELECT cover FROM \"Book\" WHERE id = 1").await[0][0]).contains("\"k2\""));
}

#[test]
fn every_file_field_is_a_detail_column() {
    let site = Site::build(schema(), &config().to_string()).unwrap();
    let book = site.resource("Book").unwrap();
    assert_eq!(book.files, ["cover", "attachment"]);
    for file in &book.files {
        assert!(book.detail.iter().any(|d| &d.field == file), "{file} is a detail column");
    }
}
