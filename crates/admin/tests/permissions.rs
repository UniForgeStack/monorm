//! Permission strategies: the names, the grants of one user, the 403 with its audit row, and the metadata.
use std::sync::Arc;

use monorm_admin::permissions::{Grants, Op};
use monorm_admin::{call, Site};
use monorm_core::{dsl, migrate, schema::Schema};
use monorm_engine::db::{self, Driver, Executor};
use monorm_engine::trace::{wrap, Trace};
use serde_json::{json, Value};

const ADMIN: &str = include_str!("../../../apps/admin/schema.prisma");
// The shape of apps/auth without its extension attributes: a user with roles and permissions through join models.
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
    let source = format!("datasource db {{\n  provider = \"sqlite\"\n}}\n\nmodel Note {{\n  id Int @id\n}}\n\nmodel Order {{\n  id Int @id\n}}\n{AUTH}\n{ADMIN}");
    Arc::new(dsl::check(dsl::compile(&source, None).unwrap()).unwrap().1)
}

/// Users 1 (reads Note through the role `reader`), 2 (a direct `admin.access` and `admin.manage_order`), 3 (no grant).
async fn database(schema: &Schema) -> Arc<dyn Driver> {
    let driver = db::connect("sqlite://:memory:", 1).await.unwrap();
    for step in migrate::plan(schema, &Default::default()).unwrap().up {
        driver.batch(step.sql).await.unwrap();
    }
    let seed = [
        "INSERT INTO \"User\" (id, username) VALUES (1, 'ann'), (2, 'bob'), (3, 'cy')",
        "INSERT INTO \"Permission\" (id, label, codename, name) VALUES (1, 'admin', 'access', 'a'), (2, 'admin', 'read_note', 'b'), (3, 'admin', 'manage_order', 'c')",
        "INSERT INTO \"Role\" (id, name) VALUES (1, 'reader')",
        "INSERT INTO \"RolePermission\" (role_id, permission_id) VALUES (1, 1), (1, 2)",
        "INSERT INTO \"UserRole\" (user_id, role_id) VALUES (1, 1)",
        "INSERT INTO \"UserPermission\" (user_id, permission_id) VALUES (2, 1), (2, 3)",
    ];
    for sql in seed {
        driver.batch(sql.to_string()).await.unwrap();
    }
    driver
}

fn site(schema: &Arc<Schema>, config: Value) -> Arc<Site> {
    Arc::new(Site::build(schema.clone(), &config.to_string()).unwrap())
}

async fn get(site: &Arc<Site>, conn: Arc<dyn Executor>, uri: &str, user: Value) -> (u16, Value) {
    let (status, body) = call(site.clone(), conn, "GET", uri, Some(user)).await;
    (status, serde_json::from_str(&body).unwrap())
}

async fn audit_rows(conn: &Arc<dyn Driver>) -> Vec<Vec<Option<String>>> {
    conn.query_text("SELECT user_key, action, resource FROM \"AdminLogEntry\" ORDER BY id".to_string()).await.unwrap()
}

#[tokio::test]
async fn page_names_are_read_and_manage_and_a_split_delete_adds_one() {
    let schema = schema();
    let site = site(&schema, json!({"resources": [{"model": "Note"}, {"model": "Order", "perms": ["read", "manage", "delete"]}]}));
    assert_eq!(site.permission_names(), ["admin.access", "admin.read_note", "admin.manage_note", "admin.read_order", "admin.manage_order", "admin.delete_order"]);
}

#[tokio::test]
async fn model_names_are_the_four_codenames_and_access_is_the_gate_only() {
    let schema = schema();
    let resources = json!([{"model": "Note", "actions": ["publish"], "field_permissions": ["see_cost"]}]);
    let model = site(&schema, json!({"permissions": "model", "label": "shop", "resources": resources}));
    assert_eq!(
        model.permission_names(),
        ["admin.access", "shop.add_note", "shop.change_note", "shop.delete_note", "shop.view_note", "shop.see_cost", "shop.publish_note"]
    );
    let access = site(&schema, json!({"permissions": "access", "resources": [{"model": "Note"}]}));
    assert_eq!(access.permission_names(), ["admin.access"]);
}

#[test]
fn a_wrong_permission_config_is_rejected_with_the_model_name() {
    let schema = schema();
    let build = |config: Value| Site::build(schema.clone(), &config.to_string()).err().unwrap().to_string();
    assert!(build(json!({"permissions": "role"})).contains("not \"page\", \"model\" or \"access\""));
    assert!(build(json!({"resources": [{"model": "Note", "allow": ["write"]}]})).contains("Note: allow has \"write\""));
    assert!(build(json!({"resources": [{"model": "Note", "perms": ["read"]}]})).contains("Note: perms must name read and manage"));
    assert!(build(json!({"permissions": "model", "resources": [{"model": "Note", "perms": ["read", "manage"]}]})).contains("page strategy only"));
    assert!(build(json!({"permissions": "access", "resources": [{"model": "Note", "actions": ["x"]}]})).contains("page or model strategy"));
    assert!(build(json!({"resources": [{"model": "Note", "actions": ["a.b"]}]})).contains("lowercase letters"));
}

