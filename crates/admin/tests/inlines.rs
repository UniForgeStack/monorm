//! Inlines: a to-many relation edited in place on the detail page of its parent, with the page of the child, on SQLite in memory.
use std::sync::{Arc, Mutex};

use monorm_admin::{call_body, call_full, AuthConfig, BoxFuture, BridgeError, CallContext, HostBridge, Security, SecurityOptions, Site};
use monorm_core::{dsl, migrate, schema::Schema};
use monorm_engine::db::{self, Executor};
use serde_json::{json, Value};

const ADMIN: &str = include_str!("../../../apps/admin/schema.prisma");
// The shape of apps/auth without its extension attributes.
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
const MODELS: &str = r#"
datasource db {
  provider = "sqlite"
}

model Order {
  id      Int         @id @default(autoincrement())
  number  String      @unique @db.VarChar(10)
  status  String      @default("open")
  lines   OrderLine[]
  notes   Note[]
}

model OrderLine {
  id       Int    @id @default(autoincrement())
  order_id Int
  sku      String @db.VarChar(10)
  qty      Int    @default(1) @check(raw("qty >= 0"))
  kind     String @default("goods")
  order    Order  @relation(fields: [order_id], references: [id], onDelete: Cascade)
}

model Note {
  id       Int    @id @default(autoincrement())
  order_id Int
  body     String
  order    Order  @relation(fields: [order_id], references: [id], onDelete: Cascade)
}

model Author {
  id      Int    @id @default(autoincrement())
  name    String
  written Book[] @relation("written")
  edited  Book[] @relation("edited")
}

model Book {
  id        Int     @id @default(autoincrement())
  author_id Int
  editor_id Int?
  title     String
  author    Author  @relation("written", fields: [author_id], references: [id])
  editor    Author? @relation("edited", fields: [editor_id], references: [id])
}
"#;

fn schema() -> Arc<Schema> {
    let source = format!("{MODELS}\n{AUTH}\n{ADMIN}");
    Arc::new(dsl::check(dsl::compile(&source, None).unwrap()).unwrap().1)
}

fn build(config: Value) -> Result<Site, String> {
    Site::build(schema(), &config.to_string()).map_err(|e| e.to_string())
}

fn refused(config: Value, needle: &str) {
    let e = build(config.clone()).err().unwrap_or_else(|| panic!("the build fails: {config}"));
    assert!(e.contains(needle), "{e}");
}

#[test]
fn two_relations_to_the_parent_and_no_via_fail_the_build() {
    refused(json!({"resources": [{"model": "Author", "inlines": [{"model": "Book"}]}, {"model": "Book"}]}), "ambiguous");
    let ok = json!({"resources": [{"model": "Author", "inlines": [{"model": "Book", "via": "written"}]}, {"model": "Book"}]});
    assert!(build(ok).is_ok());
}

async fn database(schema: &Schema) -> Arc<dyn Executor> {
    let conn = db::connect("sqlite://:memory:", 1).await.unwrap();
    for step in migrate::plan(schema, &Default::default()).unwrap().up {
        conn.batch(step.sql).await.unwrap();
    }
    let seed = "INSERT INTO \"Order\" (id, number) VALUES (1, 'A1'), (2, 'B2');
        INSERT INTO \"OrderLine\" (id, order_id, sku, qty) VALUES (1, 1, 'S1', 1), (2, 1, 'S2', 2), (3, 1, 'S3', 3), (4, 2, 'X', 9);
        INSERT INTO \"Note\" (id, order_id, body) VALUES (1, 1, 'first'), (2, 2, 'other');
        INSERT INTO \"User\" (id, username) VALUES (1, 'ann'), (2, 'bob'), (3, 'cy'), (4, 'di'), (5, 'ed'), (6, 'flo');
        INSERT INTO \"Permission\" (id, label, codename, name) VALUES (1, 'admin', 'access', 'a'), (2, 'admin', 'read_order', 'b'), (3, 'admin', 'manage_order', 'c'), (4, 'admin', 'delete_order', 'd'), (5, 'admin', 'edit_lines', 'f'), (6, 'admin', 'view_order', 'g'), (7, 'admin', 'change_order', 'h'), (8, 'admin', 'view_orderline', 'i'), (9, 'admin', 'change_orderline', 'j');
        INSERT INTO \"UserPermission\" (user_id, permission_id) VALUES (1, 1), (1, 2), (2, 1), (2, 2), (2, 3), (3, 1), (3, 2), (3, 5), (4, 1), (4, 2), (4, 3), (4, 5), (5, 1), (5, 6), (5, 7), (6, 1), (6, 6), (6, 7), (6, 8), (6, 9);";
    conn.batch(seed.into()).await.unwrap();
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
    json!({"resources": [{"model": "Order", "inlines": [{"model": "OrderLine", "max": 3}, {"model": "Note", "save": "row"}]}, {"model": "OrderLine"}, {"model": "Note"}]})
}

