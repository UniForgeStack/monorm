//! List edit: `PATCH /resources/<name>` saves the changed cells of several rows in one transaction.
//! The plain path and the host path (a fake host), on SQLite in memory.
use std::sync::{Arc, Mutex};

use monorm_admin::{call_full, AuthConfig, BoxFuture, BridgeError, CallContext, HostBridge, Security, SecurityOptions, Site};
use monorm_core::{behavior::SCHEMA_CONTRACT, dsl, migrate, schema::Schema};
use monorm_engine::db::{self, Executor};
use serde_json::{json, Value};

const ADMIN: &str = include_str!("../../../apps/admin/schema.prisma");
const MODELS: &str = r#"
datasource db {
  provider = "sqlite"
}

model Author {
  id    Int    @id @default(autoincrement())
  name  String
  books Book[]
}

model Book {
  id        Int    @id @default(autoincrement())
  author_id Int
  title     String @db.VarChar(10)
  pages     Int    @default(0)
  note      String @default("")
  author    Author @relation(fields: [author_id], references: [id], onDelete: Restrict)
  @@unique([title])
}
"#;

fn schema() -> Arc<Schema> {
    let source = format!("{MODELS}\n{ADMIN}");
    Arc::new(dsl::check(dsl::compile(&source, None).unwrap()).unwrap().1)
}

/// The schema with `Book.pages` as an instance guard, as an extension (a version counter) would add it.
fn guarded() -> Arc<Schema> {
    let mut ir = serde_json::to_value(dsl::compile(&format!("{MODELS}\n{ADMIN}"), None).unwrap()).unwrap();
    ir["behavior"]["schema_contract"] = json!(SCHEMA_CONTRACT);
    ir["behavior"]["instance_guards"] = json!([{"source": "test-ext", "model": "Book", "field": "pages"}]);
    Arc::new(Schema::from_ir(serde_json::from_value(ir).unwrap()).unwrap())
}

/// The fake host. `update_rows` calls run the SQL of each row in the transaction of the call; `answer` replaces the reply.
struct Host {
    seen: Mutex<Vec<(String, Value)>>,
    conn: Arc<dyn Executor>,
    answer: Mutex<Option<Value>>,
}

impl HostBridge for Host {
    fn call(&self, name: &str, payload: Value, ctx: CallContext) -> BoxFuture<Result<Value, BridgeError>> {
        self.seen.lock().unwrap().push((name.to_owned(), payload.clone()));
        let canned = self.answer.lock().unwrap().clone();
        let conn = self.conn.clone();
        let tx = ctx.tx.clone();
        Box::pin(async move {
            if let Some(answer) = canned {
                return Ok(answer);
            }
            let mut results = Vec::new();
            for row in payload["rows"].as_array().expect("an update_rows call") {
                let sql = format!("UPDATE \"Book\" SET title = '{}' WHERE id = {}", row["values"]["title"].as_str().unwrap(), row["key"]["id"]);
                match &tx {
                    Some(t) => t.batch(sql).await.unwrap(),
                    None => conn.batch(sql).await.unwrap(),
                };
                results.push(json!({"ok": true}));
            }
            Ok(json!({"ok": true, "results": results}))
        })
    }
}

struct World {
    site: Arc<Site>,
    conn: Arc<dyn Executor>,
    host: Arc<Host>,
}

fn book(extra: Value) -> Value {
    let mut r = json!({"model": "Book", "list": {"columns": ["title", "pages", "note"], "editable": ["title", "pages"], "order": ["id"]}});
    for (k, v) in extra.as_object().unwrap() {
        r[k] = v.clone();
    }
    r
}

