//! Field permissions: a view of a resource is built once per granted set, and the cost is measured.
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Instant;

use monorm_admin::permissions::Grants;
use monorm_admin::{call, Site};
use monorm_core::{dsl, migrate, schema::Schema};
use monorm_engine::db::{self, Driver, Executor};
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
    let source = format!(
        "datasource db {{\n  provider = \"sqlite\"\n}}\n\nmodel Product {{\n  id    Int    @id\n  name  String\n  code  String\n  cost  Int\n}}\n\nmodel Note {{\n  id Int @id\n}}\n{AUTH}\n{ADMIN}"
    );
    Arc::new(dsl::check(dsl::compile(&source, None).unwrap()).unwrap().1)
}

fn site(schema: &Arc<Schema>) -> Arc<Site> {
    let config = json!({"resources": [
        {"model": "Product", "list": {"columns": ["name", "code", "cost"], "order": ["id"]}, "see": {"cost": "see_cost", "code": "see_code"}},
        {"model": "Note"},
    ]});
    Arc::new(Site::build(schema.clone(), &config.to_string()).unwrap())
}

/// Users 1 and 2 hold the page level only, 3 adds `see_cost`, 4 is a superuser.
async fn database(schema: &Schema) -> Arc<dyn Driver> {
    let driver = db::connect("sqlite://:memory:", 1).await.unwrap();
    for step in migrate::plan(schema, &Default::default()).unwrap().up {
        driver.batch(step.sql).await.unwrap();
    }
    let seed = [
        "INSERT INTO \"User\" (id, username, is_superuser) VALUES (1, 'ann', 0), (2, 'bob', 0), (3, 'cy', 0), (4, 'di', 1)",
        "INSERT INTO \"Permission\" (id, label, codename, name) VALUES (1, 'admin', 'access', 'a'), (2, 'admin', 'read_product', 'b'), (3, 'admin', 'see_cost', 'c')",
        "INSERT INTO \"UserPermission\" (user_id, permission_id) VALUES (1, 1), (1, 2), (2, 1), (2, 2), (3, 1), (3, 2), (3, 3)",
        "INSERT INTO \"Product\" (id, name, code, cost) VALUES (1, 'bolt', 'B1', 5), (2, 'nut', 'N1', 3)",
    ];
    for sql in seed {
        driver.batch(sql.to_string()).await.unwrap();
    }
    driver
}

async fn list(site: &Arc<Site>, conn: &Arc<dyn Executor>, resource: &str, key: i64) -> (u16, Value) {
    let user = if key == 4 { json!({"is_superuser": true}) } else { json!({"key": key}) };
    let (status, body) = call(site.clone(), conn.clone(), "GET", &format!("/admin/api/resources/{resource}"), Some(user)).await;
    (status, serde_json::from_str(&body).unwrap())
}

#[tokio::test]
async fn a_view_is_built_once_per_granted_set() {
    let schema = schema();
    let driver = database(&schema).await;
    let conn: Arc<dyn Executor> = driver;
    let site = site(&schema);
    let built = site.meta_builds();
    assert_eq!(built, 6, "one page of Product and Note and the four built-in resources");

    let (status, body) = list(&site, &conn, "Product", 1).await;
    assert_eq!(status, 200);
    assert_eq!(body["columns"].as_array().unwrap().len(), 2, "ann sees name and the key: {body}");
    assert_eq!(site.meta_builds(), built + 1, "the first user of a granted set builds its view");

    for key in [1, 2, 1, 2] {
        assert_eq!(list(&site, &conn, "Product", key).await.0, 200);
    }
    assert_eq!(site.meta_builds(), built + 1, "two users with the same granted set share one view");

    let (_, body) = list(&site, &conn, "Product", 3).await;
    assert_eq!(body["columns"].as_array().unwrap().len(), 3, "cy sees name, cost and the key: {body}");
    assert_eq!(site.meta_builds(), built + 2, "a new granted set adds one view");

    let (_, body) = list(&site, &conn, "Product", 4).await;
    assert_eq!(body["columns"].as_array().unwrap().len(), 4, "a superuser holds every name");
    assert_eq!(site.meta_builds(), built + 2, "a full grant uses the page");

    assert_eq!(list(&site, &conn, "Note", 1).await.0, 403);
    assert_eq!(site.meta_builds(), built + 2, "a resource without a bound name never builds a view");
}

/// Prints the cost of a view: `cargo test -p monorm-admin --test field_permissions cost -- --nocapture`.
#[test]
fn the_cost_of_a_view_is_one_build_and_then_a_lookup() {
    let schema = schema();
    let site = site(&schema);
    let grants = Grants { all: false, names: BTreeSet::from(["admin.read_product".to_string()]) };
    let built = site.meta_builds();
    let start = Instant::now();
    let first = site.sight("Product", &grants).unwrap();
    let build = start.elapsed();
    let labels: Vec<&str> = first.columns.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, ["name", "id"]);
    let rounds = 10_000u32;
    let start = Instant::now();
    for _ in 0..rounds {
        let again = site.sight("Product", &grants).unwrap();
        assert_eq!(again.columns.len(), 2);
    }
    let lookup = start.elapsed() / rounds;
    eprintln!("view build (first call, includes the metadata): {build:?}; cache hit: {lookup:?} per call");
    assert_eq!(site.meta_builds(), built + 1);
}

