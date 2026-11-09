//! Row scope, `can=` rules, the bypass of a read default and a tenant read default that the admin ignores, on SQLite in memory.
use std::sync::{Arc, Mutex};

use monorm_admin::permissions::Grants;
use monorm_admin::{AuthConfig, BoxFuture, BridgeError, CallContext, HostBridge, Security, SecurityOptions, Site};
use monorm_core::{behavior::SCHEMA_CONTRACT, dsl, migrate, schema::Schema};
use monorm_engine::db::{self, Executor};
use serde_json::{json, Value};

const ADMIN: &str = include_str!("../../../apps/admin/schema.prisma");
const SOURCE: &str = r#"
datasource db {
  provider = "sqlite"
}

model Owner {
  id    Int    @id
  name  String
  items Item[]
  @@map("owners")
}

model Item {
  id       Int     @id
  owner_id Int
  title    String
  rank     Int
  hidden   Boolean
  owner    Owner   @relation(fields: [owner_id], references: [id])
  @@map("items")
}
"#;

/// The schema with two named read defaults on `Item`: `visible` hides the hidden rows, and `tenant` keeps the rows of `scope.tenant`.
/// The extensions that normally add them are not needed.
fn schema() -> Arc<Schema> {
    let mut ir = serde_json::to_value(dsl::compile(&format!("{SOURCE}\n{ADMIN}"), None).unwrap()).unwrap();
    ir["behavior"]["schema_contract"] = json!(SCHEMA_CONTRACT);
    ir["behavior"]["read_defaults"] = json!([
        {
            "source": "test-ext", "name": "visible", "model": "Item",
            "filter": {"t": "cmp", "op": "eq", "l": {"t": "col", "path": [], "name": "hidden"}, "r": {"t": "const", "value": false}}
        },
        {
            "source": "test-ext", "name": "tenant", "model": "Item",
            "filter": {"t": "cmp", "op": "eq", "l": {"t": "col", "path": [], "name": "owner_id"}, "r": {"t": "scope", "name": "tenant"}}
        }
    ]);
    Arc::new(Schema::from_ir(serde_json::from_value(ir).unwrap()).unwrap())
}

async fn database() -> Arc<dyn Executor> {
    let conn = db::connect("sqlite://:memory:", 1).await.unwrap();
    for step in migrate::plan(&schema(), &Default::default()).unwrap().up {
        conn.batch(step.sql).await.unwrap();
    }
    conn.batch(
        "INSERT INTO owners (id, name) VALUES (1, 'Ada'), (2, 'Bo');
         INSERT INTO items (id, owner_id, title, rank, hidden) VALUES
           (1, 1, 'one', 1, 0), (2, 1, 'two', 2, 1), (3, 2, 'three', 3, 0), (4, 1, 'four', 4, 0);"
            .into(),
    )
    .await
    .unwrap();
    conn
}

fn site(config: Value) -> Arc<Site> {
    Arc::new(Site::build(schema(), &config.to_string()).unwrap())
}

fn build_error(config: Value) -> String {
    Site::build(schema(), &config.to_string()).err().expect("a build error").to_string()
}

async fn get_as(site: &Arc<Site>, conn: &Arc<dyn Executor>, uri: &str, user: Value, auth: Option<AuthConfig>) -> (u16, Value) {
    let (status, body) = monorm_admin::call_with(site.clone(), conn.clone(), "GET", uri, Some(user), auth).await;
    (status, serde_json::from_str(&body).unwrap())
}

async fn get(site: &Arc<Site>, conn: &Arc<dyn Executor>, uri: &str) -> (u16, Value) {
    get_as(site, conn, uri, json!({"key": 1, "is_superuser": true}), None).await
}

fn titles(body: &Value) -> Vec<String> {
    body["rows"].as_array().unwrap().iter().map(|r| r[0].as_str().unwrap().to_owned()).collect()
}

const ITEMS: &str = "/admin/api/resources/Item";

/// `owner_id = user key`, as a prepared filter.
fn mine() -> Value {
    json!({"filter": {"t": "cmp", "op": "eq", "l": {"t": "col", "path": [], "name": "owner_id"}, "r": {"t": "param", "i": 0}}, "slots": ["user"]})
}

fn items(extra: Value) -> Arc<Site> {
    let mut resource = json!({"model": "Item", "list": {"columns": ["title", "rank"], "order": ["id"], "search": ["title"]}});
    resource.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    site(json!({"resources": [resource, {"model": "Owner", "list": {"columns": ["name"], "order": ["id"]}}]}))
}