async fn world(resource: Value) -> World {
    let schema = schema();
    let conn = db::connect("sqlite://:memory:", 1).await.unwrap();
    for step in migrate::plan(&schema, &Default::default()).unwrap().up {
        conn.batch(step.sql).await.unwrap();
    }
    conn.batch("INSERT INTO \"Author\" (id, name) VALUES (7, 'Me'), (8, 'Other'); INSERT INTO \"Book\" (id, author_id, title, pages, note) VALUES (1, 7, 'One', 1, 'a'), (2, 7, 'Two', 2, 'b'), (3, 8, 'Three', 3, 'c'), (4, 7, 'Four', 4, 'd')".into()).await.unwrap();
    let site = Arc::new(Site::build(schema, &json!({"resources": [{"model": "Author"}, resource]}).to_string()).unwrap());
    let host = Arc::new(Host { seen: Mutex::new(Vec::new()), conn: conn.clone(), answer: Mutex::new(None) });
    World { site, conn, host }
}

fn build_error(resource: Value) -> String {
    match Site::build(schema(), &json!({"resources": [{"model": "Author"}, resource]}).to_string()) {
        Ok(_) => String::from("built"),
        Err(e) => e.to_string(),
    }
}

impl World {
    async fn send(&self, method: &str, uri: &str, body: Value) -> (u16, Value) {
        let auth = AuthConfig { security: Security::new(&[7u8; 32], SecurityOptions::default()).unwrap(), bridge: self.host.clone() };
        let (status, _, out) = call_full(self.site.clone(), self.conn.clone(), method, &format!("/admin/api{uri}"), Some(json!({"key": 7, "is_superuser": true})), Some(auth), Some(("application/json", body.to_string().into_bytes()))).await;
        (status, serde_json::from_slice(&out).unwrap_or(Value::Null))
    }

    async fn save(&self, rows: Value) -> (u16, Value) {
        self.send("PATCH", "/resources/Book", json!({"rows": rows})).await
    }

    async fn titles(&self) -> Vec<String> {
        self.conn.query_text("SELECT title || ':' || pages FROM \"Book\" ORDER BY id".into()).await.unwrap().into_iter().map(|r| r[0].clone().unwrap()).collect()
    }

    async fn audit(&self) -> Vec<(String, String, Value)> {
        self.conn.query_text("SELECT action, record_key, changes FROM \"AdminLogEntry\" WHERE action = 'update' ORDER BY id".into()).await.unwrap().into_iter().map(|r| (r[0].clone().unwrap(), r[1].clone().unwrap(), serde_json::from_str(r[2].as_deref().unwrap_or("null")).unwrap_or(Value::Null))).collect()
    }
}

fn row(key: &str, set: Value, loaded: Value) -> Value {
    json!({"key": key, "set": set, "loaded": loaded})
}

#[test]
fn a_wrong_editable_list_is_a_build_error() {
    let cases = [
        (json!({"list": {"columns": ["title"], "editable": ["pages"]}}), "not a column of the list"),
        (json!({"list": {"columns": ["title", "author.name"], "editable": ["author.name"]}}), "not a scalar field"),
        (json!({"list": {"columns": ["id", "title"], "editable": ["id"]}}), "a key field"),
        (json!({"readonly": ["pages"]}), "a read-only field of the update form"),
        (json!({"allow": ["read"]}), "needs the page to allow update"),
        (json!({"list": {"columns": ["title"], "editable": ["title", "title"]}}), "twice"),
        (json!({"service": {"update": {}}, "inputs": [{"name": "why", "type": "text", "required": true}]}), "required input"),
    ];
    for (extra, message) in cases {
        let found = build_error(book(extra));
        assert!(found.contains(message), "{message}: {found}");
    }
    assert_eq!(build_error(book(json!({}))), "built");
}

#[test]
fn a_guard_field_outside_the_list_columns_is_a_build_error() {
    let build = |columns: Value| Site::build(guarded(), &json!({"resources": [{"model": "Author"}, book(json!({"list": {"columns": columns, "editable": ["title"]}}))]}).to_string()).map(|_| ()).map_err(|e| e.to_string());
    let found = build(json!(["title", "note"])).unwrap_err();
    assert!(found.contains("needs the guard field \"pages\" among the list columns"), "{found}");
    build(json!(["title", "pages"])).unwrap();
}

