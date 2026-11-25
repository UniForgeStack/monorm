//! The error of a wrong config and wrong settings: the code, the path, and the spelling of an option key.
use std::sync::Arc;

use monorm_admin::config::{security_options, Config};
use monorm_admin::{code, AdminError, PathPart, Site};
use monorm_core::{dsl, schema::Schema};
use serde_json::{json, Value};

const ADMIN: &str = include_str!("../../../apps/admin/schema.prisma");

fn schema() -> Arc<Schema> {
    let source = format!("datasource db {{\n  provider = \"sqlite\"\n}}\n\nmodel Note {{\n  id Int @id\n}}\n\nmodel Author {{\n  id Int @id @default(autoincrement())\n  name String\n  books Book[]\n}}\n\nmodel Book {{\n  id Int @id @default(autoincrement())\n  author_id Int\n  title String\n  author Author @relation(fields: [author_id], references: [id])\n}}\n{ADMIN}");
    Arc::new(dsl::check(dsl::compile(&source, None).unwrap()).unwrap().1)
}

fn build(config: Value) -> AdminError {
    Site::build(schema(), &config.to_string()).err().unwrap_or_else(|| panic!("{config} builds"))
}

#[test]
fn a_model_error_has_a_code_a_path_and_the_old_text() {
    let e = build(json!({"resources": [{"model": "Note", "list": {"page_size": 0}}]}));
    assert_eq!((e.code, e.class()), (code::INVALID_VALUE, "config"));
    assert_eq!(e.path, vec![PathPart::Model("Note".into())]);
    assert_eq!(e.message, "Note: list.page_size is 0, between 1 and 200");
}

#[test]
fn the_spelling_changes_the_option_keys_and_nothing_else() {
    let resources = json!([{"model": "Note", "list": {"page_size": 0}}]);
    let snake = build(json!({"resources": resources}));
    let camel = build(json!({"spelling": "camel", "resources": resources}));
    assert_eq!(camel.message, "Note: list.pageSize is 0, between 1 and 200");
    assert_eq!((snake.code, snake.path), (camel.code, camel.path));
}

#[test]
fn an_unknown_option_and_an_unknown_model_have_their_own_codes() {
    assert_eq!(build(json!({"nope": 1})).code, code::UNKNOWN_OPTION);
    assert_eq!(build(json!({"resources": [{"model": "Missing"}]})).code, code::UNKNOWN_MODEL);
    assert_eq!(build(json!({"spelling": "klingon"})).code, code::INVALID_VALUE);
}

#[test]
fn a_serde_error_names_an_option_in_the_spelling_of_the_host() {
    let e = build(json!({"spelling": "camel", "resources": [{"model": "Note", "bulk_delete": "yes"}]}));
    assert_eq!(e.code, code::INVALID_VALUE);
    assert!(e.message.starts_with("invalid admin config: "), "{}", e.message);
}

#[test]
fn a_route_error_keeps_its_text_and_has_a_code() {
    let e = build(json!({"routes": [{"method": "GET", "path": "a b"}]}));
    assert_eq!(e.class(), "config");
    assert!(e.message.contains("must be letters"), "{}", e.message);
}

