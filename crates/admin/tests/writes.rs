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

model Chore {
  id    Int       @id @default(autoincrement())
  every Duration?
}

model Locked {
  id Int @id @default(autoincrement())
  x  Int
}

model Journal {
  id Int @id @default(autoincrement())
  note String
}

model Sealed {
  id Int @id @default(autoincrement())
  x  Int
}

model Fielded {
  id   Int    @id @default(autoincrement())
  code String
  note String
}

model Gated {
  id   Int    @id @default(autoincrement())
  note String
}
"#;

fn schema() -> Arc<Schema> {
    let source = format!("{MODELS}\n{ADMIN}");
    Arc::new({
        let mut ir = dsl::compile(&source, None).unwrap();
        ir.behavior.write_rules.push(serde_json::from_value(serde_json::json!({
            "source": "test", "model": "Locked", "name": "protect",
            "operations": ["insert", "update", "delete"], "layer": "app"
        })).unwrap());
        ir.behavior.write_rules.push(serde_json::from_value(serde_json::json!({
            "source": "test", "model": "Journal", "name": "append_only",
            "operations": ["update", "delete"]
        })).unwrap());
        for rule in [
            json!({"source": "test", "model": "Sealed", "name": "sealed", "operations": ["update"], "layer": "db"}),
            json!({"source": "test", "model": "Fielded", "name": "code_fixed", "operations": ["update"], "fields": ["code"], "layer": "app"}),
            json!({"source": "test", "model": "Gated", "name": "gate", "operations": ["update"], "layer": "db", "condition": {"sql": "OLD.note = 'locked'"}}),
        ] {
            ir.behavior.write_rules.push(serde_json::from_value(rule).unwrap());
        }
        ir.stamp_expression_records();
        dsl::check(ir).unwrap().1
    })
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
    json!({"resources": [{"model": "Author"}, {"model": "Book", "check_messages": {"no_banned_title": "That title is not allowed."}}, {"model": "Locked"}, {"model": "Chore"}]})
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
    assert!(out["error"]["message"].as_str().unwrap().contains("write-protected"));
    let (_, meta) = w.send("GET", "/resources", None).await;
    let of = |name: &str| meta["resources"].as_array().unwrap().iter().find(|r| r["name"] == name).cloned().unwrap();
    assert_eq!(of("Locked")["writable"], false);
    assert_eq!(of("Book")["writable"], true);
    assert!(of("Locked")["read_only_reason"].is_string());
}

#[tokio::test]
async fn append_only_rules_leave_create_available_in_the_admin() {
    let w = world(json!({"resources": [{"model": "Journal"}]})).await;
    let (_, meta) = w.send("GET", "/resources", None).await;
    assert_eq!(meta["resources"][0]["ops"], json!(["create"]));
    assert_eq!(meta["resources"][0]["bulk_delete"], false);
    let (status, out) = w.send("POST", "/resources/Journal", Some(json!({"set": {"note": "entry"}}))).await;
    assert_eq!(status, 201, "{out}");
    let (status, _) = w.send("DELETE", "/resources/Journal/1", None).await;
    assert_eq!(status, 403);
}

#[tokio::test]
async fn a_database_only_rule_takes_its_operation_away_and_a_field_rule_marks_only_its_field() {
    let w = world(json!({"resources": [{"model": "Sealed"}, {"model": "Fielded"}, {"model": "Gated"}]})).await;
    let (_, meta) = w.send("GET", "/resources", None).await;
    let of = |name: &str| meta["resources"].as_array().unwrap().iter().find(|r| r["name"] == name).cloned().unwrap();
    // A rule on the database layer alone hides the update; create and delete stay.
    assert_eq!(of("Sealed")["ops"], json!(["create", "delete"]));
    assert!(of("Sealed")["writable"] == true);
    let (status, out) = w.send("PATCH", "/resources/Sealed/1", Some(json!({"set": {"x": 2}}))).await;
    assert_eq!((status, &out["error"]["kind"]), (403, &json!("ReadOnly")), "{out}");
    // A rule on one field keeps the update and marks only that field (and the key) read-only on update.
    assert_eq!(of("Fielded")["ops"], json!(["create", "update", "delete"]));
    let readonly: Vec<(String, bool)> = of("Fielded")["form"].as_array().unwrap().iter().map(|f| (f["name"].as_str().unwrap().to_owned(), f["readonly_on_update"].as_bool().unwrap())).collect();
    assert_eq!(readonly, [("id".to_owned(), true), ("code".to_owned(), true), ("note".to_owned(), false)]);
    w.conn.batch("INSERT INTO \"Fielded\" (id, code, note) VALUES (1, 'c', 'n')".into()).await.unwrap();
    let (status, out) = w.send("PATCH", "/resources/Fielded/1", Some(json!({"set": {"note": "edited"}, "loaded": {"note": "n"}}))).await;
    assert_eq!(status, 200, "{out}");
    // A conditional rule leaves the update offered, because the admin cannot evaluate the condition.
    assert_eq!(of("Gated")["ops"], json!(["create", "update", "delete"]));
}