#[tokio::test]
async fn the_queryset_filter_limits_the_list_the_count_the_search_and_the_detail() {
    let conn = database().await;
    let site = items(json!({"queryset": mine()}));
    let (status, body) = get(&site, &conn, ITEMS).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(titles(&body), ["one", "four"], "`two` is hidden by the read default, `three` is the other owner's");
    let (_, counted) = get(&site, &conn, "/admin/api/counts/Item?exact=1").await;
    assert_eq!(counted["count"], 2);
    let (_, found) = get(&site, &conn, &format!("{ITEMS}?q=t")).await;
    assert!(titles(&found).is_empty(), "the search stays inside the queryset: `three` is the other owner's");
    assert_eq!(titles(&get(&site, &conn, &format!("{ITEMS}?q=fo")).await.1), ["four"]);
    assert_eq!(get(&site, &conn, &format!("{ITEMS}/1")).await.0, 200);
    assert_eq!(get(&site, &conn, &format!("{ITEMS}/3")).await.0, 404, "a row outside the filter is a 404");
    // Another user key sees another set.
    let (_, other) = get_as(&site, &conn, ITEMS, json!({"key": 2, "is_superuser": true}), None).await;
    assert_eq!(titles(&other), ["three"]);
}

#[tokio::test]
async fn a_parent_outside_the_queryset_has_no_tabs() {
    let conn = database().await;
    let own = json!({"filter": {"t": "cmp", "op": "eq", "l": {"t": "col", "path": [], "name": "id"}, "r": {"t": "param", "i": 0}}, "slots": ["user"]});
    let site = site(json!({"resources": [
        {"model": "Owner", "queryset": own, "list": {"columns": ["name"], "order": ["id"]}},
        {"model": "Item", "queryset": mine(), "list": {"columns": ["title"], "order": ["id"]}}
    ]}));
    let (status, tab) = get(&site, &conn, "/admin/api/resources/Owner/1/items").await;
    assert_eq!(status, 200, "{tab}");
    assert_eq!(titles(&tab), ["one", "four"]);
    assert_eq!(get(&site, &conn, "/admin/api/resources/Owner/2/items").await.0, 404);
    assert_eq!(get(&site, &conn, "/admin/api/counts/Owner/2/items").await.0, 404);
}

#[tokio::test]
async fn the_bypass_on_a_tab_route_applies_to_the_child_and_the_queryset_still_holds() {
    let conn = database().await;
    let site = site(json!({"resources": [
        {"model": "Owner", "list": {"columns": ["name"], "order": ["id"]}},
        {"model": "Item", "queryset": mine(), "list": {"columns": ["title"], "order": ["id"]}}
    ]}));
    let tab = "/admin/api/resources/Owner/1/items";
    assert_eq!(titles(&get(&site, &conn, tab).await.1), ["one", "four"]);
    assert_eq!(titles(&get(&site, &conn, &format!("{tab}?without=visible")).await.1), ["one", "two", "four"]);
    assert_eq!(get(&site, &conn, "/admin/api/counts/Owner/1/items?without=visible&exact=1").await.1["count"], 3);
    assert_eq!(get(&site, &conn, &format!("{tab}?without=nope")).await.0, 400);
}

#[tokio::test]
async fn the_size_probe_counts_the_rows_of_the_scope_and_no_cache_holds_it() {
    let conn = database().await;
    conn.batch(
        "INSERT INTO items (id, owner_id, title, rank, hidden)
         WITH RECURSIVE n(i) AS (SELECT 10 UNION ALL SELECT i + 1 FROM n WHERE i < 50020)
         SELECT i, 1, 'row' || i, i, 0 FROM n;"
            .into(),
    )
    .await
    .unwrap();
    let site = items(json!({"queryset": mine()}));
    let sorted = format!("{ITEMS}?sort=rank");
    let (big, small) = (json!({"key": 1, "is_superuser": true}), json!({"key": 2, "is_superuser": true}));
    let (status, body) = get_as(&site, &conn, &sorted, big.clone(), None).await;
    assert_eq!(status, 400, "the rows of owner 1 are more than 50,000: {body}");
    assert!(body["error"]["message"].as_str().unwrap_or_default().contains("no index"), "{body}");
    let (status, body) = get_as(&site, &conn, &sorted, small.clone(), None).await;
    assert_eq!((status, titles(&body)), (200, vec!["three".to_string()]), "owner 2 has one row, whatever the size of the table: {body}");
    assert_eq!(get_as(&site, &conn, &sorted, big, None).await.0, 400, "the answer of one user is not cached for another");
    assert_eq!(get_as(&site, &conn, &sorted, small, None).await.0, 200);
}