impl World {
    async fn send(&self, method: &str, uri: &str, body: Option<Value>) -> (u16, Value) {
        let text = body.map(|b| b.to_string());
        let (status, out) = call_body(self.site.clone(), self.conn.clone(), method, &format!("/admin/api{uri}"), Some(json!({"key": 1, "is_superuser": true})), None, text.as_deref()).await;
        (status, serde_json::from_str(&out).unwrap_or(Value::Null))
    }

    async fn as_user(&self, user: i64, method: &str, uri: &str, body: Option<Value>) -> u16 {
        let text = body.map(|b| b.to_string());
        call_body(self.site.clone(), self.conn.clone(), method, &format!("/admin/api{uri}"), Some(json!({"key": user})), None, text.as_deref()).await.0
    }

    async fn detail_as(&self, user: i64) -> Value {
        let (_, body) = call_body(self.site.clone(), self.conn.clone(), "GET", "/admin/api/resources/Order/1", Some(json!({"key": user})), None, None).await;
        serde_json::from_str(&body).unwrap()
    }

    async fn rows(&self, sql: &str) -> Vec<Vec<Option<String>>> {
        self.conn.query_text(sql.to_string()).await.unwrap()
    }

    async fn audit(&self) -> Vec<Vec<Option<String>>> {
        self.rows("SELECT action, resource, record_key, record_label, changes FROM \"AdminLogEntry\" ORDER BY id").await
    }

    async fn lines(&self) -> Vec<String> {
        self.rows("SELECT id || ':' || order_id || ':' || sku || ':' || qty FROM \"OrderLine\" ORDER BY id").await.into_iter().map(|r| r[0].clone().unwrap()).collect()
    }
}

fn text(v: &Option<String>) -> &str {
    v.as_deref().unwrap_or_default()
}

const LINES: [&str; 4] = ["1:1:S1:1", "2:1:S2:2", "3:1:S3:3", "4:2:X:9"];

#[tokio::test]
async fn the_detail_lists_the_rows_of_the_record_within_the_cap_and_the_queryset() {
    let w = world(config()).await;
    let (_, body) = w.send("GET", "/resources/Order/1", None).await;
    let rows = body["inlines"]["lines"]["rows"].as_array().unwrap();
    assert_eq!(rows.iter().map(|r| r["key"].as_str().unwrap()).collect::<Vec<_>>(), ["1", "2", "3"]);
    assert_eq!(body["inlines"]["lines"]["truncated"], false);
    assert_eq!(body["inlines"]["lines"]["link"], "OrderLine");
    w.conn.batch("INSERT INTO \"OrderLine\" (id, order_id, sku) VALUES (5, 1, 'S5'), (6, 1, 'S6')".into()).await.unwrap();
    let (_, body) = w.send("GET", "/resources/Order/1", None).await;
    assert_eq!(body["inlines"]["lines"]["rows"].as_array().unwrap().len(), 3);
    assert_eq!(body["inlines"]["lines"]["truncated"], true);
    let (_, other) = w.send("GET", "/resources/Order/2", None).await;
    assert_eq!(other["inlines"]["lines"]["rows"][0]["values"], json!([4, 2, "X", 9, "goods"]));
}

