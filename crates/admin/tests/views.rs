//! A view field: the host shows stored bytes as JSON (or as text) in the list and the detail, and the admin
//! carries the JSON of a write and of a conflict. SQLite in memory with a fake host that stores JSON as bytes.
use std::sync::{Arc, Mutex};

use base64::{engine::general_purpose::STANDARD, Engine};
use monorm_admin::{call_full, AuthConfig, BoxFuture, BridgeError, CallContext, HostBridge, Security, SecurityOptions, Site};
use monorm_core::{dsl, migrate, schema::Schema};
use monorm_engine::db::{self, Executor};
use serde_json::{json, Value};

const ADMIN: &str = include_str!("../../../apps/admin/schema.prisma");
const MODELS: &str = r#"
datasource db {
  provider = "sqlite"
}

model Config {
  id      Int    @id
  name    String
  payload Bytes?
  raw     Bytes?
}
"#;

/// The stored JSON `{"n": 1}` as the host stores a message: bytes.
const STORED: &str = "7b226e223a317d";

struct Host {
    seen: Mutex<Vec<(String, Value)>>,
    answer: Mutex<Option<Value>>,
}

impl HostBridge for Host {
    fn call(&self, name: &str, payload: Value, _ctx: CallContext) -> BoxFuture<Result<Value, BridgeError>> {
        self.seen.lock().unwrap().push((name.to_owned(), payload.clone()));
        let canned = self.answer.lock().unwrap().clone();
        let name = name.to_owned();
        Box::pin(async move {
            if name == "admin.view" {
                let shown: Vec<Value> = payload["values"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| match v.as_str() {
                        None => Value::Null,
                        Some(b64) => {
                            let bytes = STANDARD.decode(b64).unwrap();
                            if payload["field"] == "payload" { serde_json::from_slice(&bytes).unwrap() } else { json!(format!("{} bytes", bytes.len())) }
                        }
                    })
                    .collect();
                return Ok(Value::Array(shown));
            }
            canned.ok_or_else(|| BridgeError::Bridge(format!("unknown host function {name}")))
        })
    }
}

struct World {
    site: Arc<Site>,
    conn: Arc<dyn Executor>,
    host: Arc<Host>,
}

fn schema() -> Arc<Schema> {
    Arc::new(dsl::check(dsl::compile(&format!("{MODELS}\n{ADMIN}"), None).unwrap()).unwrap().1)
}

fn config(views: Value) -> Value {
    json!({"resources": [{"model": "Config", "host_behavior": true, "views": views}]})
}

async fn world(config: Value) -> World {
    let schema = schema();
    let conn = db::connect("sqlite://:memory:", 1).await.unwrap();
    for step in migrate::plan(&schema, &Default::default()).unwrap().up {
        conn.batch(step.sql).await.unwrap();
    }
    conn.batch(format!("INSERT INTO \"Config\" (id, name, payload, raw) VALUES (1, 'a', X'{STORED}', X'7878'), (2, 'b', NULL, NULL)")).await.unwrap();
    let site = Arc::new(Site::build(schema, &config.to_string()).unwrap());
    World { site, conn, host: Arc::new(Host { seen: Mutex::new(Vec::new()), answer: Mutex::new(None) }) }
}

fn build_error(config: Value) -> String {
    Site::build(schema(), &config.to_string()).err().map_or_else(|| "built".into(), |e| e.to_string())
}

impl World {
    async fn send(&self, method: &str, uri: &str, body: Value) -> (u16, Value) {
        let auth = AuthConfig { security: Security::new(&[7u8; 32], SecurityOptions::default()).unwrap(), bridge: self.host.clone() };
        let (status, _, out) = call_full(self.site.clone(), self.conn.clone(), method, &format!("/admin/api{uri}"), Some(json!({"key": 7, "is_superuser": true})), Some(auth), Some(("application/json", body.to_string().into_bytes()))).await;
        (status, serde_json::from_slice(&out).unwrap_or(Value::Null))
    }

    fn views_asked(&self) -> Vec<Value> {
        self.host.seen.lock().unwrap().iter().filter(|(n, _)| n == "admin.view").map(|(_, p)| p.clone()).collect()
    }
}

#[tokio::test]
async fn the_list_and_the_detail_show_a_view_field_as_the_host_converts_it() {
    let w = world(config(json!({"payload": "json", "raw": "text"}))).await;
    let (status, list) = w.send("GET", "/resources/Config", json!(null)).await;
    assert_eq!(status, 200, "{list}");
    let at = |n: &str| list["columns"].as_array().unwrap().iter().position(|c| c["label"] == n).unwrap();
    assert_eq!((&list["rows"][0][at("payload")], &list["rows"][0][at("raw")]), (&json!({"n": 1}), &json!("2 bytes")));
    assert_eq!((&list["rows"][1][at("payload")], &list["rows"][1][at("raw")]), (&Value::Null, &Value::Null));
    // One call for each view column and page, with the null cells in the list.
    let asked = w.views_asked();
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert_eq!(asked[0], json!({"model": "Config", "field": "payload", "values": [STANDARD.encode(b"{\"n\":1}"), null]}));

    let (status, detail) = w.send("GET", "/resources/Config/1", json!(null)).await;
    assert_eq!(status, 200, "{detail}");
    let d = |n: &str| detail["values"][detail["fields"].as_array().unwrap().iter().position(|c| c["label"] == n).unwrap()].clone();
    assert_eq!((d("payload"), d("raw")), (json!({"n": 1}), json!("2 bytes")));
}