#[tokio::test]
async fn a_conditional_rule_refuses_in_the_database_and_the_admin_answers_with_a_403() {
    let w = world(json!({"resources": [{"model": "Gated"}]})).await;
    w.conn.batch("INSERT INTO \"Gated\" (id, note) VALUES (1, 'open'), (2, 'locked')".into()).await.unwrap();
    let (status, out) = w.send("PATCH", "/resources/Gated/1", Some(json!({"set": {"note": "edited"}, "loaded": {"note": "open"}}))).await;
    assert_eq!(status, 200, "{out}");
    let (status, out) = w.send("PATCH", "/resources/Gated/2", Some(json!({"set": {"note": "edited"}, "loaded": {"note": "locked"}}))).await;
    assert_eq!((status, &out["error"]["kind"]), (403, &json!("WriteProtected")), "{out}");
    assert!(out["error"]["message"].as_str().unwrap().contains("gate"), "{out}");
    assert_eq!(w.rows("SELECT note FROM \"Gated\" WHERE id = 2").await[0][0].as_deref(), Some("locked"));
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

#[tokio::test]
async fn a_duration_is_written_as_text_and_read_back_as_the_same_text() {
    let w = world(config()).await;
    let meta = w.send("GET", "/resources", None).await.1;
    let chore = meta["resources"].as_array().unwrap().iter().find(|r| r["name"] == "Chore").unwrap();
    let every = chore["fields"].as_array().unwrap().iter().find(|f| f["label"] == "every").unwrap();
    assert_eq!(every["widget"], "text", "{meta}");
    for (given, micros) in [("01:30:00", "5400000000"), ("1 02:03:04.5", "93784500000"), ("PT1H30M", "5400000000"), ("-00:00:01", "-1000000")] {
        let (status, body) = w.send("POST", "/resources/Chore", Some(json!({"set": {"every": given}}))).await;
        assert_eq!(status, 201, "{given}: {body}");
        let stored = w.rows(&format!("SELECT every FROM \"Chore\" WHERE id = {}", body["key"].as_str().unwrap())).await;
        assert_eq!(text(&stored[0][0]), micros, "{given}");
    }
    let (status, body) = w.send("GET", "/resources/Chore/1", None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["values"][1], "01:30:00", "{body}");
    let (status, body) = w.send("POST", "/resources/Chore", Some(json!({"set": {"every": "soon"}}))).await;
    assert_eq!((status, &body["fields"]["every"]), (422, &json!(["Must be a duration (HH:MM:SS, 1 02:03:04 or PT1H30M)"])), "{body}");
}

/// On PostgreSQL the `interval` column takes its text through the `write_sql` template: create, a stale-checked update and a range filter.
#[tokio::test]
async fn a_duration_writes_and_filters_on_postgres() {
    let Ok(url) = std::env::var("ORM_RUST_TEST_POSTGRES_URL") else {
        eprintln!("skipped: ORM_RUST_TEST_POSTGRES_URL is not set");
        return;
    };
    let models = "datasource db {\n  provider = \"postgresql\"\n}\n\nmodel PChore {\n  id Int @id @default(autoincrement())\n  every Duration?\n  @@map(\"p_chores\")\n}\n";
    let schema = Arc::new(dsl::check(dsl::compile(&format!("{models}\n{ADMIN}"), None).unwrap()).unwrap().1);
    let conn = db::connect(&url, 2).await.unwrap();
    conn.batch("DROP TABLE IF EXISTS p_chores; DROP TABLE IF EXISTS adminlogentry, userpreference, adminsessionrevocation".into()).await.unwrap();
    for step in migrate::plan(&schema, &Default::default()).unwrap().up {
        conn.batch(step.sql).await.unwrap();
    }
    let site = Arc::new(Site::build(schema, &json!({"resources": [{"model": "PChore", "list": {"columns": ["every"], "order": ["id"], "filters": ["every"]}}]}).to_string()).unwrap());
    let w = World { site, conn };
    for given in ["00:00:01", "01:30:00", "2 00:00:00"] {
        let (status, body) = w.send("POST", "/resources/PChore", Some(json!({"set": {"every": given}}))).await;
        assert_eq!(status, 201, "{given}: {body}");
    }
    let stored = w.rows("SELECT CAST(EXTRACT(EPOCH FROM every) AS bigint) FROM p_chores ORDER BY id").await;
    assert_eq!(stored.iter().map(|r| text(&r[0]).to_string()).collect::<Vec<_>>(), ["1", "5400", "172800"]);
    let (status, out) = w.send("PATCH", "/resources/PChore/1", Some(json!({"set": {"every": "00:00:02"}, "loaded": {"every": "00:00:01"}}))).await;
    assert_eq!((status, &out["changed"]), (200, &json!(["every"])), "{out}");
    let (status, out) = w.send("PATCH", "/resources/PChore/1", Some(json!({"set": {"every": "00:00:03"}, "loaded": {"every": "00:00:01"}}))).await;
    assert_eq!(status, 409, "{out}");
    let (status, out) = w.send("GET", "/resources/PChore?f.every:gte=00:01:00&f.every:lt=1%2000:00:00", None).await;
    assert_eq!(status, 200, "{out}");
    assert_eq!(out["rows"], json!([["01:30:00", 2]]), "{out}");
}
