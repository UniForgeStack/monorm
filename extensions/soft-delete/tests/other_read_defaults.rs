//! The soft-delete check reads the read defaults that other installed extensions write.
//! The test installs the built libraries of the soft-delete and query-defaults packages and compiles real schemas.
use monorm_core::dsl::{compile_file_with, Installed};
use std::{fs, path::{Path, PathBuf}, process::Command, sync::OnceLock};

const PACKAGES: [(&str, &str, &str); 2] = [
    ("monorm.soft-delete", "monorm-soft-delete", "monorm_soft_delete"),
    ("monorm.query-defaults", "monorm-query-defaults", "monorm_query_defaults"),
];

fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("other-read-defaults-{name}"));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Builds both libraries once and installs each next to its manifest.
fn installed() -> &'static Installed {
    static INSTALLED: OnceLock<Installed> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let target = scratch("target");
        let mut build = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
        build.args(["build", "--offline", "--quiet", "--manifest-path"]).arg(root.join("../Cargo.toml")).arg("--target-dir").arg(&target);
        for (_, package, _) in PACKAGES { build.args(["-p", package]); }
        assert!(build.status().unwrap().success(), "the extensions did not build");
        let extensions = PACKAGES.iter().map(|(id, _, library)| {
            let file = format!("{}{library}{}", std::env::consts::DLL_PREFIX, std::env::consts::DLL_SUFFIX);
            let dir = scratch(library);
            fs::copy(target.join("debug").join(&file), dir.join(&file)).unwrap();
            let manifest = dir.join("monorm-extension.toml");
            fs::copy(root.join(library.strip_prefix("monorm_").unwrap().replace('_', "-")).join("monorm-extension.toml"), &manifest).unwrap();
            ((*id).to_owned(), manifest)
        }).collect();
        Installed { extensions, ..Installed::new() }
    })
}

fn compile_schema(name: &str, schema: &str) -> Result<(), String> {
    let path = scratch(name).join("schema.prisma");
    fs::write(&path, schema).unwrap();
    compile_file_with(&path, installed()).map(drop)
}

fn compile(name: &str, extra: &str) -> Result<(), String> {
    compile_schema(name, &format!("model Author {{\n id Int @id\n name String\n @@soft_delete.deleted_at(\"deleted_at\")\n{extra}}}\n"))
}

/// The native expression is the documented form; the string form is deprecated and still reaches the check.
#[test]
fn a_filter_that_repeats_the_deleted_default_is_rejected_with_its_position() {
    for (name, line, text) in [
        ("filter", "@@query.filter(\"live\", deleted_at == null)", "@query.filter"),
        ("filter-parens", "@@query.filter(\"live\", (deleted_at == null))", "@query.filter"),
        ("defaults", "@@query.defaults(filter: deleted_at == null)", "@query.defaults"),
        ("filter-string", "@@query.filter(\"live\", \"deleted_at == null\")", "@query.filter"),
        ("defaults-string", "@@query.defaults(filter: \"deleted_at == null\")", "@query.defaults"),
    ] {
        let error = compile(name, &format!(" {line}\n")).unwrap_err();
        assert!(error.contains(text) && error.contains("Author already hides soft-deleted rows by default; remove this filter"), "{name}: {error}");
        assert!(error.contains("schema.prisma:5:"), "{name}: {error}");
    }
}

#[test]
fn other_filters_on_the_model_pass() {
    compile("other", " @@query.filter(\"mine\", deleted_at == null & id > 0)\n @@query.defaults(filter: id > 0)\n").unwrap();
    compile("alone", "").unwrap();
}

const PARENT: &str = "model Note {\n id Int @id\n deleted_at DateTime?\n @@query.filter(\"live\", deleted_at == null)\n}\n";

/// A filter that a model inherits from a parent policy hides soft-deleted rows from `all_with_deleted()` too.
#[test]
fn an_inherited_filter_that_repeats_the_deleted_default_is_rejected() {
    let child = "model Author {\n id Int @id\n @@soft_delete.deleted_at(\"deleted_at\")\n @@query.parent(\"Note\")\n}\n";
    let error = compile_schema("inherited", &format!("{PARENT}{child}")).unwrap_err();
    assert!(error.contains("Author already hides soft-deleted rows by default; remove this filter"), "{error}");
    assert!(error.contains("schema.prisma:4:") && error.contains("this declaration is on Note, which Author inherits"), "the error names the parent's declaration: {error}");
}

/// The child overrides the inherited filter by name, so the soft-delete default is the only live filter.
#[test]
fn a_child_can_override_the_inherited_filter() {
    let child = "model Author {\n id Int @id\n @@soft_delete.deleted_at(\"deleted_at\")\n @@query.parent(\"Note\")\n @@query.filter(\"live\", \"none\")\n}\n";
    compile_schema("override", &format!("{PARENT}{child}")).unwrap();
}