#[tokio::test]
async fn a_row_links_to_its_page_only_when_the_user_can_view_it() {
    let w = world(json!({"resources": [{"model": "Order", "inlines": [{"model": "OrderLine"}]}]})).await;
    let (_, body) = w.send("GET", "/resources/Order/1", None).await;
    assert_eq!(body["inlines"]["lines"]["link"], Value::Null);
}

#[tokio::test]
async fn with_parent_saves_creates_updates_and_deletes_with_the_parent_in_one_change_set() {
    let w = world(config()).await;
    let body = json!({"set": {"status": "paid"}, "loaded": {"status": "open"}, "inlines": {"lines": {
        "create": [{"set": {"sku": "N1", "qty": 5}}],
        "update": [{"key": "1", "set": {"qty": 10}, "loaded": {"qty": 1}}],
        "delete": ["2"]
    }}});
    let (status, out) = w.send("PATCH", "/resources/Order/1", Some(body)).await;
    assert_eq!(status, 200, "{out}");
    assert_eq!(w.lines().await, ["1:1:S1:10", "3:1:S3:3", "4:2:X:9", "5:1:N1:5"]);
    assert_eq!(text(&w.rows("SELECT status FROM \"Order\" WHERE id = 1").await[0][0]), "paid");
    let audit = w.audit().await;
    let actions: Vec<(&str, &str)> = audit.iter().map(|r| (text(&r[0]), text(&r[1]))).collect();
    assert_eq!(actions, [("delete", "OrderLine"), ("update", "OrderLine"), ("create", "OrderLine"), ("update", "Order")]);
    let child: Value = serde_json::from_str(text(&audit[1][4])).unwrap();
    assert_eq!(child["inline"], "Order.lines");
    assert_eq!(child["diff"]["qty"], json!({"from": 1, "to": 10}));
}

#[tokio::test]
async fn an_inline_only_change_saves_without_a_parent_change() {
    let w = world(config()).await;
    let (status, out) = w.send("PATCH", "/resources/Order/1", Some(json!({"set": {}, "inlines": {"lines": {"update": [{"key": "3", "set": {"qty": 7}, "loaded": {"qty": 3}}]}}}))).await;
    assert_eq!(status, 200, "{out}");
    assert_eq!(w.lines().await[2], "3:1:S3:7");
}

#[tokio::test]
async fn one_refused_row_refuses_the_whole_change_set() {
    let w = world(config()).await;
    let body = json!({"set": {"status": "paid"}, "loaded": {"status": "open"}, "inlines": {"lines": {
        "update": [{"key": "1", "set": {"qty": 10}, "loaded": {"qty": 1}}],
        "create": [{"set": {"qty": 1}}]
    }}});
    let (status, out) = w.send("PATCH", "/resources/Order/1", Some(body)).await;
    assert_eq!(status, 422, "{out}");
    assert_eq!(out["fields"]["lines.new.0.sku"], json!(["This field is required."]));
    assert_eq!(w.lines().await, LINES);
    assert_eq!(text(&w.rows("SELECT status FROM \"Order\" WHERE id = 1").await[0][0]), "open");
    assert!(w.audit().await.is_empty());
}

#[tokio::test]
async fn a_database_error_in_a_late_row_rolls_back_the_earlier_rows_and_the_parent() {
    let w = world(config()).await;
    let body = json!({"set": {"status": "paid"}, "loaded": {"status": "open"}, "inlines": {"lines": {"update": [{"key": "1", "set": {"qty": 10}, "loaded": {"qty": 1}}, {"key": "2", "set": {"qty": -1}, "loaded": {"qty": 2}}]}}});
    let (status, out) = w.send("PATCH", "/resources/Order/1", Some(body)).await;
    assert_eq!(status, 422, "{out}");
    assert_eq!(w.lines().await, LINES);
    assert_eq!(text(&w.rows("SELECT status FROM \"Order\" WHERE id = 1").await[0][0]), "open");
    assert!(w.audit().await.is_empty());
}

