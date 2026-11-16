//! Actions: the placements, the query set, `applies_to`, the second confirmation, bulk delete, the four
//! results and the summary row of the admin log, on SQLite in memory with a fake host.
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
  id     Int    @id @default(autoincrement())
  title  String @db.VarChar(20)
  pages  Int    @default(0)
  status String @default("draft")
  author_id Int?
  author Author? @relation(fields: [author_id], references: [id])
}

model Author {
  id    Int    @id @default(autoincrement())
  name  String
  books Book[]
}

model Vault {
  id   Int    @id @default(autoincrement())
  note String

  @@protected_write
}
"#;

fn schema() -> Arc<Schema> {
    let source = format!("{MODELS}\n{ADMIN}");
    Arc::new(dsl::check(dsl::compile(&source, None).unwrap()).unwrap().1)
}

/// The host answers every action with the canned envelope and keeps what it saw.
struct Host {
    seen: Mutex<Vec<(String, Value)>>,
    answer: Mutex<Value>,
}

impl HostBridge for Host {
    fn call(&self, name: &str, payload: Value, ctx: CallContext) -> BoxFuture<Result<Value, BridgeError>> {
        assert!(ctx.tx.is_none(), "the host action owns its transactions");
        // A file reaches the host as a path that holds the bytes, for the time of the call.
        let mut payload = payload;
        if let Some(path) = payload["inputs"]["attachment"]["path"].as_str() {
            payload["file_text"] = json!(std::fs::read_to_string(path).unwrap());
        }
        self.seen.lock().unwrap().push((name.to_owned(), payload));
        let answer = self.answer.lock().unwrap().clone();
        Box::pin(async move { Ok(answer) })
    }
}

struct World {
    site: Arc<Site>,
    conn: Arc<dyn Executor>,
    host: Arc<Host>,
}

fn actions() -> Value {
    json!([
        {"name": "archive", "placement": "bulk", "permission": "archive"},
        {"name": "retire", "placement": "bulk", "applies_to": {"t": "cmp", "op": "ne", "l": {"t": "col", "path": [], "name": "status"}, "r": {"t": "text", "value": "retired"}}},
        {"name": "touch", "placement": "row"},
        {"name": "rebuild", "placement": "list"},
        {"name": "submit_hook", "placement": "submit"},
        {"name": "detail_hook", "placement": "detail", "confirm": "Sure?"},
        {"name": "import", "placement": "list", "inputs": [{"name": "attachment", "type": "file", "required": true}, {"name": "note", "type": "text"}]},
        {"name": "perm_only", "permission": "perm_only"}
    ])
}

async fn world(extra: Value) -> World {
    world_with(extra, Vec::new()).await
}

async fn world_with(extra: Value, others: Vec<Value>) -> World {
    let schema = schema();
    let conn = db::connect("sqlite://:memory:", 1).await.unwrap();
    for step in migrate::plan(&schema, &Default::default()).unwrap().up {
        conn.batch(step.sql).await.unwrap();
    }
    conn.batch("INSERT INTO \"Book\" (id, title, pages, status) VALUES (1, 'One', 10, 'draft'), (2, 'Two', 20, 'draft'), (3, 'Three', 30, 'retired'), (4, 'Four', 40, 'draft')".into()).await.unwrap();
    let mut resource = json!({"model": "Book", "actions": actions(), "list": {"columns": ["title", "pages", "status"], "filters": ["status", "pages"]}});
    for (k, v) in extra.as_object().unwrap() {
        resource[k] = v.clone();
    }
    let mut resources = vec![resource, json!({"model": "Author"})];
    resources.extend(others);
    let site = Arc::new(Site::build(schema, &json!({"resources": resources}).to_string()).unwrap());
    let host = Arc::new(Host { seen: Mutex::new(Vec::new()), answer: Mutex::new(json!({"ok": true})) });
    World { site, conn, host }
}

