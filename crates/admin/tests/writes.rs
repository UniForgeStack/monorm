//! Plain writes: create, change and delete, the 409 and 422 answers, the read-only reason and the audit row, on SQLite in memory.
use std::sync::Arc;

use monorm_admin::{call_body, Site};
use monorm_core::{dsl, migrate, schema::Schema};
use monorm_engine::db::{self, Executor};
use serde_json::{json, Value};

const ADMIN: &str = include_str!("../../../apps/admin/schema.prisma");
const MODELS: &str = r#"
datasource db {
  provider = "sqlite"
}

enum Status {
  draft
  published
  @@storage(text)
}

model Author {
  id    Int    @id @default(autoincrement())
  email String @unique
  name  String @db.VarChar(10)
  books Book[]
}

model Book {
  id        Int     @id @default(autoincrement())
  author_id Int
  title     String  @db.VarChar(20)
  pages     Int     @default(0) @check(raw("pages >= 0"))
  status    Status  @default(draft)
  meta      Json?
  author    Author  @relation(fields: [author_id], references: [id], onDelete: Restrict)
  @@check("title <> 'banned'", name: "no_banned_title")
}

model Locked {
  id Int @id @default(autoincrement())
  x  Int
  @@protected_write
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
    conn.batch("INSERT INTO \"Author\" (id, email, name) VALUES (1, 'a@x.io', 'Ada'), (2, 'b@x.io', 'Bo'); INSERT INTO \"Book\" (id, author_id, title, pages, status) VALUES (1, 1, 'First', 10, 'draft');".into()).await.unwrap();
    conn
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

fn config() -> Value {
    json!({"resources": [{"model": "Author"}, {"model": "Book", "check_messages": {"no_banned_title": "That title is not allowed."}}, {"model": "Locked"}]})
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

    async fn audit(&self) -> Vec<Vec<Option<String>>> {
        self.rows("SELECT action, resource, record_key, record_label, changes FROM \"AdminLogEntry\" ORDER BY id").await
    }
}

fn text(v: &Option<String>) -> &str {
    v.as_deref().unwrap_or_default()
}

#[tokio::test]
async fn a_create_stores_the_row_and_writes_one_audit_row_with_the_diff() {
    let w = world(config()).await;
    let (status, body) = w.send("POST", "/resources/Book", Some(json!({"set": {"author_id": 2, "title": "Second", "status": "published", "meta": {"a": [1]}}}))).await;
    assert_eq!(status, 201, "{body}");
    assert_eq!(body["key"], "2");
    let stored = w.rows("SELECT author_id, title, pages, status, meta FROM \"Book\" WHERE id = 2").await;
    assert_eq!(stored[0].iter().map(text).collect::<Vec<_>>(), ["2", "Second", "0", "published", "{\"a\":[1]}"]);
    let audit = w.audit().await;
    assert_eq!(audit.len(), 1);
    assert_eq!((text(&audit[0][0]), text(&audit[0][1]), text(&audit[0][2]), text(&audit[0][3])), ("create", "Book", "2", "Second"));
    let changes: Value = serde_json::from_str(text(&audit[0][4])).unwrap();
    assert_eq!(changes["diff"]["title"], json!({"from": null, "to": "Second"}));
    assert!(changes["request_id"].is_string() && changes["ip"].is_string() && changes["user_agent"].is_string());
}

#[tokio::test]
async fn a_bad_create_is_a_422_with_one_message_per_field_and_stores_nothing() {
    let w = world(config()).await;
    let (status, body) = w.send("POST", "/resources/Book", Some(json!({"set": {"title": "x".repeat(21), "pages": 1.5, "status": "gone", "meta": 1, "nope": 1}}))).await;
    assert_eq!(status, 422);
    let fields = &body["fields"];
    assert_eq!(fields["title"], json!(["Must have at most 20 characters"]));
    assert_eq!(fields["pages"], json!(["Must be a whole number"]));
    assert_eq!(fields["status"], json!(["Must be one of the listed values"]));
    assert_eq!(fields["author_id"], json!(["This field is required."]));
    assert_eq!(fields["nope"], json!(["This field cannot be written."]));
    assert_eq!(w.rows("SELECT count(*) FROM \"Book\"").await[0][0].as_deref(), Some("1"));
    assert!(w.audit().await.is_empty());
}

#[tokio::test]
async fn database_rules_come_back_as_field_errors_and_a_named_check_as_a_form_message() {
    let w = world(config()).await;
    let (status, body) = w.send("POST", "/resources/Author", Some(json!({"set": {"email": "a@x.io", "name": "Cy"}}))).await;
    assert_eq!((status, &body["fields"]["email"]), (422, &json!(["A record with this value already exists."])), "{body}");
    let (status, body) = w.send("POST", "/resources/Book", Some(json!({"set": {"author_id": 99, "title": "T"}}))).await;
    assert_eq!((status, &body["fields"]["author_id"]), (422, &json!(["No such record."])), "{body}");
    let (status, body) = w.send("POST", "/resources/Book", Some(json!({"set": {"author_id": 1, "title": "T", "pages": -1}}))).await;
    assert_eq!((status, &body["fields"]["pages"]), (422, &json!(["This value is not allowed."])), "{body}");
    let (status, body) = w.send("POST", "/resources/Book", Some(json!({"set": {"author_id": 1, "title": "banned"}}))).await;
    assert_eq!((status, &body["form"]), (422, &json!(["That title is not allowed."])), "{body}");
    assert!(w.audit().await.is_empty());
}

#[tokio::test]
async fn an_update_sends_only_the_changed_fields_and_the_audit_row_holds_the_diff() {
    let w = world(config()).await;
    let body = json!({"set": {"title": "Renamed", "pages": 10}, "loaded": {"title": "First", "pages": 10}});
    let (status, out) = w.send("PATCH", "/resources/Book/1", Some(body)).await;
    assert_eq!((status, &out["changed"]), (200, &json!(["title"])), "{out}");
    assert_eq!(w.rows("SELECT title FROM \"Book\" WHERE id = 1").await[0][0].as_deref(), Some("Renamed"));
    let audit = w.audit().await;
    let changes: Value = serde_json::from_str(text(&audit[0][4])).unwrap();
    assert_eq!(changes["diff"], json!({"title": {"from": "First", "to": "Renamed"}}));
    // No change: 200, and no second audit row.
    let (status, out) = w.send("PATCH", "/resources/Book/1", Some(json!({"set": {"title": "Renamed"}, "loaded": {"title": "Renamed"}}))).await;
    assert_eq!((status, &out["changed"]), (200, &json!([])));
    assert_eq!(w.audit().await.len(), 1);
}

#[tokio::test]
async fn a_stale_loaded_value_is_a_409_with_the_current_value_and_writes_nothing() {
    let w = world(config()).await;
    let body = json!({"set": {"title": "Mine"}, "loaded": {"title": "Old"}});
    let (status, out) = w.send("PATCH", "/resources/Book/1", Some(body)).await;
    assert_eq!(status, 409, "{out}");
    assert_eq!(out["conflicts"]["title"], json!({"loaded": "Old", "current": "First"}));
    assert_eq!(w.rows("SELECT title FROM \"Book\" WHERE id = 1").await[0][0].as_deref(), Some("First"));
    assert!(w.audit().await.is_empty());
}

#[tokio::test]
async fn an_update_checks_the_request_before_it_writes() {
    let w = world(config()).await;
    let go = |body: Value| w.send("PATCH", "/resources/Book/1", Some(body));
    assert_eq!(go(json!({"set": {"title": "x"}})).await.0, 400, "a changed field needs its loaded value");
    let (status, out) = go(json!({"set": {"id": 5}, "loaded": {"id": 1}})).await;
    assert_eq!((status, &out["fields"]["id"]), (422, &json!(["The key of a record cannot change."])));
    assert_eq!(w.send("PATCH", "/resources/Book/9", Some(json!({"set": {}}))).await.0, 404);
    assert_eq!(w.send("PATCH", "/resources/Nope/1", Some(json!({"set": {}}))).await.0, 404);
    assert_eq!(w.send("PATCH", "/resources/Book/1", Some(json!([1]))).await.0, 400);
}

#[tokio::test]
async fn a_delete_removes_the_row_and_a_referenced_row_is_a_form_message() {
    let w = world(config()).await;
    let (status, out) = w.send("DELETE", "/resources/Author/1", None).await;
    assert_eq!(status, 422, "{out}");
    assert!(out["form"][0].as_str().unwrap().contains("still refer"), "{out}");
    assert_eq!(w.rows("SELECT count(*) FROM \"Author\"").await[0][0].as_deref(), Some("2"));
    let (status, out) = w.send("DELETE", "/resources/Book/1", Some(json!({"loaded": {"title": "Stale"}}))).await;
    assert_eq!((status, &out["conflicts"]["title"]["current"]), (409, &json!("First")));
    assert_eq!(w.send("DELETE", "/resources/Book/1", Some(json!({"loaded": {"title": "First"}}))).await.0, 200);
    assert_eq!(w.send("DELETE", "/resources/Book/1", None).await.0, 404);
    let audit = w.audit().await;
    assert_eq!(audit.len(), 1);
    assert_eq!((text(&audit[0][0]), text(&audit[0][2]), text(&audit[0][3])), ("delete", "1", "First"));
}

#[tokio::test]
async fn a_protected_model_is_read_only_with_the_reason_and_the_metadata_says_so() {
    let w = world(config()).await;
    let (status, out) = w.send("POST", "/resources/Locked", Some(json!({"set": {"x": 1}}))).await;
    assert_eq!((status, &out["error"]["kind"]), (403, &json!("ReadOnly")));
    assert!(out["error"]["message"].as_str().unwrap().contains("@@protected_write"));
    let (_, meta) = w.send("GET", "/resources", None).await;
    let of = |name: &str| meta["resources"].as_array().unwrap().iter().find(|r| r["name"] == name).cloned().unwrap();
    assert_eq!(of("Locked")["writable"], false);
    assert_eq!(of("Book")["writable"], true);
    assert!(of("Locked")["read_only_reason"].is_string());
}

#[tokio::test]
async fn an_operation_that_the_resource_does_not_allow_is_a_403_with_an_audit_row() {
    let w = world(json!({"resources": [{"model": "Book", "allow": ["read", "create"]}]})).await;
    let (status, _) = w.send("PATCH", "/resources/Book/1", Some(json!({"set": {}}))).await;
    assert_eq!(status, 403);
    assert_eq!(w.send("DELETE", "/resources/Book/1", None).await.0, 403);
    assert_eq!(w.audit().await.iter().map(|r| text(&r[0]).to_owned()).collect::<Vec<_>>(), ["permission_denied", "permission_denied"]);
}

#[tokio::test]
async fn a_raw_json_value_counts_by_what_it_stores() {
    let w = world(config()).await;
    let go = |set: Value, loaded: Value| w.send("PATCH", "/resources/Book/1", Some(json!({"set": {"pages": set}, "loaded": {"pages": loaded}})));
    let (status, out) = go(json!("10"), json!("10")).await;
    assert_eq!((status, &out["changed"]), (200, &json!([])), "{out}");
    assert!(w.audit().await.is_empty(), "a value that equals the stored one writes no audit row");
    let (status, out) = go(json!("11"), json!("10")).await;
    assert_eq!((status, &out["changed"]), (200, &json!(["pages"])), "{out}");
    let changes: Value = serde_json::from_str(text(&w.audit().await[0][4])).unwrap();
    assert_eq!(changes["diff"], json!({"pages": {"from": 10, "to": 11}}));
    let (status, out) = go(json!(12), json!("10")).await;
    assert_eq!(status, 409, "{out}");
    assert_eq!(out["conflicts"]["pages"], json!({"loaded": "10", "current": 11}));
}

#[tokio::test]
async fn a_delete_ignores_a_loaded_key_that_is_not_a_field() {
    let w = world(config()).await;
    let (status, out) = w.send("DELETE", "/resources/Author/2", Some(json!({"loaded": {"nope": 1}}))).await;
    assert_eq!(status, 200, "{out}");
}

#[test]
fn a_check_message_for_a_missing_check_fails_the_build() {
    let config = json!({"resources": [{"model": "Book", "check_messages": {"no_such_check": "x"}}]});
    let e = Site::build(schema(), &config.to_string()).err().expect("the build fails");
    assert!(e.to_string().contains("no_such_check"), "{e}");
}