#[tokio::test]
async fn a_row_of_another_parent_is_a_404_and_the_foreign_key_comes_from_the_parent() {
    let mut cfg = config();
    cfg["resources"][0]["inlines"][0]["max"] = json!(10);
    let w = world(cfg).await;
    let go = |inlines: Value| w.send("PATCH", "/resources/Order/1", Some(json!({"set": {}, "inlines": {"lines": inlines}})));
    assert_eq!(go(json!({"update": [{"key": "4", "set": {"qty": 1}, "loaded": {"qty": 9}}]})).await.0, 404);
    assert_eq!(go(json!({"delete": ["4"]})).await.0, 404);
    let (status, out) = go(json!({"create": [{"set": {"sku": "Z", "order_id": 2}}]})).await;
    assert_eq!(status, 422, "{out}");
    assert_eq!(w.lines().await, LINES);
    assert_eq!(go(json!({"create": [{"set": {"sku": "Z"}}]})).await.0, 200);
    assert_eq!(w.lines().await[4], "5:1:Z:1");
}

#[tokio::test]
async fn the_row_cap_and_the_delete_switch_hold_on_the_server() {
    let w = world(config()).await;
    let go = |inlines: Value| w.send("PATCH", "/resources/Order/1", Some(json!({"set": {}, "inlines": {"lines": inlines}})));
    let (status, out) = go(json!({"create": [{"set": {"sku": "N"}}]})).await;
    assert_eq!(status, 422, "{out}");
    assert!(out["form"].to_string().contains("at most 3"), "{out}");
    assert_eq!(go(json!({"create": [{"set": {"sku": "N"}}], "delete": ["1"]})).await.0, 200);
    let off = world(json!({"resources": [{"model": "Order", "inlines": [{"model": "OrderLine", "delete": false}]}, {"model": "OrderLine"}]})).await;
    let (status, _) = off.send("PATCH", "/resources/Order/1", Some(json!({"set": {}, "inlines": {"lines": {"delete": ["1"]}}}))).await;
    assert_eq!(status, 403);
    assert_eq!(off.lines().await, LINES);
}

#[tokio::test]
async fn a_stale_loaded_value_is_a_409_with_a_prefixed_key_and_writes_nothing() {
    let w = world(config()).await;
    let body = json!({"set": {"status": "paid"}, "loaded": {"status": "open"}, "inlines": {"lines": {"update": [{"key": "1", "set": {"qty": 10}, "loaded": {"qty": 99}}]}}});
    let (status, out) = w.send("PATCH", "/resources/Order/1", Some(body)).await;
    assert_eq!(status, 409, "{out}");
    assert_eq!(out["conflicts"]["lines.1.qty"], json!({"loaded": 99, "current": 1}));
    assert_eq!(w.lines().await, LINES);
    assert!(w.audit().await.is_empty());
}

#[tokio::test]
async fn unknown_row_mode_and_malformed_inlines_are_a_400() {
    let w = world(config()).await;
    for inlines in [json!({"nope": {}}), json!({"notes": {"create": [{"set": {"body": "x"}}]}}), json!({"lines": {"zip": []}}), json!({"lines": []}), json!({"lines": {"delete": ["1", "1"]}}), json!({"lines": {"update": [{"key": "1", "set": {"qty": 2}, "loaded": {"qty": 1}}], "delete": ["1"]}})] {
        let (status, _) = w.send("PATCH", "/resources/Order/1", Some(json!({"set": {}, "inlines": inlines}))).await;
        assert_eq!(status, 400, "{inlines}");
    }
    assert_eq!(w.lines().await, LINES);
}

#[tokio::test]
async fn a_new_parent_saves_its_create_rows_and_refuses_update_rows() {
    let w = world(config()).await;
    let (status, out) = w.send("POST", "/resources/Order", Some(json!({"set": {"number": "C3"}, "inlines": {"lines": {"update": [{"key": "1", "set": {"qty": 1}}]}}}))).await;
    assert_eq!(status, 400, "{out}");
    let (status, out) = w.send("POST", "/resources/Order", Some(json!({"set": {"number": "C3"}, "inlines": {"lines": {"create": [{"set": {"sku": "K", "qty": 2}}, {"set": {"sku": "L"}}]}}}))).await;
    assert_eq!(status, 201, "{out}");
    assert_eq!(w.lines().await[4..], ["5:3:K:2", "6:3:L:1"]);
}

