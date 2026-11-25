//! The error of a wrong config and wrong settings: the code, the path, and the spelling of an option key.
use std::sync::Arc;

use monorm_admin::config::{security_options, Config};
use monorm_admin::{code, AdminConfigBuilder, AdminError, PathPart, Site, Spelling};
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
    assert_eq!(e.message, "Note: bulkDelete is true or false");
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

#[test]
fn an_unknown_option_names_its_place_and_has_its_own_code() {
    let cases = [
        (json!({"resources": [{"model": "Note", "colum": 1}]}), "Note: unknown option 'colum'"),
        (json!({"resources": [{"model": "Note", "list": {"colums": []}}]}), "Note: unknown list option 'colums'"),
        (json!({"resources": [{"model": "Note", "list": {"date_hierarchy": {"field": "id", "nope": 1}}}]}), "Note: unknown date_hierarchy option 'nope'"),
        (json!({"resources": [{"model": "Book", "relations": {"author": {"nope": 1}}}]}), "Book: relations['author'] has an unknown option 'nope'"),
        (json!({"resources": [{"model": "Book", "form": [{"fields": ["title"], "nope": 1}]}]}), "Book: unknown section option 'nope'"),
        (json!({"brand": {"nope": 1}}), "unknown brand option 'nope'"),
        (json!({"theme": {"nope": 1}}), "unknown theme option 'nope'"),
        (json!({"switcher": {"nope": 1}}), "unknown switcher option 'nope'"),
    ];
    for (config, text) in cases {
        let e = build(config);
        assert_eq!((e.code, e.message.as_str()), (code::UNKNOWN_OPTION, text));
    }
}

#[test]
fn a_date_hierarchy_without_a_field_is_refused() {
    let e = build(json!({"resources": [{"model": "Note", "list": {"date_hierarchy": {"counts": false}}}]}));
    assert_eq!((e.code, e.message.as_str()), (code::INVALID_VALUE, "Note: date_hierarchy needs a field"));
}

fn builder(options: Value) -> AdminConfigBuilder {
    AdminConfigBuilder::new(&options.to_string(), 15.0, "{}", Spelling::Snake).unwrap()
}

fn keys(list: &[&str]) -> Vec<String> {
    list.iter().map(|k| k.to_string()).collect()
}

