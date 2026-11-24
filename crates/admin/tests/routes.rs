//! Custom routes: the match, the 401 / 403 / 404 / 405, the audit row of a denial, the request the host sees,
//! the JSON and the stream answers, and the build errors, on SQLite in memory with a fake host.
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use base64::engine::general_purpose::STANDARD as B64;
use axum::body::Body;
use axum::http::Request;
use base64::Engine;
use http_body_util::BodyExt;
use monorm_admin::server::api_with;
use monorm_admin::User;
use tower::ServiceExt;
use monorm_admin::{call_full, AuthConfig, BoxFuture, BridgeError, CallContext, HostBridge, Security, SecurityOptions, Site};
use monorm_core::{dsl, migrate, schema::Schema};
use monorm_engine::db::{self, Executor};
use monorm_engine::trace::{wrap, Trace};
use serde_json::{json, Value};

const ADMIN: &str = include_str!("../../../apps/admin/schema.prisma");
const AUTH: &str = r#"
model User {
  id           Int          @id @default(autoincrement())
  username     String       @unique
  is_active    Boolean      @default(true)
  is_superuser Boolean      @default(false)
  roles        Role[]       @relation(through: UserRole)
  permissions  Permission[] @relation(through: UserPermission)
}

model Role {
  id          Int          @id @default(autoincrement())
  name        String       @unique
  permissions Permission[] @relation(through: RolePermission)
}

model Permission {
  id       Int    @id @default(autoincrement())
  label    String
  codename String
  name     String
  @@unique([label, codename])
}

model RolePermission {
  id            Int        @id @default(autoincrement())
  role_id       Int
  permission_id Int
  role          Role       @relation(fields: [role_id], references: [id], onDelete: Cascade)
  permission    Permission @relation(fields: [permission_id], references: [id], onDelete: Cascade)
}

model UserRole {
  id      Int  @id @default(autoincrement())
  user_id Int
  role_id Int
  user    User @relation(fields: [user_id], references: [id], onDelete: Cascade)
  role    Role @relation(fields: [role_id], references: [id], onDelete: Cascade)
}

model UserPermission {
  id            Int        @id @default(autoincrement())
  user_id       Int
  permission_id Int
  user          User       @relation(fields: [user_id], references: [id], onDelete: Cascade)
  permission    Permission @relation(fields: [permission_id], references: [id], onDelete: Cascade)
}
"#;

fn schema() -> Arc<Schema> {
    let source = format!("datasource db {{\n  provider = \"sqlite\"\n}}\n\nmodel Note {{\n  id Int @id\n}}\n{AUTH}\n{ADMIN}");
    Arc::new(dsl::check(dsl::compile(&source, None).unwrap()).unwrap().1)
}

/// Answers each route function with a canned envelope, and holds the streams that it opened.
#[derive(Default)]
struct Host {
    seen: Mutex<Vec<(String, Value)>>,
    answers: Mutex<BTreeMap<String, Value>>,
    streams: Mutex<BTreeMap<String, VecDeque<Result<Vec<u8>, String>>>>,
    closed: Mutex<Vec<String>>,
}

impl HostBridge for Host {
    fn call(&self, name: &str, payload: Value, ctx: CallContext) -> BoxFuture<Result<Value, BridgeError>> {
        assert!(ctx.tx.is_none(), "a route owns no transaction");
        self.seen.lock().unwrap().push((name.to_owned(), payload.clone()));
        let out = match name {
            "route.__next" => {
                let id = payload["stream"].as_str().unwrap().to_owned();
                match self.streams.lock().unwrap().get_mut(&id).and_then(|q| q.pop_front()) {
                    Some(Ok(bytes)) => Ok(json!({"chunk": B64.encode(bytes)})),
                    Some(Err(m)) => Err(BridgeError::Host { kind: "RuntimeError".into(), message: m }),
                    None => Ok(json!({"chunk": null})),
                }
            }
            "route.__close" => {
                self.closed.lock().unwrap().push(payload["stream"].as_str().unwrap().to_owned());
                Ok(Value::Null)
            }
            other => self.answers.lock().unwrap().get(other).cloned().ok_or_else(|| BridgeError::Bridge(format!("no host function {other}"))),
        };
        Box::pin(async move { out })
    }
}

