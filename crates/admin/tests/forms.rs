//! Form layout and widgets: sections, read-only fields, widgets by field, type and path, and the custom form module.
use std::sync::Arc;

use monorm_admin::{call_body, Site};
use monorm_core::{dsl, migrate, schema::Schema};
use monorm_engine::db::{self, Executor};
use serde_json::{json, Value};

const ADMIN: &str = include_str!("../../../apps/admin/schema.prisma");
const MODELS: &str = r#"
datasource db {
  provider = "sqlite"
}

enum Status {
  draft
  published
  @@storage(text)
}

type Geo {
  lat Float
  lng Float
}

type Pin {
  tag String
}

type Address {
  city String
  pin  Pin
}

model Place {
  id   Int      @id @default(autoincrement())
  addr Address? @storage(columns)
}

model Book {
  id     Int     @id @default(autoincrement())
  title  String  @db.VarChar(20)
  blurb  String?
  pages  Int     @default(0)
  status Status  @default(draft)
  meta   Json?
  geo    Geo?    @storage(columns)
}

model Stamp {
  id   Int    @id @default(autoincrement())
  code String
}
"#;

fn schema() -> Arc<Schema> {
    let source = format!("{MODELS}\n{ADMIN}");
    Arc::new(dsl::check(dsl::compile(&source, None).unwrap()).unwrap().1)
}

struct World {
    site: Arc<Site>,
    conn: Arc<dyn Executor>,
}

async fn world(config: Value) -> World {
    let schema = schema();
    let conn = db::connect("sqlite://:memory:", 1).await.unwrap();
    for step in migrate::plan(&schema, &Default::default()).unwrap().up {
        conn.batch(step.sql).await.unwrap();
    }
    conn.batch("INSERT INTO \"Book\" (id, title, pages, status) VALUES (1, 'First', 10, 'draft'); INSERT INTO \"Stamp\" (id, code) VALUES (1, 'a');".into()).await.unwrap();
    let site = Arc::new(Site::build(schema, &config.to_string()).unwrap());
    World { site, conn }
}

fn error(config: Value) -> String {
    Site::build(schema(), &config.to_string()).err().expect("the build fails").to_string()
}

impl World {
    async fn call(&self, method: &str, uri: &str, user: bool, body: Option<Value>) -> (u16, String) {
        let text = body.map(|b| b.to_string());
        let user = user.then(|| json!({"key": 1, "is_superuser": true}));
        call_body(self.site.clone(), self.conn.clone(), method, &format!("/admin/api{uri}"), user, None, text.as_deref()).await
    }

    async fn send(&self, method: &str, uri: &str, body: Option<Value>) -> (u16, Value) {
        let (status, out) = self.call(method, uri, true, body).await;
        (status, serde_json::from_str(&out).unwrap_or(Value::Null))
    }

    async fn book(&self) -> Value {
        let (status, site) = self.send("GET", "/resources", None).await;
        assert_eq!(status, 200, "{site}");
        site["resources"].as_array().unwrap().iter().find(|r| r["name"] == "Book").unwrap().clone()
    }
}

fn names(sections: &Value) -> Vec<(Value, Vec<String>)> {
    sections.as_array().unwrap().iter().map(|s| (s["title"].clone(), s["fields"].as_array().unwrap().iter().map(|f| f.as_str().unwrap().to_owned()).collect())).collect()
}

fn book(extra: Value) -> Value {
    let mut r = json!({"model": "Book"});
    r.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    json!({"resources": [r, {"model": "Stamp"}]})
}

#[tokio::test]
async fn without_a_layout_one_untitled_section_holds_every_writable_field() {
    let w = world(book(json!({}))).await;
    let book = w.book().await;
    let create = names(&book["sections"]["create"]);
    assert_eq!(create.len(), 1);
    assert_eq!(create[0].0, Value::Null);
    assert!(create[0].1.contains(&"title".to_owned()) && create[0].1.contains(&"geo_lat".to_owned()), "{create:?}");
    assert_eq!(book["sections"]["edit"], book["sections"]["create"]);
    assert_eq!(book["form_component"], false);
}