#[tokio::test]
async fn can_rules_are_flags_of_every_row_in_the_list_and_the_detail() {
    let conn = database().await;
    let rank_over = |n: i64| json!({"t": "cmp", "op": "gt", "l": {"t": "col", "path": [], "name": "rank"}, "r": {"t": "int", "value": n}});
    let site = items(json!({"can": {"change": rank_over(1), "delete": rank_over(3)}}));
    let (_, body) = get(&site, &conn, ITEMS).await;
    assert_eq!(body["flags"], json!([{"change": false, "delete": false}, {"change": true, "delete": false}, {"change": true, "delete": true}]));
    assert_eq!(body["rows"][0].as_array().unwrap().len(), 3, "the flags are not cells: two columns and the key");
    let (_, one) = get(&site, &conn, &format!("{ITEMS}/4")).await;
    assert_eq!(one["flags"], json!({"change": true, "delete": true}));
    let (_, meta) = get(&site, &conn, "/admin/api/resources").await;
    let item = meta["resources"].as_array().unwrap().iter().find(|r| r["name"] == "Item").unwrap();
    assert_eq!(item["can"], json!(["change", "delete"]));
    // The flags are selected in the one statement of the list: no host call and no extra query.
}

async fn send(site: &Arc<Site>, conn: &Arc<dyn Executor>, method: &str, uri: &str, body: Value) -> (u16, Value) {
    let user = Some(json!({"key": 1, "is_superuser": true}));
    let (status, out) = monorm_admin::call_body(site.clone(), conn.clone(), method, uri, user, None, Some(&body.to_string())).await;
    (status, serde_json::from_str(&out).unwrap_or(Value::Null))
}

#[tokio::test]
async fn a_write_stays_inside_the_queryset_the_can_rule_and_the_default_and_sets_no_tenant_value() {
    let conn = database().await;
    let rank_over = |n: i64| json!({"t": "cmp", "op": "gt", "l": {"t": "col", "path": [], "name": "rank"}, "r": {"t": "int", "value": n}});
    let site = items(json!({"queryset": mine(), "can": {"change": rank_over(1), "delete": rank_over(3)}}));
    let edit = |title: &str, was: &str| json!({"set": {"title": title}, "loaded": {"title": was}});
    // The `tenant` default reads `scope.tenant`, which the admin never sets: the lock read must not fail on it.
    let (status, out) = send(&site, &conn, "PATCH", &format!("{ITEMS}/4"), edit("FOUR", "four")).await;
    assert_eq!(status, 200, "{out}");
    assert_eq!(send(&site, &conn, "PATCH", &format!("{ITEMS}/3"), edit("x", "three")).await.0, 404, "outside the queryset");
    assert_eq!(send(&site, &conn, "PATCH", &format!("{ITEMS}/2"), edit("x", "two")).await.0, 404, "the `visible` default hides the row");
    assert_eq!(send(&site, &conn, "PATCH", &format!("{ITEMS}/1"), edit("x", "one")).await.0, 403, "the can rule");
    assert_eq!(send(&site, &conn, "DELETE", &format!("{ITEMS}/4"), json!({})).await.0, 200, "rank 4 passes the delete rule");
    let (_, rows) = get(&site, &conn, ITEMS).await;
    assert_eq!(titles(&rows), ["one"]);
    let created = send(&site, &conn, "POST", ITEMS, json!({"set": {"id": 9, "owner_id": 1, "title": "new", "rank": 9, "hidden": false}})).await;
    assert_eq!(created.0, 201, "{}", created.1);
}

#[tokio::test]
async fn the_bypass_permission_switches_a_read_default_off_for_the_list_and_the_detail() {
    let conn = database().await;
    let site = items(json!({}));
    let (_, plain) = get(&site, &conn, ITEMS).await;
    assert_eq!(titles(&plain), ["one", "three", "four"], "the default hides `two`");
    assert_eq!(get(&site, &conn, &format!("{ITEMS}/2")).await.0, 404);
    let (_, open) = get(&site, &conn, &format!("{ITEMS}?without=visible")).await;
    assert_eq!(titles(&open), ["one", "two", "three", "four"]);
    assert_eq!(get(&site, &conn, &format!("{ITEMS}/2?without=visible")).await.0, 200);
    assert_eq!(get(&site, &conn, "/admin/api/counts/Item?without=visible&exact=1").await.1["count"], 4);
    assert_eq!(get(&site, &conn, &format!("{ITEMS}?without=nope")).await.0, 400);
    let (_, meta) = get(&site, &conn, "/admin/api/resources").await;
    let item = meta["resources"].as_array().unwrap().iter().find(|r| r["name"] == "Item").unwrap();
    assert_eq!(item["bypass"], json!(["visible"]));
}

