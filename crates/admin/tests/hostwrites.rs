//! Writes through the host bridge: a protected model that the resource opens, services, inputs, and the
//! 404, 409, 422 and 500 answers of the host, on SQLite in memory with a fake host.
use std::sync::{Arc, Mutex};

use monorm_admin::{call_full, AuthConfig, BoxFuture, BridgeError, CallContext, HostBridge, Security, SecurityOptions, Site};
use monorm_core::{dsl, migrate, schema::Schema};
use monorm_engine::db::{self, Executor};
use serde_json::{json, Value};

const ADMIN: &str = include_str!("../../../apps/admin/schema.prisma");
const MODELS: &str = r#"
datasource db {
  provider = "sqlite"
}

model Book {
  id    Int    @id @default(autoincrement())
  title String @db.VarChar(20)
  pages Int    @default(0)
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

/// The host answers by function name: it writes the row itself, as host code would, or answers a canned envelope.
struct Host {
    seen: Mutex<Vec<(String, Value)>>,
    conn: Arc<dyn Executor>,
    answer: Mutex<Option<Value>>,
}

impl HostBridge for Host {
    fn call(&self, name: &str, payload: Value, ctx: CallContext) -> BoxFuture<Result<Value, BridgeError>> {
        assert!(ctx.tx.is_none(), "the service owns its transactions: the call holds no transaction");
        self.seen.lock().unwrap().push((name.to_owned(), payload.clone()));
        let canned = self.answer.lock().unwrap().clone();
        let conn = self.conn.clone();
        let name = name.to_owned();
        Box::pin(async move {
            if let Some(answer) = canned {
                return Ok(answer);
            }
            match name.as_str() {
                "admin.write" => {
                    conn.batch(format!("INSERT INTO \"Locked\" (x) VALUES ({})", payload["values"]["x"])).await.unwrap();
                    Ok(json!({"ok": true, "key": {"id": 1}}))
                }
                "service.Book.update" => {
                    conn.batch(format!("UPDATE \"Book\" SET title = '{}' WHERE id = 1", payload["values"]["title"].as_str().unwrap())).await.unwrap();
                    Ok(json!({"ok": true}))
                }
                "service.Book.create" => {
                    conn.batch(format!("INSERT INTO \"Book\" (title) VALUES ('{}')", payload["values"]["title"].as_str().unwrap())).await.unwrap();
                    Ok(json!({"ok": true, "key": {"id": 2}}))
                }
                "service.Book.delete" => {
                    conn.batch("DELETE FROM \"Book\" WHERE id = 1".into()).await.unwrap();
                    Ok(json!({"ok": true}))
                }
                other => Err(BridgeError::Bridge(format!("unknown host function {other}"))),
            }
        })
    }
}

struct World {
    site: Arc<Site>,
    conn: Arc<dyn Executor>,
    host: Arc<Host>,
}

fn book_with_service() -> Value {
    json!({"model": "Book", "service": {"create": {}, "update": {}, "delete": {}}, "inputs": [{"name": "reason", "type": "text", "required": true}, {"name": "notify", "type": "checkbox"}]})
}

async fn world(config: Value) -> World {
    let schema = schema();
    let conn = db::connect("sqlite://:memory:", 1).await.unwrap();
    for step in migrate::plan(&schema, &Default::default()).unwrap().up {
        conn.batch(step.sql).await.unwrap();
    }
    conn.batch("INSERT INTO \"Book\" (id, title, pages) VALUES (1, 'First', 10)".into()).await.unwrap();
    let site = Arc::new(Site::build(schema, &config.to_string()).unwrap());
    let host = Arc::new(Host { seen: Mutex::new(Vec::new()), conn: conn.clone(), answer: Mutex::new(None) });
    World { site, conn, host }
}

fn build_error(config: Value) -> String {
    match Site::build(schema(), &config.to_string()) {
        Ok(_) => String::from("built"),
        Err(e) => e.to_string(),
    }
}

fn auth(host: Arc<Host>) -> AuthConfig {
    AuthConfig { security: Security::new(&[7u8; 32], SecurityOptions::default()).unwrap(), bridge: host }
}

impl World {
    async fn send(&self, method: &str, uri: &str, body: Value) -> (u16, Value) {
        self.send_with(method, uri, body, true).await
    }

    async fn send_with(&self, method: &str, uri: &str, body: Value, bridge: bool) -> (u16, Value) {
        let auth = bridge.then(|| auth(self.host.clone()));
        let (status, _, out) = call_full(self.site.clone(), self.conn.clone(), method, &format!("/admin/api{uri}"), Some(json!({"key": 7, "is_superuser": true})), auth, Some(("application/json", body.to_string().into_bytes()))).await;
        (status, serde_json::from_slice(&out).unwrap_or(Value::Null))
    }

    fn answer(&self, v: Value) {
        *self.host.answer.lock().unwrap() = Some(v);
    }

    fn seen(&self) -> Vec<(String, Value)> {
        self.host.seen.lock().unwrap().clone()
    }

    async fn rows(&self, sql: &str) -> Vec<Vec<Option<String>>> {
        self.conn.query_text(sql.to_string()).await.unwrap()
    }

    async fn audit(&self) -> Vec<(String, Value)> {
        self.rows("SELECT action, changes FROM \"AdminLogEntry\" ORDER BY id").await.into_iter().map(|r| (r[0].clone().unwrap(), serde_json::from_str(r[1].as_deref().unwrap_or("null")).unwrap_or(Value::Null))).collect()
    }
}

#[tokio::test]
async fn a_protected_model_stays_closed_until_the_resource_opens_it() {
    let w = world(json!({"resources": [{"model": "Locked"}]})).await;
    let (status, out) = w.send("POST", "/resources/Locked", json!({"set": {"x": 1}})).await;
    assert_eq!((status, &out["error"]["kind"]), (403, &json!("ReadOnly")));
    assert!(w.seen().is_empty());
}

#[tokio::test]
async fn an_open_protected_model_writes_through_the_host_and_the_audit_row_records_the_opt_in() {
    let w = world(json!({"resources": [{"model": "Locked", "allow_protected_writes": true}]})).await;
    let (status, out) = w.send("POST", "/resources/Locked", json!({"set": {"x": 5}})).await;
    assert_eq!(status, 201, "{out}");
    assert_eq!(out["key"], "1");
    assert_eq!(out["values"], json!([1, 5]));
    let seen = w.seen();
    assert_eq!(seen[0].0, "admin.write");
    assert_eq!((&seen[0].1["op"], &seen[0].1["values"], &seen[0].1["allow_protected"], &seen[0].1["user"]["key"]), (&json!("create"), &json!({"x": 5}), &json!(true), &json!(7)));
    let audit = w.audit().await;
    assert_eq!(audit.len(), 1);
    assert_eq!((audit[0].0.as_str(), &audit[0].1["allow_protected_writes"]), ("create", &json!(true)));
}

#[tokio::test]
async fn a_write_that_needs_host_code_is_a_403_when_the_admin_has_no_host() {
    let w = world(json!({"resources": [{"model": "Locked", "allow_protected_writes": true}]})).await;
    let (status, out) = w.send_with("POST", "/resources/Locked", json!({"set": {"x": 5}}), false).await;
    assert_eq!((status, &out["error"]["kind"]), (403, &json!("ReadOnly")));
    assert!(w.rows("SELECT id FROM \"Locked\"").await.is_empty());
}

#[tokio::test]
async fn a_create_with_the_plain_host_writer_still_needs_the_required_fields_but_a_service_create_may_fill_them() {
    let w = world(json!({"resources": [{"model": "Locked", "allow_protected_writes": true}, book_with_service()]})).await;
    let (status, out) = w.send("POST", "/resources/Locked", json!({"set": {}})).await;
    assert_eq!((status, &out["fields"]["x"]), (422, &json!(["This field is required."])));
    assert!(w.seen().is_empty());
    w.answer(json!({"ok": true, "key": {"id": 2}}));
    let (status, out) = w.send("POST", "/resources/Book", json!({"set": {}, "inputs": {"reason": "seed"}})).await;
    assert_eq!(status, 201, "{out}");
    assert_eq!(w.seen()[0].0, "service.Book.create");
}

#[tokio::test]
async fn the_form_metadata_is_read_only_for_a_host_write_when_the_admin_has_no_host() {
    let w = world(json!({"resources": [{"model": "Locked", "allow_protected_writes": true}, {"model": "Book"}]})).await;
    let writable = |meta: &Value, name: &str| meta["resources"].as_array().unwrap().iter().find(|r| r["name"] == name).map(|r| (r["writable"].clone(), r["read_only_reason"].clone())).unwrap();
    let (_, with_host) = w.send_with("GET", "/resources", json!({}), true).await;
    assert_eq!(writable(&with_host, "Locked").0, true);
    let (_, none) = w.send_with("GET", "/resources", json!({}), false).await;
    let (locked, reason) = writable(&none, "Locked");
    assert_eq!((locked, reason.as_str().unwrap().contains("no host")), (json!(false), true));
    assert_eq!(writable(&none, "Book").0, true);
}

#[tokio::test]
async fn a_service_update_gets_the_key_the_values_the_loaded_values_the_inputs_and_the_user() {
    let w = world(json!({"resources": [book_with_service()]})).await;
    let body = json!({"set": {"title": "Second"}, "loaded": {"title": "First"}, "inputs": {"reason": "typo"}});
    let (status, out) = w.send("PATCH", "/resources/Book/1", body).await;
    assert_eq!(status, 200, "{out}");
    assert_eq!(out["changed"], json!(["title"]));
    let (name, payload) = &w.seen()[0];
    assert_eq!(name, "service.Book.update");
    assert_eq!((&payload["key"], &payload["values"], &payload["loaded"], &payload["inputs"], &payload["user"]["key"]), (&json!({"id": 1}), &json!({"title": "Second"}), &json!({"title": "First"}), &json!({"reason": "typo", "notify": false}), &json!(7)));
    let audit = w.audit().await;
    assert_eq!(audit[0].1["inputs"], json!({"reason": "typo", "notify": false}));
    assert_eq!(audit[0].1["diff"]["title"], json!({"from": "First", "to": "Second"}));
}

#[tokio::test]
async fn a_service_create_and_delete_run_in_the_host() {
    let w = world(json!({"resources": [book_with_service()]})).await;
    let (status, out) = w.send("POST", "/resources/Book", json!({"set": {"title": "New"}, "inputs": {"reason": "seed"}})).await;
    assert_eq!((status, &out["key"], &out["values"][1]), (201, &json!("2"), &json!("New")));
    let (status, _) = w.send("DELETE", "/resources/Book/1", json!({"inputs": {"reason": "gone"}})).await;
    assert_eq!(status, 200);
    assert_eq!(w.rows("SELECT id FROM \"Book\"").await.len(), 1);
    assert_eq!(w.audit().await.iter().map(|a| a.0.clone()).collect::<Vec<_>>(), ["create", "delete"]);
}

#[tokio::test]
async fn an_input_problem_is_a_422_and_the_host_is_not_called() {
    let w = world(json!({"resources": [book_with_service()]})).await;
    let (status, out) = w.send("PATCH", "/resources/Book/1", json!({"set": {"title": "Second"}, "loaded": {"title": "First"}, "inputs": {"other": 1}})).await;
    assert_eq!(status, 422);
    assert_eq!(out["fields"]["reason"], json!(["This field is required."]));
    assert_eq!(out["fields"]["other"], json!(["This input does not exist."]));
    assert!(w.seen().is_empty());
}

#[tokio::test]
async fn the_envelope_of_the_host_is_a_404_a_409_a_422_or_a_500() {
    let w = world(json!({"resources": [book_with_service()]})).await;
    let body = json!({"set": {"title": "Second"}, "loaded": {"title": "First"}, "inputs": {"reason": "x"}});
    w.answer(json!({"ok": false, "kind": "not_found"}));
    assert_eq!(w.send("PATCH", "/resources/Book/1", body.clone()).await.0, 404);
    w.answer(json!({"ok": false, "kind": "conflict"}));
    w.conn.batch("UPDATE \"Book\" SET title = 'Other' WHERE id = 1".into()).await.unwrap();
    let (status, out) = w.send("PATCH", "/resources/Book/1", body.clone()).await;
    assert_eq!(status, 409);
    assert_eq!(out["current"]["title"], "Other");
    assert_eq!(out["conflicts"]["title"], json!({"loaded": "First", "current": "Other"}));
    w.answer(json!({"ok": false, "kind": "invalid", "fields": {"title": ["Too short."]}, "form": ["Bad."]}));
    let (status, out) = w.send("PATCH", "/resources/Book/1", body.clone()).await;
    assert_eq!((status, &out["fields"]["title"], &out["form"]), (422, &json!(["Too short."]), &json!(["Bad."])));
    w.answer(json!({"ok": false, "kind": "error"}));
    let (status, out) = w.send("PATCH", "/resources/Book/1", body).await;
    assert_eq!((status, &out["error"]["kind"]), (500, &json!("ServerError")));
    assert!(w.audit().await.is_empty());
}

#[tokio::test]
async fn a_host_that_wrote_the_audit_row_leaves_the_admin_to_write_none() {
    let w = world(json!({"resources": [book_with_service()]})).await;
    w.answer(json!({"ok": true, "audited": true}));
    let (status, _) = w.send("PATCH", "/resources/Book/1", json!({"set": {"title": "Same"}, "loaded": {"title": "First"}, "inputs": {"reason": "x"}})).await;
    assert_eq!(status, 200);
    assert!(w.audit().await.is_empty());
}

#[tokio::test]
async fn a_secret_input_reaches_the_host_as_given_and_the_audit_row_stores_a_mask() {
    let book = json!({"model": "Book", "service": {"update": {}}, "inputs": [{"name": "pw", "type": "text", "secret": true}, {"name": "reason", "type": "text"}, {"name": "empty", "type": "text", "secret": true}]});
    let w = world(json!({"resources": [book]})).await;
    let body = json!({"set": {"title": "Second"}, "loaded": {"title": "First"}, "inputs": {"pw": "hunter2", "reason": "typo"}});
    let (status, out) = w.send("PATCH", "/resources/Book/1", body).await;
    assert_eq!(status, 200, "{out}");
    let (_, payload) = &w.seen()[0];
    assert_eq!(payload["inputs"]["pw"], json!("hunter2"));
    assert_eq!(payload["audit_inputs"]["pw"], json!("***"));
    assert_eq!(payload["audit_inputs"]["reason"], json!("typo"));
    let audit = w.audit().await;
    assert_eq!(audit[0].1["inputs"]["pw"], json!("***"));
    assert_eq!(audit[0].1["inputs"]["reason"], json!("typo"));
    assert!(!audit[0].1.to_string().contains("hunter2"));
}

#[tokio::test]
async fn the_admin_fills_the_blank_label_of_a_row_the_host_wrote_and_keeps_a_label_that_is_set() {
    let w = world(json!({"resources": [book_with_service()]})).await;
    w.conn.batch("INSERT INTO \"AdminLogEntry\" (user_key, action, resource, record_key, record_label) VALUES ('7', 'service', 'Book', '1', ''), ('7', 'service', 'Book', '2', 'Kept'), ('7', 'service', 'Book', '1', 'Older')".into()).await.unwrap();
    w.answer(json!({"ok": true, "audited": true}));
    let (status, _) = w.send("PATCH", "/resources/Book/1", json!({"set": {"title": "Same"}, "loaded": {"title": "First"}, "inputs": {"reason": "x"}})).await;
    assert_eq!(status, 200);
    let labels = w.rows("SELECT record_key, record_label FROM \"AdminLogEntry\" ORDER BY id").await;
    let labels: Vec<_> = labels.into_iter().map(|r| (r[0].clone().unwrap(), r[1].clone().unwrap())).collect();
    assert_eq!(labels, [("1".into(), "First".into()), ("2".into(), "Kept".into()), ("1".into(), "Older".into())]);
}

#[tokio::test]
async fn the_form_metadata_lists_the_inputs() {
    let w = world(json!({"resources": [book_with_service()]})).await;
    let (_, meta) = w.send("GET", "/resources", json!({})).await;
    let book = meta["resources"].as_array().unwrap().iter().find(|r| r["name"] == "Book").cloned().unwrap();
    assert_eq!(book["inputs"][0]["name"], "reason");
    assert_eq!(book["inputs"][0]["required"], true);
    assert_eq!(book["host_write"], true);
}

#[test]
fn an_input_without_a_service_is_a_build_error() {
    let message = build_error(json!({"resources": [{"model": "Book", "inputs": [{"name": "reason"}]}]}));
    assert!(message.contains("inputs need a service"), "{message}");
    assert_eq!(build_error(json!({"resources": [book_with_service()]})), "built");
}

#[test]
fn a_sync_service_must_be_threaded() {
    let message = build_error(json!({"resources": [{"model": "Book", "service": {"update": {"sync": true}}}]}));
    assert!(message.contains("sync function") && message.contains("ctx.run"), "{message}");
    assert_eq!(build_error(json!({"resources": [{"model": "Book", "service": {"update": {"sync": true, "threaded": true}}}]})), "built");
}

#[test]
fn a_wrong_input_declaration_is_a_build_error() {
    let with = |input: Value| build_error(json!({"resources": [{"model": "Book", "service": {"update": {}}, "inputs": [input]}]}));
    assert!(with(json!({"name": "title"})).contains("name of a field"));
    assert!(with(json!({"name": "a b"})).contains("letters, digits"));
    assert!(with(json!({"name": "a", "type": "tree"})).contains("not one of"));
    assert!(with(json!({"name": "a", "type": "select"})).contains("choices"));
}