#[test]
fn the_builder_refuses_a_wrong_option_when_it_is_made() {
    let made = |options: Value, secs: f64, roles: &str| AdminConfigBuilder::new(&options.to_string(), secs, roles, Spelling::Snake).err().unwrap();
    let e = made(json!({"brand": {"nope": 1}}), 15.0, "{}");
    assert_eq!((e.code, e.message.as_str()), (code::UNKNOWN_OPTION, "unknown brand option 'nope'"));
    assert_eq!(made(json!({"theme": {"nope": 1}}), 15.0, "{}").message, "unknown theme option 'nope'");
    assert_eq!(made(json!({"switcher": {"nope": 1}}), 15.0, "{}").message, "unknown switcher option 'nope'");
    for bad in [-1.0, f64::NAN, f64::INFINITY] {
        let e = made(json!({}), bad, "{}");
        assert_eq!((e.code, e.message.as_str()), (code::INVALID_VALUE, "statement_timeout is a number of seconds, 0 or more (0 sets no limit)"));
    }
    let e = made(json!({}), 15.0, r#"{"staff": "all"}"#);
    assert_eq!(e.message, "roles['staff'] must be a list of patterns");
}

#[test]
fn the_builder_refuses_a_duplicate_and_an_unknown_key() {
    let mut b = builder(json!({}));
    b.register("Note", &keys(&["list", "bulk_delete"])).unwrap();
    let e = b.register("Note", &[]).unwrap_err();
    assert_eq!((e.code, e.message.as_str()), (code::DUPLICATE, "Note is already registered"));
    let e = b.register("Author", &keys(&["nope"])).unwrap_err();
    assert_eq!((e.code, e.message.as_str()), (code::UNKNOWN_OPTION, "Author: unknown option 'nope'"));
    assert_eq!(e.path, vec![PathPart::Model("Author".into())]);
    // A refused call changes nothing: the model can register again.
    b.register("Author", &[]).unwrap();
}

#[test]
fn the_builder_takes_the_keys_in_the_spelling_of_the_host() {
    let mut camel = AdminConfigBuilder::new("{}", 15.0, "{}", Spelling::Camel).unwrap();
    camel.register("Note", &keys(&["bulkDelete", "checkMessages"])).unwrap();
    assert_eq!(camel.register("Author", &keys(&["bulk_delete"])).unwrap_err().message, "Author: unknown option 'bulk_delete'");
    let mut snake = builder(json!({}));
    assert_eq!(snake.register("Author", &keys(&["bulkDelete"])).unwrap_err().message, "Author: unknown option 'bulkDelete'");
}

#[test]
fn the_builder_refuses_a_package_a_route_and_an_inline_that_is_wrong() {
    let mut b = builder(json!({}));
    b.package("tools", "{}").unwrap();
    let e = b.package("tools", "{}").unwrap_err();
    assert_eq!((e.code, e.message.as_str()), (code::PACKAGE, "the package 'tools' is added twice"));
    let e = b.route("GET", "/x", None, false).unwrap_err();
    assert_eq!((e.code, e.message.as_str()), (code::ROUTE, "a route needs a function: route(method, path, fn)"));
    b.route("get", "/x", None, true).unwrap();
    let e = b.route("GET", "x", Some("admin.x"), true).unwrap_err();
    assert_eq!((e.code, e.message.as_str()), (code::DUPLICATE, "the route GET x is registered twice"));
    AdminConfigBuilder::check_inline("Book", "row", "table", Spelling::Snake).unwrap();
    assert_eq!(AdminConfigBuilder::check_inline("Book", "later", "table", Spelling::Snake).unwrap_err().message, "inline Book: save is one of with_parent, row");
    assert_eq!(AdminConfigBuilder::check_inline("Book", "row", "wide", Spelling::Snake).unwrap_err().message, "inline Book: style is one of table, stacked");
}

#[test]
fn the_builder_builds_more_than_once_from_its_state() {
    let mut b = builder(json!({"brand": {"name": "Shop"}}));
    b.route("GET", "ping", None, true).unwrap();
    for _ in 0..2 {
        let (site, handles) = b.build(schema(), &json!({"resources": [{"model": "Note"}]}).to_string()).unwrap();
        assert!(site.permission_names().contains(&"admin.access".to_string()));
        assert!(handles.is_empty());
    }
    assert!(b.build(schema(), "[]").is_err());
}

#[test]
fn the_inline_values_of_the_builder_are_the_values_that_the_site_takes() {
    for save in ["with_parent", "row"] {
        for style in ["table", "stacked"] {
            AdminConfigBuilder::check_inline("Book", save, style, Spelling::Snake).unwrap();
            let config = json!({"resources": [{"model": "Author", "inlines": [{"model": "Book", "save": save, "style": style}]}, {"model": "Book"}]});
            Site::build(schema(), &config.to_string()).unwrap_or_else(|e| panic!("{save} {style}: {}", e.message));
        }
    }
}

#[test]
fn the_builder_sends_its_statement_timeout_at_each_build() {
    let late = json!({"resources": [{"model": "Note"}]}).to_string();
    assert!(AdminConfigBuilder::new("{}", 15.0, "{}", Spelling::Snake).unwrap().build(schema(), &late).is_ok());
    assert!(AdminConfigBuilder::new("{}", 20.0, "{}", Spelling::Snake).unwrap().build(schema(), &late).is_ok());
    let e = AdminConfigBuilder::new("{}", 3_000_000.0, "{}", Spelling::Snake).unwrap().build(schema(), &late).err().unwrap();
    assert_eq!(e.code, code::INVALID_VALUE);
    assert!(e.message.contains("statement_timeout is 3000000"), "{}", e.message);
}

#[test]
fn a_wrong_type_of_an_option_waits_for_the_build_but_an_unknown_key_does_not() {
    assert!(builder(json!({"brand": "Shop"})).build(schema(), "{}").is_err());
    assert_eq!(AdminConfigBuilder::new(&json!({"brand": {"nope": 1}}).to_string(), 15.0, "{}", Spelling::Snake).err().unwrap().code, code::UNKNOWN_OPTION);
}

#[test]
fn files_need_a_storage_registry() {
    let resources = json!([{"model": "Note", "files": ["cover"]}]);
    let e = build(json!({"resources": resources}));
    assert_eq!(e.code, code::INVALID_VALUE);
    assert_eq!(e.message, "Note: files needs a file storage registry: pass `storage` to Admin");
}

fn refusal(resource: Value) -> (&'static str, String) {
    let e = build(json!({"resources": [resource]}));
    (e.code, e.message)
}

#[test]
fn a_relations_option_names_its_relation_and_its_fault() {
    let resource = |relations: Value| json!({"model": "Book", "relations": relations});
    assert_eq!(refusal(resource(json!([1]))).0, code::INVALID_VALUE);
    assert_eq!(refusal(resource(json!({"editor": {}}))), (code::RELATION_UNKNOWN, "Book: relations has 'editor', which is not a relation of the model".into()));
    assert_eq!(refusal(resource(json!({"author": 3}))).0, code::INVALID_VALUE);
    assert_eq!(refusal(resource(json!({"author": {"nope": 1}}))), (code::UNKNOWN_OPTION, "Book: relations['author'] has an unknown option 'nope'".into()));
    assert_eq!(refusal(resource(json!({"author": {"create": "yes"}}))).0, code::INVALID_VALUE);
}

#[test]
fn a_computed_column_sets_one_of_expr_and_function() {
    let list = |computed: Value| json!({"model": "Author", "list": {"computed": [computed]}});
    let both = refusal(list(json!({"name": "n", "type": "string", "expr": {"t": "col", "name": "name"}, "function": "f"})));
    assert_eq!(both, (code::INVALID_VALUE, "Author: computed 'n' sets expr or function, one of them".into()));
    assert_eq!(refusal(list(json!({"name": "n", "type": "string"}))), (code::INVALID_VALUE, "Author: computed 'n' sets expr or function, one of them".into()));
}

#[test]
fn an_action_needs_a_function_a_known_placement_and_one_listing() {
    let action = |a: Value| json!({"model": "Author", "actions": a});
    assert_eq!(refusal(action(json!([{"name": "x", "placement": "bulk", "function": "plain"}]))), (code::INVALID_VALUE, "Author: action 'x' needs a function".into()));
    let e = refusal(action(json!([{"name": "x", "placement": "middle", "function": {"$fn": "f1"}}])));
    assert_eq!(e.0, code::INVALID_VALUE);
    assert!(e.1.contains("placement"), "{}", e.1);
    let twice = json!({"name": "x", "placement": "bulk", "function": {"$fn": "f1"}});
    assert_eq!(refusal(action(json!([twice, twice]))), (code::DUPLICATE, "Author: action 'x' is listed twice".into()));
}

#[test]
fn a_service_function_is_a_host_function() {
    let e = refusal(json!({"model": "Author", "service": {"create": {"$host": "int"}}}));
    assert_eq!(e, (code::INVALID_VALUE, "Author: create is a service function (ctx, ...)".into()));
}

#[test]
fn an_action_function_is_a_host_function() {
    let e = refusal(json!({"model": "Author", "actions": [{"name": "x", "placement": "bulk", "function": {"$host": "int"}}]}));
    assert_eq!(e.0, code::INVALID_VALUE);
    assert_eq!(e.1, "Author: action 'x' needs a function");
}

#[test]
fn an_option_of_a_wrong_type_names_its_model_and_key() {
    assert_eq!(refusal(json!({"model": "Note", "set_password": "no"})).1, "Note: set_password: invalid type: string \"no\", expected a boolean");
    assert_eq!(refusal(json!({"model": "Note", "bulk_delete": 1})).1, "Note: bulk_delete is true or false");
}

fn refusal_in(spelling: &str, resource: Value) -> (&'static str, String) {
    let e = build(json!({"spelling": spelling, "resources": [resource]}));
    (e.code, e.message)
}

#[test]
fn a_map_option_of_a_wrong_shape_says_what_it_maps() {
    for (key, text) in [
        ("widgets", "maps a field, a path or a type to a widget name"),
        ("see", "maps a field name to a permission name"),
        ("change", "maps a field name to a permission name"),
        ("check_messages", "maps a check name to a message"),
    ] {
        assert_eq!(refusal(json!({"model": "Note", key: ["x"]})), (code::INVALID_VALUE, format!("Note: {key} {text}")), "{key}");
    }
    assert_eq!(refusal_in("camel", json!({"model": "Note", "check_messages": 1})).1, "Note: checkMessages maps a check name to a message");
}

#[test]
fn the_site_widgets_option_of_a_wrong_shape_says_what_it_maps() {
    for widgets in [json!("title"), json!(["title"]), json!(5), json!({"title": 1})] {
        let e = build(json!({"widgets": widgets, "resources": []}));
        assert_eq!((e.code, e.message.as_str()), (code::INVALID_VALUE, "widgets maps a field, a path or a type to a widget name"));
    }
}

#[test]
fn a_form_that_is_not_a_list_or_a_map_of_sections_is_refused() {
    let e = refusal(json!({"model": "Note", "form": "x"}));
    assert_eq!(e, (code::INVALID_VALUE, "Note: form is a list of sections, or an object with create and edit".into()));
}

#[test]
fn a_list_option_of_a_wrong_shape_says_what_it_is() {
    assert_eq!(refusal(json!({"model": "Note", "readonly": "id"})).1, "Note: readonly is a list of field names");
    assert_eq!(refusal(json!({"model": "Note", "form_component": 3})).1, "Note: form_component is the path of a JavaScript file");
    assert_eq!(refusal_in("camel", json!({"model": "Note", "form_component": 3})).1, "Note: formComponent is the path of a JavaScript file");
    assert_eq!(refusal(json!({"model": "Note", "detail_tabs": "a"})).1, "Note: detail_tabs is a list of package tab names");
}

#[test]
fn a_form_section_has_a_title_and_a_list_of_field_names() {
    let text = "Note: form create: a section has a title and a list of field names";
    assert_eq!(refusal(json!({"model": "Note", "form": {"create": [{"title": "A", "fields": "id"}]}})), (code::INVALID_VALUE, text.into()));
    assert_eq!(refusal(json!({"model": "Note", "form": {"create": [1]}})), (code::INVALID_VALUE, text.into()));
}

#[test]
fn a_can_rule_is_change_or_delete() {
    let e = refusal(json!({"model": "Note", "can": {"edit": true}}));
    assert_eq!(e, (code::INVALID_VALUE, "Note: can has \"edit\", not one of change, delete".into()));
}

#[test]
fn an_inline_page_takes_no_refused_key() {
    let inline = |page: Value| json!({"model": "Author", "inlines": [{"model": "Book", "page": page}]});
    for (key, reason) in [
        ("create", "a service writes one record"),
        ("inlines", "an inline is never nested"),
        ("files", "a file field needs a multipart body"),
        ("perms", "use permission of the inline"),
        ("see", "field permissions belong to the registered page of the child"),
        ("bulk_delete", "an inline has no bulk delete"),
        ("form_component", "a replaced form cannot be a row of a table"),
        ("allow_protected_writes", "the opt-in belongs to the registered page of the child"),
    ] {
        let (c, m) = refusal(inline(json!({key: null})));
        assert_eq!(c, code::UNSUPPORTED, "{key}");
        assert!(m.starts_with(&format!("Author: inline Book: page does not take '{key}': ")) && m.contains(reason), "{key}: {m}");
    }
    let (_, m) = refusal_in("camel", inline(json!({"bulk_delete": true})));
    assert!(m.starts_with("Author: inline Book: page does not take 'bulkDelete': "), "{m}");
}

#[test]
fn an_inline_page_names_a_key_that_no_page_has() {
    let page = json!({"model": "Author", "inlines": [{"model": "Book", "page": {"nope": 1}}]});
    assert_eq!(refusal(page.clone()), (code::UNKNOWN_OPTION, "Author: inline Book: unknown page option 'nope'".into()));
    let camel = json!({"model": "Author", "inlines": [{"model": "Book", "page": {"page_size": null, "no_pe": 1}}]});
    assert_eq!(refusal_in("camel", camel).1, "Author: inline Book: unknown page option 'noPe'");
}

#[test]
fn an_inline_page_takes_the_keys_that_a_binding_lowers() {
    let config = json!({"resources": [{"model": "Author", "inlines": [{"model": "Book", "save": "row", "page": {"model": "Book", "service": {}, "host_behavior": true, "views": {}, "list": {"page_size": 5}}}]}, {"model": "Book"}]});
    let built = Site::build(schema(), &config.to_string());
    assert!(built.is_ok(), "{:?}", built.err().map(|e| e.message));
}