#[test]
fn a_settings_error_has_the_settings_class() {
    let dir = std::env::temp_dir().join(format!("monorm-admin-error-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("monorm.toml"), "[admin]\napp = \"a:b\"\nnope = 1\n").unwrap();
    let e = Config::load(&dir, |_| None).unwrap_err();
    assert_eq!((e.code, e.class()), (code::SETTINGS_UNKNOWN_KEY, "settings"));
    std::fs::write(dir.join("monorm.toml"), "[admin]\nport = 1\n").unwrap();
    assert_eq!(Config::load(&dir, |_| None).unwrap_err().code, code::SETTINGS_MISSING);
    assert_eq!(security_options("[]").err().unwrap().code, code::SETTINGS_INVALID);
    assert_eq!(security_options("{\"x\": 1}").err().unwrap().code, code::SETTINGS_UNKNOWN_KEY);
    let _ = std::fs::remove_dir_all(dir);
}

fn positive() -> Value {
    json!({"t": "cmp", "op": "gt", "l": {"t": "col", "path": [], "name": "id"}, "r": {"t": "int", "value": 0}})
}

fn qs(extra: Value) -> Value {
    let mut body = json!({"model": "Note", "filter": positive()});
    body.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    json!({"$qs": body})
}

fn note(page: Value) -> Value {
    let mut resource = json!({"model": "Note"});
    resource.as_object_mut().unwrap().extend(page.as_object().unwrap().clone());
    json!({"resources": [resource]})
}

#[test]
fn a_prepared_query_set_lowers_to_a_filter_and_its_order_moves_to_the_list() {
    let config = note(json!({"queryset": qs(json!({"order": ["-id"]}))})).to_string();
    let (site, handles) = Site::build_with_handles(schema(), &config).unwrap();
    assert!(handles.is_empty());
    let note = site.resources.iter().find(|r| r.name == "Note").unwrap();
    assert!(note.queryset.is_some());
    assert!(note.order[0].desc, "the order of the query set is the order of the list");
}

#[test]
fn a_queryset_refusal_has_the_code_the_path_and_the_text() {
    for (flag, what) in [("load", "load()"), ("slice", "a slice"), ("join", "from_() or join()"), ("lock", "lock()"), ("without_defaults", "without_defaults()")] {
        let e = build(note(json!({"queryset": qs(json!({flag: true}))})));
        assert_eq!((e.code, e.path), (code::UNSUPPORTED, vec![PathPart::Model("Note".into())]), "{flag}");
        assert_eq!(e.message, format!("Note: queryset takes a filter and an order; it does not take {what}"));
    }
    let mut camel = note(json!({"queryset": qs(json!({"join": true}))}));
    camel["spelling"] = json!("camel");
    assert_eq!(build(camel).message, "Note: queryset takes a filter and an order; it does not take from() or join()");
    let mut camel = note(json!({"queryset": qs(json!({"without_defaults": true}))}));
    camel["spelling"] = json!("camel");
    assert_eq!(build(camel).message, "Note: queryset takes a filter and an order; it does not take withoutDefaults()");
    let e = build(note(json!({"queryset": {"$qs": {"model": "Note", "filter": null}}})));
    assert_eq!((e.code, e.message.as_str()), (code::INVALID_VALUE, "Note: queryset needs a filter"));
    let e = build(note(json!({"queryset": {"$qs": {"model": "Other", "filter": positive()}}})));
    assert_eq!(e.message, "Note: queryset is a query set of Other");
}

#[test]
fn the_order_is_in_the_queryset_or_in_the_list_not_in_both() {
    let e = build(note(json!({"queryset": qs(json!({"order": ["id"]})), "list": {"order": ["id"]}})));
    assert_eq!((e.code, e.message.as_str()), (code::INVALID_VALUE, "Note: the order is in the queryset or in the list, not in both"));
}

#[test]
fn a_wrong_tag_names_the_hint_in_the_spelling_of_the_host() {
    let e = build(note(json!({"queryset": {"$nope": 1}})));
    assert_eq!(e.message, "Note: queryset takes a prepared query set (Model.objects.filter(...).prepare()) or an async function");
    let e = build(json!({"spelling": "camel", "resources": [{"model": "Note", "queryset": 5}]}));
    assert!(e.message.contains("prepared query set"), "{}", e.message);
    let e = build(note(json!({"queryset": {"$host": "str"}})));
    assert_eq!((e.code, e.message.as_str()), (code::INVALID_VALUE, "Note: queryset takes a prepared query set (Model.objects.filter(...).prepare()) or an async function, not a value of type str"));
    let e = build(note(json!({"list": {"search_with": {"$host": "str"}}})));
    assert!(e.message.contains("is not a host function"), "{}", e.message);
}

#[test]
fn a_host_function_gets_its_name_from_rust() {
    let config = note(json!({
        "queryset": {"$fn": "q1"},
        "list": {"search_with": {"$fn": "s1"}, "computed": [{"name": "double", "type": "int", "nullable": false, "function": {"$fn": "c1"}}]},
    }));
    let (_, handles) = Site::build_with_handles(schema(), &config.to_string()).unwrap();
    let names: Vec<_> = handles.iter().map(|(k, v)| format!("{k}={v}")).collect();
    assert_eq!(names, ["c1=column.Note.double", "q1=queryset.Note", "s1=search.Note"]);
}

fn book(relations: Value) -> Value {
    json!({"resources": [{"model": "Author", "list": {"search": ["name"]}}, {"model": "Book", "relations": relations}]})
}

#[test]
fn relation_choices_takes_a_query_set_of_the_target() {
    let body = |model: &str| json!({"$qs": {"model": model, "filter": positive()}});
    let e = build(book(json!({"author": {"choices": body("Book")}})));
    assert_eq!((e.code, e.message.as_str()), (code::INVALID_VALUE, "Book: relation \"author\": choices is a query set of Book, not of Author"));
    for choices in [json!({"$fn": "f0"}), json!("all"), json!(5), json!({"$host": "str"})] {
        let e = build(book(json!({"author": {"choices": choices}})));
        assert_eq!(e.message, "Book: relation \"author\": choices takes a prepared query set of the target", "{choices}");
    }
    let ok = Site::build(schema(), &book(json!({"author": {"choices": body("Author")}})).to_string());
    assert!(ok.is_ok(), "{:?}", ok.err());
}

#[test]
fn an_inline_page_function_gets_the_inline_tag_in_its_name() {
    let config = json!({"resources": [{"model": "Author", "inlines": [{"model": "Book", "page": {"model": "Book", "list": {"search_with": {"$fn": "s1"}}}}]}, {"model": "Book"}]});
    let (_, handles) = Site::build_with_handles(schema(), &config.to_string()).unwrap();
    assert_eq!(handles.get("s1").map(String::as_str), Some("search.Book@Author.Book"));
}

#[test]
fn two_functions_with_one_host_name_are_refused() {
    let page = |id: &str| json!({"model": "Book", "page": {"model": "Book", "list": {"search_with": {"$fn": id}}}});
    let config = json!({"resources": [{"model": "Author", "inlines": [page("s1"), page("s2")]}, {"model": "Book"}]});
    let e = build(config);
    assert_eq!(e.code, code::INVALID_VALUE);
    assert!(e.message.contains("two host functions get the name \"search.Book@Author.Book\""), "{}", e.message);
}

fn author(list: Value) -> Value {
    json!({"resources": [{"model": "Author", "list": list}, {"model": "Book"}]})
}

fn at(model: &str, path: &str) -> Value {
    json!({"$ref": {"model": model, "path": path}})
}

#[test]
fn a_ref_lowers_to_its_path_and_checks_its_model() {
    let ok = Site::build(schema(), &author(json!({"columns": [at("Author", "id"), "name"], "order": [{"$ref": {"model": "Author", "path": "name"}, "desc": true}]})).to_string());
    assert!(ok.is_ok(), "{:?}", ok.err());
    let e = build(author(json!({"columns": [at("Book", "title")]})));
    assert_eq!((e.code, e.message.as_str()), (code::INVALID_VALUE, "Author: Book.title belongs to Book, not to Author"));
    let e = build(author(json!({"search": [{"$host": "Q"}]})));
    assert_eq!((e.code, e.message.as_str()), (code::INVALID_VALUE, "Author: search: a value of type Q is not a column or a relation of Author"));
}

#[test]
fn a_ref_takes_no_key_beside_it_except_desc_and_nulls_in_an_order() {
    // The text puts `desc` first; the tag is found by its `$`, not by its place.
    let order: Value = serde_json::from_str(r#"[{"desc": true, "$ref": {"model": "Author", "path": "name"}}]"#).unwrap();
    let ok = Site::build(schema(), &author(json!({"order": order})).to_string());
    assert!(ok.is_ok(), "{:?}", ok.err());
    let sibling = json!({"$ref": {"model": "Author", "path": "id"}, "desc": true});
    let e = build(author(json!({"columns": [sibling]})));
    assert_eq!(e.message, "Author: columns: a reference of Author takes no key beside \"$ref\"");
    let e = build(author(json!({"order": [{"$ref": {"model": "Author", "path": "id"}, "asc": true}]})));
    assert_eq!(e.message, "Author: order takes no key \"asc\" beside \"$ref\"");
}

#[test]
fn an_order_takes_columns() {
    let e = build(author(json!({"order": [{"$ir": {"t": "col", "path": [], "name": "id"}}]})));
    assert_eq!((e.code, e.message.as_str()), (code::INVALID_VALUE, "Author: order takes columns, not an expression"));
    let e = build(author(json!({"order": [{"$host": "Q"}]})));
    assert_eq!(e.message, "Author: order takes columns, not a value of type Q");
    let e = build(author(json!({"order": [{"$ref": {"model": "Author", "path": "id"}, "nulls": "first"}]})));
    assert_eq!(e.message, "Author: order does not take a NULLs placement");
}

#[test]
fn a_slot_is_the_user_param_and_a_host_value_is_not_a_literal() {
    let slot = |name: &str, transform: bool| json!({"t": "slot", "name": name, "transform": transform});
    let can = |rule: Value| note(json!({"can": {"delete": {"$ir": rule}}}));
    let e = build(can(json!({"t": "cmp", "op": "eq", "l": slot("x", false), "r": {"t": "int", "value": 1}})));
    assert_eq!((e.code, e.message.as_str()), (code::INVALID_VALUE, "Note: can 'delete': takes the params 'user', not param('x')"));
    let e = build(can(json!({"t": "cmp", "op": "eq", "l": slot("user", true), "r": {"t": "int", "value": 1}})));
    assert_eq!(e.code, code::INVALID_VALUE);
    let e = build(can(json!({"t": "cmp", "op": "eq", "l": {"$host": "Decimal"}, "r": {"t": "int", "value": 1}})));
    assert!(e.message.contains("a value of type Decimal is not a literal of the admin"), "{}", e.message);
    let e = build(note(json!({"actions": [{"name": "a", "applies_to": {"$ir": slot("user", false)}}]})));
    assert!(e.message.contains("takes no request values here"), "{}", e.message);
}

#[test]
fn a_query_set_body_takes_its_known_keys_only() {
    let e = build(note(json!({"queryset": {"$qs": {"model": "Note", "filter": positive(), "slots": []}}})));
    assert_eq!(e.code, code::INVALID_VALUE);
    let e = build(note(json!({"queryset": {"$qs": {"model": "Note", "filter": positive(), "lock": true}}})));
    assert_eq!((e.code, e.message.as_str()), (code::UNSUPPORTED, "Note: queryset takes a filter and an order; it does not take lock()"));
}