#[tokio::test]
async fn a_role_grant_and_a_direct_grant_both_count_and_a_missing_grant_is_a_403_with_a_row() {
    let schema = schema();
    let driver = database(&schema).await;
    let site = site(&schema, json!({"resources": [{"model": "Note"}, {"model": "Order"}]}));
    let conn: Arc<dyn Executor> = driver.clone();
    // 1 reads Note through the role; 2 has manage_order directly, and manage includes read.
    assert_eq!(get(&site, conn.clone(), "/admin/api/resources/Note", json!({"key": 1})).await.0, 200);
    assert_eq!(get(&site, conn.clone(), "/admin/api/resources/Order", json!({"key": 2})).await.0, 200);
    assert!(audit_rows(&driver).await.is_empty());
    let (status, body) = get(&site, conn.clone(), "/admin/api/resources/Order", json!({"key": 1})).await;
    assert_eq!((status, body["error"]["kind"].as_str()), (403, Some("Forbidden")));
    assert_eq!(get(&site, conn.clone(), "/admin/api/resources/Note", json!({"key": 3})).await.0, 403);
    assert_eq!(get(&site, conn.clone(), "/admin/api/resources/Nope", json!({"key": 3})).await.0, 403);
    assert_eq!(get(&site, conn.clone(), "/admin/api/resources/Nope", json!({"key": 1})).await.0, 404);
    let rows = audit_rows(&driver).await;
    let rows: Vec<Vec<&str>> = rows.iter().map(|r| r.iter().map(|c| c.as_deref().unwrap_or("")).collect()).collect();
    assert_eq!(rows, [["1", "permission_denied", "Order"], ["3", "permission_denied", ""], ["3", "permission_denied", ""]]);
}

#[tokio::test]
async fn an_inactive_superuser_and_a_missing_key_get_nothing_and_an_active_one_gets_everything() {
    let schema = schema();
    let driver = database(&schema).await;
    let site = site(&schema, json!({}));
    let conn: Arc<dyn Executor> = driver;
    let uri = "/admin/api/resources/Note";
    assert_eq!(get(&site, conn.clone(), uri, json!({"is_superuser": true})).await.0, 200);
    assert_eq!(get(&site, conn.clone(), uri, json!({"is_superuser": true, "is_active": false})).await.0, 403);
    assert_eq!(get(&site, conn.clone(), uri, json!({"is_superuser": false})).await.0, 403);
    assert_eq!(get(&site, conn.clone(), uri, json!({"key": 1, "is_active": false})).await.0, 403);
}

#[tokio::test]
async fn allow_limits_a_resource_for_a_superuser_too() {
    let schema = schema();
    let driver = database(&schema).await;
    let site = site(&schema, json!({"resources": [{"model": "Note", "allow": ["create"]}, {"model": "Order"}]}));
    let conn: Arc<dyn Executor> = driver.clone();
    assert_eq!(get(&site, conn.clone(), "/admin/api/resources/Note", json!({"is_superuser": true})).await.0, 403);
    let rows = driver.query_text("SELECT changes FROM \"AdminLogEntry\" ORDER BY id".to_string()).await.unwrap();
    let changes: Value = serde_json::from_str(rows[0][0].as_deref().unwrap()).unwrap();
    assert_eq!(changes, json!({"reason": "allow"}));
    let (_, body) = get(&site, conn, "/admin/api/resources", json!({"is_superuser": true})).await;
    let names: Vec<&str> = body["resources"].as_array().unwrap().iter().map(|r| r["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Order"]);
}

#[tokio::test]
async fn the_access_strategy_needs_only_the_gate_and_the_page_strategy_needs_the_gate_too() {
    let schema = schema();
    let driver = database(&schema).await;
    let conn: Arc<dyn Executor> = driver;
    let access = site(&schema, json!({"permissions": "access"}));
    assert_eq!(get(&access, conn.clone(), "/admin/api/resources/Order", json!({"key": 1})).await.0, 200);
    assert_eq!(get(&access, conn.clone(), "/admin/api/resources/Order", json!({"key": 3})).await.0, 403);
    let model = site(&schema, json!({"permissions": "model"}));
    assert_eq!(get(&model, conn.clone(), "/admin/api/resources/Order", json!({"key": 2})).await.0, 403);
}

#[tokio::test]
async fn metadata_lists_only_what_the_user_can_use_with_the_filtered_names() {
    let schema = schema();
    let site = site(&schema, json!({"resources": [{"model": "Note", "actions": ["publish"]}, {"model": "Order"}]}));
    let mut grants = Grants::default();
    grants.names.extend(["admin.access", "admin.read_note", "admin.publish_note", "admin.manage_order", "other.x"].map(String::from));
    let meta = site.metadata(&grants);
    let names: Vec<&str> = meta["resources"].as_array().unwrap().iter().map(|r| r["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Note", "Order"]);
    assert_eq!(meta["resources"][0]["actions"], json!(["publish"]));
    assert_eq!(meta["permissions"], json!(["admin.access", "admin.read_note", "admin.publish_note", "admin.manage_order"]));
    grants.names.remove("admin.access");
    assert_eq!(site.metadata(&grants), json!({"resources": [], "permissions": []}));
    assert!(!site.allows(&grants, &site.resources[0], Op::Read));
}

#[tokio::test]
async fn the_grants_cost_one_statement_for_a_user_and_none_for_a_superuser() {
    let schema = schema();
    let driver = database(&schema).await;
    let site = site(&schema, json!({"permissions": "access"}));
    let count = |trace: &Trace| trace.take().len();
    let trace = Trace::default();
    let conn: Arc<dyn Executor> = wrap(driver.clone(), Some(&trace));
    get(&site, conn.clone(), "/admin/api/resources", json!({"is_superuser": true})).await;
    let superuser = count(&trace);
    get(&site, conn.clone(), "/admin/api/resources", json!({"key": 1})).await;
    assert_eq!(count(&trace), superuser + 1);
}