#[tokio::test]
async fn sections_collapse_and_unlisted_fields_trail_and_create_and_edit_differ() {
    let layout = json!({"create": [{"title": "Main", "fields": ["title", "pages"]}], "edit": [{"title": "Main", "fields": ["title"]}, {"title": "More", "fields": ["meta"], "collapsed": true}]});
    let w = world(book(json!({"form": layout}))).await;
    let book = w.book().await;
    let create = names(&book["sections"]["create"]);
    assert_eq!(create[0], (json!("Main"), vec!["title".to_owned(), "pages".to_owned()]));
    assert_eq!(create[1].0, Value::Null, "unlisted fields trail in an untitled section");
    assert!(create[1].1.contains(&"status".to_owned()));
    let edit = book["sections"]["edit"].as_array().unwrap();
    assert_eq!((edit[1]["title"].clone(), edit[1]["collapsed"].clone()), (json!("More"), json!(true)));
    assert_eq!(edit[0]["collapsed"], json!(false));
}

#[tokio::test]
async fn a_plain_list_is_the_layout_of_both_forms() {
    let w = world(book(json!({"form": [{"fields": ["pages"]}]}))).await;
    let book = w.book().await;
    assert_eq!(book["sections"]["create"], book["sections"]["edit"]);
    assert_eq!(names(&book["sections"]["create"])[0].1, ["pages"]);
}

#[tokio::test]
async fn widgets_resolve_by_field_then_path_then_type_and_the_resource_wins_in_its_level() {
    let config = json!({
        "widgets": {"Text": "textarea", "String": "email", "Status": "radio"},
        "resources": [{"model": "Book", "widgets": {"title": "url", "Geo.lat": "number"}}, {"model": "Stamp"}],
    });
    let w = world(config).await;
    let book = w.book().await;
    let widget = |n: &str| book["form"].as_array().unwrap().iter().find(|f| f["name"] == n).unwrap()["widget"].clone();
    assert_eq!(widget("title"), "url", "a field key beats a type key");
    assert_eq!(widget("blurb"), "email", "a site-wide type key applies to a field of that type");
    assert_eq!(widget("status"), "radio");
    assert_eq!(widget("pages"), "number", "the built-in default stays");
    let stamp = w.send("GET", "/resources", None).await.1["resources"].as_array().unwrap().iter().find(|r| r["name"] == "Stamp").unwrap().clone();
    assert_eq!(stamp["form"].as_array().unwrap().iter().find(|f| f["name"] == "code").unwrap()["widget"], "email");
}

#[tokio::test]
async fn a_read_only_field_shows_in_the_meta_and_a_write_to_it_is_a_422_on_create_and_update() {
    let w = world(book(json!({"readonly": ["pages"]}))).await;
    let book = w.book().await;
    let pages = book["form"].as_array().unwrap().iter().find(|f| f["name"] == "pages").unwrap();
    assert_eq!(pages["readonly"], true);
    let (status, body) = w.send("POST", "/resources/Book", Some(json!({"set": {"title": "T", "pages": 5}}))).await;
    assert_eq!((status, &body["fields"]["pages"]), (422, &json!(["This field is read-only."])), "{body}");
    let (status, body) = w.send("PATCH", "/resources/Book/1", Some(json!({"set": {"pages": 5}, "loaded": {"pages": 10}}))).await;
    assert_eq!((status, &body["fields"]["pages"]), (422, &json!(["This field is read-only."])), "{body}");
    let pages = w.conn.query_text("SELECT pages FROM \"Book\" WHERE id = 1".into()).await.unwrap();
    assert_eq!(pages[0][0].as_deref(), Some("10"), "nothing was written");
    let (status, body) = w.send("POST", "/resources/Book", Some(json!({"set": {"title": "T"}}))).await;
    assert_eq!(status, 201, "{body}");
}

#[test]
fn a_bad_layout_is_a_build_error_that_names_the_problem() {
    let cases = [
        (json!({"form": [{"fields": ["nope"]}]}), "\"nope\" is not a field of the form"),
        (json!({"form": [{"fields": ["title"]}, {"fields": ["title"]}]}), "is in two places"),
        (json!({"form": [{"fields": []}]}), "a section has no fields"),
        (json!({"form": [{"fields": ["title"], "collapsed": true}]}), "a collapsed section needs a title"),
        (json!({"form": {"new": []}}), "not one of create, edit"),
        (json!({"form": "x"}), "a list of sections"),
    ];
    for (extra, want) in cases {
        let e = error(book(extra.clone()));
        assert!(e.contains(want), "{extra}: {e}");
    }
}

