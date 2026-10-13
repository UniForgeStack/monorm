fn main() {
    println!("cargo:rerun-if-changed=examples/schema.prisma");
    println!("cargo:rerun-if-changed=examples/loading.prisma");
    let source = std::fs::read_to_string("examples/schema.prisma").unwrap()
        + &std::fs::read_to_string("examples/loading.prisma").unwrap();
    let ir = policy(orm_core::dsl::compile(&source, None).unwrap());
    let (ir, schema) = orm_core::dsl::check(ir).unwrap();
    let postgres = source.replace("sqlite", "postgresql") + "\nmodel Extra {\n id Int @id\n amount Decimal\n tags String[]\n priorities Priority[]\n @@map(\"rust_read_extra\")\n}\n";
    let (pg_ir, pg_schema) =
        orm_core::dsl::check(policy(orm_core::dsl::compile(&postgres, None).unwrap())).unwrap();
    std::fs::write(
        std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("postgres.rs"),
        orm_core::codegen::rust::generate(&pg_ir, &pg_schema).unwrap(),
    )
    .unwrap();
    let generated = orm_core::codegen::rust::generate(&ir, &schema).unwrap();
    std::fs::write(
        std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("models.rs"),
        generated,
    )
    .unwrap();
}

fn policy(mut ir: orm_core::ir::SchemaIr) -> orm_core::ir::SchemaIr {
    ir.behavior.schema_contract = 1;
    ir.behavior.query_defaults = orm_client_fixture_defaults();
    ir
}
fn orm_client_fixture_defaults() -> Vec<orm_core::behavior::QueryDefaults> {
    vec![
        orm_core::behavior::QueryDefaults {
            model: "OrderedAuthor".into(),
            parent: None,
            fields: Some(vec!["name".into()]),
            related: vec![],
            filter: Some(serde_json::json!({"t":"col","path":[],"name":"visible"})),
            order: vec![orm_core::behavior::OrderKey {
                field: "name".into(),
                desc: true,
                nulls: None,
            }],
        },
        orm_core::behavior::QueryDefaults {
            model: "PolicyAuthor".into(),
            parent: None,
            fields: Some(vec!["name".into(), "note".into()]),
            related: vec![],
            filter: Some(serde_json::json!({"t":"col","path":[],"name":"visible"})),
            order: vec![],
        },
        orm_core::behavior::QueryDefaults {
            model: "PolicyBook".into(),
            parent: None,
            fields: Some(vec!["title".into()]),
            related: vec![vec!["author".into()]],
            filter: None,
            order: vec![],
        },
    ]
}
