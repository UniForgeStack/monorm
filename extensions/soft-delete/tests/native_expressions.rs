//! A native check or index filter can name the field that the soft-delete extension adds.
//! The test installs the built soft-delete library and compiles real schemas.
use monorm_core::dsl::{compile_file_with, Installed};
use monorm_core::ir::{ConstraintIr, SchemaIr};
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

fn compile(name: &str, dialect: &str, body: &str) -> Result<SchemaIr, String> {
    let path = scratch(name).join("schema.prisma");
    fs::write(&path, format!("datasource db {{\n provider = \"{dialect}\"\n}}\nmodel Doc {{\n id Int @id\n kind String\n @@soft_delete.deleted_at(\"deleted_at\")\n{body}}}\n")).unwrap();
    compile_file_with(&path, installed())
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
