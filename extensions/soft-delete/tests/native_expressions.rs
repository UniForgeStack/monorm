//! A native check or index filter can name the field that the soft-delete extension adds.
//! The test installs the built soft-delete library and compiles real schemas.
use monorm_core::dsl::{compile_file_with, generate_identities_with, Installed};
use monorm_core::ir::{ConstraintIr, SchemaIr};
use monorm_core::{migrate, schema::Schema};
use std::{fs, path::{Path, PathBuf}, process::Command, sync::OnceLock};

fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("native-expressions-{name}"));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn installed() -> &'static Installed {
    static INSTALLED: OnceLock<Installed> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let target = scratch("target");
        let mut build = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
        build.args(["build", "--offline", "--quiet", "--manifest-path"]).arg(root.join("../../Cargo.toml")).arg("--target-dir").arg(&target).args(["-p", "monorm-soft-delete"]);
        assert!(build.status().unwrap().success(), "the extension did not build");
        let file = format!("{}monorm_soft_delete{}", std::env::consts::DLL_PREFIX, std::env::consts::DLL_SUFFIX);
        let dir = scratch("library");
        fs::copy(target.join("debug").join(&file), dir.join(&file)).unwrap();
        let manifest = dir.join("monorm-extension.toml");
        fs::copy(root.join("monorm-extension.toml"), &manifest).unwrap();
        Installed { extensions: [("monorm.soft-delete".to_owned(), manifest)].into(), ..Installed::new() }
    })
}

fn write(name: &str, dialect: &str, body: &str) -> PathBuf {
    let path = scratch(name).join("schema.prisma");
    fs::write(&path, format!("datasource db {{\n provider = \"{dialect}\"\n}}\nmodel Doc {{\n id Int @id\n kind String\n @@soft_delete.deleted_at(\"deleted_at\")\n{body}}}\n")).unwrap();
    path
}

fn compile(name: &str, dialect: &str, body: &str) -> Result<SchemaIr, String> {
    compile_file_with(&write(name, dialect, body), installed())
}

const NATIVE: &str = " @@check(deleted_at == null | kind != \"x\")\n @@index([kind], where: deleted_at == null)\n @@index([kind], name: \"doc_live_kind\", unique: true, where: deleted_at == null)\n";

#[test]
fn a_native_check_and_filter_name_the_added_field() {
    for dialect in ["postgresql", "sqlite"] {
        let ir = compile(&format!("native-{dialect}"), dialect, NATIVE).unwrap_or_else(|e| panic!("{dialect}: {e}"));
        let model = ir.models.iter().find(|m| m.name == "Doc").unwrap();
        let ConstraintIr::Check { expr, .. } = &model.constraints[0] else { panic!("no check") };
        assert!(expr.contains("deleted_at") && !expr.contains('\u{1}'), "{dialect}: {expr}");
        assert!(model.indexes.iter().all(|ix| ix.where_.as_deref().is_none_or(|w| w.contains("deleted_at") && !w.contains('\u{1}'))), "{dialect}: {:?}", model.indexes);
        assert!(model.indexes.iter().any(|ix| ix.where_.is_some()), "{dialect}");
        // The physical copy holds the same text, and every slot is native.
        let storage = ir.behavior.storage.as_ref().expect("storage");
        let physical = storage.models.iter().find(|m| m.name == "Doc").unwrap();
        assert!(physical.indexes.iter().all(|ix| ix.where_.as_deref().is_none_or(|w| !w.contains('\u{1}'))), "{dialect}");
        assert!(ir.native_expression_slots.iter().any(|(m, p)| m == "Doc" && p == "check:0"), "{dialect}: {:?}", ir.native_expression_slots);
    }
}

#[test]
fn a_field_that_no_extension_adds_still_fails_with_the_same_message() {
    let error = compile("unknown", "postgresql", " @@check(nothing == null)\n").unwrap_err();
    assert!(error.contains("unknown field `nothing` on model `Doc`") && error.contains("schema.prisma:8:"), "{error}");
    let error = compile("unknown-filter", "postgresql", " @@index([kind], where: nothing == null)\n").unwrap_err();
    assert!(error.contains("unknown field `nothing` on model `Doc`"), "{error}");
}

/// The migration planner adopts an equivalent spelling of the SQL only where the expression is native:
/// the slots point at the physical indexes and checks that hold the resolved text.
#[test]
fn a_migration_plans_nothing_for_an_equivalent_spelling_of_a_native_expression() {
    let ir = compile("migrate", "postgresql", NATIVE).unwrap();
    let mut plain = ir.clone();
    plain.native_expression_slots.clear();
    let schema = Schema::from_ir(ir).unwrap();
    let mut previous = migrate::snapshot(&schema).unwrap();
    assert!(migrate::plan(&schema, &previous).unwrap().up.is_empty());
    let table = previous.tables.iter_mut().find(|t| t.name == "doc").expect("table doc");
    let mut changed = 0;
    for ix in &mut table.indexes {
        if let Some(w) = &mut ix.where_ { *w = format!("({w})"); changed += 1; }
    }
    for c in &mut table.checks { c.expr = format!("({})", c.expr); changed += 1; }
    assert!(changed >= 3, "{changed}");
    let plan = migrate::plan(&schema, &previous).unwrap();
    assert!(plan.up.is_empty(), "{:?}", plan.up.iter().map(|s| &s.sql).collect::<Vec<_>>());
    // Without the slots the same difference is a change, so the slots are what adopts the spelling.
    let schema = Schema::from_ir(plain).unwrap();
    assert!(!migrate::plan(&schema, &previous).unwrap().up.is_empty());
}

/// A filter that names a deprecated field and an added field is an error, as one that names only the deprecated field is.
#[test]
fn a_filter_naming_a_deprecated_field_and_an_added_field_is_an_error() {
    let body = " legacy String @default(\"x\") @deprecated\n @@index([kind], name: \"doc_both\", where: legacy == \"x\" & deleted_at == null)\n";
    let error = compile("deprecated", "postgresql", body).unwrap_err();
    assert!(error.contains("unknown field `legacy` on model `Doc`"), "{error}");
}

/// The identities command skips the extension passes, so it cannot resolve a field that a pass adds. It still accepts the schema.
#[test]
fn the_identities_command_accepts_a_native_expression_on_an_added_field() {
    let path = write("identities", "postgresql", NATIVE);
    generate_identities_with(&path, &[], &[], installed()).unwrap_or_else(|e| panic!("{e}"));
}

/// With no extension, no pass can add a field, so the identities command still reports an unknown field.
#[test]
fn the_identities_command_reports_an_unknown_field_when_no_extension_is_installed() {
    let path = scratch("identities-plain").join("schema.prisma");
    fs::write(&path, "datasource db {\n provider = \"postgresql\"\n}\nmodel Doc {\n id Int @id\n @@check(nothing == null)\n}\n").unwrap();
    let error = generate_identities_with(&path, &[], &[], &Installed::new()).unwrap_err();
    assert!(error.contains("unknown field `nothing` on model `Doc`"), "{error}");
}