struct World {
    site: Arc<Site>,
    conn: Arc<dyn Executor>,
    /// The same database through a recording executor: the routes under test run on it.
    traced: Arc<dyn Executor>,
    trace: Trace,
    host: Arc<Host>,
}

const ROUTES: &str = r#"[
  {"method": "GET", "path": "reports/summary"},
  {"method": "GET", "path": "reports/{id}", "permission": "admin.run_report"},
  {"method": "POST", "path": "reports/{id}/rerun", "permission": "admin.run_report"},
  {"method": "GET", "path": "download"}
]"#;

async fn world_with(config: Value) -> World {
    let schema = schema();
    let conn = db::connect("sqlite://:memory:", 1).await.unwrap();
    for step in migrate::plan(&schema, &Default::default()).unwrap().up {
        conn.batch(step.sql).await.unwrap();
    }
    for sql in [
        "INSERT INTO \"User\" (id, username) VALUES (1, 'ann'), (2, 'bob'), (3, 'cy')",
        "INSERT INTO \"Permission\" (id, label, codename, name) VALUES (1, 'admin', 'access', 'a'), (2, 'admin', 'run_report', 'b')",
        "INSERT INTO \"UserPermission\" (user_id, permission_id) VALUES (1, 1), (1, 2), (2, 1)",
    ] {
        conn.batch(sql.into()).await.unwrap();
    }
    let site = Arc::new(Site::build(schema, &config.to_string()).unwrap());
    let trace = Trace::new();
    let traced = wrap(conn.clone(), Some(&trace));
    World { site, conn, traced, trace, host: Arc::new(Host::default()) }
}

async fn world() -> World {
    world_with(json!({"routes": serde_json::from_str::<Value>(ROUTES).unwrap(), "resources": [{"model": "Note"}]})).await
}

impl World {
    fn answer(&self, function: &str, v: Value) {
        self.host.answers.lock().unwrap().insert(function.to_owned(), v);
    }

    async fn send(&self, method: &str, path: &str, user: Option<i64>, body: Option<&str>) -> (u16, Vec<(String, String)>, Vec<u8>) {
        let auth = AuthConfig { security: Security::new(&[7u8; 32], SecurityOptions::default()).unwrap(), bridge: self.host.clone() };
        let body = body.map(|b| ("application/json", b.as_bytes().to_vec()));
        call_full(self.site.clone(), self.traced.clone(), method, &format!("/admin/api/routes/{path}"), user.map(|k| json!({"key": k})), Some(auth), body).await
    }

    async fn json(&self, method: &str, path: &str, user: Option<i64>, body: Option<&str>) -> (u16, Value) {
        let (status, _, out) = self.send(method, path, user, body).await;
        (status, serde_json::from_slice(&out).unwrap_or(Value::Null))
    }

    /// The statements that the requests since the last call sent to the database.
    fn statements(&self) -> Vec<String> {
        self.trace.take().into_iter().map(|e| e.sql).collect()
    }

    async fn audit(&self) -> Vec<(String, String, Value)> {
        let rows = self.conn.query_text("SELECT action, resource, changes FROM \"AdminLogEntry\" ORDER BY id".into()).await.unwrap();
        rows.into_iter().map(|r| (r[0].clone().unwrap(), r[1].clone().unwrap_or_default(), serde_json::from_str(r[2].as_deref().unwrap_or("null")).unwrap_or(Value::Null))).collect()
    }
}

