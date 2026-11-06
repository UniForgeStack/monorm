//! The standalone `monorm` binary. The commands themselves are tested through
//! `python -m monorm` (languages/python/tests/test_migrations.py) and `npx monorm` (languages/js/test/migrations.test.ts),
//! which run the same code; this checks what is specific to the binary.

use std::path::Path;
use std::process::{Command, Output};

const SCHEMA: &str = "model Book {\n  id    BigInt @id @default(autoincrement())\n  title String\n}\n";

fn monorm(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_monorm")).args(args).current_dir(dir).env_remove("MONORM_DATABASE_URL").output().unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn binary() {
    let dir = std::env::temp_dir().join(format!("monorm-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("db")).unwrap();
    std::fs::write(dir.join("db/schema.prisma"), SCHEMA).unwrap();

    let help = monorm(&dir, &["--help"]);
    assert!(help.status.success());
    assert!(text(&help.stdout).starts_with("usage: monorm "));
    assert_eq!(monorm(&dir, &[]).status.code(), Some(2));

    // no schema.prisma in the working directory
    assert_eq!(monorm(&dir, &["check"]).status.code(), Some(1));
    // configuration: pyproject.toml
    std::fs::write(dir.join("pyproject.toml"), "[tool.monorm]\nschema = \"db/schema.prisma\"\nmigrations = \"db/migrations\"\n").unwrap();
    let check = monorm(&dir, &["check"]);
    assert_eq!(text(&check.stdout), "db/schema.prisma: ok (1 models)\n");

    // the binary has no default language: it needs one, or an output file to tell
    assert_eq!(monorm(&dir, &["generate"]).status.code(), Some(2));
    assert!(monorm(&dir, &["generate", "-o", "out/models.ts"]).status.success());
    assert!(dir.join("out/models.ts").is_file());
    assert!(monorm(&dir, &["generate", "python"]).status.success());
    assert!(dir.join("db/models.py").is_file() && dir.join("db/models.pyi").is_file());

    assert!(monorm(&dir, &["makemigrations", "--name", "start"]).status.success());
    assert!(dir.join("db/migrations/0001_start/up.sql").is_file());
    let sql = monorm(&dir, &["sqlmigrate", "1"]);
    assert!(text(&sql.stdout).contains("CREATE TABLE \"book\""), "{}", text(&sql.stdout));

    let no_db = monorm(&dir, &["migrate"]);
    assert_eq!(no_db.status.code(), Some(2));
    assert!(text(&no_db.stderr).contains("MONORM_DATABASE_URL"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_warning_prints_with_its_file_line_and_column() {
    let dir = std::env::temp_dir().join(format!("monorm-cli-warning-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("schema.prisma"), "import \"library.prisma\"\n").unwrap();
    std::fs::write(dir.join("library.prisma"), "\nmodel Item {\n  id Int @id\n  qty Int @check(\"qty > 0\")\n}\n").unwrap();
    let check = monorm(&dir, &["check", "--schema", "schema.prisma"]);
    assert!(check.status.success(), "{}", text(&check.stderr));
    let stderr = text(&check.stderr);
    assert!(stderr.contains("library.prisma:4:") && stderr.contains("warning:") && stderr.contains("raw("), "{stderr}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn schema_module_output_collisions_fail_before_writing() {
    let dir = std::env::temp_dir().join(format!("monorm-cli-import-collision-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("schema.prisma"), "import \"library.prisma\"\n").unwrap();
    std::fs::write(dir.join("library.prisma"), SCHEMA).unwrap();
    let result = monorm(&dir, &["generate", "python"]);
    assert!(!result.status.success());
    assert!(text(&result.stderr).contains("each schema in its own directory"));
    assert!(!dir.join("models.py").exists());
    assert!(!dir.join("_monorm_models.py").exists());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn identity_cli_allocates_explicitly_and_compile_never_mutates() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.test-tmp").join(format!("monorm-cli-identities-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("schema.prisma");
    std::fs::write(&path, "model Post {\nid Int @id\n}\nmodel Tag {\nid Int @id\nkind ContentType\nobject_id Int\n}").unwrap();
    assert!(!monorm(&dir, &["compile"]).status.success());
    let generated = monorm(&dir, &["identities"]);
    assert!(generated.status.success(), "{}", text(&generated.stderr));
    assert!(text(&generated.stdout).contains("added Post = 1\nadded Tag = 2\n"), "{}", text(&generated.stdout));
    let before = std::fs::read(dir.join("schema.identities.json")).unwrap();
    for args in [&["compile"][..], &["check"][..], &["generate", "python"][..], &["generate", "typescript"][..]] {
        let out = monorm(&dir, args); assert!(out.status.success(), "{}", text(&out.stderr));
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
    let dir = std::env::temp_dir().join(format!("monorm-cli-pull-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let url = format!("sqlite://{}", dir.join("live.db").display());
    let source = format!("datasource db {{\n  provider = \"sqlite\"\n}}\n\n{SCHEMA}");
    std::fs::write(dir.join("made.prisma"), &source).unwrap();
    let made = ["--url", url.as_str(), "--schema", "made.prisma", "--dir", "made"];
    assert!(monorm(&dir, &[&made[..], &["makemigrations"]].concat()).status.success());
    assert!(monorm(&dir, &[&made[..], &["migrate"]].concat()).status.success());

    // pull writes the configured schema file, and refuses to overwrite one
    let pulled = monorm(&dir, &["--url", &url, "pull"]);
    assert!(pulled.status.success(), "{}", text(&pulled.stderr));
    assert!(text(&pulled.stdout).contains("The schema reproduces the database."), "{}", text(&pulled.stdout));
    assert!(std::fs::read_to_string(dir.join("schema.prisma")).unwrap().contains("model Book {"));
    assert_eq!(monorm(&dir, &["--url", &url, "pull"]).status.code(), Some(2));

    // baseline refuses a database that has applied migrations, before writing one
    let refused = monorm(&dir, &["--url", &url, "baseline"]);
    assert_eq!(refused.status.code(), Some(1));
    assert!(text(&refused.stderr).contains("already has applied migrations"), "{}", text(&refused.stderr));
    assert!(!dir.join("migrations").exists());

    let drift = monorm(&dir, &[&made[..], &["drift"]].concat());
    assert_eq!(drift.status.code(), Some(0), "{}", text(&drift.stdout));
    assert!(text(&drift.stdout).contains("No drift from 0001_initial."));
    // a newer snapshot than the database: drift exits 1 and names the change
    std::fs::write(dir.join("made.prisma"), source.replace("title String", "title String\n  pages Int @default(0)")).unwrap();
    assert!(monorm(&dir, &[&made[..], &["makemigrations"]].concat()).status.success());
    let drift = monorm(&dir, &[&made[..], &["drift"]].concat());
    assert_eq!(drift.status.code(), Some(1));
    assert!(text(&drift.stdout).contains("add column book.pages"), "{}", text(&drift.stdout));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn app_flag_gives_the_loader_the_schema_of_an_app() {
    let dir = std::env::temp_dir().join(format!("monorm-cli-app-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("vendor")).unwrap();
    std::fs::write(dir.join("vendor/auth.prisma"), "model User {\n  id Int @id\n}\n").unwrap();
    std::fs::write(dir.join("schema.prisma"), "import \"app:monorm-auth\" (prefix: \"auth_\")\nmodel Post {\n  id Int @id\n}\n").unwrap();

    let missing = monorm(&dir, &["check", "--schema", "schema.prisma"]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(text(&missing.stderr).contains("install the `monorm-auth` package (or pass --app monorm-auth=PATH)"), "{}", text(&missing.stderr));
    assert_eq!(monorm(&dir, &["check", "--app", "monorm-auth"]).status.code(), Some(2));

    let ok = monorm(&dir, &["check", "--schema", "schema.prisma", "--app", "monorm-auth=vendor/auth.prisma"]);
    assert!(ok.status.success(), "{}", text(&ok.stderr));
    assert!(text(&ok.stdout).contains("(2 models)"), "{}", text(&ok.stdout));
}

#[test]
fn generate_python_writes_nothing_into_an_app_package() {
    let dir = std::env::temp_dir().join(format!("monorm-cli-app-gen-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("site/monorm_auth")).unwrap();
    std::fs::create_dir_all(dir.join("proj")).unwrap();
    std::fs::write(dir.join("site/monorm_auth/schema.prisma"), "model User {\n  id Int @id\n}\n").unwrap();
    std::fs::write(dir.join("proj/schema.prisma"), "import \"app:monorm-auth\"\nmodel Post {\n  id Int @id\n}\n").unwrap();
    let before = std::fs::read_dir(dir.join("site/monorm_auth")).unwrap().count();

    let out = monorm(&dir.join("proj"), &["--app", "monorm-auth=../site/monorm_auth/schema.prisma", "generate", "python"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(std::fs::read_dir(dir.join("site/monorm_auth")).unwrap().count(), before, "{}", text(&out.stdout));
    let module = std::fs::read_to_string(dir.join("proj/models.py")).unwrap();
    assert!(module.contains("\"User\",") && module.contains("\"Post\","), "{module}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_app_flags_of_the_user_win_over_later_ones() {
    let dir = std::env::temp_dir().join(format!("monorm-cli-app-win-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("dev.prisma"), "model User {\n  id Int @id\n}\nmodel Dev {\n  id Int @id\n}\n").unwrap();
    std::fs::write(dir.join("installed.prisma"), "model User {\n  id Int @id\n}\n").unwrap();
    std::fs::write(dir.join("schema.prisma"), "import \"app:monorm-auth\" (prefix: \"auth_\")\n").unwrap();

    let out = monorm(&dir, &["check", "--schema", "schema.prisma", "--app", "monorm-auth=dev.prisma", "--app", "monorm-auth=installed.prisma"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(text(&out.stdout).contains("(2 models)"), "{}", text(&out.stdout));
    let _ = std::fs::remove_dir_all(&dir);
}

fn extension_fixture(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("monorm-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/outside-extension");
    let target = Path::new(env!("CARGO_TARGET_TMPDIR")).join("cli-outside");
    let build = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["build", "--offline", "--quiet", "--manifest-path"]).arg(root.join("Cargo.toml")).arg("--target-dir").arg(&target).status().unwrap();
    assert!(build.success(), "the outside extension did not build");
    let library = format!("{}outside_extension{}", std::env::consts::DLL_PREFIX, std::env::consts::DLL_SUFFIX);
    std::fs::copy(target.join("debug").join(&library), dir.join(&library)).unwrap();
    std::fs::copy(root.join("monorm-extension.toml"), dir.join("monorm-extension.toml")).unwrap();
    std::fs::write(dir.join("schema.prisma"), "model Post {\n  id Int @id\n  @@outside.tag(\"blue\")\n}\n").unwrap();

    dir
}

#[test]
fn extension_flag_gives_the_loader_an_installed_library() {
    let dir = extension_fixture("extension");

    let missing = monorm(&dir, &["check", "--schema", "schema.prisma"]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(text(&missing.stderr).contains("outside.tag"), "{}", text(&missing.stderr));
    assert_eq!(monorm(&dir, &["check", "--extension", "acme.outside"]).status.code(), Some(2));

    let ok = monorm(&dir, &["check", "--schema", "schema.prisma", "--extension", "acme.outside=monorm-extension.toml"]);
    assert!(ok.status.success(), "{}", text(&ok.stderr));
    let generated = monorm(&dir, &["generate", "--schema", "schema.prisma", "--extension", "acme.outside=monorm-extension.toml", "--extension-module", "acme.outside=acme_outside", "-o", "out/models.ts"]);
    assert!(generated.status.success(), "{}", text(&generated.stderr));
    assert!(std::fs::read_to_string(dir.join("out/models.ts")).unwrap().contains("tag"));

    let manifest = ["--extension", "acme.outside=monorm-extension.toml"];
    let made = monorm(&dir, &[&["makemigrations", "--schema", "schema.prisma"][..], &manifest].concat());
    assert!(made.status.success(), "{}", text(&made.stderr));
    let sql = monorm(&dir, &[&["sqlmigrate", "1", "--schema", "schema.prisma"][..], &manifest].concat());
    assert!(sql.status.success(), "{}", text(&sql.stderr));
    assert!(text(&sql.stdout).contains("\"tag\""), "{}", text(&sql.stdout));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn outside_adapter_generates_its_runtime_and_rejects_a_method_collision() {
    let dir = extension_fixture("adapter");
    let flags = ["--extension", "acme.outside=monorm-extension.toml", "--extension-module", "acme.outside=acme_outside"];
    std::fs::write(dir.join("schema.prisma"), "model Note {\n  id Int @id\n  note Json? @outside.wrapped(\"w:\")\n}\n").unwrap();
    for (language, output, expected) in [
        ("python", "models.py", ["from acme_outside import install", "configure_outside"]),
        ("typescript", "models.ts", ["from \"@acme/outside\"", "configureOutside"]),
    ] {
        let generated = monorm(&dir, &[&["generate", language, "--schema", "schema.prisma", "-o", output][..], &flags].concat());
        assert!(generated.status.success(), "{language}: {}", text(&generated.stderr));
        let models = std::fs::read_to_string(dir.join(output)).unwrap();
        for part in expected { assert!(models.contains(part), "{language}: {part} is missing from {models}"); }
    }
    // `note_unwrap` is the Python method and `noteUnwrap` the TypeScript one; TypeScript also camel-cases the field name.
    for (language, field, method) in [("python", "note_unwrap", "note_unwrap"), ("typescript", "noteUnwrap", "noteUnwrap"), ("typescript", "note_unwrap", "noteUnwrap")] {
        std::fs::write(dir.join("schema.prisma"), format!("model Note {{\n  id Int @id\n  note Json? @outside.wrapped(\"w:\")\n  {field} String\n}}\n")).unwrap();
        let rejected = monorm(&dir, &[&["generate", language, "--schema", "schema.prisma", "-o", "clash.out"][..], &flags].concat());
        assert_eq!(rejected.status.code(), Some(1), "{language} {field}");
        assert!(text(&rejected.stderr).contains(method), "{language} {field}: {}", text(&rejected.stderr));
        assert!(!dir.join("clash.out").exists(), "{language} {field}: a rejected generate wrote its output");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn check_skips_handwritten_defaults_and_rust_models_on_every_host() {
    let dir = std::env::temp_dir().join(format!("monorm-cli-check-defaults-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let schema = dir.join("schema.prisma");
    std::fs::write(&schema, SCHEMA).unwrap();
    for name in ["models.py", "models.ts", "models.rs"] {
        std::fs::write(dir.join(name), "handwritten models\n").unwrap();
    }
    for host in [monorm_cli::Host::Binary, monorm_cli::Host::Python, monorm_cli::Host::Node] {
        let args = vec!["check".into(), "--schema".into(), schema.display().to_string()];
        assert_eq!(monorm_cli::run_blocking(&args, host), 0);
    }
    // Rust output is outside this slice, including Rust's non-JSON Unicode escapes.
    std::fs::write(dir.join("models.rs"), "const JSON: &'static str = \"\\u{301}\";\n").unwrap();
    assert!(monorm(&dir, &["check"]).status.success());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn check_reads_generated_defaults_from_other_languages_on_every_host() {
    let dir = std::env::temp_dir().join(format!("monorm-cli-check-hosts-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let schema = dir.join("schema.prisma");
    std::fs::write(&schema, SCHEMA).unwrap();
    for (name, header) in [
        ("models.py", "# Generated from schema.prisma by `python -m monorm generate`. Do not edit.\n"),
        ("models.ts", "// Generated from schema.prisma by `monorm generate typescript`. Do not edit.\n"),
    ] {
        std::fs::write(dir.join(name), header).unwrap();
        for host in [monorm_cli::Host::Binary, monorm_cli::Host::Python, monorm_cli::Host::Node] {
            let args = vec!["check".into(), "--schema".into(), schema.display().to_string()];
            assert_eq!(monorm_cli::run_blocking(&args, host), 1, "{name}");
        }
        std::fs::remove_file(dir.join(name)).unwrap();
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn check_rejects_a_missing_explicit_output() {
    let dir = std::env::temp_dir().join(format!("monorm-cli-check-explicit-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("schema.prisma"), SCHEMA).unwrap();
    let result = monorm(&dir, &["check", "-o", "typo.py"]);
    assert_eq!(result.status.code(), Some(1));
    assert!(text(&result.stderr).contains("no generated models at typo.py; run `monorm generate`"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn check_compares_extension_locks_and_python_facades() {
    let dir = extension_fixture("check-lock");
    let generated = monorm(&dir, &["generate", "--extension", "acme.outside=monorm-extension.toml", "--extension-module", "acme.outside=acme_outside", "-o", "out/models.ts"]);
    assert!(generated.status.success(), "{}", text(&generated.stderr));
    // The lock comes from the same schema bytes consumed by define().
    let models = std::fs::read_to_string(dir.join("out/models.ts")).unwrap();
    assert!(models.contains("manifest_hash"), "{models}");
    // Independently measured with shasum -a 256 on the fixture's raw bytes.
    assert!(models.contains("c29639c3e3f2641f1d1040bb4a2dce575f912ea7e39e0368d327c74b4164f39c"));
    let original = std::fs::read_to_string(dir.join("monorm-extension.toml")).unwrap();
    for output in ["out/models.ts", "models.py"] {
        let generated = monorm(&dir, &["generate", "--extension", "acme.outside=monorm-extension.toml", "--extension-module", "acme.outside=acme_outside", "-o", output]);
        assert!(generated.status.success(), "{}", text(&generated.stderr));
        let check = || monorm(&dir, &["check", "--extension", "acme.outside=monorm-extension.toml", "--out", output]);
        assert!(check().status.success());
        for changed in [original.replace("0.1.0", "0.2.0"), format!("{original}\n# manifest changed\n")] {
            std::fs::write(dir.join("monorm-extension.toml"), &changed).unwrap();
            let stale = check();
            assert_eq!(stale.status.code(), Some(1));
            let error = text(&stale.stderr);
            for expected in ["acme.outside", "locked", "0.1.0", "installed", "hash", "run `monorm generate`"] {
                assert!(error.contains(expected), "missing {expected}: {error}");
            }
            if changed.contains("0.2.0") { assert!(error.contains("0.2.0"), "{error}"); }
        }
        std::fs::write(dir.join("monorm-extension.toml"), &original).unwrap();
        let missing = monorm(&dir, &["check", "-o", output]);
        assert_eq!(missing.status.code(), Some(1));
        let error = text(&missing.stderr);
        assert!(error.contains("acme.outside") && error.contains("not installed") && error.contains("run `monorm generate`"), "{error}");
        // Installation alone never adds a lock requirement.
        std::fs::write(dir.join("unused.toml"), "id = \"acme.unused\"\nversion = \"9.0.0\"\n").unwrap();
        let unused = monorm(&dir, &["check", "-o", output, "--extension", "acme.outside=monorm-extension.toml", "--extension", "acme.unused=unused.toml"]);
        assert!(unused.status.success(), "{}", text(&unused.stderr));
    }

    std::fs::write(dir.join("app.prisma"), "model AppItem {\n id Int @id\n}\n").unwrap();
    std::fs::write(dir.join("consumer.prisma"), "import \"app:sample\"\nmodel Consumer {\n id Int @id\n}\n").unwrap();
    let app = monorm(&dir, &["generate", "python", "--schema", "consumer.prisma", "--app", "sample=app.prisma", "-o", "app-models.py"]);
    assert!(app.status.success(), "{}", text(&app.stderr));
    let app_check = monorm(&dir, &["check", "--schema", "consumer.prisma", "--app", "sample=app.prisma", "-o", "app-models.py"]);
    assert!(app_check.status.success(), "{}", text(&app_check.stderr));

    // Default paths and the shared Python schema behind generated facades.
    std::fs::create_dir_all(dir.join("multi")).unwrap();
    std::fs::write(dir.join("multi/schema.prisma"), "import \"../schema.prisma\"\nmodel Other {\n id Int @id\n}\n").unwrap();
    let multi = monorm(&dir, &["generate", "python", "--schema", "multi/schema.prisma", "--extension", "acme.outside=monorm-extension.toml", "--extension-module", "acme.outside=acme_outside"]);
    assert!(multi.status.success(), "{}", text(&multi.stderr));
    assert!(dir.join("multi/_monorm_models.py").is_file());
    let current = monorm(&dir, &["check", "--schema", "multi/schema.prisma", "--extension", "acme.outside=monorm-extension.toml"]);
    assert!(current.status.success(), "{}", text(&current.stderr));
    std::fs::write(dir.join("monorm-extension.toml"), format!("{original}\n# changed\n")).unwrap();
    let stale = monorm(&dir, &["check", "--schema", "multi/schema.prisma", "--extension", "acme.outside=monorm-extension.toml"]);
    assert_eq!(stale.status.code(), Some(1));
    assert!(text(&stale.stderr).contains("run `monorm generate`"));
    std::fs::write(dir.join("monorm-extension.toml"), &original).unwrap();

    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn fmt_native_is_offline_idempotent_and_preserves_imports_comments_and_history() {
    let dir = std::env::temp_dir().join(format!("monorm-cli-native-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let root = "// root stays intact\nimport \"item.prisma\"\n";
    let old = "// comment é\nmodel Item {\n id Int @id\n qty Int @map(\"Qty\") @check(raw(\"(\\\"Qty\\\" > 0)\")) // field comment\n label String\n @@check(raw(\"mystery(label) IS NOT NULL\"), name: \"unsupported\")\n @@index([sql(\"abs( \\\"Qty\\\" )\")], where: raw(\"\\\"Qty\\\" > 0\"))\n}\n";
    std::fs::write(dir.join("schema.prisma"), root).unwrap();
    std::fs::write(dir.join("item.prisma"), old).unwrap();
    let initial = monorm(&dir, &["makemigrations", "initial"]);
    assert!(initial.status.success(), "{}", text(&initial.stderr));
    let snapshot = std::fs::read(dir.join("migrations/0001_initial/snapshot.json")).unwrap();
    let old = old.replace(" label String", " pending String?\n label String");
    std::fs::write(dir.join("item.prisma"), &old).unwrap();
    let converted = monorm(&dir, &["fmt", "--native"]);
    assert!(converted.status.success(), "{}", text(&converted.stderr));
    assert!(text(&converted.stderr).contains("item.prisma:7:"), "{}", text(&converted.stderr));
    let expected = old.replace("raw(\"(\\\"Qty\\\" > 0)\")", "((qty > 0))").replace("sql(\"abs( \\\"Qty\\\" )\")", "abs(qty)").replace("raw(\"\\\"Qty\\\" > 0\")", "(qty > 0)");
    assert_eq!(std::fs::read_to_string(dir.join("item.prisma")).unwrap(), expected);
    assert_eq!(std::fs::read_to_string(dir.join("schema.prisma")).unwrap(), root);
    let second = monorm(&dir, &["fmt", "--native"]);
    assert!(second.status.success(), "{}", text(&second.stderr));
    assert_eq!(std::fs::read_to_string(dir.join("item.prisma")).unwrap(), expected);
    assert_eq!(monorm(&dir, &["makemigrations", "--check"]).status.code(), Some(1));
    assert_eq!(std::fs::read(dir.join("migrations/0001_initial/snapshot.json")).unwrap(), snapshot);
    // Conversion must not absorb changes that still need a migration.
    std::fs::write(dir.join("item.prisma"), expected.replace(" label String", " note String?\n label String")).unwrap();
    assert_eq!(monorm(&dir, &["makemigrations", "--check"]).status.code(), Some(1));
    assert!(monorm(&dir, &["makemigrations", "unrelated"]).status.success());
    assert!(monorm(&dir, &["makemigrations", "--check"]).status.success());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn fmt_native_without_history_reports_fallbacks_and_never_writes_invalid_schemas() {
    let dir = std::env::temp_dir().join(format!("monorm-cli-native-no-history-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("schema.prisma");
    let old = "model Item {\n id Int @id\n qty Int @check(\"qty BETWEEN 1 AND 10\")\n @@check(raw(\"qty::int > 0\"), name: \"cast_rule\")\n}\n";
    std::fs::write(&path, old).unwrap();
    let converted = monorm(&dir, &["fmt", "--native"]);
    assert!(converted.status.success(), "{}", text(&converted.stderr));
    assert!(text(&converted.stderr).contains("no migration history"));
    assert!(text(&converted.stderr).contains("schema.prisma:4:"));
    let converted = std::fs::read_to_string(&path).unwrap();
    assert_eq!(converted, old.replace("\"qty BETWEEN 1 AND 10\"", "(qty).between(1, 10)"));
    assert!(!dir.join("migrations").exists());
    let invalid = converted.replace("qty Int", "qty MissingType");
    std::fs::write(&path, &invalid).unwrap();
    assert_eq!(monorm(&dir, &["fmt", "--native"]).status.code(), Some(1));
    assert_eq!(std::fs::read_to_string(path).unwrap(), invalid);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn fmt_native_rejects_a_proposed_target_that_cannot_retain_history() {
    let dir = std::env::temp_dir().join(format!("monorm-cli-native-ambiguous-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("schema.prisma");
    let history = "model Item {\n id Int @id\n qty Int\n @@check(raw(\"(qty > 0)\"))\n @@check(raw(\"((qty > 0))\"))\n}\n";
    std::fs::write(&path, history).unwrap();
    assert!(monorm(&dir, &["makemigrations", "initial"]).status.success());
    let source = "model Item {\n id Int @id\n qty Int\n @@check(raw(\"(((qty > 0)))\"))\n}\n";
    std::fs::write(&path, source).unwrap();
    let result = monorm(&dir, &["fmt", "--native"]);
    assert!(result.status.success(), "{}", text(&result.stderr));
    assert!(text(&result.stderr).contains("cannot uniquely retain"), "{}", text(&result.stderr));
    assert_eq!(std::fs::read_to_string(path).unwrap(), source);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn monorm_toml_is_the_first_settings_file_and_the_others_stay_a_fallback() {
    let dir = std::env::temp_dir().join(format!("monorm-cli-settings-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("from-toml")).unwrap();
    std::fs::create_dir_all(dir.join("from-pyproject")).unwrap();
    std::fs::create_dir_all(dir.join("from-json")).unwrap();
    for sub in ["from-toml", "from-pyproject", "from-json"] {
        std::fs::write(dir.join(sub).join("schema.prisma"), SCHEMA).unwrap();
    }
    let ok = |args: &[&str]| {
        let out = monorm(&dir, args);
        assert!(out.status.success(), "{}", text(&out.stderr));
        text(&out.stdout)
    };

    // package.json alone, then pyproject.toml over it, then monorm.toml over both: the files do not merge
    std::fs::write(dir.join("package.json"), "{\"monorm\": {\"schema\": \"from-json/schema.prisma\"}}").unwrap();
    assert_eq!(ok(&["check"]), "from-json/schema.prisma: ok (1 models)\n");
    std::fs::write(dir.join("pyproject.toml"), "[tool.monorm]\nschema = \"from-pyproject/schema.prisma\"\n").unwrap();
    assert_eq!(ok(&["check"]), "from-pyproject/schema.prisma: ok (1 models)\n");
    std::fs::write(dir.join("monorm.toml"), "migrations = \"db/migrations\"\n").unwrap();
    let no_schema = monorm(&dir, &["check"]);
    assert_eq!(no_schema.status.code(), Some(1), "monorm.toml has no schema key, so the default schema.prisma is missing");
    std::fs::write(dir.join("monorm.toml"), "schema = \"from-toml/schema.prisma\"\n[admin]\napp = \"shop.admin:admin\"\n").unwrap();
    assert_eq!(ok(&["check"]), "from-toml/schema.prisma: ok (1 models)\n");
    assert_eq!(ok(&["--schema", "from-json/schema.prisma", "check"]), "from-json/schema.prisma: ok (1 models)\n");

    // a wrong type is a usage error that names the file and the key
    std::fs::write(dir.join("monorm.toml"), "schema = 3\n").unwrap();
    let bad = monorm(&dir, &["check"]);
    assert_eq!(bad.status.code(), Some(2));
    assert!(text(&bad.stderr).contains("monorm.toml: `schema` must be a string"), "{}", text(&bad.stderr));
    std::fs::write(dir.join("monorm.toml"), "schema = \n").unwrap();
    let broken = monorm(&dir, &["check"]);
    assert_eq!(broken.status.code(), Some(2));
    assert!(text(&broken.stderr).contains("monorm.toml: "), "{}", text(&broken.stderr));

    // generate reads query_sets from the winning file
    std::fs::write(dir.join("monorm.toml"), "schema = \"from-toml/schema.prisma\"\n[query_sets]\nBook = \"app.queries:BookQueries\"\n").unwrap();
    assert!(monorm(&dir, &["generate", "python"]).status.success());
    let models = std::fs::read_to_string(dir.join("from-toml/models.py")).unwrap();
    assert!(
        models.contains("use_query_set(Book, \"app.queries:BookQueries\")"),
        "{models}"
    );
    std::fs::write(dir.join("monorm.toml"), "schema = \"from-toml/schema.prisma\"\n[query_sets]\nBook = 1\n").unwrap();
    let bad_targets = monorm(&dir, &["generate", "python"]);
    assert_eq!(bad_targets.status.code(), Some(2));
    assert!(text(&bad_targets.stderr).contains("monorm.toml: `query_sets.Book` must be a string"), "{}", text(&bad_targets.stderr));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_flag_overrides_a_wrong_setting_and_a_command_reads_only_the_keys_it_uses() {
    let dir = std::env::temp_dir().join(format!("monorm-cli-flag-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("schema.prisma"), SCHEMA).unwrap();

    // the flag is not read against the key, and `check` does not read `migrations`
    std::fs::write(dir.join("monorm.toml"), "schema = 3\nmigrations = 3\n").unwrap();
    let flagged = monorm(&dir, &["--schema", "schema.prisma", "check"]);
    assert!(flagged.status.success(), "{}", text(&flagged.stderr));
    assert_eq!(text(&flagged.stdout), "schema.prisma: ok (1 models)\n");
    std::fs::write(dir.join("monorm.toml"), "migrations = 3\n").unwrap();
    let check = monorm(&dir, &["check"]);
    assert!(check.status.success(), "{}", text(&check.stderr));

    // a command that uses the migrations directory names the key, and `--dir` answers it
    let make = monorm(&dir, &["makemigrations", "initial"]);
    assert_eq!(make.status.code(), Some(2));
    assert!(
        text(&make.stderr).contains("monorm.toml: `migrations` must be a string"),
        "{}",
        text(&make.stderr)
    );
    let make = monorm(&dir, &["--dir", "db", "makemigrations", "initial"]);
    assert!(make.status.success(), "{}", text(&make.stderr));
    assert!(dir.join("db/0001_initial").is_dir());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_spelling_of_another_file_format_is_an_error_that_names_the_file_and_the_key() {
    let dir = std::env::temp_dir().join(format!("monorm-cli-spelling-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("schema.prisma"), SCHEMA).unwrap();
    let rejects = |file: &str, content: &str, args: &[&str], message: &str| {
        for name in ["monorm.toml", "pyproject.toml", "package.json"] {
            let _ = std::fs::remove_file(dir.join(name));
        }
        std::fs::write(dir.join(file), content).unwrap();
        let out = monorm(&dir, args);
        assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
        assert!(text(&out.stderr).contains(message), "{}", text(&out.stderr));
    };

    // the TOML files spell `query_sets` and `model_methods`, `package.json` spells `querySets` and `modelMethods`
    let python = ["generate", "python"];
    let typescript = ["generate", "typescript", "-o", "models.ts"];
    rejects(
        "monorm.toml",
        "[querySets]\nBook = \"app.queries:BookQueries\"\n",
        &python,
        "query set: monorm.toml: `querySets` is not a setting; use `query_sets`",
    );
    rejects(
        "monorm.toml",
        "[modelMethods]\nBook = \"app.methods:BookMethods\"\n",
        &python,
        "model methods: monorm.toml: `modelMethods` is not a setting; use `model_methods`",
    );
    rejects(
        "pyproject.toml",
        "[tool.monorm.querySets]\nBook = \"app.queries:BookQueries\"\n",
        &python,
        "query set: pyproject.toml: `tool.monorm.querySets` is not a setting; use `query_sets`",
    );
    rejects(
        "package.json",
        "{\"monorm\": {\"query_sets\": {\"Book\": \"./q.js#Book\"}}}",
        &typescript,
        "query set: package.json: `monorm.query_sets` is not a setting; use `querySets`",
    );
    rejects(
        "package.json",
        "{\"monorm\": {\"model_methods\": {\"Book\": \"./m.js#Book\"}}}",
        &typescript,
        "model methods: package.json: `monorm.model_methods` is not a setting; use `modelMethods`",
    );
    assert!(
        !dir.join("models.py").exists() && !dir.join("models.ts").exists(),
        "a rejected setting writes nothing"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn destructive_gate_flag_on_migrate_rollback_and_makemigrations() {
    let dir = std::env::temp_dir().join(format!("monorm-cli-gate-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let url = format!("sqlite://{}", dir.join("live.db").display());
    let source = format!("datasource db {{\n  provider = \"sqlite\"\n}}\n\n{SCHEMA}\nmodel Note {{\n  id   BigInt @id @default(autoincrement())\n  body String\n}}\n");
    std::fs::write(dir.join("schema.prisma"), &source).unwrap();
    let run = |args: &[&str]| monorm(&dir, &[&["--url", url.as_str()][..], args].concat());
    assert!(run(&["makemigrations", "--name", "initial"]).status.success());
    assert!(run(&["migrate"]).status.success());
    // a hand-written migration puts a row in each table
    assert!(run(&["makemigrations", "--empty", "--name", "seed"]).status.success());
    let seed = std::fs::read_dir(dir.join("migrations")).unwrap().map(|e| e.unwrap().path()).find(|p| p.ends_with("0002_seed")).unwrap();
    std::fs::write(seed.join("up.sql"), "INSERT INTO book (title) VALUES ('a');\nINSERT INTO note (body) VALUES ('a');\n").unwrap();
    assert!(run(&["migrate"]).status.success());

    // makemigrations prints the flag next to the destructive step
    std::fs::write(dir.join("schema.prisma"), source.split("\nmodel Note").next().unwrap()).unwrap();
    let made = run(&["makemigrations", "--name", "drop_note"]);
    assert!(made.status.success(), "{}", text(&made.stderr));
    assert!(text(&made.stdout).contains("needs --allow-destructive"), "{}", text(&made.stdout));

    // migrate refuses without the flag, and applies with it
    let refused = run(&["migrate"]);
    assert_eq!(refused.status.code(), Some(1), "{}", text(&refused.stderr));
    let message = text(&refused.stderr);
    assert!(message.contains("0003_drop_note") && message.contains("drops table note") && message.contains("--allow-destructive"), "{message}");
    assert!(run(&["migrate", "--allow-destructive"]).status.success());

    // rollback: the down of the first migration drops `book`, which has a row
    let refused = run(&["rollback", "--to", "zero"]);
    assert_eq!(refused.status.code(), Some(1), "{}", text(&refused.stderr));
    let message = text(&refused.stderr);
    assert!(message.contains("0001_initial") && message.contains("drops table book") && message.contains("--allow-destructive"), "{message}");
    let done = run(&["rollback", "--to", "zero", "--allow-destructive"]);
    assert!(done.status.success(), "{}", text(&done.stderr));
    let _ = std::fs::remove_dir_all(&dir);
}