#[tokio::test]
async fn the_list_names_the_editable_fields_and_flags_each_row() {
    let w = world(book(json!({"can": {"change": {"t": "cmp", "op": "ne", "l": {"t": "col", "path": [], "name": "title"}, "r": {"t": "text", "value": "Two"}}}}))).await;
    let (status, out) = w.send("GET", "/resources", json!({})).await;
    assert_eq!(status, 200, "{out}");
    let book = out["resources"].as_array().unwrap().iter().find(|r| r["name"] == "Book").unwrap();
    assert_eq!(book["editable"], json!(["title", "pages"]));
    let (status, out) = w.send("GET", "/resources/Book", json!({})).await;
    assert_eq!(status, 200, "{out}");
    let change: Vec<&Value> = out["flags"].as_array().unwrap().iter().map(|f| &f["change"]).collect();
    assert_eq!(change, [&json!(true), &json!(false), &json!(true), &json!(true)], "{out}");
}

#[tokio::test]
async fn a_save_writes_several_rows_and_one_audit_row_each_with_the_same_request_id() {
    let w = world(book(json!({}))).await;
    let (status, out) = w
        .save(json!([
            row("1", json!({"title": "Uno", "pages": 10}), json!({"title": "One", "pages": 1})),
            row("2", json!({"title": "Two"}), json!({"title": "Two"})),
            row("4", json!({"pages": "40"}), json!({"pages": 4})),
        ]))
        .await;
    assert_eq!(status, 200, "{out}");
    assert_eq!(out["keys"], json!(["1", "2", "4"]));
    assert_eq!(out["changed"], json!({"1": ["pages", "title"], "2": [], "4": ["pages"]}));
    assert_eq!(w.titles().await, ["Uno:10", "Two:2", "Three:3", "Four:40"]);
    let audit = w.audit().await;
    assert_eq!(audit.iter().map(|a| a.1.as_str()).collect::<Vec<_>>(), ["1", "4"], "an unchanged row gets no audit row");
    assert_eq!(audit[0].2["diff"]["title"], json!({"from": "One", "to": "Uno"}));
    assert!(audit[0].2["request_id"].is_string() && audit[0].2["request_id"] == audit[1].2["request_id"]);
}

#[tokio::test]
async fn a_wrong_value_in_one_row_writes_nothing_and_names_the_row() {
    let w = world(book(json!({}))).await;
    let (status, out) = w.save(json!([row("1", json!({"title": "Uno"}), json!({"title": "One"})), row("2", json!({"title": "x".repeat(11)}), json!({"title": "Two"}))])).await;
    assert_eq!(status, 400, "{out}");
    assert_eq!(out["rows"]["1"]["fields"]["title"], json!(["Must have at most 10 characters"]));
    assert!(out["rows"].get("0").is_none());
    assert_eq!(w.titles().await, ["One:1", "Two:2", "Three:3", "Four:4"]);
    assert!(w.audit().await.is_empty());
}

#[tokio::test]
async fn a_unique_clash_in_the_second_row_rolls_back_the_first() {
    let w = world(book(json!({}))).await;
    let (status, out) = w.save(json!([row("1", json!({"title": "Uno"}), json!({"title": "One"})), row("2", json!({"title": "Four"}), json!({"title": "Two"}))])).await;
    assert_eq!(status, 400, "{out}");
    assert!(out["rows"]["1"]["fields"]["title"].is_array(), "{out}");
    assert_eq!(w.titles().await, ["One:1", "Two:2", "Three:3", "Four:4"]);
    assert!(w.audit().await.is_empty());
}

#[tokio::test]
async fn a_changed_row_is_a_409_that_writes_nothing_and_names_the_row() {
    let w = world(book(json!({}))).await;
    w.conn.batch("UPDATE \"Book\" SET title = 'Dos' WHERE id = 2".into()).await.unwrap();
    let (status, out) = w.save(json!([row("1", json!({"title": "Uno"}), json!({"title": "One"})), row("2", json!({"title": "Zwei"}), json!({"title": "Two"}))])).await;
    assert_eq!(status, 409, "{out}");
    assert_eq!(out["conflicts"], json!({"1": {"title": {"loaded": "Two", "current": "Dos"}}}));
    assert_eq!(w.titles().await, ["One:1", "Dos:2", "Three:3", "Four:4"]);
}