#[tokio::test]
async fn the_bypass_is_a_permission_of_its_own_and_never_lifts_the_queryset() {
    let conn = database().await;
    let site = items(json!({"queryset": mine()}));
    let item = site.resource("Item").unwrap();
    assert!(site.permission_names().contains(&monorm_admin::permissions::bypass_name(item, "visible")));
    let name = monorm_admin::permissions::bypass_name(item, "visible");
    let mut names: std::collections::BTreeSet<String> = site.requires(item, monorm_admin::permissions::Op::Read).into_iter().collect();
    names.insert(name);
    names.insert(monorm_admin::permissions::GATE.to_string());
    let holds = Grants { all: false, names };
    let without = Grants { all: false, names: Default::default() };
    assert_eq!(site.bypassable(&holds, item), ["visible"]);
    assert!(site.bypassable(&without, item).is_empty());
    let (_, open) = get(&site, &conn, &format!("{ITEMS}?without=visible")).await;
    assert_eq!(titles(&open), ["one", "two", "four"], "the queryset still holds: `three` is the other owner's");
}

struct Host {
    seen: Mutex<Vec<(String, Value)>>,
    answer: Value,
}

impl HostBridge for Host {
    fn call(&self, name: &str, payload: Value, ctx: CallContext) -> BoxFuture<Result<Value, BridgeError>> {
        self.seen.lock().unwrap().push((name.to_owned(), payload));
        let answer = self.answer.clone();
        Box::pin(async move { Ok(answer) })
    }
}

fn auth(host: Arc<Host>) -> AuthConfig {
    AuthConfig { security: Security::new(&[7u8; 32], SecurityOptions::default()).unwrap(), bridge: host }
}

#[tokio::test]
async fn a_callable_queryset_is_one_host_call_for_each_request() {
    let conn = database().await;
    let filter = json!({"t": "cmp", "op": "eq", "l": {"t": "col", "path": [], "name": "owner_id"}, "r": {"t": "int", "value": 2}});
    let host = Arc::new(Host { seen: Mutex::new(Vec::new()), answer: json!({"filter": filter}) });
    let site = items(json!({"queryset": {"function": "scope_items"}}));
    let user = json!({"key": 5, "is_superuser": true});
    let (status, body) = get_as(&site, &conn, ITEMS, user.clone(), Some(auth(host.clone()))).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(titles(&body), ["three"]);
    let seen = host.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0, "scope_items");
    assert_eq!(seen[0].1["user"], 5);
}

#[tokio::test]
async fn a_tenant_read_default_is_off_in_every_admin_read() {
    let conn = database().await;
    let site = items(json!({}));
    let (status, body) = get(&site, &conn, ITEMS).await;
    assert_eq!(status, 200, "no missing-scope error: {body}");
    assert_eq!(titles(&body), ["one", "three", "four"], "the rows of both owners; `visible` still hides `two`");
    assert_eq!(get(&site, &conn, &format!("{ITEMS}/3")).await.0, 200);
    assert_eq!(get(&site, &conn, "/admin/api/counts/Item?exact=1").await.1["count"], 3);
    assert_eq!(titles(&get(&site, &conn, &format!("{ITEMS}?q=t")).await.1), ["three"]);
    assert_eq!(get(&site, &conn, &format!("{ITEMS}?without=tenant")).await.0, 400, "the tenant default has no bypass name: it is always off");
    let (_, meta) = get(&site, &conn, "/admin/api/resources").await;
    assert!(meta.get("tenants").is_none());
    let item = meta["resources"].as_array().unwrap().iter().find(|r| r["name"] == "Item").unwrap();
    assert_eq!(item["bypass"], json!(["visible"]));
    let (status, tab) = get(&site, &conn, "/admin/api/resources/Owner/1/items").await;
    assert_eq!(status, 200, "{tab}");
    assert_eq!(titles(&tab), ["one", "four"], "a tab reads the child without the tenant default too");
}

#[test]
fn a_bad_queryset_or_can_rule_is_a_build_error() {
    let bad = |extra: Value| build_error(json!({"resources": [{"model": "Item", "queryset": extra}]}));
    assert!(bad(json!({})).contains("filter"));
    assert!(bad(json!({"filter": {"t": "col", "path": [], "name": "hidden"}, "function": "f"})).contains("function"));
    assert!(bad(json!({"filter": {"t": "cmp", "op": "eq", "l": {"t": "col", "path": [], "name": "owner_id"}, "r": {"t": "param", "i": 0}}})).contains("queryset filter"));
    assert!(bad(json!({"filter": {"t": "col", "path": [], "name": "nope"}})).contains("queryset"));
    let can = |rules: Value| build_error(json!({"resources": [{"model": "Item", "can": rules}]}));
    assert!(can(json!({"fly": {"t": "col", "path": [], "name": "hidden"}})).contains("fly"));
    assert!(can(json!({"change": {"t": "col", "path": [], "name": "rank"}})).contains("bool"));
}