#[tokio::test]
async fn row_mode_saves_each_row_alone_and_pins_it_to_the_parent() {
    let w = world(config()).await;
    let (status, out) = w.send("POST", "/resources/Order/1/inlines/notes", Some(json!({"set": {"body": "second"}}))).await;
    assert_eq!(status, 201, "{out}");
    let stored = w.rows("SELECT order_id, body FROM \"Note\" WHERE id = 3").await;
    assert_eq!(stored[0].iter().map(text).collect::<Vec<_>>(), ["1", "second"]);
    let (status, _) = w.send("PATCH", "/resources/Order/1/inlines/notes/3", Some(json!({"set": {"body": "edited"}, "loaded": {"body": "second"}}))).await;
    assert_eq!(status, 200);
    assert_eq!(text(&w.rows("SELECT body FROM \"Note\" WHERE id = 3").await[0][0]), "edited");
    assert_eq!(w.send("PATCH", "/resources/Order/1/inlines/notes/2", Some(json!({"set": {"body": "x"}, "loaded": {"body": "other"}}))).await.0, 404);
    assert_eq!(w.send("DELETE", "/resources/Order/1/inlines/notes/2", None).await.0, 404);
    let (status, _) = w.send("PATCH", "/resources/Order/1/inlines/notes/3", Some(json!({"set": {"order_id": 2}, "loaded": {"order_id": 1}}))).await;
    assert_eq!(status, 422);
    assert_eq!(w.send("DELETE", "/resources/Order/1/inlines/notes/3", None).await.0, 200);
    assert_eq!(w.rows("SELECT count(*) FROM \"Note\"").await[0][0].as_deref(), Some("2"));
    let audit = w.audit().await;
    assert_eq!(audit.iter().map(|r| (text(&r[0]), text(&r[1]))).collect::<Vec<_>>(), [("create", "Note"), ("update", "Note"), ("delete", "Note")]);
    assert_eq!(w.send("POST", "/resources/Order/1/inlines/lines", Some(json!({"set": {"sku": "q"}}))).await.0, 404);
}

#[tokio::test]
async fn the_inline_queryset_filter_applies_on_read_and_on_inline_writes() {
    let mut cfg = config();
    cfg["resources"][0]["inlines"][0]["page"] = json!({"queryset": {"filter": {"t": "cmp", "op": "ne", "l": {"t": "col", "path": [], "name": "kind"}, "r": {"t": "text", "value": "fee"}}}});
    let w = world(cfg).await;
    w.conn.batch("UPDATE \"OrderLine\" SET kind = 'fee' WHERE id = 2".into()).await.unwrap();
    let (_, body) = w.send("GET", "/resources/Order/1", None).await;
    let keys: Vec<&str> = body["inlines"]["lines"]["rows"].as_array().unwrap().iter().map(|r| r["key"].as_str().unwrap()).collect();
    assert_eq!(keys, ["1", "3"]);
    let (status, _) = w.send("PATCH", "/resources/Order/1", Some(json!({"set": {}, "inlines": {"lines": {"update": [{"key": "2", "set": {"qty": 5}, "loaded": {"qty": 2}}]}}}))).await;
    assert_eq!(status, 404);
    assert_eq!(w.lines().await, LINES);
}

#[test]
fn each_wrong_wrapper_is_a_build_error() {
    let inline = |wrapper: Value| {
        let mut w = json!({"model": "OrderLine"});
        w.as_object_mut().unwrap().extend(wrapper.as_object().unwrap().clone());
        json!({"resources": [{"model": "Order", "inlines": [w]}, {"model": "OrderLine"}]})
    };
    refused(inline(json!({"via": "notes"})), "not a to-many relation");
    refused(inline(json!({"via": "nope"})), "not a relation");
    refused(inline(json!({"save": "later"})), "save is");
    refused(inline(json!({"style": "grid"})), "style is");
    refused(inline(json!({"max": 0})), "max is");
    refused(inline(json!({"max": 201})), "max is");
    refused(inline(json!({"label": ""})), "label");
    let mut access = inline(json!({"permission": "x"}));
    access["permissions"] = json!("access");
    refused(access, "page or model");
    refused(inline(json!({"page": {"inlines": [{"model": "Note"}]}})), "never nested");
    refused(inline(json!({"page": {"model": "Note"}})), "model of the inline");
    refused(json!({"resources": [{"model": "Order", "inlines": [{"model": "Order"}]}]}), "no to-many relation");
    refused(json!({"resources": [{"model": "Order", "inlines": [{"model": "OrderLine"}, {"model": "OrderLine"}]}]}), "already an inline");
    refused(json!({"resources": [{"model": "Order", "inlines": [{"model": "Nope"}]}]}), "unknown model");
    assert!(build(json!({"resources": [{"model": "Order", "inlines": [{"model": "OrderLine", "permission": "edit_lines"}]}], "permissions": "page"})).is_ok());
}