#[test]
fn a_bad_widget_or_read_only_config_is_a_build_error() {
    let cases = [
        (json!({"widgets": {"title": "slider"}}), "not one of"),
        (json!({"widgets": {"pages": "checkbox"}}), "does not fit"),
        (json!({"widgets": {"nothing": "text"}}), "widgets key \"nothing\""),
        (json!({"widgets": {"Geo.nope": "text"}}), "no subfield"),
        (json!({"readonly": ["nope"]}), "not a field of the form"),
        (json!({"readonly": ["title"]}), "read-only and required with no default"),
        (json!({"form_component": "/no/such/file.js"}), "form_component"),
    ];
    for (extra, want) in cases {
        let e = error(book(extra.clone()));
        assert!(e.contains(want), "{extra}: {e}");
    }
}

#[test]
fn an_unknown_site_widget_key_is_a_build_error() {
    let e = error(json!({"widgets": {"Nothing": "text"}, "resources": [{"model": "Book"}]}));
    assert!(e.contains("widgets key \"Nothing\""), "{e}");
    let e = error(json!({"widgets": {"String": "slider"}, "resources": [{"model": "Book"}]}));
    assert!(e.contains("not one of"), "{e}");
}

#[tokio::test]
async fn the_form_module_needs_a_user_and_a_module_and_comes_back_as_javascript() {
    let module = std::env::temp_dir().join(format!("monorm-form-module-{}.js", std::process::id()));
    std::fs::write(&module, "export default (React) => function Form() { return null; };").unwrap();
    let w = world(book(json!({"form_component": module.to_str().unwrap()}))).await;
    assert_eq!(w.book().await["form_component"], true);
    let (status, _) = w.call("GET", "/forms/Book", false, None).await;
    assert_eq!(status, 401);
    let (status, text) = w.call("GET", "/forms/Book", true, None).await;
    assert_eq!(status, 200);
    assert!(text.contains("export default"), "{text}");
    let (status, _) = w.call("GET", "/forms/Stamp", true, None).await;
    assert_eq!(status, 404);
    std::fs::remove_file(&module).unwrap();
}

#[tokio::test]
async fn a_path_key_names_a_subfield_from_any_type_on_the_way_and_the_outer_type_wins() {
    let place = |own: Value, site: Value| async move {
        let w = world(json!({"widgets": site, "resources": [{"model": "Place", "widgets": own}]})).await;
        let (_, meta) = w.send("GET", "/resources", None).await;
        let place = meta["resources"].as_array().unwrap().iter().find(|r| r["name"] == "Place").unwrap().clone();
        let field = place["form"].as_array().unwrap().iter().find(|f| f["name"] == "addr_pin_tag").unwrap().clone();
        field["widget"].clone()
    };
    assert_eq!(place(json!({}), json!({})).await, "text");
    assert_eq!(place(json!({"Pin.tag": "textarea"}), json!({})).await, "textarea", "the inner type names the subfield");
    assert_eq!(place(json!({"Address.pin.tag": "url"}), json!({})).await, "url", "the outer type names the same subfield");
    assert_eq!(place(json!({"Pin.tag": "textarea", "Address.pin.tag": "url"}), json!({})).await, "url", "the outer type wins");
    assert_eq!(place(json!({}), json!({"Pin.tag": "email"})).await, "email", "the site-wide key works the same");
}

#[test]
fn a_resource_key_names_only_a_field_of_its_own_form() {
    let e = error(book(json!({"widgets": {"Stamp.code": "textarea"}})));
    assert!(e.contains("names a field of Stamp"), "{e}");
    let e = error(book(json!({"widgets": {"Book.nope": "textarea"}})));
    assert!(e.contains("Book.nope is not a field of the form"), "{e}");
    Site::build(schema(), &book(json!({"widgets": {"Book.blurb": "textarea"}})).to_string()).expect("a field of its own form");
}
