//! Read-only links: a union relation and a relation with fixed conditions show as links to the related rows, on SQLite in memory.
use std::sync::Arc;

use monorm_admin::Site;
use monorm_core::ir::{RelKind, RelationCondition, RelationIr, SchemaIr};
use monorm_core::{dsl, migrate};
use monorm_engine::db::{self, Executor};
use serde_json::{json, Value};

const ADMIN: &str = include_str!("../../../apps/admin/schema.prisma");
const MODELS: &str = r#"
datasource db {
  provider = "sqlite"
}

model Post {
  id   Int    @id @default(autoincrement())
  name String
}

model Photo {
  id   Int    @id @default(autoincrement())
  name String
}

model Tag {
  id          Int    @id @default(autoincrement())
  name        String
  target_type Int?
  target_id   Int?
}
"#;

fn condition(side: &str, field: &str, value: i64) -> RelationCondition {
    serde_json::from_value(json!({"side": side, "field": field, "value": value})).unwrap()
}

/// `Tag.target` as the union of Post (type 1) and Photo (type 2), and `Post.tags` as the tags of type 1.
fn schema() -> Arc<monorm_core::schema::Schema> {
    let mut ir: SchemaIr = dsl::compile(&format!("{MODELS}\n{ADMIN}"), None).unwrap();
    let mut union = RelationIr::one("target", "Post", "target_id", "id");
    for (model, value) in [("Post", 1), ("Photo", 2)] {
        let mut member = RelationIr::one(&format!("target#{model}"), model, "target_id", "id");
        member.conditions.push(condition("source", "target_type", value));
        union.members.push(member);
    }
    let mut tags = RelationIr::one("tags", "Tag", "id", "target_id");
    tags.kind = RelKind::Many;
    tags.conditions.push(condition("target", "target_type", 1));
    let model = |ir: &mut SchemaIr, name: &str, rel: RelationIr| ir.models.iter_mut().find(|m| m.name == name).unwrap().relations.push(rel);
    model(&mut ir, "Tag", union);
    model(&mut ir, "Post", tags);
    Arc::new(dsl::check(ir).unwrap().1)
}

fn config(extra: Value) -> Value {
    let mut tag = json!({"model": "Tag", "list": {"columns": ["name", "target_id", "target_type"], "order": ["id"]}, "relations": {"target": {}}});
    tag.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    json!({"resources": [
        {"model": "Post", "list": {"columns": ["name"], "order": ["id"]}, "relations": {"tags": {}}},
        {"model": "Photo", "list": {"columns": ["name"], "order": ["id"]}},
        tag,
    ]})
}

async fn world(config: Value) -> (Arc<Site>, Arc<dyn Executor>) {
    let schema = schema();
    let conn = db::connect("sqlite://:memory:", 1).await.unwrap();
    for step in migrate::plan(&schema, &Default::default()).unwrap().up {
        conn.batch(step.sql).await.unwrap();
    }
    conn.batch("INSERT INTO \"Post\" (id, name) VALUES (1, 'Hello'), (2, 'Other'); INSERT INTO \"Photo\" (id, name) VALUES (1, 'Sunset'); INSERT INTO \"Tag\" (id, name, target_type, target_id) VALUES (1, 'red', 1, 1), (2, 'blue', 1, 1), (3, 'warm', 2, 1), (4, 'none', NULL, NULL), (5, 'gone', 1, 9)".into()).await.unwrap();
    (Arc::new(Site::build(schema, &config.to_string()).unwrap()), conn)
}

async fn get(site: &Arc<Site>, conn: &Arc<dyn Executor>, uri: &str) -> (u16, Value) {
    let (status, out) = monorm_admin::call_body(site.clone(), conn.clone(), "GET", &format!("/admin/api{uri}"), Some(json!({"key": 1, "is_superuser": true})), None, None).await;
    (status, serde_json::from_str(&out).unwrap_or(Value::Null))
}

fn items(body: &Value) -> Vec<(String, String)> {
    body["items"].as_array().unwrap().iter().map(|i| (i["resource"].as_str().unwrap().to_owned(), i["label"].as_str().unwrap().to_owned())).collect()
}

#[tokio::test]
async fn a_union_link_leads_to_the_row_that_the_type_selects() {
    let (site, conn) = world(config(json!({}))).await;
    let (status, body) = get(&site, &conn, "/links/Tag/1/target").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(items(&body), [("Post".to_owned(), "Hello".to_owned())]);
    let (_, body) = get(&site, &conn, "/links/Tag/3/target").await;
    assert_eq!(items(&body), [("Photo".to_owned(), "Sunset".to_owned())]);
    // No type, or a row that is gone, leaves no link.
    for key in [4, 5] {
        let (status, body) = get(&site, &conn, &format!("/links/Tag/{key}/target")).await;
        assert_eq!((status, items(&body).len()), (200, 0), "{body}");
    }
    let (status, _) = get(&site, &conn, "/links/Tag/99/target").await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn a_conditioned_to_many_link_lists_the_rows_that_match() {
    let (site, conn) = world(config(json!({}))).await;
    let (status, body) = get(&site, &conn, "/links/Post/1/tags").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(items(&body), [("Tag".to_owned(), "red".to_owned()), ("Tag".to_owned(), "blue".to_owned())]);
    let (_, body) = get(&site, &conn, "/links/Post/2/tags").await;
    assert!(items(&body).is_empty());
}

#[tokio::test]
async fn a_link_has_no_picker_and_the_meta_names_it() {
    let (site, conn) = world(config(json!({}))).await;
    let (status, _) = get(&site, &conn, "/autocomplete/Tag/target").await;
    assert_eq!(status, 404);
    let (_, meta) = get(&site, &conn, "/site").await;
    let tag = meta["resources"].as_array().unwrap().iter().find(|r| r["name"] == "Tag").unwrap();
    assert_eq!(tag["linked"], json!([{"name": "target", "title": "Target", "many": false}]));
    let column = tag["fields"].as_array().unwrap().iter().find(|c| c["label"] == "target_id").unwrap();
    assert_eq!(column["ref_by"], json!({"column": "target_type", "targets": {"1": "Post", "2": "Photo"}}));
}

#[test]
fn a_link_refuses_a_picker_option_and_a_write() {
    let build = |extra: Value| {
        let mut config = config(json!({}));
        config["resources"][2]["relations"]["target"] = extra;
        Site::build(schema(), &config.to_string()).err().map(|e| e.to_string()).unwrap_or_default()
    };
    assert!(build(json!({"create": true})).contains("create"));
    assert!(build(json!({"label": "name"})).contains("label"));
    assert!(build(json!({})).is_empty());
}