#[tokio::test]
async fn the_audit_log_drops_the_values_of_fields_the_reader_cannot_see() {
    let schema = schema();
    let driver = database(&schema).await;
    for sql in [
        "INSERT INTO \"Permission\" (id, label, codename, name) VALUES (4, 'admin', 'read_adminlogentry', 'd')",
        "INSERT INTO \"UserPermission\" (user_id, permission_id) VALUES (1, 4), (3, 4)",
        r#"INSERT INTO "AdminLogEntry" (id, user_key, action, resource, record_key, record_label, changes) VALUES (1, '4', 'update', 'Product', '1', 'bolt', '{"request_id":"r","diff":{"name":{"from":"a","to":"bolt"},"cost":{"from":1,"to":5}}}')"#,
    ] {
        driver.batch(sql.to_string()).await.unwrap();
    }
    let conn: Arc<dyn Executor> = driver;
    let site = site(&schema);
    let changes_of = |body: &Value| -> Value {
        let at = body["columns"].as_array().unwrap().iter().position(|c| c["field"] == "changes" || c["label"] == "changes").expect("a changes column");
        body["rows"][0][at].clone()
    };
    let (status, ann) = list(&site, &conn, "AdminLogEntry", 1).await;
    assert_eq!(status, 200, "{ann}");
    assert_eq!(changes_of(&ann)["diff"], json!({"name": {"from": "a", "to": "bolt"}}), "ann may not see cost");
    let (_, cy) = list(&site, &conn, "AdminLogEntry", 3).await;
    assert_eq!(changes_of(&cy)["diff"]["cost"], json!({"from": 1, "to": 5}), "cy holds see_cost");
    let (_, di) = list(&site, &conn, "AdminLogEntry", 4).await;
    assert_eq!(changes_of(&di)["diff"]["cost"], json!({"from": 1, "to": 5}), "a superuser sees every field");
}

#[tokio::test]
async fn a_list_edit_drops_the_fields_the_user_may_not_change() {
    let schema = schema();
    let conn: Arc<dyn Executor> = database(&schema).await;
    let config = json!({"resources": [
        {"model": "Product", "list": {"columns": ["name", "cost"], "editable": ["name", "cost"], "order": ["id"]}, "change": {"cost": "change_cost"}},
    ]});
    let site = Arc::new(Site::build(schema.clone(), &config.to_string()).unwrap());
    let seed = [
        "INSERT INTO \"User\" (id, username) VALUES (5, 'eve'), (6, 'fay')",
        "INSERT INTO \"Permission\" (id, label, codename, name) VALUES (4, 'admin', 'manage_product', 'd'), (5, 'admin', 'change_cost', 'e')",
        "INSERT INTO \"UserPermission\" (user_id, permission_id) VALUES (5, 1), (5, 2), (5, 4), (6, 1), (6, 2), (6, 4), (6, 5)",
    ];
    for sql in seed {
        conn.batch(sql.to_string()).await.unwrap();
    }
    let editable = |key: i64| {
        let (site, conn) = (site.clone(), conn.clone());
        async move {
            let (_, body) = call(site, conn, "GET", "/admin/api/resources", Some(json!({"key": key}))).await;
            let body: Value = serde_json::from_str(&body).unwrap();
            body["resources"].as_array().unwrap().iter().find(|r| r["name"] == "Product").unwrap()["editable"].clone()
        }
    };
    assert_eq!(editable(5).await, json!(["name"]), "eve may not change cost");
    assert_eq!(editable(6).await, json!(["name", "cost"]));
    let save = |key: i64, set: Value| {
        let (site, conn) = (site.clone(), conn.clone());
        async move {
            let body = json!({"rows": [{"key": "1", "set": set, "loaded": {"name": "bolt", "cost": 5}}]}).to_string();
            let (status, _, out) = monorm_admin::call_full(site, conn, "PATCH", "/admin/api/resources/Product", Some(json!({"key": key})), None, Some(("application/json", body.into_bytes()))).await;
            (status, serde_json::from_slice::<Value>(&out).unwrap())
        }
    };
    let (status, out) = save(5, json!({"cost": 9})).await;
    assert_eq!((status, &out["error"]["kind"]), (400, &json!("BadRequest")), "{out}");
    assert_eq!(save(6, json!({"cost": 9})).await.0, 200);
    let rows = conn.query_text("SELECT cost FROM \"Product\" WHERE id = 1".into()).await.unwrap();
    assert_eq!(rows[0][0].as_deref(), Some("9"));
}
