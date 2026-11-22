//! The `dates` route of the date drill-down: range, counts per period, filters, row scope, field permissions and the statement count.
use std::sync::Arc;

use monorm_admin::{call, Site};
use monorm_core::{dsl, migrate, schema::Schema};
use monorm_engine::db::{self, Executor};
use monorm_engine::trace::{self, Trace};
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
        "datasource db {{\n  provider = \"sqlite\"\n}}\n\nmodel Event {{\n  id       Int       @id\n  title    String\n  owner_id Int\n  at       DateTime\n  day      DateTime? @db.Date\n  seen     DateTime?\n  @@index([at])\n  @@map(\"events\")\n}}\n{AUTH}\n{ADMIN}"
    );
    Arc::new(dsl::check(dsl::compile(&source, None).unwrap()).unwrap().1)
}

async fn database() -> Arc<dyn Executor> {
    let schema = schema();
    let conn = db::connect("sqlite://:memory:", 1).await.unwrap();
    for step in migrate::plan(&schema, &Default::default()).unwrap().up {
        conn.batch(step.sql).await.unwrap();
    }
    conn.batch(
        "INSERT INTO events (id, title, owner_id, at, day, seen) VALUES
           (1, 'one', 1, '2026-01-02T03:04:05Z', '2026-01-02', NULL),
           (2, 'two', 1, '2026-02-01T00:00:00Z', '2026-02-01', NULL),
           (3, 'three', 2, '2026-03-01T00:00:00Z', NULL, NULL),
           (4, 'four', 1, '2026-03-15T12:00:00Z', '2026-03-15', NULL),
           (5, 'five', 1, '2026-03-31T22:30:00Z', '2026-03-31', NULL);
         INSERT INTO \"User\" (id, username, is_superuser) VALUES (1, 'ann', 0), (2, 'bob', 0);
         INSERT INTO \"Permission\" (id, label, codename, name) VALUES (1, 'admin', 'access', 'a'), (2, 'admin', 'read_event', 'b'), (3, 'admin', 'see_at', 'c');
         INSERT INTO \"UserPermission\" (user_id, permission_id) VALUES (1, 1), (1, 2), (2, 1), (2, 2), (2, 3);"
            .into(),
    )
    .await
    .unwrap();
    conn
}

fn mine() -> Value {
    json!({"filter": {"t": "cmp", "op": "eq", "l": {"t": "col", "path": [], "name": "owner_id"}, "r": {"t": "param", "i": 0}}, "slots": ["user"]})
}

fn site(extra: Value) -> Arc<Site> {
    let mut resource = json!({"model": "Event", "list": {"columns": ["title"], "order": ["id"], "search": ["title"], "date_hierarchy": "at"}});
    resource.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    Arc::new(Site::build(schema(), &json!({"resources": [resource]}).to_string()).unwrap())
}

async fn get(site: &Arc<Site>, conn: &Arc<dyn Executor>, uri: &str, user: Value) -> (u16, Value, usize) {
    let trace = Trace::default();
    let wrapped = trace::wrap(conn.clone(), Some(&trace));
    let (status, body) = call(site.clone(), wrapped, "GET", uri, Some(user)).await;
    (status, serde_json::from_str(&body).unwrap(), trace.take().len())
}

async fn dates(site: &Arc<Site>, conn: &Arc<dyn Executor>, query: &str) -> (u16, Value, usize) {
    get(site, conn, &format!("/admin/api/dates/Event?{query}"), json!({"key": 1, "is_superuser": true})).await
}

const MONTHS: &str = "2026-01-01T00:00:00Z,2026-02-01T00:00:00Z,2026-03-01T00:00:00Z,2026-04-01T00:00:00Z";

#[tokio::test]
async fn the_range_is_one_statement_and_follows_the_filters_and_the_search() {
    let conn = database().await;
    let site = site(json!({}));
    let (status, body, statements) = dates(&site, &conn, "").await;
    assert_eq!((status, statements), (200, 1), "{body}");
    assert_eq!((body["min"].as_str(), body["max"].as_str()), (Some("2026-01-02T03:04:05+00:00"), Some("2026-03-31T22:30:00+00:00")));
    let (_, body, _) = dates(&site, &conn, "q=th").await;
    assert_eq!((body["min"].as_str(), body["max"].as_str()), (Some("2026-03-01T00:00:00+00:00"), Some("2026-03-01T00:00:00+00:00")), "the search narrows the range");
    let (_, body, _) = dates(&site, &conn, "f.at:gte=2026-02-01T00:00:00Z").await;
    assert_eq!(body["min"], "2026-02-01T00:00:00+00:00", "a filter narrows the range");
    let (status, body, statements) = dates(&site, &conn, "q=nothing").await;
    assert_eq!((status, &body["min"], &body["max"]), (200, &Value::Null, &Value::Null));
    assert!(statements <= 1);
}