#[tokio::test]
async fn no_session_is_401_and_the_host_is_not_called() {
    let w = world().await;
    let (status, body) = w.json("GET", "reports/summary", None, None).await;
    assert_eq!(status, 401);
    assert_eq!(body["error"]["kind"], "Unauthorized");
    assert!(w.host.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_user_without_the_gate_is_403() {
    let w = world().await;
    let (status, _) = w.json("GET", "reports/summary", Some(3), None).await;
    assert_eq!(status, 403);
    assert!(w.host.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_route_without_a_permission_needs_the_gate_only() {
    let w = world().await;
    w.answer("route.GET.reports/summary", json!({"json": {"rows": 3}}));
    let (status, body) = w.json("GET", "reports/summary", Some(2), None).await;
    assert_eq!((status, body), (200, json!({"rows": 3})));
}

#[tokio::test]
async fn a_missing_permission_is_403_with_an_audit_row_and_no_host_call() {
    let w = world().await;
    w.answer("route.GET.reports/{id}", json!({"json": 1}));
    let (status, _) = w.json("GET", "reports/7", Some(2), None).await;
    assert_eq!(status, 403);
    assert!(w.host.seen.lock().unwrap().is_empty());
    let rows = w.audit().await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "permission_denied");
    assert_eq!(rows[0].1, "route:reports/{id}");
    assert_eq!(rows[0].2["needs"], json!(["admin.run_report"]));
    let (status, body) = w.json("GET", "reports/7", Some(1), None).await;
    assert_eq!((status, body), (200, json!(1)));
}

#[tokio::test]
async fn the_host_gets_the_request_with_params_query_body_and_user() {
    let w = world().await;
    w.answer("route.POST.reports/{id}/rerun", json!({"json": {"queued": true}, "status": 202}));
    let (status, _, out) = w.send("POST", "reports/42/rerun?mode=fast&tag=a&tag=b&empty=", Some(1), Some(r#"{"limit": 5}"#)).await;
    assert_eq!(status, 202);
    assert_eq!(serde_json::from_slice::<Value>(&out).unwrap(), json!({"queued": true}));
    let seen = w.host.seen.lock().unwrap();
    let (name, p) = &seen[0];
    assert_eq!(name, "route.POST.reports/{id}/rerun");
    assert_eq!(p["method"], "POST");
    assert_eq!(p["params"], json!({"id": "42"}));
    assert_eq!(p["query"], json!({"mode": "fast", "tag": ["a", "b"], "empty": ""}));
    assert_eq!(p["body"], json!({"limit": 5}));
    assert_eq!(p["user"]["key"], 1);
}

#[tokio::test]
async fn the_literal_path_wins_over_a_parameter_and_no_match_is_404_or_405() {
    let w = world().await;
    w.answer("route.GET.reports/summary", json!({"json": "literal"}));
    w.answer("route.GET.reports/{id}", json!({"json": "param"}));
    assert_eq!(w.json("GET", "reports/summary", Some(1), None).await.1, json!("literal"));
    assert_eq!(w.json("GET", "reports/other", Some(1), None).await.1, json!("param"));
    assert_eq!(w.json("GET", "nothing/here", Some(1), None).await.0, 404);
    let (status, headers, _) = w.send("DELETE", "reports/summary", Some(1), None).await;
    assert_eq!(status, 405);
    assert!(headers.iter().any(|(k, v)| k == "allow" && v == "GET"));
}

#[tokio::test]
async fn a_body_that_is_not_json_is_400() {
    let w = world().await;
    let (status, body) = w.json("POST", "reports/1/rerun", Some(1), Some("{nope")).await;
    assert_eq!(status, 400);
    assert_eq!(body["error"]["kind"], "BadRequest");
    assert!(w.host.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_host_error_is_a_500_that_names_the_request_id_only() {
    let w = world().await;
    let (status, body) = w.json("GET", "reports/summary", Some(1), None).await;
    assert_eq!(status, 500);
    assert!(!body.to_string().contains("no host function"), "the cause stays in the log");
}

#[tokio::test]
async fn a_stream_reaches_the_client_chunk_by_chunk_and_the_host_closes_it() {
    let w = world().await;
    w.answer("route.GET.download", json!({"stream": "s1", "content_type": "text/csv; charset=utf-8", "filename": "a\"b.csv"}));
    w.host.streams.lock().unwrap().insert("s1".into(), VecDeque::from([Ok(b"id,name\n".to_vec()), Ok(b"1,ann\n".to_vec()), Ok(b"2,bob\n".to_vec())]));
    let (status, headers, out) = w.send("GET", "download", Some(1), None).await;
    assert_eq!(status, 200);
    assert_eq!(String::from_utf8(out).unwrap(), "id,name\n1,ann\n2,bob\n");
    assert!(headers.iter().any(|(k, v)| k == "content-type" && v == "text/csv; charset=utf-8"));
    assert!(headers.iter().any(|(k, v)| k == "content-disposition" && v == "attachment; filename=\"a_b.csv\"; filename*=UTF-8''a%22b.csv"));
    assert_eq!(*w.host.closed.lock().unwrap(), vec!["s1".to_owned()]);
}

#[tokio::test]
async fn a_stream_that_fails_is_cut_and_still_closed() {
    let w = world().await;
    w.answer("route.GET.download", json!({"stream": "s2", "content_type": "text/plain"}));
    w.host.streams.lock().unwrap().insert("s2".into(), VecDeque::from([Ok(b"first".to_vec()), Err("boom".into())]));
    let (status, _, out) = w.send("GET", "download", Some(1), None).await;
    // The head was 200; the cut body is an error to the in-memory caller.
    assert_eq!(status, 500);
    assert!(String::from_utf8_lossy(&out).contains("boom"));
    assert_eq!(*w.host.closed.lock().unwrap(), vec!["s2".to_owned()]);
}

#[tokio::test]
async fn a_stream_with_a_bad_status_is_closed_and_the_route_fails() {
    let w = world().await;
    w.answer("route.GET.download", json!({"stream": "s3", "status": 99, "content_type": "text/plain"}));
    w.host.streams.lock().unwrap().insert("s3".into(), VecDeque::from([Ok(b"never".to_vec())]));
    let (status, body) = w.json("GET", "download", Some(1), None).await;
    assert_eq!(status, 500);
    assert!(!body.to_string().contains("not saved"), "a GET route does not talk about saving");
    assert_eq!(*w.host.closed.lock().unwrap(), vec!["s3".to_owned()], "the host stream is not left open");
}

#[tokio::test]
async fn a_body_over_the_limit_is_413_and_the_host_is_not_called() {
    let w = world().await;
    let big = format!("{{\"x\":\"{}\"}}", "a".repeat(1 << 20));
    let (status, body) = w.json("POST", "reports/1/rerun", Some(1), Some(&big)).await;
    assert_eq!(status, 413);
    assert_eq!(body["error"]["kind"], "PayloadTooLarge");
    assert!(w.host.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_raw_path_test_looks_at_the_start_of_the_path_only() {
    let w = world().await;
    for raw in ["/admin/api/routes/x", "/admin/api/routes/a/b?q=1", "/admin/api/files/Note/1/cover", "/admin/api/export/Note"] {
        assert!(w.site.is_raw(raw), "{raw}");
    }
    for plain in ["/admin/api/resources", "/admin/api/resources/Note/records?next=/routes/", "/admin/api/resources/Note/records/routes/1"] {
        assert!(!w.site.is_raw(plain), "{plain}");
    }
}

#[tokio::test]
async fn the_route_permission_shows_in_the_permission_names() {
    let w = world().await;
    let names = w.site.permission_names();
    assert!(names.contains(&"admin.run_report".to_owned()));
    assert_eq!(names.iter().filter(|n| *n == "admin.run_report").count(), 1);
}

fn build(routes: Value, extra: Value) -> Result<Site, String> {
    let mut config = json!({"routes": routes});
    for (k, v) in extra.as_object().unwrap() {
        config[k] = v.clone();
    }
    Site::build(schema(), &config.to_string()).map_err(|e| e.message)
}

#[test]
fn build_rejects_a_wrong_route() {
    let cases = [
        (json!([{"method": "FETCH", "path": "a"}]), json!({}), "GET, POST, PUT, PATCH or DELETE"),
        (json!([{"method": "GET", "path": ""}]), json!({}), "path is empty"),
        (json!([{"method": "GET", "path": "a b"}]), json!({}), "must be letters"),
        (json!([{"method": "GET", "path": "../x"}]), json!({}), "must be letters"),
        (json!([{"method": "GET", "path": "a/{id}/{id}"}]), json!({}), "used twice"),
        (json!([{"method": "GET", "path": "a/{x}"}, {"method": "GET", "path": "a/{y}"}]), json!({}), "same method and path shape"),
        (json!([{"method": "GET", "path": "a", "permission": "run_report"}]), json!({}), "is not <label>.<name>"),
        (json!([{"method": "GET", "path": "a", "permission": "nope.run_report"}]), json!({}), "unknown label \"nope\""),
        (json!([{"method": "GET", "path": "a", "permission": "admin.Run-Report"}]), json!({}), "lowercase letters"),
        (json!([{"method": "GET", "path": "a", "permission": "admin.run"}]), json!({"permissions": "access"}), "page or model strategy"),
    ];
    for (routes, extra, want) in cases {
        let e = build(routes.clone(), extra).err().unwrap_or_else(|| panic!("{routes} builds"));
        assert!(e.contains(want), "{routes}: {e:?} lacks {want:?}");
    }
}

#[test]
fn a_route_under_a_second_label_is_allowed_when_a_resource_uses_it() {
    let site = build(json!([{"method": "GET", "path": "a", "permission": "shop.sync"}]), json!({"resources": [{"model": "Note", "label": "shop"}]}));
    let site = site.unwrap_or_else(|e| panic!("{e}"));
    assert!(site.permission_names().contains(&"shop.sync".to_owned()));
}

#[test]
fn the_built_in_pages_sit_in_one_group_that_a_site_with_its_own_groups_need_not_list() {
    let all = ["Note", "User", "Role", "Permission", "UserRole", "AdminLogEntry"];
    let group_of = |site: &Site| {
        let names: Vec<String> = site.resources.iter().filter(|r| r.group.as_deref() == Some("Users and access")).map(|r| r.name.clone()).collect();
        names
    };
    let plain = Site::build(schema(), &json!({"resources": [{"model": "Note"}, {"model": "User"}]}).to_string()).unwrap();
    let in_group = group_of(&plain);
    assert!(in_group.contains(&"AdminLogEntry".to_owned()) && in_group.contains(&"Role".to_owned()) && in_group.contains(&"Permission".to_owned()) && in_group.contains(&"User".to_owned()), "{in_group:?}");
    assert!(!in_group.contains(&"Note".to_owned()));
    let nav = plain.nav(&all);
    assert_eq!(nav[0]["group"], Value::Null);
    assert_eq!(nav[1]["group"], "Users and access");
    let own = Site::build(schema(), &json!({"groups": ["Content"], "resources": [{"model": "Note", "group": "Content"}, {"model": "User"}]}).to_string()).unwrap();
    let nav = own.nav(&all);
    assert_eq!((nav[0]["group"].as_str(), nav[1]["group"].as_str()), (Some("Content"), Some("Users and access")));
    let moved = Site::build(schema(), &json!({"groups": ["Content"], "resources": [{"model": "Note", "group": "Content"}, {"model": "Role", "group": "Content"}]}).to_string()).unwrap();
    assert_eq!(moved.resource("Role").unwrap().group.as_deref(), Some("Content"));
}

// The grants read is the only statement of a served route; a denial adds the audit write.
const SERVED: usize = 1;
const DENIED: usize = 2;

#[tokio::test]
async fn a_route_costs_a_fixed_number_of_statements() {
    let w = world().await;
    w.answer("route.GET.reports/summary", json!({"json": 1}));
    w.answer("route.GET.reports/{id}", json!({"json": 1}));
    w.trace.take();
    for (path, user) in [("reports/summary", 2), ("reports/7", 1), ("reports/8", 1)] {
        assert_eq!(w.json("GET", path, Some(user), None).await.0, 200);
        let sql = w.statements();
        assert_eq!(sql.len(), SERVED, "{path}: {sql:?}");
    }
}

#[tokio::test]
async fn the_statements_do_not_grow_with_the_grants_of_the_user() {
    let w = world().await;
    w.answer("route.GET.reports/{id}", json!({"json": 1}));
    w.trace.take();
    w.json("GET", "reports/1", Some(1), None).await;
    let before = w.statements().len();
    let permissions: Vec<String> = (10..60).map(|i| format!("({i}, 'shop', 'p{i}', 'n{i}')")).collect();
    let grants: Vec<String> = (10..60).map(|i| format!("(1, {i})")).collect();
    w.conn.batch(format!("INSERT INTO \"Permission\" (id, label, codename, name) VALUES {}", permissions.join(", "))).await.unwrap();
    w.conn.batch(format!("INSERT INTO \"UserPermission\" (user_id, permission_id) VALUES {}", grants.join(", "))).await.unwrap();
    assert_eq!(w.json("GET", "reports/1", Some(1), None).await.0, 200);
    assert_eq!(w.statements().len(), before);
}

#[tokio::test]
async fn a_denied_route_costs_the_preamble_and_one_audit_write() {
    let w = world().await;
    w.trace.take();
    assert_eq!(w.json("GET", "reports/7", Some(2), None).await.0, 403);
    let sql = w.statements();
    assert_eq!(sql.len(), DENIED, "{sql:?}");
    assert_eq!(sql.iter().filter(|s| s.contains("adminlogentry")).count(), 1, "{sql:?}");
}

#[tokio::test]
async fn a_request_without_a_session_sends_no_statement() {
    let w = world().await;
    w.trace.take();
    assert_eq!(w.json("GET", "reports/summary", None, None).await.0, 401);
    assert!(w.statements().is_empty());
}

/// A host that makes endless chunks and counts the pulls and the closes.
#[derive(Default)]
struct Endless {
    pulls: Mutex<usize>,
    closed: Mutex<usize>,
}

impl HostBridge for Endless {
    fn call(&self, name: &str, _payload: Value, _ctx: CallContext) -> BoxFuture<Result<Value, BridgeError>> {
        let out = match name {
            "route.GET.download" => json!({"stream": "e", "content_type": "text/plain"}),
            "route.__next" => {
                *self.pulls.lock().unwrap() += 1;
                json!({"chunk": B64.encode(vec![b'x'; 1024])})
            }
            "route.__close" => {
                *self.closed.lock().unwrap() += 1;
                Value::Null
            }
            _ => Value::Null,
        };
        Box::pin(async move { Ok(out) })
    }
}

#[tokio::test]
async fn a_client_that_leaves_a_stream_stops_the_pulls_and_closes_it() {
    let w = world().await;
    let host = Arc::new(Endless::default());
    let auth = AuthConfig { security: Security::new(&[7u8; 32], SecurityOptions::default()).unwrap(), bridge: host.clone() };
    let app = api_with(w.site.clone(), w.conn.clone(), Some(auth));
    let mut request = Request::builder().method("GET").uri("/admin/api/routes/download").body(Body::empty()).unwrap();
    request.extensions_mut().insert(User(json!({"key": 1})));
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), 200);
    let mut body = response.into_body();
    assert!(body.frame().await.unwrap().unwrap().into_data().is_ok());
    drop(body);
    // The producer never ends by itself: only the leave can close the stream.
    for _ in 0..200 {
        if *host.closed.lock().unwrap() > 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(*host.closed.lock().unwrap(), 1, "the stream is closed once");
    let pulled = *host.pulls.lock().unwrap();
    assert!(pulled < 100, "the pulls stop soon after the leave: {pulled}");
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(*host.pulls.lock().unwrap(), pulled, "no pull after the close");
}

const ROUTE_ROW: &str = "INSERT INTO \"AdminLogEntry\" (id, user_key, action, resource, record_key, record_label, changes) VALUES (41, '2', 'service', 'Note', '', '', '{}')";

impl World {
    async fn route_keys(&self) -> Vec<String> {
        let rows = self.conn.query_text("SELECT record_key FROM \"AdminLogEntry\" WHERE id = 41".into()).await.unwrap();
        rows.into_iter().map(|r| r[0].clone().unwrap_or_default()).collect()
    }
}

#[tokio::test]
async fn rust_writes_the_key_text_of_the_audit_rows_that_a_route_wrote() {
    let w = world().await;
    w.conn.batch(ROUTE_ROW.into()).await.unwrap();
    w.answer("route.GET.reports/summary", json!({"json": {"ok": true}, "audit_keys": [{"id": 41, "key": ["a b", "x,y"]}]}));
    let (status, body) = w.json("GET", "reports/summary", Some(2), None).await;
    assert_eq!((status, body), (200, json!({"ok": true})));
    assert_eq!(w.route_keys().await, ["a%20b,x%2Cy"]);
}

#[tokio::test]
async fn a_bad_audit_key_entry_of_a_route_is_a_500_and_the_key_text_stays_blank() {
    let w = world().await;
    w.conn.batch(ROUTE_ROW.into()).await.unwrap();
    w.answer("route.GET.reports/summary", json!({"json": {"ok": true}, "audit_keys": [{"id": 41, "key": "a"}]}));
    let (status, body) = w.json("GET", "reports/summary", Some(2), None).await;
    assert_eq!(status, 500);
    assert_eq!(body["error"]["kind"], "ServerError");
    assert_eq!(w.route_keys().await, [""]);
}