impl World {
    async fn touch(&self) -> (u16, Value) {
        self.post("/actions/Book/touch", json!({"keys": ["1"]})).await
    }

    async fn post(&self, uri: &str, body: Value) -> (u16, Value) {
        self.post_with(uri, body, true).await
    }

    async fn post_with(&self, uri: &str, body: Value, bridge: bool) -> (u16, Value) {
        let auth = bridge.then(|| AuthConfig { security: Security::new(&[7u8; 32], SecurityOptions::default()).unwrap(), bridge: self.host.clone() });
        let (status, _, out) = call_full(self.site.clone(), self.conn.clone(), "POST", &format!("/admin/api{uri}"), Some(json!({"key": 7, "is_superuser": true})), auth, Some(("application/json", body.to_string().into_bytes()))).await;
        (status, serde_json::from_slice(&out).unwrap_or(Value::Null))
    }

    fn answer(&self, v: Value) {
        *self.host.answer.lock().unwrap() = v;
    }

    fn seen(&self) -> Vec<(String, Value)> {
        self.host.seen.lock().unwrap().clone()
    }

    async fn titles(&self) -> Vec<String> {
        self.conn.query_text("SELECT title FROM \"Book\" ORDER BY id".into()).await.unwrap().into_iter().map(|r| r[0].clone().unwrap()).collect()
    }

    async fn audit(&self) -> Vec<(String, String, String, Value)> {
        let rows = self.conn.query_text("SELECT action, resource, record_key, changes FROM \"AdminLogEntry\" ORDER BY id".into()).await.unwrap();
        rows.into_iter().map(|r| (r[0].clone().unwrap(), r[1].clone().unwrap_or_default(), r[2].clone().unwrap_or_default(), serde_json::from_str(r[3].as_deref().unwrap_or("null")).unwrap_or(Value::Null))).collect()
    }
}

#[tokio::test]
async fn bulk_delete_on_keys_is_one_set_based_delete_with_one_summary_row() {
    let w = world(json!({})).await;
    let (status, out) = w.post("/actions/Book/bulk_delete", json!({"keys": ["1", "2"]})).await;
    assert_eq!((status, &out["result"]["message"], &out["affected"]), (200, &json!("Deleted 2 row(s)."), &json!(2)));
    assert_eq!(w.titles().await, ["Three", "Four"]);
    assert!(w.seen().is_empty(), "no service: the engine deletes");
    let audit = w.audit().await;
    assert_eq!(audit.len(), 1);
    let (action, resource, record_key, changes) = &audit[0];
    assert_eq!((action.as_str(), resource.as_str(), record_key.as_str()), ("bulk_delete", "Book", ""));
    assert_eq!((&changes["count"], &changes["selection"], &changes["keys"], &changes["affected"]), (&json!(2), &json!("keys"), &json!(["1", "2"]), &json!(2)));
    assert!(changes.get("request_id").is_some(), "the row carries the id that groups it with the rows of the action");
}

#[tokio::test]
async fn bulk_delete_on_all_rows_that_match_uses_the_list_filters() {
    let w = world(json!({})).await;
    let (status, out) = w.post("/actions/Book/bulk_delete?f.status=draft", json!({"all": true})).await;
    assert_eq!((status, &out["affected"]), (200, &json!(3)));
    assert_eq!(w.titles().await, ["Three"]);
    let audit = w.audit().await;
    assert_eq!((&audit[0].3["selection"], &audit[0].3["count"], &audit[0].3["query"]), (&json!("all"), &json!(3), &json!("f.status=draft")));
}