#[tokio::test]
async fn a_row_out_of_reach_or_gone_writes_nothing() {
    let mine = json!({"filter": {"t": "cmp", "op": "eq", "l": {"t": "col", "path": [], "name": "author_id"}, "r": {"t": "param", "i": 0}}, "slots": ["user"]});
    let w = world(book(json!({"queryset": mine}))).await;
    let (status, out) = w.save(json!([row("1", json!({"title": "Uno"}), json!({"title": "One"})), row("3", json!({"title": "Drei"}), json!({"title": "Three"}))])).await;
    assert_eq!((status, &out["error"]["kind"]), (403, &json!("Forbidden")));
    let (status, _) = w.save(json!([row("1", json!({"title": "Uno"}), json!({"title": "One"})), row("99", json!({"title": "Neun"}), json!({"title": "Nine"}))])).await;
    assert_eq!(status, 404);
    assert_eq!(w.titles().await, ["One:1", "Two:2", "Three:3", "Four:4"]);
    let rule = json!({"change": {"t": "cmp", "op": "ne", "l": {"t": "col", "path": [], "name": "title"}, "r": {"t": "text", "value": "Two"}}});
    let w = world(book(json!({"can": rule}))).await;
    let (status, _) = w.save(json!([row("1", json!({"title": "Uno"}), json!({"title": "One"})), row("2", json!({"title": "Zwei"}), json!({"title": "Two"}))])).await;
    assert_eq!(status, 403);
    assert_eq!(w.titles().await, ["One:1", "Two:2", "Three:3", "Four:4"]);
}

#[tokio::test]
async fn a_field_outside_editable_a_missing_loaded_value_and_a_big_save_are_400() {
    let w = world(book(json!({}))).await;
    for rows in [
        json!([row("1", json!({"note": "z"}), json!({"note": "a"}))]),
        json!([row("1", json!({"title": "Uno"}), json!({}))]),
        json!([row("1", json!({"title": "Uno"}), json!({"title": "One"})), row("1", json!({"title": "Uno"}), json!({"title": "One"}))]),
        json!([]),
        Value::Array((0..201).map(|i| row(&i.to_string(), json!({"title": "x"}), json!({"title": "y"}))).collect()),
    ] {
        let (status, out) = w.save(rows.clone()).await;
        assert_eq!((status, &out["error"]["kind"]), (400, &json!("BadRequest")), "{rows}: {out}");
    }
    assert_eq!(w.titles().await, ["One:1", "Two:2", "Three:3", "Four:4"]);
}

fn service() -> Value {
    json!({"service": {"update": {}}})
}

#[tokio::test]
async fn a_service_is_called_once_for_all_rows_and_the_unchanged_rows_stay_out() {
    let w = world(book(service())).await;
    let (status, out) = w
        .save(json!([
            row("1", json!({"title": "Uno"}), json!({"title": "One"})),
            row("2", json!({"title": "Two"}), json!({"title": "Two"})),
            row("4", json!({"title": "Cuatro"}), json!({"title": "Four"})),
        ]))
        .await;
    assert_eq!(status, 200, "{out}");
    assert_eq!(out["changed"], json!({"1": ["title"], "2": [], "4": ["title"]}));
    let seen = w.host.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!((seen[0].0.as_str(), &seen[0].1["op"]), ("service.Book.update", &json!("update_rows")));
    // The Python and TypeScript hosts pick the writer by `model` of the call, not of a row.
    assert_eq!((&seen[0].1["model"], &seen[0].1["resource"]), (&json!("Book"), &json!("Book")));
    let sent: Vec<&Value> = seen[0].1["rows"].as_array().unwrap().iter().map(|r| &r["key"]["id"]).collect();
    assert_eq!(sent, [&json!(1), &json!(4)]);
    assert_eq!(w.titles().await, ["Uno:1", "Two:2", "Three:3", "Cuatro:4"]);
    assert_eq!(w.audit().await.len(), 2);
}

