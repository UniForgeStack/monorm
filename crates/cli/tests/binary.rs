//! The standalone `orm` binary. The commands themselves are tested through
//! `python -m orm` (languages/python/tests/test_migrations.py) and `npx orm` (languages/js/test/migrations.test.ts),
//! which run the same code; this checks what is specific to the binary.

use std::path::Path;
use std::process::{Command, Output};

const SCHEMA: &str = "model Book {\n  id    BigInt @id @default(autoincrement())\n  title String\n}\n";

fn orm(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orm")).args(args).current_dir(dir).env_remove("ORM_DATABASE_URL").output().unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn binary() {
    let dir = std::env::temp_dir().join(format!("orm-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("db")).unwrap();
    std::fs::write(dir.join("db/schema.prisma"), SCHEMA).unwrap();

    let help = orm(&dir, &["--help"]);
    assert!(help.status.success());
    assert!(text(&help.stdout).starts_with("usage: orm "));
    assert_eq!(orm(&dir, &[]).status.code(), Some(2));

    // no schema.prisma in the working directory
    assert_eq!(orm(&dir, &["check"]).status.code(), Some(1));
    // configuration: pyproject.toml
    std::fs::write(dir.join("pyproject.toml"), "[tool.orm]\nschema = \"db/schema.prisma\"\nmigrations = \"db/migrations\"\n").unwrap();
    let check = orm(&dir, &["check"]);
    assert_eq!(text(&check.stdout), "db/schema.prisma: ok (1 models)\n");

    // the binary has no default language: it needs one, or an output file to tell
    assert_eq!(orm(&dir, &["generate"]).status.code(), Some(2));
    assert!(orm(&dir, &["generate", "-o", "out/models.ts"]).status.success());
    assert!(dir.join("out/models.ts").is_file());
    assert!(orm(&dir, &["generate", "python"]).status.success());
    assert!(dir.join("db/models.py").is_file() && dir.join("db/models.pyi").is_file());

    assert!(orm(&dir, &["makemigrations", "--name", "start"]).status.success());
    assert!(dir.join("db/migrations/0001_start/up.sql").is_file());
    let sql = orm(&dir, &["sqlmigrate", "1"]);
    assert!(text(&sql.stdout).contains("CREATE TABLE \"book\""), "{}", text(&sql.stdout));

    let no_db = orm(&dir, &["migrate"]);
    assert_eq!(no_db.status.code(), Some(2));
    assert!(text(&no_db.stderr).contains("ORM_DATABASE_URL"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_warning_prints_with_its_file_line_and_column() {
    let dir = std::env::temp_dir().join(format!("orm-cli-warning-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("schema.prisma"), "import \"library.prisma\"\n").unwrap();
    std::fs::write(dir.join("library.prisma"), "\nmodel Item {\n  id Int @id\n  qty Int @check(\"qty > 0\")\n}\n").unwrap();
    let check = orm(&dir, &["check", "--schema", "schema.prisma"]);
    assert!(check.status.success(), "{}", text(&check.stderr));
    let stderr = text(&check.stderr);
    assert!(stderr.contains("library.prisma:4:") && stderr.contains("warning:") && stderr.contains("raw("), "{stderr}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn schema_module_output_collisions_fail_before_writing() {
    let dir = std::env::temp_dir().join(format!("orm-cli-import-collision-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("schema.prisma"), "import \"library.prisma\"\n").unwrap();
    std::fs::write(dir.join("library.prisma"), SCHEMA).unwrap();
    let result = orm(&dir, &["generate", "python"]);
    assert!(!result.status.success());
    assert!(text(&result.stderr).contains("each schema in its own directory"));
    assert!(!dir.join("models.py").exists());
    assert!(!dir.join("_orm_models.py").exists());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn identity_cli_allocates_explicitly_and_compile_never_mutates() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.test-tmp").join(format!("orm-cli-identities-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("schema.prisma");
    std::fs::write(&path, "model Post {\nid Int @id\n}\nmodel Tag {\nid Int @id\nkind ContentType\nobject_id Int\n}").unwrap();
    assert!(!orm(&dir, &["compile"]).status.success());
    let generated = orm(&dir, &["identities"]);
    assert!(generated.status.success(), "{}", text(&generated.stderr));
    assert!(text(&generated.stdout).contains("added Post = 1\nadded Tag = 2\n"), "{}", text(&generated.stdout));
    let before = std::fs::read(dir.join("schema.identities.json")).unwrap();
    for args in [&["compile"][..], &["check"][..], &["generate", "python"][..], &["generate", "typescript"][..]] {
        let out = orm(&dir, args); assert!(out.status.success(), "{}", text(&out.stderr));
    }
    assert_eq!(std::fs::read(dir.join("schema.identities.json")).unwrap(), before);
    let py = std::fs::read_to_string(dir.join("models.pyi")).unwrap();
    let ts = std::fs::read_to_string(dir.join("models.ts")).unwrap();
    assert!(py.contains("class ContentType(IntEnum)"));
    assert!(ts.contains("Post: 1"), "{ts}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn pull_baseline_and_drift_on_sqlite() {
    let dir = std::env::temp_dir().join(format!("orm-cli-pull-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let url = format!("sqlite://{}", dir.join("live.db").display());
    let source = format!("datasource db {{\n  provider = \"sqlite\"\n}}\n\n{SCHEMA}");
    std::fs::write(dir.join("made.prisma"), &source).unwrap();
    let made = ["--url", url.as_str(), "--schema", "made.prisma", "--dir", "made"];
    assert!(orm(&dir, &[&made[..], &["makemigrations"]].concat()).status.success());
    assert!(orm(&dir, &[&made[..], &["migrate"]].concat()).status.success());

    // pull writes the configured schema file, and refuses to overwrite one
    let pulled = orm(&dir, &["--url", &url, "pull"]);
    assert!(pulled.status.success(), "{}", text(&pulled.stderr));
    assert!(text(&pulled.stdout).contains("The schema reproduces the database."), "{}", text(&pulled.stdout));
    assert!(std::fs::read_to_string(dir.join("schema.prisma")).unwrap().contains("model Book {"));
    assert_eq!(orm(&dir, &["--url", &url, "pull"]).status.code(), Some(2));

    // baseline refuses a database that has applied migrations, before writing one
    let refused = orm(&dir, &["--url", &url, "baseline"]);
    assert_eq!(refused.status.code(), Some(1));
    assert!(text(&refused.stderr).contains("already has applied migrations"), "{}", text(&refused.stderr));
    assert!(!dir.join("migrations").exists());

    let drift = orm(&dir, &[&made[..], &["drift"]].concat());
    assert_eq!(drift.status.code(), Some(0), "{}", text(&drift.stdout));
    assert!(text(&drift.stdout).contains("No drift from 0001_initial."));
    // a newer snapshot than the database: drift exits 1 and names the change
    std::fs::write(dir.join("made.prisma"), source.replace("title String", "title String\n  pages Int @default(0)")).unwrap();
    assert!(orm(&dir, &[&made[..], &["makemigrations"]].concat()).status.success());
    let drift = orm(&dir, &[&made[..], &["drift"]].concat());
    assert_eq!(drift.status.code(), Some(1));
    assert!(text(&drift.stdout).contains("add column book.pages"), "{}", text(&drift.stdout));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn app_flag_gives_the_loader_the_schema_of_an_app() {
    let dir = std::env::temp_dir().join(format!("orm-cli-app-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("vendor")).unwrap();
    std::fs::write(dir.join("vendor/auth.prisma"), "model User {\n  id Int @id\n}\n").unwrap();
    std::fs::write(dir.join("schema.prisma"), "import \"app:orm-auth\" (prefix: \"auth_\")\nmodel Post {\n  id Int @id\n}\n").unwrap();

    let missing = orm(&dir, &["check", "--schema", "schema.prisma"]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(text(&missing.stderr).contains("install the `orm-auth` package (or pass --app orm-auth=PATH)"), "{}", text(&missing.stderr));
    assert_eq!(orm(&dir, &["check", "--app", "orm-auth"]).status.code(), Some(2));

    let ok = orm(&dir, &["check", "--schema", "schema.prisma", "--app", "orm-auth=vendor/auth.prisma"]);
    assert!(ok.status.success(), "{}", text(&ok.stderr));
    assert!(text(&ok.stdout).contains("(2 models)"), "{}", text(&ok.stdout));
}

#[test]
fn generate_python_writes_nothing_into_an_app_package() {
    let dir = std::env::temp_dir().join(format!("orm-cli-app-gen-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("site/orm_auth")).unwrap();
    std::fs::create_dir_all(dir.join("proj")).unwrap();
    std::fs::write(dir.join("site/orm_auth/schema.prisma"), "model User {\n  id Int @id\n}\n").unwrap();
    std::fs::write(dir.join("proj/schema.prisma"), "import \"app:orm-auth\"\nmodel Post {\n  id Int @id\n}\n").unwrap();
    let before = std::fs::read_dir(dir.join("site/orm_auth")).unwrap().count();

    let out = orm(&dir.join("proj"), &["--app", "orm-auth=../site/orm_auth/schema.prisma", "generate", "python"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(std::fs::read_dir(dir.join("site/orm_auth")).unwrap().count(), before, "{}", text(&out.stdout));
    let module = std::fs::read_to_string(dir.join("proj/models.py")).unwrap();
    assert!(module.contains("\"User\",") && module.contains("\"Post\","), "{module}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_app_flags_of_the_user_win_over_later_ones() {
    let dir = std::env::temp_dir().join(format!("orm-cli-app-win-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("dev.prisma"), "model User {\n  id Int @id\n}\nmodel Dev {\n  id Int @id\n}\n").unwrap();
    std::fs::write(dir.join("installed.prisma"), "model User {\n  id Int @id\n}\n").unwrap();
    std::fs::write(dir.join("schema.prisma"), "import \"app:orm-auth\" (prefix: \"auth_\")\n").unwrap();

    let out = orm(&dir, &["check", "--schema", "schema.prisma", "--app", "orm-auth=dev.prisma", "--app", "orm-auth=installed.prisma"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(text(&out.stdout).contains("(2 models)"), "{}", text(&out.stdout));
    let _ = std::fs::remove_dir_all(&dir);
}

fn extension_fixture(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("orm-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/outside-extension");
    let target = Path::new(env!("CARGO_TARGET_TMPDIR")).join("cli-outside");
    let build = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["build", "--offline", "--quiet", "--manifest-path"]).arg(root.join("Cargo.toml")).arg("--target-dir").arg(&target).status().unwrap();
    assert!(build.success(), "the outside extension did not build");
    let library = format!("{}outside_extension{}", std::env::consts::DLL_PREFIX, std::env::consts::DLL_SUFFIX);
    std::fs::copy(target.join("debug").join(&library), dir.join(&library)).unwrap();
    std::fs::copy(root.join("orm-extension.toml"), dir.join("orm-extension.toml")).unwrap();
    std::fs::write(dir.join("schema.prisma"), "model Post {\n  id Int @id\n  @@outside.tag(\"blue\")\n}\n").unwrap();

    dir
}

#[test]
fn extension_flag_gives_the_loader_an_installed_library() {
    let dir = extension_fixture("extension");

    let missing = orm(&dir, &["check", "--schema", "schema.prisma"]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(text(&missing.stderr).contains("outside.tag"), "{}", text(&missing.stderr));
    assert_eq!(orm(&dir, &["check", "--extension", "acme.outside"]).status.code(), Some(2));

    let ok = orm(&dir, &["check", "--schema", "schema.prisma", "--extension", "acme.outside=orm-extension.toml"]);
    assert!(ok.status.success(), "{}", text(&ok.stderr));
    let generated = orm(&dir, &["generate", "--schema", "schema.prisma", "--extension", "acme.outside=orm-extension.toml", "--extension-module", "acme.outside=acme_outside", "-o", "out/models.ts"]);
    assert!(generated.status.success(), "{}", text(&generated.stderr));
    assert!(std::fs::read_to_string(dir.join("out/models.ts")).unwrap().contains("tag"));

    let manifest = ["--extension", "acme.outside=orm-extension.toml"];
    let made = orm(&dir, &[&["makemigrations", "--schema", "schema.prisma"][..], &manifest].concat());
    assert!(made.status.success(), "{}", text(&made.stderr));
    let sql = orm(&dir, &[&["sqlmigrate", "1", "--schema", "schema.prisma"][..], &manifest].concat());
    assert!(sql.status.success(), "{}", text(&sql.stderr));
    assert!(text(&sql.stdout).contains("\"tag\""), "{}", text(&sql.stdout));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn outside_adapter_generates_its_runtime_and_rejects_a_method_collision() {
    let dir = extension_fixture("adapter");
    let flags = ["--extension", "acme.outside=orm-extension.toml", "--extension-module", "acme.outside=acme_outside"];
    std::fs::write(dir.join("schema.prisma"), "model Note {\n  id Int @id\n  note Json? @outside.wrapped(\"w:\")\n}\n").unwrap();
    for (language, output, expected) in [
        ("python", "models.py", ["from acme_outside import install", "configure_outside"]),
        ("typescript", "models.ts", ["from \"@acme/outside\"", "configureOutside"]),
    ] {
        let generated = orm(&dir, &[&["generate", language, "--schema", "schema.prisma", "-o", output][..], &flags].concat());
        assert!(generated.status.success(), "{language}: {}", text(&generated.stderr));
        let models = std::fs::read_to_string(dir.join(output)).unwrap();
        for part in expected { assert!(models.contains(part), "{language}: {part} is missing from {models}"); }
    }
    // `note_unwrap` is the Python method and `noteUnwrap` the TypeScript one; TypeScript also camel-cases the field name.
    for (language, field, method) in [("python", "note_unwrap", "note_unwrap"), ("typescript", "noteUnwrap", "noteUnwrap"), ("typescript", "note_unwrap", "noteUnwrap")] {
        std::fs::write(dir.join("schema.prisma"), format!("model Note {{\n  id Int @id\n  note Json? @outside.wrapped(\"w:\")\n  {field} String\n}}\n")).unwrap();
        let rejected = orm(&dir, &[&["generate", language, "--schema", "schema.prisma", "-o", "clash.out"][..], &flags].concat());
        assert_eq!(rejected.status.code(), Some(1), "{language} {field}");
        assert!(text(&rejected.stderr).contains(method), "{language} {field}: {}", text(&rejected.stderr));
        assert!(!dir.join("clash.out").exists(), "{language} {field}: a rejected generate wrote its output");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn check_skips_handwritten_defaults_and_rust_models_on_every_host() {
    let dir = std::env::temp_dir().join(format!("orm-cli-check-defaults-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let schema = dir.join("schema.prisma");
    std::fs::write(&schema, SCHEMA).unwrap();
    for name in ["models.py", "models.ts", "models.rs"] {
        std::fs::write(dir.join(name), "handwritten models\n").unwrap();
    }
    for host in [orm_cli::Host::Binary, orm_cli::Host::Python, orm_cli::Host::Node] {
        let args = vec!["check".into(), "--schema".into(), schema.display().to_string()];
        assert_eq!(orm_cli::run_blocking(&args, host), 0);
    }
    // Rust output is outside this slice, including Rust's non-JSON Unicode escapes.
    std::fs::write(dir.join("models.rs"), "const JSON: &'static str = \"\\u{301}\";\n").unwrap();
    assert!(orm(&dir, &["check"]).status.success());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn check_reads_generated_defaults_from_other_languages_on_every_host() {
    let dir = std::env::temp_dir().join(format!("orm-cli-check-hosts-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let schema = dir.join("schema.prisma");
    std::fs::write(&schema, SCHEMA).unwrap();
    for (name, header) in [
        ("models.py", "# Generated from schema.prisma by `python -m orm generate`. Do not edit.\n"),
        ("models.ts", "// Generated from schema.prisma by `orm generate typescript`. Do not edit.\n"),
    ] {
        std::fs::write(dir.join(name), header).unwrap();
        for host in [orm_cli::Host::Binary, orm_cli::Host::Python, orm_cli::Host::Node] {
            let args = vec!["check".into(), "--schema".into(), schema.display().to_string()];
            assert_eq!(orm_cli::run_blocking(&args, host), 1, "{name}");
        }
        std::fs::remove_file(dir.join(name)).unwrap();
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn check_rejects_a_missing_explicit_output() {
    let dir = std::env::temp_dir().join(format!("orm-cli-check-explicit-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("schema.prisma"), SCHEMA).unwrap();
    let result = orm(&dir, &["check", "-o", "typo.py"]);
    assert_eq!(result.status.code(), Some(1));
    assert!(text(&result.stderr).contains("no generated models at typo.py; run `orm generate`"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn check_compares_extension_locks_and_python_facades() {
    let dir = extension_fixture("check-lock");
    let generated = orm(&dir, &["generate", "--extension", "acme.outside=orm-extension.toml", "--extension-module", "acme.outside=acme_outside", "-o", "out/models.ts"]);
    assert!(generated.status.success(), "{}", text(&generated.stderr));
    // The lock comes from the same schema bytes consumed by define().
    let models = std::fs::read_to_string(dir.join("out/models.ts")).unwrap();
    assert!(models.contains("manifest_hash"), "{models}");
    // Independently measured with shasum -a 256 on the fixture's raw bytes.
    assert!(models.contains("7ff5781269600af573026349e08745d47ed333f95cf5e41244ee5507205a6861"));
    let original = std::fs::read_to_string(dir.join("orm-extension.toml")).unwrap();
    for output in ["out/models.ts", "models.py"] {
        let generated = orm(&dir, &["generate", "--extension", "acme.outside=orm-extension.toml", "--extension-module", "acme.outside=acme_outside", "-o", output]);
        assert!(generated.status.success(), "{}", text(&generated.stderr));
        let check = || orm(&dir, &["check", "--extension", "acme.outside=orm-extension.toml", "--out", output]);
        assert!(check().status.success());
        for changed in [original.replace("0.1.0", "0.2.0"), format!("{original}\n# manifest changed\n")] {
            std::fs::write(dir.join("orm-extension.toml"), &changed).unwrap();
            let stale = check();
            assert_eq!(stale.status.code(), Some(1));
            let error = text(&stale.stderr);
            for expected in ["acme.outside", "locked", "0.1.0", "installed", "hash", "run `orm generate`"] {
                assert!(error.contains(expected), "missing {expected}: {error}");
            }
            if changed.contains("0.2.0") { assert!(error.contains("0.2.0"), "{error}"); }
        }
        std::fs::write(dir.join("orm-extension.toml"), &original).unwrap();
        let missing = orm(&dir, &["check", "-o", output]);
        assert_eq!(missing.status.code(), Some(1));
        let error = text(&missing.stderr);
        assert!(error.contains("acme.outside") && error.contains("not installed") && error.contains("run `orm generate`"), "{error}");
        // Installation alone never adds a lock requirement.
        std::fs::write(dir.join("unused.toml"), "id = \"acme.unused\"\nversion = \"9.0.0\"\n").unwrap();
        let unused = orm(&dir, &["check", "-o", output, "--extension", "acme.outside=orm-extension.toml", "--extension", "acme.unused=unused.toml"]);
        assert!(unused.status.success(), "{}", text(&unused.stderr));
    }

    std::fs::write(dir.join("app.prisma"), "model AppItem {\n id Int @id\n}\n").unwrap();
    std::fs::write(dir.join("consumer.prisma"), "import \"app:sample\"\nmodel Consumer {\n id Int @id\n}\n").unwrap();
    let app = orm(&dir, &["generate", "python", "--schema", "consumer.prisma", "--app", "sample=app.prisma", "-o", "app-models.py"]);
    assert!(app.status.success(), "{}", text(&app.stderr));
    let app_check = orm(&dir, &["check", "--schema", "consumer.prisma", "--app", "sample=app.prisma", "-o", "app-models.py"]);
    assert!(app_check.status.success(), "{}", text(&app_check.stderr));

    // Default paths and the shared Python schema behind generated facades.
    std::fs::create_dir_all(dir.join("multi")).unwrap();
    std::fs::write(dir.join("multi/schema.prisma"), "import \"../schema.prisma\"\nmodel Other {\n id Int @id\n}\n").unwrap();
    let multi = orm(&dir, &["generate", "python", "--schema", "multi/schema.prisma", "--extension", "acme.outside=orm-extension.toml", "--extension-module", "acme.outside=acme_outside"]);
    assert!(multi.status.success(), "{}", text(&multi.stderr));
    assert!(dir.join("multi/_orm_models.py").is_file());
    let current = orm(&dir, &["check", "--schema", "multi/schema.prisma", "--extension", "acme.outside=orm-extension.toml"]);
    assert!(current.status.success(), "{}", text(&current.stderr));
    std::fs::write(dir.join("orm-extension.toml"), format!("{original}\n# changed\n")).unwrap();
    let stale = orm(&dir, &["check", "--schema", "multi/schema.prisma", "--extension", "acme.outside=orm-extension.toml"]);
    assert_eq!(stale.status.code(), Some(1));
    assert!(text(&stale.stderr).contains("run `orm generate`"));
    std::fs::write(dir.join("orm-extension.toml"), &original).unwrap();

    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn fmt_native_is_offline_idempotent_and_preserves_imports_comments_and_history() {
    let dir = std::env::temp_dir().join(format!("orm-cli-native-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let root = "// root stays intact\nimport \"item.prisma\"\n";
    let old = "// comment é\nmodel Item {\n id Int @id\n qty Int @map(\"Qty\") @check(raw(\"(\\\"Qty\\\" > 0)\")) // field comment\n label String\n @@check(raw(\"mystery(label) IS NOT NULL\"), name: \"unsupported\")\n @@index([sql(\"abs( \\\"Qty\\\" )\")], where: raw(\"\\\"Qty\\\" > 0\"))\n}\n";
    std::fs::write(dir.join("schema.prisma"), root).unwrap();
    std::fs::write(dir.join("item.prisma"), old).unwrap();
    let initial = orm(&dir, &["makemigrations", "initial"]);
    assert!(initial.status.success(), "{}", text(&initial.stderr));
    let snapshot = std::fs::read(dir.join("migrations/0001_initial/snapshot.json")).unwrap();
    let old = old.replace(" label String", " pending String?\n label String");
    std::fs::write(dir.join("item.prisma"), &old).unwrap();
    let converted = orm(&dir, &["fmt", "--native"]);
    assert!(converted.status.success(), "{}", text(&converted.stderr));
    assert!(text(&converted.stderr).contains("item.prisma:7:"), "{}", text(&converted.stderr));
    let expected = old.replace("raw(\"(\\\"Qty\\\" > 0)\")", "((qty > 0))").replace("sql(\"abs( \\\"Qty\\\" )\")", "abs(qty)").replace("raw(\"\\\"Qty\\\" > 0\")", "(qty > 0)");
    assert_eq!(std::fs::read_to_string(dir.join("item.prisma")).unwrap(), expected);
    assert_eq!(std::fs::read_to_string(dir.join("schema.prisma")).unwrap(), root);
    let second = orm(&dir, &["fmt", "--native"]);
    assert!(second.status.success(), "{}", text(&second.stderr));
    assert_eq!(std::fs::read_to_string(dir.join("item.prisma")).unwrap(), expected);
    assert_eq!(orm(&dir, &["makemigrations", "--check"]).status.code(), Some(1));
    assert_eq!(std::fs::read(dir.join("migrations/0001_initial/snapshot.json")).unwrap(), snapshot);
    // Conversion must not absorb changes that still need a migration.
    std::fs::write(dir.join("item.prisma"), expected.replace(" label String", " note String?\n label String")).unwrap();
    assert_eq!(orm(&dir, &["makemigrations", "--check"]).status.code(), Some(1));
    assert!(orm(&dir, &["makemigrations", "unrelated"]).status.success());
    assert!(orm(&dir, &["makemigrations", "--check"]).status.success());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn fmt_native_without_history_reports_fallbacks_and_never_writes_invalid_schemas() {
    let dir = std::env::temp_dir().join(format!("orm-cli-native-no-history-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("schema.prisma");
    let old = "model Item {\n id Int @id\n qty Int @check(\"qty BETWEEN 1 AND 10\")\n @@check(raw(\"qty::int > 0\"), name: \"cast_rule\")\n}\n";
    std::fs::write(&path, old).unwrap();
    let converted = orm(&dir, &["fmt", "--native"]);
    assert!(converted.status.success(), "{}", text(&converted.stderr));
    assert!(text(&converted.stderr).contains("no migration history"));
    assert!(text(&converted.stderr).contains("schema.prisma:4:"));
    let converted = std::fs::read_to_string(&path).unwrap();
    assert_eq!(converted, old.replace("\"qty BETWEEN 1 AND 10\"", "(qty).between(1, 10)"));
    assert!(!dir.join("migrations").exists());
    let invalid = converted.replace("qty Int", "qty MissingType");
    std::fs::write(&path, &invalid).unwrap();
    assert_eq!(orm(&dir, &["fmt", "--native"]).status.code(), Some(1));
    assert_eq!(std::fs::read_to_string(path).unwrap(), invalid);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn fmt_native_rejects_a_proposed_target_that_cannot_retain_history() {
    let dir = std::env::temp_dir().join(format!("orm-cli-native-ambiguous-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("schema.prisma");
    let history = "model Item {\n id Int @id\n qty Int\n @@check(raw(\"(qty > 0)\"))\n @@check(raw(\"((qty > 0))\"))\n}\n";
    std::fs::write(&path, history).unwrap();
    assert!(orm(&dir, &["makemigrations", "initial"]).status.success());
    let source = "model Item {\n id Int @id\n qty Int\n @@check(raw(\"(((qty > 0)))\"))\n}\n";
    std::fs::write(&path, source).unwrap();
    let result = orm(&dir, &["fmt", "--native"]);
    assert!(result.status.success(), "{}", text(&result.stderr));
    assert!(text(&result.stderr).contains("cannot uniquely retain"), "{}", text(&result.stderr));
    assert_eq!(std::fs::read_to_string(path).unwrap(), source);
    std::fs::remove_dir_all(dir).unwrap();
}