#[tokio::test]
async fn bulk_delete_false_removes_the_action_and_the_can_rule_limits_it() {
    let w = world(json!({"bulk_delete": false})).await;
    let (status, out) = w.post("/actions/Book/bulk_delete", json!({"keys": ["1"]})).await;
    assert_eq!((status, &out["error"]["kind"]), (404, &json!("NotFound")));
    assert_eq!(w.titles().await.len(), 4);
    let rule = json!({"delete": {"t": "cmp", "op": "ne", "l": {"t": "col", "path": [], "name": "status"}, "r": {"t": "text", "value": "retired"}}});
    let w = world(json!({"can": rule})).await;
    let (status, out) = w.post("/actions/Book/bulk_delete", json!({"keys": ["1", "3"]})).await;
    assert_eq!((status, &out["affected"]), (200, &json!(1)));
    assert_eq!(w.titles().await, ["Two", "Three", "Four"]);
}

#[tokio::test]
async fn a_custom_bulk_action_gets_the_query_set_and_the_result_is_a_message() {
    let w = world(json!({})).await;
    w.answer(json!({"ok": true, "result": {"kind": "message", "message": "Archived", "level": "success"}}));
    let (status, out) = w.post("/actions/Book/archive?q=&f.status=draft", json!({"all": true, "inputs": {}})).await;
    assert_eq!((status, &out["count"], &out["result"]), (200, &json!(3), &json!({"kind": "message", "message": "Archived", "level": "success"})));
    let seen = w.seen();
    assert_eq!(seen.len(), 1);
    let (name, p) = &seen[0];
    assert_eq!(name, "action.Book.archive");
    assert_eq!((&p["resource"], &p["action"], &p["count"], &p["capped"], &p["selection"], &p["op"]), (&json!("Book"), &json!("archive"), &json!(3), &json!(false), &json!("all"), &Value::Null));
    assert!(p["filters"].as_array().is_some_and(|f| !f.is_empty()));
    assert!(p["values"].is_array());
    assert_eq!(w.titles().await.len(), 4, "the action runs in the host");
    let audit = w.audit().await;
    assert_eq!((audit.len(), audit[0].0.as_str()), (1, "action:archive"));
    assert_eq!(audit[0].3["request_id"], p["request_id"], "the host gets the id of the summary row");
}

#[tokio::test]
async fn keys_are_checked_by_placement() {
    let w = world(json!({})).await;
    let bad = |status: u16, out: &Value| assert_eq!((status, &out["error"]["kind"]), (400, &json!("BadRequest")), "{out}");
    let (s, o) = w.post("/actions/Book/archive", json!({})).await;
    bad(s, &o);
    let (s, o) = w.post("/actions/Book/archive", json!({"keys": ["1"], "all": true})).await;
    bad(s, &o);
    let (s, o) = w.post("/actions/Book/archive", json!({"keys": []})).await;
    bad(s, &o);
    let (s, o) = w.post("/actions/Book/touch", json!({"keys": ["1", "2"]})).await;
    bad(s, &o);
    let (s, o) = w.post("/actions/Book/touch", json!({"all": true})).await;
    bad(s, &o);
    let (s, o) = w.post("/actions/Book/rebuild", json!({"keys": ["1"]})).await;
    bad(s, &o);
    let (s, o) = w.post("/actions/Book/archive", json!({"keys": ["x"]})).await;
    bad(s, &o);
    assert!(w.seen().is_empty());
    let (s, o) = w.post("/actions/Book/nope", json!({"keys": ["1"]})).await;
    assert_eq!((s, &o["error"]["kind"]), (404, &json!("NotFound")));
    let (s, o) = w.post("/actions/Book/perm_only", json!({"keys": ["1"]})).await;
    assert_eq!((s, &o["error"]["kind"]), (404, &json!("NotFound")), "an action with no placement is a permission only");
}

