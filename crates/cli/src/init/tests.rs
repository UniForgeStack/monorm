use std::path::{Path, PathBuf};

use monorm_core::dsl;

use super::*;

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("monorm-init-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn request<'a>(lang: &'a str, db: &'a str, ext: &'a str) -> Request<'a> {
    Request { lang: Some(lang), db: Some(db), ext: Some(ext), ..Request::default() }
}

fn plan(lang: &str, db: &str, ext: &str) -> Plan {
    resolve(&request(lang, db, ext), Host::Binary, &Env::default(), "my-app".into()).unwrap()
}

/// The built-in extensions as the staged Python packages ship them (`languages/python/packaging/stage.py`).
fn installed() -> dsl::Installed {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../languages/python");
    let mut installed = dsl::Installed::new();
    for ext in &EXTENSIONS {
        let id = format!("monorm.{}", ext.name);
        let manifest = root.join(id.replace(['.', '-'], "_")).join("monorm-extension.toml");
        assert!(manifest.exists(), "{} is missing: run `python languages/python/packaging/stage.py`", manifest.display());
        installed.extensions.push((id, manifest));
    }
    installed
}

/// The command line exactly as given.
fn plain(args: &[&str]) -> Result<i32> {
    let argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(crate::command(&argv, Host::Binary))
}

/// The command line that the host builds: the installed built-in extensions as `--extension` options.
fn command(args: &[&str]) -> Result<i32> {
    let extensions: Vec<String> = installed().extensions.iter().map(|(id, manifest)| format!("{id}={}", manifest.display())).collect();
    let mut all = args.to_vec();
    for extension in &extensions {
        all.extend(["--extension", extension]);
    }
    plain(&all)
}

const COMBINATIONS: [(&str, &str, &str); 8] = [
    ("python", "sqlite", "none"),
    ("python", "sqlite", "all"),
    ("python", "postgresql", "none"),
    ("python", "postgresql", "all"),
    ("typescript", "sqlite", "none"),
    ("typescript", "sqlite", "all"),
    ("typescript", "postgresql", "none"),
    ("typescript", "postgresql", "all"),
];