#[tokio::test]
async fn a_wrapper_overrides_a_key_of_the_registered_page_and_a_child_without_a_page_takes_its_config() {
    let mut cfg = config();
    cfg["resources"][1]["readonly"] = json!(["kind"]);
    cfg["resources"][0]["inlines"][0]["page"] = json!({"readonly": ["qty"]});
    let w = world(cfg).await;
    let (_, meta) = w.send("GET", "/resources", None).await;
    let form = &meta["resources"][0]["inlines"][0]["form"];
    let readonly: Vec<(&str, bool)> = form.as_array().unwrap().iter().map(|f| (f["name"].as_str().unwrap(), f["readonly"].as_bool().unwrap())).collect();
    assert_eq!(readonly, [("id", false), ("sku", false), ("qty", true), ("kind", false)]);
    let (status, out) = w.send("PATCH", "/resources/Order/1", Some(json!({"set": {}, "inlines": {"lines": {"update": [{"key": "1", "set": {"qty": 4}, "loaded": {"qty": 1}}]}}}))).await;
    assert_eq!(status, 422, "{out}");
    let bare = world(json!({"resources": [{"model": "Order", "inlines": [{"model": "OrderLine", "page": {"readonly": ["kind"]}}]}]})).await;
    let (_, meta) = bare.send("GET", "/resources", None).await;
    assert_eq!(meta["resources"][0]["inlines"][0]["form"][3]["readonly"], true);
    assert_eq!(bare.lines().await, LINES);
}

#[tokio::test]
async fn the_page_strategy_follows_the_parent_and_an_own_permission_names_the_write() {
    let follow = world(json!({"permissions": "page", "resources": [{"model": "Order", "perms": ["read", "manage", "delete"], "inlines": [{"model": "OrderLine"}]}]})).await;
    let edit = json!({"set": {}, "inlines": {"lines": {"update": [{"key": "1", "set": {"qty": 4}, "loaded": {"qty": 1}}]}}});
    assert_eq!(follow.as_user(1, "GET", "/resources/Order/1", None).await, 200);
    assert_eq!(follow.as_user(1, "PATCH", "/resources/Order/1", Some(edit.clone())).await, 403);
    assert_eq!(follow.lines().await, LINES);
    assert_eq!(follow.as_user(2, "PATCH", "/resources/Order/1", Some(edit.clone())).await, 200);
    let denied = follow.audit().await;
    assert_eq!(denied.len(), 2);
    assert_eq!((text(&denied[0][0]), text(&denied[0][1])), ("permission_denied", "Order"));
    let own = world(json!({"permissions": "page", "resources": [{"model": "Order", "perms": ["read", "manage"], "inlines": [{"model": "OrderLine", "permission": "edit_lines"}, {"model": "Note", "permission": "edit_lines", "save": "row"}]}]})).await;
    // The order and the line names are both needed to save a change set: user 2 lacks `edit_lines`, user 3 lacks `manage_order`.
    assert_eq!(own.as_user(2, "PATCH", "/resources/Order/1", Some(edit.clone())).await, 403);
    assert_eq!(own.as_user(3, "PATCH", "/resources/Order/1", Some(edit.clone())).await, 403);
    assert_eq!(own.as_user(4, "PATCH", "/resources/Order/1", Some(edit.clone())).await, 200);
    assert_eq!(own.lines().await[0], "1:1:S1:4");
    // A row of a `row` inline saves with its own name only.
    let note = json!({"set": {"body": "n"}});
    assert_eq!(own.as_user(2, "POST", "/resources/Order/1/inlines/notes", Some(note.clone())).await, 403);
    assert_eq!(own.as_user(3, "POST", "/resources/Order/1/inlines/notes", Some(note)).await, 201);
    assert_eq!(own.as_user(3, "GET", "/resources/Order/1", None).await, 200);
}