#[tokio::test]
async fn a_text_view_is_read_only_in_the_form_and_a_json_view_is_not() {
    let w = world(config(json!({"payload": "json", "raw": "text"}))).await;
    let (status, site) = w.send("GET", "/resources", json!(null)).await;
    assert_eq!(status, 200, "{site}");
    let resource = site["resources"].as_array().unwrap().iter().find(|r| r["name"] == "Config").unwrap();
    let field = |n: &str| resource["form"].as_array().unwrap().iter().find(|f| f["name"] == n).unwrap().clone();
    assert_eq!(field("raw")["readonly"], json!(true), "{resource}");
    assert_ne!(field("payload")["readonly"], json!(true), "{resource}");
    let (status, out) = w.send("PATCH", "/resources/Config/1", json!({"set": {"raw": "x"}, "loaded": {"raw": "2 bytes"}})).await;
    assert_eq!((status, &out["fields"]["raw"]), (422, &json!(["This field is read-only."])), "{out}");
}

#[tokio::test]
async fn a_json_view_sends_its_json_to_the_host_and_a_conflict_compares_the_json_by_value() {
    let w = world(config(json!({"payload": "json"}))).await;
    // The host stores `{"n": 1}`; the form loaded `{"n": 1.0}`, the same value.
    *w.host.answer.lock().unwrap() = Some(json!({"ok": false, "kind": "conflict"}));
    let (status, out) = w.send("PATCH", "/resources/Config/1", json!({"set": {"payload": {"n": 2}}, "loaded": {"payload": {"n": 1.0}}})).await;
    assert_eq!(status, 409, "{out}");
    let (_, write) = w.host.seen.lock().unwrap().iter().find(|(n, _)| n == "admin.write").cloned().expect("the host writes");
    assert_eq!((&write["values"]["payload"], &write["loaded"]["payload"]), (&json!({"n": 2}), &json!({"n": 1.0})));
    assert!(out["conflicts"].get("payload").is_none(), "1 and 1.0 are the same value: {out}");

    let (status, out) = w.send("PATCH", "/resources/Config/1", json!({"set": {"payload": {"n": 2}}, "loaded": {"payload": {"n": 5}}})).await;
    assert_eq!(status, 409);
    assert_eq!(out["conflicts"]["payload"], json!({"loaded": {"n": 5}, "current": {"n": 1}}));
}

#[tokio::test]
async fn a_field_error_of_the_host_for_a_view_field_is_a_422() {
    let w = world(config(json!({"payload": "json"}))).await;
    *w.host.answer.lock().unwrap() = Some(json!({"ok": false, "kind": "invalid", "fields": {"payload": ["Not a Note."]}, "form": []}));
    let (status, out) = w.send("PATCH", "/resources/Config/1", json!({"set": {"payload": {"nope": 1}}, "loaded": {"payload": {"n": 1}}})).await;
    assert_eq!((status, &out["fields"]["payload"]), (422, &json!(["Not a Note."])));
}

#[test]
fn a_view_needs_the_host_a_bytes_field_a_known_mode_and_the_detail_page() {
    let named = |views: Value| json!({"resources": [{"model": "Config", "host_behavior": true, "views": views}]});
    assert!(build_error(json!({"resources": [{"model": "Config", "views": {"payload": "json"}}]})).contains("needs host_behavior"));
    assert!(build_error(named(json!({"name": "json"}))).contains("not a Bytes field"));
    assert!(build_error(named(json!({"nope": "json"}))).contains("not a field"));
    assert!(build_error(named(json!({"payload": "yaml"}))).contains("not json or text"));
    let editable = json!({"resources": [{"model": "Config", "host_behavior": true, "views": {"payload": "json"}, "list": {"editable": ["payload"]}}]});
    assert!(build_error(editable).contains("edit it on the detail page"));
}

#[test]
fn same_json_compares_numbers_by_value_and_the_rest_by_parts() {
    use monorm_admin::json::same_json;
    assert!(same_json(&json!({"a": [1, {"b": 2}]}), &json!({"a": [1.0, {"b": 2.0}]})));
    assert!(!same_json(&json!({"a": 1}), &json!({"a": 1, "b": null})));
    assert!(!same_json(&json!([1, 2]), &json!([2, 1])));
    assert!(!same_json(&json!("1"), &json!(1)));
}
