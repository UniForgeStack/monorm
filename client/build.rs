fn main() {
    println!("cargo:rerun-if-changed=examples/schema.prisma");
    let source = std::fs::read_to_string("examples/schema.prisma").unwrap();
    let ir = orm_core::dsl::compile(&source, None).unwrap();
    let (ir, schema) = orm_core::dsl::check(ir).unwrap();
    let postgres = source.replace("sqlite", "postgresql") + "\nmodel Extra {\n id Int @id\n amount Decimal\n tags String[]\n priorities Priority[]\n @@map(\"rust_read_extra\")\n}\n";
    let (pg_ir, pg_schema) =
        orm_core::dsl::check(orm_core::dsl::compile(&postgres, None).unwrap()).unwrap();
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
