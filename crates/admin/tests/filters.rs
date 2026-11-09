//! Filters, computed and aggregate columns and to-many tabs on a real database.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use monorm_admin::{AuthConfig, BoxFuture, BridgeError, CallContext, HostBridge, Security, SecurityOptions, Site};
use monorm_core::{dsl, migrate, schema::Schema};
use monorm_engine::db::{self, Executor};
use monorm_engine::trace::{self, Trace};
use serde_json::{json, Value};

const SOURCE: &str = r#"
datasource db {
  provider = "sqlite"
}

enum Level {
  low  @value(1)
  high @value(10)

  @@storage(int)
}

model Author {
  id       Int       @id
  name     String
  active   Boolean
  nick     String?
  comments Comment[]
  @@map("authors")
}

model Comment {
  id        Int      @id
  body      String
  author_id Int?
  level     Level
  rank      Int
  at        DateTime
  author    Author?  @relation(fields: [author_id], references: [id])
  @@map("comments")
}
"#;

fn schema() -> Arc<Schema> {
    Arc::new(dsl::check(dsl::compile(SOURCE, None).unwrap()).unwrap().1)
}

async fn database() -> Arc<dyn Executor> {
    let conn = db::connect("sqlite://:memory:", 1).await.unwrap();
    for sql in migrate::create_statements(&migrate::snapshot(&schema()).unwrap()) {
        conn.batch(sql).await.unwrap();
    }
    conn.batch(
        "INSERT INTO authors (id, name, active, nick) VALUES (1, 'Ada', 1, NULL), (2, 'Bo', 0, ''), (3, 'Cy', 1, 'cy');
         INSERT INTO comments (id, body, author_id, level, rank, at) VALUES
           (1, 'one', 1, 10, 1, '2026-01-02T03:04:05Z'),
           (2, 'two', NULL, 1, 1, '2026-02-01T00:00:00Z'),
           (3, 'three', 2, 10, 2, '2026-03-01T00:00:00Z'),
           (4, 'four', 1, 1, 3, '2026-03-15T12:00:00Z');"
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

async fn get(site: &Arc<Site>, conn: &Arc<dyn Executor>, uri: &str) -> (u16, Value, Vec<String>) {
    let trace = Trace::default();
    let wrapped = trace::wrap(conn.clone(), Some(&trace));
    let (status, body) = monorm_admin::call(site.clone(), wrapped, "GET", uri, Some(json!({"is_superuser": true}))).await;
    (status, serde_json::from_str(&body).unwrap(), trace.take().into_iter().map(|e| e.sql).collect())
}

fn column(body: &Value, i: usize) -> Vec<Value> {
    body["rows"].as_array().unwrap().iter().map(|r| r[i].clone()).collect()
}

fn bodies(body: &Value) -> Vec<String> {
    column(body, 0).into_iter().map(|v| v.as_str().unwrap().to_owned()).collect()
}

const COMMENTS: &str = "/admin/api/resources/Comment";
const AUTHORS: &str = "/admin/api/resources/Author";

fn comments(extra: Value) -> Arc<Site> {
    let mut list = json!({"columns": ["body"], "order": ["id"], "filters": ["level", "rank", "at", "author", "author.name", "author.active"]});
    for (k, v) in extra.as_object().unwrap() {
        list[k] = v.clone();
    }
    site(json!({"resources": [{"model": "Comment", "list": list}, {"model": "Author", "list": {"columns": ["name", "active", "nick"], "order": ["id"], "filters": ["active", "nick", "name"]}}]}))
}

#[tokio::test]
async fn an_enum_a_bool_and_a_field_of_a_related_model_filter_and_several_values_mean_or() {
    let conn = database().await;
    let site = comments(json!({}));
    let ask = |q: &str| {
        let uri = format!("{COMMENTS}?{q}");
        let (site, conn) = (site.clone(), conn.clone());
        async move { get(&site, &conn, &uri).await }
    };
    assert_eq!(bodies(&ask("f.level=high").await.1), ["one", "three"], "an enum label");
    assert_eq!(bodies(&ask("f.level=high&f.level=low").await.1), ["one", "two", "three", "four"], "several values");
    assert_eq!(bodies(&ask("f.author.active=true").await.1), ["one", "four"], "a bool of the related model");
    assert_eq!(bodies(&ask("f.author.name=Bo").await.1), ["three"]);
    assert_eq!(bodies(&ask("f.level=high&f.author.name=Ada").await.1), ["one"], "filters are and");
    let (_, body, sql) = ask("f.author.name=Bo").await;
    assert_eq!(sql.len(), 1, "{sql:?}");
    assert!(body["rows"].as_array().unwrap().len() == 1);
    let (status, body, _) = get(&site, &conn, &format!("{AUTHORS}?f.active=false")).await;
    assert_eq!((status, column(&body, 0)), (200, vec![json!("Bo")]));
}

#[tokio::test]
async fn a_range_and_a_relation_filter_and_the_empty_operators() {
    let conn = database().await;
    let site = comments(json!({}));
    let ask = |q: &str| {
        let uri = format!("{COMMENTS}?{q}");
        let (site, conn) = (site.clone(), conn.clone());
        async move { get(&site, &conn, &uri).await }
    };
    assert_eq!(bodies(&ask("f.rank:gte=2").await.1), ["three", "four"]);
    assert_eq!(bodies(&ask("f.rank:gt=1&f.rank:lt=3").await.1), ["three"]);
    assert_eq!(bodies(&ask("f.at:gte=2026-03-01&f.at:lt=2026-03-02").await.1), ["three"], "a date is midnight UTC");
    assert_eq!(bodies(&ask("f.at:gte=2026-03-15T12:00:00Z").await.1), ["four"]);
    assert_eq!(bodies(&ask("f.author=1").await.1), ["one", "four"], "a relation takes the key of the target");
    assert_eq!(bodies(&ask("f.author=1&f.author=2").await.1), ["one", "three", "four"]);
    assert_eq!(bodies(&ask("f.author:empty").await.1), ["two"]);
    assert_eq!(bodies(&ask("f.author:notempty").await.1), ["one", "three", "four"]);
    let (_, body, _) = get(&site, &conn, &format!("{AUTHORS}?f.nick:empty")).await;
    assert_eq!(column(&body, 0), [json!("Ada"), json!("Bo")], "a text is empty when it is null or ''");
    let (_, body, _) = get(&site, &conn, &format!("{AUTHORS}?f.nick:notempty")).await;
    assert_eq!(column(&body, 0), [json!("Cy")]);
}

#[tokio::test]
async fn a_wrong_value_is_422_and_an_unknown_filter_is_400() {
    let conn = database().await;
    let site = comments(json!({}));
    for q in ["f.level=middle", "f.rank=x", "f.rank:gte=1&f.rank:gte=2", "f.at=yesterday", "f.author=x", "f.level:gt=1", "f.level="] {
        let (status, body, sql) = get(&site, &conn, &format!("{COMMENTS}?{q}")).await;
        assert_eq!(status, 422, "{q}: {body}");
        assert_eq!(body["error"]["kind"], "Invalid", "{q}");
        assert!(sql.is_empty(), "{q}: nothing ran");
    }
    for q in ["f.body=one", "n.nothing=x", "f.rank:between=1"] {
        let (status, _, _) = get(&site, &conn, &format!("{COMMENTS}?{q}")).await;
        assert_eq!(status, 400, "{q}");
    }
    let (status, _, _) = get(&site, &conn, &format!("{AUTHORS}?f.active:empty")).await;
    assert_eq!(status, 422, "a column that is never empty");
}

#[test]
fn an_unknown_or_unfit_filter_field_is_a_build_error() {
    let one = |filters: Value| build_error(json!({"resources": [{"model": "Comment", "list": {"filters": filters}}]}));
    assert!(one(json!(["nope"])).contains("Comment") && one(json!(["nope"])).contains("\"nope\""));
    assert!(one(json!(["author.nope"])).contains("author.nope"));
    let e = one(json!(["author.comments"]));
    assert!(e.contains("comments"), "{e}");
    // The target of a relation filter is a resource.
    assert!(build_error(json!({"resources": [{"model": "Comment", "list": {"filters": ["author"]}}]})).contains("Author"));
}

#[tokio::test]
async fn a_named_filter_has_a_default_and_an_all_choice() {
    let conn = database().await;
    let named = json!([{
        "name": "recent", "label": "Age", "default": "march",
        "choices": [
            {"name": "march", "label": "Rank two or more", "expr": {"t": "cmp", "op": "ge", "l": {"t": "col", "path": [], "name": "rank"}, "r": {"t": "int", "value": 2}}},
            {"name": "first", "label": "Rank one", "expr": {"t": "cmp", "op": "eq", "l": {"t": "col", "path": [], "name": "rank"}, "r": {"t": "int", "value": 1}}}
        ]
    }]);
    let site = comments(json!({"named_filters": named}));
    let ask = |q: &str| {
        let uri = format!("{COMMENTS}{q}");
        let (site, conn) = (site.clone(), conn.clone());
        async move { get(&site, &conn, &uri).await }
    };
    assert_eq!(bodies(&ask("").await.1), ["three", "four"], "the default applies");
    assert_eq!(bodies(&ask("?n.recent=first").await.1), ["one", "two"]);
    assert_eq!(bodies(&ask("?n.recent=all").await.1), ["one", "two", "three", "four"]);
    assert_eq!(ask("?n.recent=nope").await.0, 422);
    let (_, meta, _) = get(&site, &conn, "/admin/api/resources").await;
    let r = meta["resources"].as_array().unwrap().iter().find(|r| r["name"] == "Comment").unwrap();
    assert_eq!(r["named_filters"][0]["default"], "march");
    assert_eq!(r["named_filters"][0]["choices"][1], json!({"name": "first", "title": "Rank one"}));
    let (_, count, _) = get(&site, &conn, "/admin/api/counts/Comment").await;
    assert_eq!(count["count"], 2, "the count uses the default too");
}

#[tokio::test]
async fn an_aggregate_over_a_relation_is_one_statement() {
    let conn = database().await;
    let list = json!({"columns": ["name", "comment_count"], "order": ["id"], "aggregates": [{"name": "comment_count", "label": "Comments", "relation": "comments", "fn": "count"}]});
    let site = site(json!({"resources": [{"model": "Author", "list": list}, {"model": "Comment"}]}));
    let (status, body, sql) = get(&site, &conn, AUTHORS).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(sql.len(), 1, "no N+1: {sql:?}");
    assert_eq!(column(&body, 1), [json!("2"), json!("1"), json!("0")], "a count is a bigint: a string");
    assert_eq!(body["columns"][1]["title"], "Comments");
    assert_eq!(body["columns"][1]["computed"], true);
}

#[tokio::test]
async fn a_computed_expression_column_is_in_the_query_and_sorts_when_the_config_allows() {
    let conn = database().await;
    let computed = json!([{"name": "body_length", "type": "int", "nullable": false, "sortable": true, "expr": {"t": "func", "name": "length", "args": [{"t": "col", "path": [], "name": "body"}]}}]);
    let site = comments(json!({"columns": ["body", "body_length"], "computed": computed}));
    let (_, body, sql) = get(&site, &conn, &format!("{COMMENTS}?sort=body_length")).await;
    assert_eq!(sql.len(), 1);
    assert_eq!(bodies(&body), ["one", "two", "four", "three"], "ties end in the key");
    assert_eq!(column(&body, 1), [json!(3), json!(3), json!(4), json!(5)]);
    let (_, desc, _) = get(&site, &conn, &format!("{COMMENTS}?sort=-body_length")).await;
    assert_eq!(bodies(&desc), ["three", "four", "one", "two"].map(String::from).to_vec().into_iter().collect::<Vec<_>>());
    // Not sortable unless the config says so.
    let plain = comments(json!({"columns": ["body", "body_length"], "computed": [{"name": "body_length", "type": "int", "expr": {"t": "func", "name": "length", "args": [{"t": "col", "path": [], "name": "body"}]}}]}));
    assert_eq!(get(&plain, &conn, &format!("{COMMENTS}?sort=body_length")).await.0, 400);
}

#[test]
fn a_computed_column_with_the_wrong_type_or_a_name_clash_is_a_build_error() {
    let len = json!({"t": "func", "name": "length", "args": [{"t": "col", "path": [], "name": "body"}]});
    let one = |c: Value| build_error(json!({"resources": [{"model": "Comment", "list": {"computed": [c]}}]}));
    assert!(one(json!({"name": "n", "type": "text", "expr": len})).contains("declared"));
    assert!(one(json!({"name": "body", "type": "int", "expr": len})).contains("name of a field"));
    assert!(one(json!({"name": "n", "type": "int"})).contains("expr or function"));
    assert!(one(json!({"name": "n", "type": "int", "function": "f", "sortable": true})).contains("cannot sort"));
}

struct Batch {
    calls: AtomicUsize,
    seen: Mutex<Vec<Value>>,
    answer: Mutex<Value>,
}

impl HostBridge for Batch {
    fn call(&self, name: &str, payload: Value, _: CallContext) -> BoxFuture<Result<Value, BridgeError>> {
        assert_eq!(name, "double_rank");
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen.lock().unwrap().push(payload["keys"].clone());
        let answer = self.answer.lock().unwrap().clone();
        let n = payload["keys"].as_array().unwrap().len();
        let value = if answer.is_null() { Value::Array((0..n).map(|i| json!(i as i64 * 2)).collect()) } else { answer };
        Box::pin(async move { Ok(value) })
    }
}

#[tokio::test]
async fn a_batch_function_column_runs_once_for_the_page() {
    let conn = database().await;
    let computed = json!([{"name": "doubled", "type": "int", "nullable": false, "function": "double_rank"}]);
    let site = comments(json!({"columns": ["body", "doubled"], "computed": computed, "page_size": 3}));
    let host = Arc::new(Batch { calls: AtomicUsize::new(0), seen: Mutex::new(Vec::new()), answer: Mutex::new(Value::Null) });
    let auth = AuthConfig { security: Security::new(&[7u8; 32], SecurityOptions::default()).unwrap(), bridge: host.clone() };
    let ask = |q: &str| {
        let site = site.clone();
        let (conn, auth) = (conn.clone(), auth.clone());
        let uri = format!("{COMMENTS}{q}");
        async move {
            let (status, body) = monorm_admin::call_with(site, conn, "GET", &uri, Some(json!({"is_superuser": true})), Some(auth)).await;
            (status, serde_json::from_str::<Value>(&body).unwrap())
        }
    };
    let (status, body) = ask("").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(host.calls.load(Ordering::SeqCst), 1, "one call for three rows");
    assert_eq!(host.seen.lock().unwrap()[0], json!([[1], [2], [3]]));
    assert_eq!(column(&body, 1), [json!(0), json!(2), json!(4)]);
    // A value that does not fit the declared type is a server error, not a wrong cell.
    *host.answer.lock().unwrap() = json!(["x", "y", "z"]);
    assert_eq!(ask("").await.0, 500);
    *host.answer.lock().unwrap() = json!([1]);
    assert_eq!(ask("").await.0, 500, "one value for each key");
    // A batch column is neither sortable nor filterable.
    assert_eq!(ask("?sort=doubled").await.0, 400);
}

#[tokio::test]
async fn a_to_many_tab_lists_the_children_of_one_record_with_the_child_list_features() {
    let conn = database().await;
    let site = comments(json!({"search": ["body"], "page_size": 10}));
    let (status, body, sql) = get(&site, &conn, "/admin/api/resources/Author/1/comments").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(bodies(&body), ["one", "four"]);
    assert_eq!(sql.len(), 2, "the parent row, then the children: {sql:?}");
    assert_eq!(bodies(&get(&site, &conn, "/admin/api/resources/Author/1/comments?f.level=high").await.1), ["one"], "filters");
    assert_eq!(bodies(&get(&site, &conn, "/admin/api/resources/Author/1/comments?q=fou").await.1), ["four"], "search");
    assert_eq!(bodies(&get(&site, &conn, "/admin/api/resources/Author/1/comments?sort=-id").await.1), ["four", "one"], "sort");
    let (_, count, _) = get(&site, &conn, "/admin/api/counts/Author/1/comments").await;
    assert_eq!(count["count"], 2);
    assert_eq!(get(&site, &conn, "/admin/api/resources/Author/99/comments").await.0, 404, "no such parent");
    assert_eq!(get(&site, &conn, "/admin/api/resources/Author/1/nothing").await.0, 404, "no such tab");
    let (_, meta, _) = get(&site, &conn, "/admin/api/resources").await;
    let author = meta["resources"].as_array().unwrap().iter().find(|r| r["name"] == "Author").unwrap();
    assert_eq!(author["tabs"], json!([{"name": "comments", "title": "Comments", "target": "Comment"}]));
}

#[tokio::test]
async fn labels_and_empty_texts_reach_the_metadata() {
    let conn = database().await;
    let site = comments(json!({"columns": ["body", "author.name"], "labels": {"body": "Text"}, "empty": "none", "empty_fields": {"author.name": "no author"}}));
    let (_, meta, _) = get(&site, &conn, "/admin/api/resources").await;
    let r = meta["resources"].as_array().unwrap().iter().find(|r| r["name"] == "Comment").unwrap();
    assert_eq!(r["columns"][0]["title"], "Text");
    assert_eq!(r["columns"][0]["empty"], "none");
    assert_eq!(r["columns"][1]["empty"], "no author");
    let err = build_error(json!({"resources": [{"model": "Comment", "list": {"labels": {"nope": "x"}}}]}));
    assert!(err.contains("nope"), "{err}");
}

/// The same filters on PostgreSQL: a native enum, a boolean, a timestamp and a count bind with their own types.
#[tokio::test]
async fn the_filters_bind_their_types_on_postgres() {
    let Ok(url) = std::env::var("ORM_RUST_TEST_POSTGRES_URL") else {
        eprintln!("skipped: ORM_RUST_TEST_POSTGRES_URL is not set");
        return;
    };
    let source = r#"
datasource db {
  provider = "postgresql"
}

enum Mood {
  calm
  tense
}

model FAuthor {
  id       Int        @id
  name     String
  active   Boolean
  comments FComment[]
  @@map("f_authors")
}

model FComment {
  id        Int      @id
  body      String
  mood      Mood
  rank      BigInt
  at        DateTime
  author_id Int?
  author    FAuthor? @relation(fields: [author_id], references: [id])
  @@map("f_comments")
}
"#;
    let schema = Arc::new(dsl::check(dsl::compile(source, None).unwrap()).unwrap().1);
    let conn = db::connect(&url, 2).await.unwrap();
    conn.batch("DROP TABLE IF EXISTS f_comments; DROP TABLE IF EXISTS f_authors; DROP TYPE IF EXISTS mood".into()).await.unwrap();
    for step in migrate::plan(&schema, &Default::default()).unwrap().up {
        conn.batch(step.sql).await.unwrap();
    }
    conn.batch(
        "INSERT INTO f_authors (id, name, active) VALUES (1, 'Ada', true), (2, 'Bo', false);
         INSERT INTO f_comments (id, body, mood, rank, at, author_id) VALUES
           (1, 'one', 'calm', 1, '2026-01-02T03:04:05Z', 1),
           (2, 'two', 'tense', 2, '2026-03-01T00:00:00Z', NULL),
           (3, 'three', 'tense', 3, '2026-03-15T12:00:00Z', 2);"
            .into(),
    )
    .await
    .unwrap();
    let list = json!({"columns": ["body"], "order": ["id"], "filters": ["mood", "rank", "at", "author", "author.active"], "aggregates": []});
    let authors = json!({"columns": ["name", "n"], "order": ["id"], "aggregates": [{"name": "n", "relation": "comments", "fn": "count"}]});
    let site = Arc::new(Site::build(schema, &json!({"resources": [{"model": "FComment", "list": list}, {"model": "FAuthor", "list": authors}]}).to_string()).unwrap());
    let ask = |uri: &str| {
        let (site, uri) = (site.clone(), uri.to_owned());
        let conn: Arc<dyn Executor> = conn.clone();
        async move { get(&site, &conn, &uri).await }
    };
    let base = "/admin/api/resources/FComment";
    assert_eq!(bodies(&ask(&format!("{base}?f.mood=tense")).await.1), ["two", "three"], "a native enum");
    assert_eq!(bodies(&ask(&format!("{base}?f.mood=calm&f.mood=tense")).await.1), ["one", "two", "three"]);
    assert_eq!(bodies(&ask(&format!("{base}?f.rank:gte=2")).await.1), ["two", "three"], "a bigint");
    assert_eq!(bodies(&ask(&format!("{base}?f.at:gte=2026-03-01&f.at:lt=2026-03-15")).await.1), ["two"], "a timestamp");
    assert_eq!(bodies(&ask(&format!("{base}?f.author.active=false")).await.1), ["three"], "a related boolean");
    assert_eq!(bodies(&ask(&format!("{base}?f.author:empty")).await.1), ["two"]);
    let (status, body, sql) = ask("/admin/api/resources/FAuthor").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(sql.iter().filter(|s| s.contains("f_comments")).count(), 1, "no N+1: {sql:?}");
    assert_eq!(column(&body, 1), [json!("1"), json!("1")]);
    let (status, body, _) = ask("/admin/api/resources/FAuthor/1/comments").await;
    assert_eq!((status, bodies(&body)), (200, vec!["one".to_owned()]), "a tab");
    assert_eq!(ask(&format!("{base}?f.mood=angry")).await.0, 422);
}

#[tokio::test]
async fn every_way_of_paging_keeps_a_filter_and_a_computed_sort() {
    let conn = database().await;
    let computed = json!([{"name": "body_length", "type": "int", "nullable": false, "sortable": true, "expr": {"t": "func", "name": "length", "args": [{"t": "col", "path": [], "name": "body"}]}}]);
    let site = comments(json!({"columns": ["body", "body_length"], "computed": computed, "page_size": 2}));
    // Level high keeps one, three and four's neighbours: ids 1 and 3.
    let query = format!("{COMMENTS}?sort=-body_length&f.level=high&limit=1");
    let (status, first, _) = get(&site, &conn, &query).await;
    assert_eq!(status, 200, "{first}");
    assert_eq!(bodies(&first), ["three"]);
    let next = first["next"].as_str().expect("a next cursor");
    let (_, second, _) = get(&site, &conn, &format!("{query}&cursor={next}")).await;
    assert_eq!(bodies(&second), ["one"]);
    assert!(second["next"].is_null(), "{second}");
    let (_, back, _) = get(&site, &conn, &format!("{query}&before={}", second["prev"].as_str().expect("a prev cursor"))).await;
    assert_eq!(bodies(&back), ["three"]);
    let (_, last, _) = get(&site, &conn, &format!("{query}&last=1")).await;
    assert_eq!(bodies(&last), ["one"]);
    let (_, page, _) = get(&site, &conn, &format!("{query}&page=2")).await;
    assert_eq!(bodies(&page), ["one"]);
}

#[tokio::test]
async fn the_sql_of_a_related_filter_and_an_aggregate_has_the_subquery_and_binds() {
    let conn = database().await;
    let site = comments(json!({}));
    let (status, body, sql) = get(&site, &conn, &format!("{COMMENTS}?f.author.name=Ada")).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(bodies(&body), ["one", "four"]);
    let page = sql.iter().find(|s| s.contains("FROM \"comments\"")).expect("the page statement");
    assert!(page.contains("EXISTS(SELECT ? FROM \"authors\"") && page.contains("\"t1\".\"name\" = ?"), "{page}");
    assert!(!page.contains("Ada"), "a filter value is a bind: {page}");
    let list = json!({"columns": ["name", "comment_count"], "order": ["id"], "aggregates": [{"name": "comment_count", "label": "Comments", "relation": "comments", "fn": "count"}]});
    let site = self::site(json!({"resources": [{"model": "Author", "list": list}, {"model": "Comment"}]}));
    let (_, _, sql) = get(&site, &conn, AUTHORS).await;
    assert!(sql[0].contains("(SELECT COUNT(*) FROM \"comments\" AS \"a1\" WHERE \"a1\".\"author_id\" = \"authors\".\"id\")"), "{}", sql[0]);
}

#[test]
fn a_pattern_ops_index_serves_a_prefix_search_but_not_a_sort() {
    let source = r#"
datasource db {
  provider = "postgresql"
}

model Pattern {
  id   Int    @id
  name String
  @@index([name(ops: text_pattern_ops)])
  @@map("patterns")
}

model Plain {
  id   Int    @id
  name String
  @@index([name])
  @@map("plains")
}
"#;
    let schema = Arc::new(dsl::check(dsl::compile(source, None).unwrap()).unwrap().1);
    let resources: Vec<Value> = ["Pattern", "Plain"].iter().map(|m| json!({"model": m, "list": {"columns": ["name"], "search": ["name"], "sortable": ["name"]}})).collect();
    let config = json!({ "resources": resources });
    let site = Site::build(schema, &config.to_string()).unwrap();
    let flags: Vec<(bool, bool)> = site.resources.iter().map(|r| (r.sortable[0].indexed, r.search[0].indexed)).collect();
    assert_eq!(flags, [(false, true), (true, true)], "pattern_ops: search yes, sort no; plain: both");
}

#[tokio::test]
async fn a_prefix_search_on_a_plain_btree_warns_from_the_catalog() {
    let Ok(url) = std::env::var("ORM_RUST_TEST_POSTGRES_URL") else {
        eprintln!("skipped: ORM_RUST_TEST_POSTGRES_URL is not set");
        return;
    };
    let source = r#"
datasource db {
  provider = "postgresql"
}

model CPlain {
  id   Int    @id
  name String @unique
  @@map("c_plain")
}

model CPattern {
  id   Int    @id
  name String @unique
  @@index([name(ops: text_pattern_ops)])
  @@map("c_pattern")
}

model CCollated {
  id   Int    @id
  name String @unique
  @@map("c_collated")
}
"#;
    let schema = Arc::new(dsl::check(dsl::compile(source, None).unwrap()).unwrap().1);
    let conn = db::connect(&url, 2).await.unwrap();
    conn.batch("DROP TABLE IF EXISTS c_plain; DROP TABLE IF EXISTS c_pattern; DROP TABLE IF EXISTS c_collated".into()).await.unwrap();
    for step in migrate::plan(&schema, &Default::default()).unwrap().up {
        conn.batch(step.sql).await.unwrap();
    }
    conn.batch(r#"ALTER TABLE c_collated ALTER COLUMN name TYPE text COLLATE "C""#.into()).await.unwrap();
    let resources: Vec<Value> = ["CPlain", "CPattern", "CCollated"].iter().map(|m| json!({"model": m, "list": {"search": ["name"]}})).collect();
    let config = json!({ "resources": resources });
    let site = Site::build(schema, &config.to_string()).unwrap();
    assert!(site.warnings.is_empty(), "{:?}", site.warnings);
    let default_is_c = conn.query_text("SELECT datcollate FROM pg_database WHERE datname = current_database()".into()).await.unwrap()[0][0].clone().unwrap_or_default();
    let warnings = monorm_admin::catalog::prefix_warnings(&site, conn.as_ref()).await.unwrap();
    if matches!(default_is_c.as_str(), "C" | "POSIX") {
        assert!(warnings.is_empty(), "{warnings:?}");
    } else {
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].starts_with("CPlain: search field \"name\""), "{warnings:?}");
    }
    conn.batch("DROP TABLE c_plain; DROP TABLE c_pattern; DROP TABLE c_collated".into()).await.unwrap();
}

#[tokio::test]
async fn the_catalog_check_reads_the_collation_of_the_index() {
    let Ok(url) = std::env::var("ORM_RUST_TEST_POSTGRES_URL") else {
        eprintln!("skipped: ORM_RUST_TEST_POSTGRES_URL is not set");
        return;
    };
    let source = r#"
datasource db {
  provider = "postgresql"
}

model CIndexC {
  id   Int    @id
  name String
  @@index([name])
  @@map("c_index_c")
}

model CIndexOther {
  id   Int    @id
  name String
  @@index([name])
  @@map("c_index_other")
}
"#;
    let schema = Arc::new(dsl::check(dsl::compile(source, None).unwrap()).unwrap().1);
    let conn = db::connect(&url, 2).await.unwrap();
    conn.batch("DROP TABLE IF EXISTS c_index_c; DROP TABLE IF EXISTS c_index_other".into()).await.unwrap();
    for step in migrate::plan(&schema, &Default::default()).unwrap().up {
        conn.batch(step.sql).await.unwrap();
    }
    // Any collation that is not C: the test does not depend on the locale of the database.
    let others = conn
        .query_text("SELECT collname FROM pg_collation WHERE collname NOT IN ('C', 'POSIX', 'default', 'ucs_basic', 'pg_c_utf8') AND collencoding IN (-1, (SELECT encoding FROM pg_database WHERE datname = current_database())) ORDER BY collname LIMIT 1".into())
        .await
        .unwrap();
    let Some(other) = others.first().and_then(|r| r[0].clone()) else {
        conn.batch("DROP TABLE c_index_c; DROP TABLE c_index_other".into()).await.unwrap();
        eprintln!("skipped: the server has only the C collations");
        return;
    };
    // A C index on a column with another collation serves LIKE 'x%': no warning. The reverse cannot: one warning.
    conn.batch(r#"ALTER TABLE c_index_c ALTER COLUMN name TYPE text COLLATE "pg_unicode_fast""#.into()).await.ok();
    conn.batch(r#"ALTER TABLE c_index_other ALTER COLUMN name TYPE text COLLATE "C""#.into()).await.unwrap();
    for (table, collation) in [("c_index_c", "C".to_owned()), ("c_index_other", other.clone())] {
        let name = conn.query_text(format!("SELECT indexname FROM pg_indexes WHERE tablename = '{table}' AND indexname NOT LIKE '%pkey'")).await.unwrap()[0][0].clone().unwrap();
        conn.batch(format!(r#"DROP INDEX "{name}"; CREATE INDEX "{name}" ON {table} (name COLLATE "{collation}")"#)).await.unwrap();
    }
    let resources: Vec<Value> = ["CIndexC", "CIndexOther"].iter().map(|m| json!({"model": m, "list": {"search": ["name"]}})).collect();
    let site = Site::build(schema, &json!({ "resources": resources }).to_string()).unwrap();
    let warnings = monorm_admin::catalog::prefix_warnings(&site, conn.as_ref()).await.unwrap();
    conn.batch("DROP TABLE c_index_c; DROP TABLE c_index_other".into()).await.unwrap();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].starts_with("CIndexOther: search field \"name\""), "{warnings:?}");
}
