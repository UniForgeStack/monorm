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
    ir.behavior.schema_contract = orm_core::behavior::SCHEMA_CONTRACT;
    ir.behavior.read_defaults = orm_client_fixture_defaults();
    ir
}
fn orm_client_fixture_defaults() -> Vec<orm_core::behavior::ReadDefault> {
    vec![
        orm_core::behavior::ReadDefault {
            model: "OrderedAuthor".into(),
            source: "orm.query-defaults".into(), name: "query".into(), annotate: Default::default(),
            fields: Some(vec!["name".into()]),
            load: vec![],
            filter: Some(serde_json::json!({"t":"col","path":[],"name":"visible"})),
            order: Some(vec![orm_core::behavior::OrderKey {
                field: "name".into(),
                desc: true,
                nulls: None,
            }]),
        },
        orm_core::behavior::ReadDefault {
            model: "PolicyAuthor".into(),
            source: "orm.query-defaults".into(), name: "query".into(), annotate: Default::default(),
            fields: Some(vec!["name".into(), "note".into()]),
            load: vec![],
            filter: Some(serde_json::json!({"t":"col","path":[],"name":"visible"})),
            order: None,
        },
        orm_core::behavior::ReadDefault {
            model: "ScopedNote".into(),
            source: "orm.query-defaults".into(), name: "query".into(), annotate: Default::default(),
            fields: None,
            load: vec![],
            filter: Some(serde_json::json!({"t":"cmp","op":"eq","l":{"t":"col","path":[],"name":"shop"},"r":{"t":"scope","name":"shop"}})),
            order: None,
        },
        orm_core::behavior::ReadDefault {
            model: "PolicyBook".into(),
            source: "orm.query-defaults".into(), name: "query".into(), annotate: Default::default(),
            fields: Some(vec!["title".into()]),
            load: vec![vec!["author".into()]],
            filter: None,
            order: None,
        },
    ]
}