#[tokio::test]
async fn row_detail_submit_and_list_placements_run_on_the_rows_they_name() {
    let w = world(json!({})).await;
    for (name, body) in [("touch", json!({"keys": ["2"]})), ("detail_hook", json!({"keys": ["2"]})), ("submit_hook", json!({"keys": ["2"]})), ("rebuild", json!({}))] {
        let (status, out) = w.post(&format!("/actions/Book/{name}"), body).await;
        assert_eq!((status, &out["result"]["kind"]), (200, &json!("refresh")), "{name}: {out}");
    }
    let seen = w.seen();
    let counts: Vec<_> = seen.iter().map(|(n, p)| (n.as_str(), p["count"].clone(), p["selection"].clone())).collect();
    assert_eq!(counts, [("action.Book.touch", json!(1), json!("keys")), ("action.Book.detail_hook", json!(1), json!("keys")), ("action.Book.submit_hook", json!(1), json!("keys")), ("action.Book.rebuild", json!(4), json!("filter"))]);
    let audit = w.audit().await;
    assert_eq!(audit[0].2, "2", "one key: the row of the log names the record");
    let (s, o) = w.post("/actions/Book/touch", json!({"keys": ["99"]})).await;
    assert_eq!((s, &o["error"]["kind"]), (404, &json!("NotFound")));
}

#[tokio::test]
async fn applies_to_excludes_the_rows_it_does_not_cover() {
    let w = world(json!({})).await;
    let (status, out) = w.post("/actions/Book/retire", json!({"keys": ["1", "3", "4"]})).await;
    assert_eq!((status, &out["count"]), (200, &json!(2)));
    let (status, out) = w.post("/actions/Book/retire", json!({"keys": ["3"]})).await;
    assert_eq!((status, &out["result"]["message"]), (200, &json!("No row matched.")));
    assert_eq!(w.seen().len(), 1, "the host never sees an empty query set");
}

#[tokio::test]
async fn above_ten_thousand_rows_the_action_needs_a_second_confirmation() {
    let w = world(json!({})).await;
    w.conn.batch("WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c WHERE n < 10001) INSERT INTO \"Book\" (title, pages) SELECT 'bulk', n FROM c".into()).await.unwrap();
    let (status, out) = w.post("/actions/Book/archive", json!({"all": true})).await;
    assert_eq!((status, &out["error"]["kind"], &out["count"], &out["capped"]), (409, &json!("ConfirmLarge"), &json!(10000), &json!(true)));
    assert!(w.seen().is_empty());
    let (status, out) = w.post("/actions/Book/archive", json!({"all": true, "confirm_large": true})).await;
    assert_eq!((status, &out["count"], &out["capped"]), (200, &json!(10000), &json!(true)));
    assert_eq!(w.seen()[0].1["capped"], json!(true));
    let (status, _) = w.post("/actions/Book/bulk_delete?f.pages=5", json!({"all": true})).await;
    assert_eq!(status, 200, "a count under the cap needs no confirmation");
}

#[tokio::test]
async fn the_four_results_are_checked() {
    let w = world(json!({})).await;
    w.answer(json!({"ok": true, "result": {"kind": "redirect", "to": "/admin/Book"}}));
    assert_eq!(w.touch().await.1["result"], json!({"kind": "redirect", "to": "/admin/Book"}));
    w.answer(json!({"ok": true, "result": {"kind": "download", "filename": "a.csv", "content_type": "text/csv", "base64": "YSxi"}}));
    assert_eq!(w.touch().await.1["result"]["filename"], json!("a.csv"));
    w.answer(json!({"ok": true, "result": {"kind": "refresh"}}));
    assert_eq!(w.touch().await.1["result"], json!({"kind": "refresh"}));
    for bad in [json!({"kind": "redirect", "to": "//evil.example"}), json!({"kind": "redirect", "to": "https://evil.example"}), json!({"kind": "download", "filename": "a"}), json!({"kind": "nope"})] {
        w.answer(json!({"ok": true, "result": bad}));
        let (status, out) = w.touch().await;
        assert_eq!((status, &out["error"]["kind"]), (500, &json!("ServerError")), "{bad}");
    }
    assert_eq!(w.audit().await.len(), 3, "a bad result writes no summary row");
}