#[tokio::test]
async fn counts_per_month_use_the_edges_of_the_browser_zone_and_one_statement() {
    let conn = database().await;
    let site = site(json!({}));
    let (status, body, statements) = dates(&site, &conn, &format!("edges={MONTHS}")).await;
    assert_eq!((status, statements), (200, 1), "{body}");
    assert_eq!(body["counts"], json!([1, 1, 3]));
    // Edges at %2B02:00: the row at 22:30Z on March 31 is April 1 locally.
    let local = "2026-03-01T00:00:00%2B02:00,2026-04-01T00:00:00%2B02:00,2026-05-01T00:00:00%2B02:00";
    let (_, body, statements) = dates(&site, &conn, &format!("edges={local}")).await;
    assert_eq!((statements, &body["counts"]), (1, &json!([2, 1])), "{body}");
    let (_, body, _) = dates(&site, &conn, &format!("edges={MONTHS}&q=o")).await;
    assert_eq!(body["counts"], json!([1, 1, 1]), "the search applies: `one`, `two` and `four`");
}

#[tokio::test]
async fn counts_of_a_date_field_use_plain_dates_and_a_null_is_in_no_period() {
    let conn = database().await;
    let site = site(json!({"list": {"columns": ["title"], "order": ["id"], "date_hierarchy": "day"}}));
    let (status, body, statements) = dates(&site, &conn, "edges=2026-01-01,2026-02-01,2026-03-01,2026-04-01").await;
    assert_eq!((status, statements), (200, 1), "{body}");
    assert_eq!(body["counts"], json!([1, 1, 2]), "row 3 has no day");
    let (_, body, _) = dates(&site, &conn, "").await;
    assert_eq!((body["min"].as_str(), body["max"].as_str()), (Some("2026-01-02"), Some("2026-03-31")));
}

#[tokio::test]
async fn a_bad_edge_list_answers_400() {
    let conn = database().await;
    let site = site(json!({}));
    let sixty_two = (0..62).map(|i| format!("{}-01-01T00:00:00Z", 1990 + i)).collect::<Vec<_>>().join(",");
    let sixty_one = (0..61).map(|i| format!("{}-01-01T00:00:00Z", 1990 + i)).collect::<Vec<_>>().join(",");
    for (what, query) in [
        ("equal edges", "edges=2026-01-01T00:00:00Z,2026-01-01T00:00:00Z".to_owned()),
        ("falling edges", "edges=2026-02-01T00:00:00Z,2026-01-01T00:00:00Z".to_owned()),
        ("one edge", "edges=2026-01-01T00:00:00Z".to_owned()),
        ("a bad instant", "edges=2026-01-01T00:00:00Z,yesterday".to_owned()),
        ("a date without a time", "edges=2026-01-01,2026-02-01".to_owned()),
        ("62 edges", format!("edges={sixty_two}")),
    ] {
        let (status, body, _) = dates(&site, &conn, &query).await;
        assert_eq!(status, 400, "{what}: {body}");
    }
    assert_eq!(dates(&site, &conn, &format!("edges={sixty_one}")).await.0, 200, "61 edges are the most");
    let (status, _, _) = dates(&site, &conn, "edges=2026-13-01,2026-14-01").await;
    assert_eq!(status, 400);
    let date_site = {
        let list = json!({"columns": ["title"], "order": ["id"], "date_hierarchy": "day"});
        site_with_list(list)
    };
    assert_eq!(dates(&date_site, &conn, "edges=2026-01-01T00:00:00Z,2026-02-01T00:00:00Z").await.0, 400, "an instant for a date field");
}

fn site_with_list(list: Value) -> Arc<Site> {
    site(json!({"list": list}))
}

#[tokio::test]
async fn a_resource_without_date_hierarchy_answers_404() {
    let conn = database().await;
    let site = Arc::new(Site::build(schema(), &json!({"resources": [{"model": "Event", "list": {"columns": ["title"], "order": ["id"]}}]}).to_string()).unwrap());
    assert_eq!(dates(&site, &conn, "").await.0, 404);
}

#[tokio::test]
async fn the_row_scope_limits_the_range_and_the_counts() {
    let conn = database().await;
    let site = site(json!({"queryset": mine()}));
    let (_, body, _) = get(&site, &conn, &format!("/admin/api/dates/Event?edges={MONTHS}"), json!({"key": 2, "is_superuser": true})).await;
    assert_eq!(body["counts"], json!([0, 0, 1]), "owner 2 holds one row");
    let (_, body, _) = get(&site, &conn, "/admin/api/dates/Event", json!({"key": 2, "is_superuser": true})).await;
    assert_eq!((body["min"].as_str(), body["max"].as_str()), (Some("2026-03-01T00:00:00+00:00"), Some("2026-03-01T00:00:00+00:00")));
}

#[tokio::test]
async fn a_hidden_date_field_answers_403() {
    let conn = database().await;
    let site = site(json!({"see": {"at": "see_at"}}));
    let ask = |key: i64| {
        let (site, conn) = (site.clone(), conn.clone());
        async move { get(&site, &conn, "/admin/api/dates/Event", json!({"key": key})).await }
    };
    assert_eq!(ask(1).await.0, 403, "ann lacks see_at");
    let (status, body, _) = ask(2).await;
    assert_eq!(status, 200, "{body}");
}