#[tokio::test]
async fn the_row_that_the_host_refuses_is_named_by_its_place_in_the_request() {
    let w = world(book(service())).await;
    w.host.answer.lock().unwrap().replace(json!({"ok": false, "kind": "invalid", "index": 1, "fields": {"title": ["No."]}, "form": ["Stop."]}));
    let rows = json!([row("1", json!({"title": "Uno"}), json!({"title": "One"})), row("2", json!({"title": "Two"}), json!({"title": "Two"})), row("4", json!({"title": "Cuatro"}), json!({"title": "Four"}))]);
    let (status, out) = w.save(rows.clone()).await;
    assert_eq!(status, 400, "{out}");
    assert_eq!(out["rows"], json!({"2": {"fields": {"title": ["No."]}, "form": ["Stop."]}}));
    w.host.answer.lock().unwrap().replace(json!({"ok": false, "kind": "not_found", "index": 0}));
    assert_eq!(w.save(rows.clone()).await.0, 404);
    w.host.answer.lock().unwrap().replace(json!({"ok": false, "kind": "error", "index": 0}));
    assert_eq!(w.save(rows).await.0, 500);
    assert!(w.audit().await.is_empty());
}

#[tokio::test]
async fn a_host_conflict_names_the_row_and_the_stored_value() {
    let w = world(book(service())).await;
    w.conn.batch("UPDATE \"Book\" SET title = 'Dos' WHERE id = 2".into()).await.unwrap();
    w.host.answer.lock().unwrap().replace(json!({"ok": false, "kind": "conflict", "index": 1}));
    let (status, out) = w.save(json!([row("1", json!({"title": "Uno"}), json!({"title": "One"})), row("2", json!({"title": "Zwei"}), json!({"title": "Two"}))])).await;
    assert_eq!(status, 409, "{out}");
    assert_eq!(out["conflicts"], json!({"1": {"title": {"loaded": "Two", "current": "Dos"}}}));
}

#[tokio::test]
async fn a_service_gets_the_rows_in_key_order_and_the_refusal_names_the_request_place() {
    let w = world(book(service())).await;
    let rows = json!([row("4", json!({"title": "Cuatro"}), json!({"title": "Four"})), row("1", json!({"title": "Uno"}), json!({"title": "One"}))]);
    assert_eq!(w.save(rows.clone()).await.0, 200);
    let sent = |w: &World| -> Vec<Value> { w.host.seen.lock().unwrap().last().unwrap().1["rows"].as_array().unwrap().iter().map(|r| r["key"]["id"].clone()).collect() };
    assert_eq!(sent(&w), [json!(1), json!(4)]);
    let w = world(book(service())).await;
    w.host.answer.lock().unwrap().replace(json!({"ok": false, "kind": "invalid", "index": 0, "fields": {"title": ["No."]}}));
    let (status, out) = w.save(rows).await;
    assert_eq!(status, 400, "{out}");
    assert_eq!(out["rows"].as_object().unwrap().keys().collect::<Vec<_>>(), ["1"]);
}

#[tokio::test]
async fn a_service_save_checks_loaded_of_an_unchanged_row_as_the_plain_path_does() {
    let w = world(book(service())).await;
    let (status, out) = w.save(json!([row("1", json!({"title": "Uno"}), json!({"title": "One"})), row("2", json!({"title": "Two"}), json!({"title": "Zwei"}))])).await;
    assert_eq!(status, 409, "{out}");
    assert_eq!(out["conflicts"], json!({"1": {"title": {"loaded": "Zwei", "current": "Two"}}}));
    assert!(w.host.seen.lock().unwrap().is_empty());
    assert_eq!(w.titles().await, ["One:1", "Two:2", "Three:3", "Four:4"]);
}

#[tokio::test]
async fn a_host_conflict_that_no_longer_differs_still_names_the_compared_fields() {
    let w = world(book(service())).await;
    w.host.answer.lock().unwrap().replace(json!({"ok": false, "kind": "conflict", "index": 0}));
    let (status, out) = w.save(json!([row("1", json!({"title": "Uno"}), json!({"title": "One"}))])).await;
    assert_eq!(status, 409, "{out}");
    assert_eq!(out["conflicts"], json!({"0": {"title": {"loaded": "One", "current": "One"}}}));
}