#[tokio::test]
async fn host_failures_map_to_the_kinds_of_a_host_write() {
    let w = world(json!({})).await;
    w.answer(json!({"ok": false, "kind": "not_found"}));
    assert_eq!(w.touch().await.0, 404);
    w.answer(json!({"ok": false, "kind": "conflict"}));
    assert_eq!(w.touch().await.0, 409);
    w.answer(json!({"ok": false, "kind": "invalid", "fields": {"reason": ["Too short."]}, "form": ["Bad."]}));
    let (status, out) = w.touch().await;
    assert_eq!((status, &out["fields"]["reason"], &out["form"]), (422, &json!(["Too short."]), &json!(["Bad."])));
    w.answer(json!({"ok": false, "kind": "boom"}));
    assert_eq!(w.touch().await.0, 500);
    assert!(w.audit().await.is_empty());
    let (status, out) = w.post_with("/actions/Book/touch", json!({"keys": ["1"]}), false).await;
    assert_eq!((status, &out["error"]["kind"]), (403, &json!("ReadOnly")), "a custom action needs host code");
}

#[tokio::test]
async fn inputs_are_checked_and_a_file_input_arrives_as_a_path_and_is_masked_in_the_log() {
    let w = world(json!({})).await;
    let (status, out) = w.post("/actions/Book/import", json!({"inputs": {}})).await;
    assert_eq!((status, out["fields"].get("attachment").is_some()), (422, true), "{out}");
    let file = json!({"name": "books.csv", "content_type": "text/csv", "base64": "dGl0bGUKT25l"});
    let (status, out) = w.post("/actions/Book/import", json!({"inputs": {"attachment": file, "note": "hi"}})).await;
    assert_eq!(status, 200, "{out}");
    let seen = w.seen();
    let p = &seen[0].1;
    assert_eq!((&p["inputs"]["note"], &p["inputs"]["attachment"]["filename"], &p["inputs"]["attachment"]["size"], &p["file_text"]), (&json!("hi"), &json!("books.csv"), &json!(9), &json!("title\nOne")));
    let path = p["inputs"]["attachment"]["path"].as_str().unwrap();
    assert!(!std::path::Path::new(path).exists(), "the temporary file goes with the request");
    let audit = w.audit().await;
    assert_eq!(audit[0].3["inputs"]["attachment"], json!({"name": "books.csv", "content_type": "text/csv", "size": 9}));
    assert_eq!(audit[0].3["inputs"]["note"], json!("hi"));
}

fn owned_by_the_user() -> Value {
    json!({"filter": {"t": "cmp", "op": "eq", "l": {"t": "col", "path": [], "name": "author_id"}, "r": {"t": "param", "i": 0}}, "slots": ["user"]})
}

/// Authors 7 (the test user) and 8. Books 1, 2 and 4 belong to 7, book 3 to 8; book 5 is added for 8.
async fn share_the_books(w: &World) {
    w.conn.batch("INSERT INTO \"Author\" (id, name) VALUES (7, 'Me'), (8, 'Other'); INSERT INTO \"Book\" (id, title, status, author_id) VALUES (5, 'Five', 'draft', 8); UPDATE \"Book\" SET author_id = 7 WHERE id IN (1, 2, 4); UPDATE \"Book\" SET author_id = 8 WHERE id = 3".into()).await.unwrap();
}

#[tokio::test]
async fn a_custom_action_on_all_rows_stays_inside_the_queryset() {
    let w = world(json!({"queryset": owned_by_the_user()})).await;
    share_the_books(&w).await;
    let (status, out) = w.post("/actions/Book/archive", json!({"all": true})).await;
    assert_eq!((status, &out["count"]), (200, &json!(3)), "{out}");
    let (status, out) = w.post("/actions/Book/archive", json!({"keys": ["3", "5"]})).await;
    assert_eq!((status, &out["result"]["message"]), (200, &json!("No row matched.")), "keys of the other owner match nothing");
    let seen = w.seen();
    assert_eq!(seen.len(), 1, "the host never sees the other owner's rows");
    assert!(seen[0].1["filters"].as_array().is_some_and(|f| f.len() >= 1));
}