#[tokio::test]
async fn the_model_strategy_checks_the_permissions_of_the_child_model() {
    let w = world(json!({"permissions": "model", "resources": [{"model": "Order", "inlines": [{"model": "OrderLine"}]}]})).await;
    // User 5 has the names of Order only; user 6 has the names of OrderLine too.
    assert!(w.detail_as(5).await["inlines"].get("lines").is_none());
    assert!(w.detail_as(6).await["inlines"].get("lines").is_some());
    let edit = json!({"set": {}, "inlines": {"lines": {"update": [{"key": "1", "set": {"qty": 4}, "loaded": {"qty": 1}}]}}});
    assert_eq!(w.as_user(5, "PATCH", "/resources/Order/1", Some(edit.clone())).await, 403);
    assert_eq!(w.lines().await, LINES);
    assert_eq!(w.as_user(6, "PATCH", "/resources/Order/1", Some(edit)).await, 200);
    assert_eq!(w.lines().await[0], "1:1:S1:4");
}

/// A host that records each call and answers `{"ok": true}`.
struct Host(Mutex<Vec<(String, Value)>>);

impl HostBridge for Host {
    fn call(&self, name: &str, payload: Value, _: CallContext) -> BoxFuture<Result<Value, BridgeError>> {
        self.0.lock().unwrap().push((name.to_owned(), payload));
        Box::pin(async { Ok(json!({"ok": true, "key": {"id": 1}})) })
    }
}

#[tokio::test]
async fn a_service_of_the_parent_gets_the_checked_change_set_and_writes_nothing_in_the_engine() {
    let cfg = json!({"resources": [{"model": "Order", "service": {"update": {}}, "inlines": [{"model": "OrderLine"}]}]});
    let w = world(cfg).await;
    let host = Arc::new(Host(Mutex::new(Vec::new())));
    let auth = AuthConfig { security: Security::new(&[7u8; 32], SecurityOptions::default()).unwrap(), bridge: host.clone() };
    let body = json!({"set": {}, "inlines": {"lines": {"create": [{"set": {"sku": "N"}}], "update": [{"key": "1", "set": {"qty": 4}, "loaded": {"qty": 1}}], "delete": ["2"]}}});
    let (status, _, out) = call_full(w.site.clone(), w.conn.clone(), "PATCH", "/admin/api/resources/Order/1", Some(json!({"key": 1, "is_superuser": true})), Some(auth), Some(("application/json", body.to_string().into_bytes()))).await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&out));
    let seen = host.0.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0, "service.Order.update");
    let lines = &seen[0].1["inlines"]["lines"];
    assert_eq!(lines["create"][0]["set"], json!({"sku": "N"}));
    assert_eq!(lines["update"][0]["key"], "1");
    assert_eq!(lines["update"][0]["set"], json!({"qty": 4}));
    assert_eq!(lines["delete"][0]["key"], "2");
    assert_eq!(w.lines().await, LINES);
}

#[test]
fn a_child_with_a_service_needs_a_parent_service_or_the_row_save_mode() {
    let child = json!({"model": "OrderLine", "service": {"create": {}}});
    refused(json!({"resources": [{"model": "Order", "inlines": [{"model": "OrderLine"}]}, child.clone()]}), "service or host behavior");
    assert!(build(json!({"resources": [{"model": "Order", "inlines": [{"model": "OrderLine", "save": "row"}]}, child.clone()]})).is_ok());
    assert!(build(json!({"resources": [{"model": "Order", "service": {"create": {}, "update": {}}, "inlines": [{"model": "OrderLine"}]}, child]})).is_ok());
}