#[test]
fn every_combination_writes_its_files_and_passes_check() {
    let installed = installed();
    for (lang, db, ext) in COMBINATIONS {
        let dir = temp(&format!("{lang}-{db}-{ext}"));
        let request = Request { dir: Some(dir.to_str().unwrap()), ..request(lang, db, ext) };
        assert_eq!(run(&request, Host::Binary, &Env::default()).unwrap(), 0);

        let mut expected = vec![".gitignore", "README.md", "schema.prisma"];
        expected.push(if lang == "python" { "main.py" } else { "index.ts" });
        expected.push(if lang == "python" { "pyproject.toml" } else { "package.json" });
        if ext == "all" { expected.push("schema.identities.json"); }
        let mut found: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        found.sort();
        expected.sort();
        assert_eq!(found, expected, "{lang} {db} {ext}");

        let schema = dir.join("schema.prisma");
        let project = dsl::compile_project_file_with(&schema, &installed).unwrap_or_else(|e| panic!("{lang} {db} {ext}: {e}"));
        assert!(project.warnings.is_empty(), "{lang} {db} {ext}: {:?}", project.warnings);
        assert_eq!(command(&["check", "--schema", schema.to_str().unwrap()]).unwrap(), 0, "{lang} {db} {ext}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[test]
fn the_schema_holds_one_use_of_each_picked_extension() {
    let schema = plan("python", "sqlite", "all").schema();
    for attribute in [
        "@@timestamps.updated_at", "@@soft_delete.deleted_at", "@@locking.version", "@@inherit.extends", "@@query.filter",
        "@generic.relation", "@@proxy.of", "@@composition.model", "@storage.file",
    ] {
        assert_eq!(schema.matches(attribute).count(), 1, "{attribute}");
    }
    assert!(!plan("python", "sqlite", "none").schema().contains("@@"));
    let one = plan("python", "sqlite", "locking,timestamps").schema();
    assert!(one.contains("@@locking.version") && one.contains("@@timestamps.updated_at") && !one.contains("@@soft_delete"));
}

#[test]
fn the_database_sets_the_provider_and_the_script_url() {
    let sqlite = plan("typescript", "sqlite", "none");
    assert!(sqlite.schema().contains("provider = \"sqlite\""));
    assert!(sqlite.script().contains("\"sqlite://dev.db\""));
    let postgres = plan("python", "postgresql", "none");
    assert!(postgres.schema().contains("provider = \"postgresql\""));
    assert!(postgres.script().contains("postgresql://postgres:postgres@localhost:5432/my_app"));
    let request = Request { url: Some("postgres://u:p@db/x"), ..request("python", "postgresql", "none") };
    assert!(resolve(&request, Host::Binary, &Env::default(), "my-app".into()).unwrap().script().contains("postgres://u:p@db/x"));
}

#[test]
fn the_package_file_names_the_version_of_the_packages() {
    let python = plan("python", "sqlite", "none").package_file().1;
    assert!(python.contains(&format!("\"monorm>={VERSION}\"")), "{python}");
    let typescript = plan("typescript", "sqlite", "none").package_file().1;
    assert!(typescript.contains(&format!("\"monorm\": \"npm:@ufstack/monorm@^{VERSION}\"")), "{typescript}");
    assert!(typescript.contains("\"start\": \"node index.ts\""));
}

#[test]
fn the_defaults_come_from_the_caller() {
    let none = Request::default();
    let make = |host, env: Env| resolve(&none, host, &env, "app".into()).unwrap();
    let python = make(Host::Python, Env::default());
    assert_eq!((python.lang, python.tool), (Lang::Python, Tool::Pip));
    assert_eq!(make(Host::Python, Env { uv: true, ..Env::default() }).tool, Tool::Uv);
    let node = make(Host::Node, Env { user_agent: Some("bun/1.2.0 npm/? node/v24.0.0 darwin arm64".into()), ..Env::default() });
    assert_eq!((node.lang, node.tool), (Lang::TypeScript, Tool::Bun));
    assert!(node.package_file().1.contains("\"start\": \"bun index.ts\""));
    assert_eq!(node.next_commands(), ["bun install", "bun start"]);
    assert_eq!(make(Host::Node, Env { user_agent: Some("pnpm/9.0.0 npm/? node/v22.0.0".into()), ..Env::default() }).tool, Tool::Pnpm);
    assert_eq!(make(Host::Node, Env::default()).tool, Tool::Npm);
    assert_eq!(make(Host::Binary, Env::default()).lang, Lang::Python);
}

#[test]
fn a_tool_flag_picks_the_language() {
    let request = Request { tool: Some("yarn"), ..Request::default() };
    let plan = resolve(&request, Host::Python, &Env::default(), "app".into()).unwrap();
    assert_eq!((plan.lang, plan.tool), (Lang::TypeScript, Tool::Yarn));
    let request = Request { lang: Some("python"), tool: Some("bun"), ..Request::default() };
    assert!(matches!(resolve(&request, Host::Python, &Env::default(), "app".into()), Err(Failure::Usage(_))));
}

#[test]
fn a_bad_flag_value_is_a_usage_error() {
    for (request, message) in [
        (Request { lang: Some("rust"), ..Request::default() }, "--lang is python or typescript, not rust"),
        (Request { tool: Some("cargo"), ..Request::default() }, "--tool is uv, pip, npm, pnpm, yarn or bun, not cargo"),
        (Request { db: Some("mysql"), ..Request::default() }, "--db is sqlite or postgresql, not mysql"),
        (Request { ext: Some("timestamps,nothing"), ..Request::default() }, "unknown extension nothing; --ext takes none, all or a list of: timestamps"),
        (Request { url: Some("postgres://x/y"), db: Some("sqlite"), ..Request::default() }, "--url needs --db postgresql"),
    ] {
        match resolve(&request, Host::Binary, &Env::default(), "app".into()) {
            Err(Failure::Usage(m)) => assert!(m.starts_with(message), "{m}"),
            other => panic!("expected a usage error, got {other:?}"),
        }
    }
}

#[test]
fn a_postgresql_url_sets_the_database_before_any_question() {
    // `interactive` would ask for the database; only the URL can answer it here, so a question would fail without a terminal.
    let request = Request { lang: Some("python"), tool: Some("uv"), url: Some("postgresql://u:p@db/x"), ext: Some("none"), ..Request::default() };
    let plan = resolve(&request, Host::Binary, &Env { interactive: true, ..Env::default() }, "app".into()).unwrap();
    assert_eq!((plan.db, plan.url.as_str()), (Db::Postgres, "postgresql://u:p@db/x"));
}

#[test]
fn the_init_command_refuses_those_options_and_takes_the_others() {
    let dir = temp("command");
    let target = dir.to_str().unwrap();
    for option in ["--dir", "--schema", "--app", "--extension", "--extension-module"] {
        match plain(&["init", target, option, "a=b"]) {
            Err(Failure::Usage(m)) => assert_eq!(m, format!("init doesn't take {option}")),
            other => panic!("{option}: expected a usage error, got {other:?}"),
        }
    }
    assert!(!dir.exists(), "a refused command writes nothing");
    let ok = plain(&["init", target, "--lang", "python", "--db", "postgresql", "--url", "postgresql://u:p@db/x", "--ext", "none", "--yes"]);
    assert_eq!(ok.unwrap(), 0);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn monorm_identities_keeps_the_project_valid_and_the_manifest_the_same() {
    let dir = temp("identities");
    let request = Request { dir: Some(dir.to_str().unwrap()), ..request("python", "sqlite", "all") };
    assert_eq!(run(&request, Host::Binary, &Env::default()).unwrap(), 0);
    let schema = dir.join("schema.prisma");
    let manifest = dir.join("schema.identities.json");
    let written = std::fs::read_to_string(&manifest).unwrap();
    assert!(std::fs::read_to_string(&schema).unwrap().contains("@@inherit.abstract"), "the abstract model stays in the schema");
    assert_eq!(command(&["identities", "--schema", schema.to_str().unwrap()]).unwrap(), 0);
    let parsed = |text: &str| serde_json::from_str::<serde_json::Value>(text).unwrap();
    assert_eq!(parsed(&std::fs::read_to_string(&manifest).unwrap()), parsed(&written), "the command lists the same models as init");
    assert_eq!(command(&["check", "--schema", schema.to_str().unwrap()]).unwrap(), 0);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_extension_list_takes_all_none_names_and_the_old_composition_name() {
    assert_eq!(parse_ext("all").unwrap().len(), 9);
    assert!(parse_ext("none").unwrap().is_empty());
    let names: Vec<_> = parse_ext("proxy, model-composition,locking,locking").unwrap().iter().map(|e| e.name).collect();
    assert_eq!(names, ["locking", "proxy", "composition"]);
}

#[test]
fn a_directory_with_files_is_refused_and_a_missing_one_is_made() {
    let dir = temp("refuse");
    let request = Request { dir: Some(dir.to_str().unwrap()), ..Request::default() };
    assert_eq!(run(&request, Host::Binary, &Env::default()).unwrap(), 0);
    match run(&request, Host::Binary, &Env::default()) {
        Err(Failure::Failed(m)) => assert!(m.ends_with("is not empty."), "{m}"),
        other => panic!("expected a refusal, got {:?}", other.is_ok()),
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_project_name_comes_from_the_directory() {
    assert_eq!(project_name(Path::new("My App!")), "my-app");
    assert_eq!(project_name(Path::new("/tmp/-")), "monorm-app");
}