#[tokio::test]
async fn bulk_delete_of_all_rows_with_no_filter_leaves_rows_outside_the_queryset_and_the_can_rule() {
    let can = json!({"delete": {"t": "cmp", "op": "ne", "l": {"t": "col", "path": [], "name": "status"}, "r": {"t": "text", "value": "retired"}}});
    let w = world(json!({"queryset": owned_by_the_user(), "can": can})).await;
    share_the_books(&w).await;
    w.conn.batch("UPDATE \"Book\" SET status = 'retired' WHERE id = 4".into()).await.unwrap();
    let (status, out) = w.post("/actions/Book/bulk_delete", json!({"all": true})).await;
    assert_eq!((status, &out["affected"]), (200, &json!(2)), "{out}");
    assert_eq!(w.titles().await, ["Three", "Four", "Five"], "book 3 and 5 are the other owner's, book 4 is retired");
}

#[tokio::test]
async fn an_action_on_a_tab_runs_on_the_rows_of_the_parent_and_only_bulk_and_row_run_there() {
    let w = world(json!({})).await;
    w.conn.batch("INSERT INTO \"Author\" (id, name) VALUES (1, 'Ada'), (2, 'Bo'); UPDATE \"Book\" SET author_id = 1 WHERE id IN (1, 2); UPDATE \"Book\" SET author_id = 2 WHERE id IN (3, 4)".into()).await.unwrap();
    let (status, out) = w.post("/tab-actions/Author/1/books/archive", json!({"keys": ["1", "2", "3"]})).await;
    assert_eq!((status, &out["count"]), (200, &json!(2)), "book 3 belongs to the other author: {out}");
    let (status, out) = w.post("/tab-actions/Author/1/books/touch", json!({"keys": ["2"]})).await;
    assert_eq!((status, &out["result"]["kind"]), (200, &json!("refresh")), "{out}");
    let (status, out) = w.post("/tab-actions/Author/1/books/touch", json!({"keys": ["4"]})).await;
    assert_eq!((status, &out["error"]["kind"]), (404, &json!("NotFound")), "a row of the other parent");
    for name in ["rebuild", "detail_hook", "submit_hook"] {
        let (status, out) = w.post(&format!("/tab-actions/Author/1/books/{name}"), json!({"keys": ["1"]})).await;
        assert_eq!((status, &out["error"]["kind"]), (404, &json!("NotFound")), "{name}");
    }
    let (status, out) = w.post("/tab-actions/Author/1/books/bulk_delete", json!({"all": true})).await;
    assert_eq!((status, &out["affected"]), (200, &json!(2)), "{out}");
    assert_eq!(w.titles().await, ["Three", "Four"]);
    assert_eq!(w.seen().len(), 2, "archive and touch reached the host; the refused ones did not");
}

#[tokio::test]
async fn the_host_gets_allow_protected_for_a_protected_model_only_when_the_resource_allows_it() {
    for (allow, want) in [(true, json!(true)), (false, json!(false))] {
        let vault = json!({"model": "Vault", "allow_protected_writes": allow, "actions": [{"name": "seal", "placement": "bulk"}]});
        let w = world_with(json!({}), vec![vault]).await;
        w.conn.batch("INSERT INTO \"Vault\" (id, note) VALUES (1, 'x')".into()).await.unwrap();
        let (status, out) = w.post("/actions/Vault/seal", json!({"keys": ["1"]})).await;
        assert_eq!(status, 200, "{out}");
        assert_eq!(w.seen()[0].1["allow_protected"], want, "allow_protected_writes = {allow}");
    }
    let w = world(json!({})).await;
    w.post("/actions/Book/archive", json!({"keys": ["1"]})).await;
    assert_eq!(w.seen()[0].1["allow_protected"], json!(false), "a model that is not protected");
}
