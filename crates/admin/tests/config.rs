//! The error of a wrong config and wrong settings: the code, the path, and the spelling of an option key.
use std::sync::Arc;

use monorm_admin::config::{security_options, Config};
use monorm_admin::{code, AdminError, PathPart, Site};
use monorm_core::{dsl, schema::Schema};
use serde_json::{json, Value};

const ADMIN: &str = include_str!("../../../apps/admin/schema.prisma");

fn schema() -> Arc<Schema> {
    let source = format!("datasource db {{\n  provider = \"sqlite\"\n}}\n\nmodel Note {{\n  id Int @id\n}}\n{ADMIN}");
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
