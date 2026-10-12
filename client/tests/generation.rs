use orm_core::{codegen::rust, dsl};
fn generate(source: &str) -> Result<String, String> {
    let (ir, schema) = dsl::check(dsl::compile(source, None)?)?;
    rust::generate(&ir, &schema)
}
#[test]
fn collision_and_language_checks_are_offline() {
    let cases = [
        (
            "model User {\n id Int @id\n objects String\n}",
            "helper normalization collision",
        ),
        (
            "model User {\n id Int @id\n}\nmodel UserSelect0 {\n id Int @id\n}",
            "helper normalization collision",
        ),
        (
            "model User {\n id Int @id\n foo_bar String\n foo__bar String\n}",
            "shape associated type collision",
        ),
        (
            "model User {\n id Int @id\n userName String\n user_name String\n}",
            "normalization collision",
        ),
        (
            "model User {\n id Int @id\n id_key Int\n}",
            "unique token collision",
        ),
        (
            "model User {\n id Int @id\n __orm_identity Int\n}",
            "reserved Rust helper",
        ),
        (
            "model User {\n id Int @id\n}\nmodel UserFields {\n id Int @id\n}",
            "helper normalization collision",
        ),
        (
            "model User {\n id Int @id\n self String\n}",
            "supported Rust identifier",
        ),
    ];
    for (source, message) in cases {
        assert!(generate(source).unwrap_err().contains(message), "{source}");
    }
    let output = generate("model User {\n id Int @id\n type String @map(\"kind\")\n}").unwrap();
    assert!(output.contains("r#type"));
}
#[test]
fn mismatched_prepared_schema_is_rejected() {
    let (ir, _) =
        dsl::check(dsl::compile("model A {\n id Int @id\n name String\n}", None).unwrap()).unwrap();
    let (_, other) =
        dsl::check(dsl::compile("model A {\n id Int @id\n name Int\n}", None).unwrap()).unwrap();
    assert!(rust::generate(&ir, &other)
        .unwrap_err()
        .contains("does not match"));
}
#[test]
fn extension_type_without_rust_support_is_rejected() {
    let source = "model A {\n id Int @id\n label String\n}";
    let (mut ir, _) = dsl::check(dsl::compile(source, None).unwrap()).unwrap();
    ir.models[0].fields[1]
        .hints
        .insert("python".into(), "Custom".into());
    let schema = orm_core::schema::Schema::from_ir(
        orm_core::ir::SchemaIr::from_json(&orm_core::codegen::embedded_schema_json(&ir).unwrap())
            .unwrap(),
    )
    .unwrap();
    assert!(rust::generate(&ir, &schema)
        .unwrap_err()
        .contains("Rust language support"));
}

#[test]
fn unsupported_sqlite_types_fail_before_database_access() {
    for field in ["tags String[]", "amount Decimal"] {
        let source = format!(
            "datasource db {{\n provider = \"sqlite\"\n}}\nmodel A {{\n id Int @id\n {field}\n}}"
        );
        assert!(generate(&source)
            .unwrap_err()
            .contains("sqlite does not support"));
    }
}
