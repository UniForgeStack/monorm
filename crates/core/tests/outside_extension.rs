//! An extension built in its own workspace (`tests/outside-extension`) runs through the loader.
//! Each ABI feature is one build; each installed package is a copy of a build with its own manifest.
use orm_contracts::abi::{Arg, Context, Value};
use orm_contracts::extension::{Declaration, HookBinding, Manifest, SourceLocation, EXPRESSION_RECORDS_MINOR, FIELD_ADAPTER_OPTIONS_MINOR, HOST_CONTRACT_VERSION, SCHEMA_CONTRACT_MINOR, SCHEMA_CONTRACT_VERSION};
use orm_core::dsl::{compile_file_with, Installed};
use orm_core::loader::{Library, Plan};
use orm_core::ir::{ClientDefaultIr, ColType, FieldIr, SchemaIr};
use std::{cell::RefCell, collections::BTreeMap, fs, path::{Path, PathBuf}, process::Command, sync::Mutex};

const ID: &str = "acme.outside";
/// The passes and attributes that the manifest of `tests/outside-extension` stamps on a lowered schema.
/// The passes in the order the stamp lists them; `PASSES` is the order they run in.
const DECLARED_PASSES: [&str; 5] = ["acme.outside.fields", "acme.outside.tag", "acme.outside.guard", "acme.outside.wrapped", "acme.outside.view"];
const PASSES: [&str; 5] = ["acme.outside.fields", "acme.outside.wrapped", "acme.outside.tag", "acme.outside.guard", "acme.outside.view"];
const ATTRIBUTES: [&str; 6] = ["outside.tag", "outside.money", "outside.note", "outside.view", "outside.guard", "outside.wrapped"];
const SCHEMA: &str = "model Post {\n id Int @id\n @@outside.tag(\"blue\")\n}\n";

struct Builds {
    /// The library file that each feature built.
    libraries: BTreeMap<String, PathBuf>,
    /// The manifest of each installed package, by feature and by the `abi` that the manifest says.
    packages: BTreeMap<(String, String), PathBuf>,
}

static BUILDS: Mutex<Builds> = Mutex::new(Builds { libraries: BTreeMap::new(), packages: BTreeMap::new() });

fn root() -> PathBuf { Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/outside-extension") }

fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("outside-{name}"));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn variant(feature: &str) -> &str { if feature.is_empty() { "default" } else { feature } }

/// The manifest of the package, with the `abi` that it says.
fn manifest(abi: &str) -> String {
    fs::read_to_string(root().join("orm-extension.toml")).unwrap().replace("abi = \"0.2\"", &format!("abi = \"{abi}\""))
}

/// The file name of the library that every build writes.
fn library_file() -> String { format!("{}outside_extension{}", std::env::consts::DLL_PREFIX, std::env::consts::DLL_SUFFIX) }

/// Builds the library with `feature` and keeps a copy: every build writes the same file.
fn build(feature: &str) -> PathBuf {
    let target = scratch("target");
    let mut build = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    build.args(["build", "--offline", "--quiet", "--manifest-path"]).arg(root().join("Cargo.toml")).arg("--target-dir").arg(&target);
    if !feature.is_empty() { build.args(["--features", feature]); }
    assert!(build.status().unwrap().success(), "the outside extension did not build");
    let name = library_file();
    let kept = scratch(&format!("library-{}", variant(feature))).join(&name);
    fs::copy(target.join("debug").join(&name), &kept).unwrap();
    kept
}

/// The kept library of `feature`; the first call builds it.
fn built(builds: &mut Builds, feature: &str) -> PathBuf {
    builds.libraries.entry(feature.to_owned()).or_insert_with(|| build(feature)).clone()
}

/// A package in the directory `name`: a copy of `library` and the manifest `text`.
/// The loader caches a library by its path, so each directory has its own copy of the library and of its state.
fn install(name: &str, library: &Path, text: &str) -> PathBuf {
    let dir = scratch(name);
    fs::copy(library, dir.join(library.file_name().unwrap())).unwrap();
    let path = dir.join("orm-extension.toml");
    fs::write(&path, text).unwrap();
    path
}

/// A package of the library that `feature` builds, in the directory `name`.
fn package(name: &str, feature: &str, text: &str) -> PathBuf {
    let library = built(&mut BUILDS.lock().unwrap_or_else(|e| e.into_inner()), feature);
    install(name, &library, text)
}

/// The installed package of a variant: a copy of the library that `feature` builds, and a manifest that says `abi`.
fn installed(feature: &str, abi: &str) -> PathBuf {
    let mut builds = BUILDS.lock().unwrap_or_else(|e| e.into_inner());
    let key = (feature.to_owned(), abi.to_owned());
    if let Some(path) = builds.packages.get(&key) { return path.clone(); }
    let library = built(&mut builds, feature);
    let path = install(&format!("{}-{abi}", variant(feature)), &library, &manifest(abi));
    builds.packages.insert(key, path.clone());
    path
}

/// The manifest `text` without its `[[exports]]`: the text between "# Hooks of the library" and `[python]` goes.
fn without_exports(text: &str) -> String {
    format!("{}{}", text.split("# Hooks of the library").next().unwrap(), &text[text.find("[python]").unwrap()..])
}

fn compile(name: &str, schema: &str, extensions: &[(&str, &Path)]) -> Result<SchemaIr, String> {
    let path = scratch(&format!("schema-{name}")).join("schema.prisma");
    fs::write(&path, schema).unwrap();
    let installed = Installed { extensions: extensions.iter().map(|(id, path)| ((*id).to_owned(), path.to_path_buf())).collect(), ..Installed::new() };
    compile_file_with(&path, &installed)
}

/// The plan of the package at `manifest` with the passes of the artifact.
fn plan(manifest: &Path) -> Plan {
    let artifact = orm_core::behavior::artifact();
    orm_core::loader::plan(&[(ID.to_owned(), manifest.to_path_buf())], &["outside".to_owned()].into(), &[], orm_core::behavior::manifests(), &artifact.passes).unwrap().unwrap()
}

#[test]
fn an_outside_library_lowers_a_schema_that_uses_its_attribute() {
    let manifest = installed("", "0.2");
    let ir = compile("lowers", SCHEMA, &[(ID, &manifest)]).unwrap();
    let tag = ir.models[0].fields.iter().find(|f| f.name == "tag").expect("the pass adds the field");
    assert_eq!(tag.default, Some("blue".into()));
    let stamp = &ir.behavior.packages[ID];
    assert_eq!((stamp.version.as_str(), stamp.passes.as_slice(), stamp.attributes.as_slice()), ("0.1.0", DECLARED_PASSES.map(String::from).as_slice(), ATTRIBUTES.map(String::from).as_slice()));
}

const VIEW_SCHEMA: &str = "model Ticket {\n id Int @id\n status String\n note String?\n}\n\
    model OpenTicket {\n id Int @id\n status String\n note String\n @@outside.view(Ticket, \"open\")\n}\n";

#[test]
fn an_outside_view_writes_the_general_records_that_the_core_reads() {
    let manifest = installed("", "0.2");
    let ir = compile("view", VIEW_SCHEMA, &[(ID, &manifest)]).unwrap();
    let view = &ir.behavior.model_views[0];
    assert_eq!((view.model.as_str(), view.source.as_str(), view.root.as_str()), ("OpenTicket", "Ticket", "Ticket"));
    assert_eq!(view.row_filter.as_ref().map(|f| (f.field.as_str(), f.values.as_slice())), Some(("status", ["open".to_owned()].as_slice())));
    let check = &ir.behavior.result_checks[0];
    assert_eq!((check.model.as_str(), check.field.as_str(), check.non_null, check.code.as_str()), ("OpenTicket", "note", true, "acme.outside.shape"));
    assert!(ir.behavior.proxy_models.is_empty());
    assert_eq!((ir.behavior.schema_contract_minor, ir.models.iter().find(|m| m.name == "OpenTicket").unwrap().table.as_str()), (orm_contracts::extension::SCHEMA_CONTRACT_VIEWS_MINOR, "ticket"));
    let schema = orm_core::schema::Schema::from_ir(ir).unwrap();
    let at = schema.model_idx("OpenTicket").unwrap();
    let filter = schema.model(at).view.as_ref().and_then(|v| v.filter.as_ref()).expect("the filter is prepared at define()");
    assert_eq!((filter.column.as_str(), filter.values.as_str()), ("status", "'open'"));
    assert_eq!(schema.result_checks[at].as_ref().map(|c| c.model.as_str()), Some("OpenTicket"));
}

#[test]
fn an_outside_view_of_an_unknown_model_names_the_attribute() {
    let manifest = installed("", "0.2");
    let error = compile("view-unknown", &VIEW_SCHEMA.replace("(Ticket,", "(Missing,"), &[(ID, &manifest)]).unwrap_err();
    assert!(error.contains("outside.view") && error.contains("Missing"), "{error}");
}

#[test]
fn the_generator_key_loads_a_library_that_no_attribute_selects() {
    let manifest = installed("", "0.2");
    let schema = "generator client {\n provider = \"orm\"\n extensions = [\"acme.outside\"]\n}\nmodel Post {\n id Int @id\n}\n";
    assert!(compile("named", schema, &[(ID, &manifest)]).unwrap().behavior.packages.contains_key(ID));
    assert!(compile("unused", "model Post {\n id Int @id\n}\n", &[(ID, &manifest)]).unwrap().behavior.packages.is_empty());
}

#[test]
fn a_pass_error_names_the_extension_and_the_pass() {
    let manifest = installed("", "0.2");
    let error = compile("argument", "model Post {\n id Int @id\n @@outside.tag(\"\")\n}\n", &[(ID, &manifest)]).unwrap_err();
    assert!(error.contains(ID) && error.contains("acme.outside.tag") && error.contains("non-empty"), "{error}");
}

#[test]
fn a_missing_extension_says_how_to_install_it() {
    let schema = "generator client {\n extensions = [\"acme.gone\"]\n}\nmodel Post {\n id Int @id\n}\n";
    let error = compile("missing", schema, &[]).unwrap_err();
    assert!(error.contains("acme.gone") && error.contains("not installed") && error.contains("--extension"), "{error}");
    let error = compile("unknown", SCHEMA, &[]).unwrap_err();
    assert!(error.contains("outside.tag"), "{error}");
}

#[test]
fn two_extensions_that_own_one_effect_conflict() {
    let manifest = installed("", "0.2");
    let rival = scratch("rival").join("orm-extension.toml");
    fs::write(&rival, "id = \"acme.rival\"\nversion = \"1.0.0\"\nschema_contract = \"3.0\"\nabi = \"0.2\"\nlibrary = \"none\"\nlanguages = [\"python\"]\ndatabases = [\"sqlite\"]\n[[passes]]\nid = \"acme.rival.tag\"\nphase = \"declaration\"\neffects = [\"models.outside\"]\n").unwrap();
    let schema = "generator client {\n extensions = [\"acme.outside\", \"acme.rival\"]\n}\nmodel Post {\n id Int @id\n}\n";
    let error = compile("conflict", schema, &[(ID, &manifest), ("acme.rival", &rival)]).unwrap_err();
    assert!(error.contains("models.outside") && error.contains(ID) && error.contains("acme.rival"), "{error}");
}

#[test]
fn a_broken_manifest_of_another_package_fails_only_a_compile_that_it_can_own() {
    let manifest = installed("", "0.2");
    let broken = scratch("broken").join("orm-extension.toml");
    fs::write(&broken, "id = \"acme.broken\"\nversion = [").unwrap();
    compile("broken-other", SCHEMA, &[(ID, &manifest), ("acme.broken", &broken)]).unwrap();
    let error = compile("broken-owner", SCHEMA, &[("acme.broken", &broken)]).unwrap_err();
    assert!(error.contains("outside") && error.contains("do not parse") && error.contains("acme.broken"), "{error}");
}

// The ABI tests name where each check fires: a manifest that fits leaves the check to the library, and a
// library that is missing shows that the manifest check ran first.

#[test]
fn a_library_of_another_abi_major_fails_before_any_pass_runs() {
    let manifest = installed("abi-major", "0.2");
    let error = compile("lib-major", SCHEMA, &[(ID, &manifest)]).unwrap_err();
    assert!(error.contains(ID) && error.contains("ABI 1.0") && error.contains("ABI 0.2") && error.contains("major") && error.contains("install a release of"), "{error}");
}

#[test]
fn a_library_with_a_higher_abi_minor_asks_for_an_update() {
    let manifest = installed("abi-newer", "0.2");
    let error = compile("lib-minor", SCHEMA, &[(ID, &manifest)]).unwrap_err();
    assert!(error.contains(ID) && error.contains("ABI 0.3") && error.contains("ABI 0.2") && error.contains("update the ORM"), "{error}");
}

#[test]
fn a_manifest_of_another_abi_fails_before_any_library_loads() {
    for (abi, advice) in [("1.0", "install a release of"), ("0.3", "update the ORM")] {
        let path = scratch(&format!("manifest-{abi}")).join("orm-extension.toml");
        fs::write(&path, manifest(abi).replace("library = \"outside_extension\"", "library = \"gone\"")).unwrap();
        let error = compile(&format!("manifest-{abi}"), SCHEMA, &[(ID, &path)]).unwrap_err();
        assert!(error.contains(ID) && error.contains(&format!("ABI {abi}")) && error.contains("ABI 0.2") && error.contains(advice) && !error.contains("gone"), "{error}");
    }
}

#[test]
fn a_manifest_that_disagrees_with_the_library_abi_fails() {
    let manifest = installed("", "0.0");
    let error = compile("disagree", SCHEMA, &[(ID, &manifest)]).unwrap_err();
    assert!(error.contains(ID) && error.contains("the library reports ABI 0.2") && error.contains("the manifest says 0.0"), "{error}");
}

#[test]
fn a_manifest_that_disagrees_still_fails_when_the_library_is_already_loaded() {
    let manifest = installed("", "0.2");
    compile("disagree-first", SCHEMA, &[(ID, &manifest)]).unwrap();
    let older = manifest.with_file_name("older.toml");
    fs::write(&older, fs::read_to_string(&manifest).unwrap().replace("abi = \"0.2\"", "abi = \"0.0\"")).unwrap();
    let error = compile("disagree-loaded", SCHEMA, &[(ID, &older)]).unwrap_err();
    assert!(error.contains("the library reports ABI 0.2") && error.contains("the manifest says 0.0"), "{error}");
}

#[test]
fn lowering_a_lowered_schema_again_keeps_one_field() {
    let manifest = installed("", "0.2");
    let mut ir = compile("again", SCHEMA, &[(ID, &manifest)]).unwrap();
    orm_core::behavior::prepare_with(&mut ir, None, Some(&plan(&manifest))).unwrap();
    assert_eq!(ir.models[0].fields.iter().filter(|f| f.name == "tag").count(), 1);
}

/// A schema that no pass has lowered: one model and the declaration `@@outside.tag("blue")`.
fn unlowered() -> SchemaIr {
    let id = FieldIr { primary_key: true, ..FieldIr::plain("id", ColType::Int) };
    let post = serde_json::json!({ "name": "Post", "table": "post", "fields": [id] });
    let mut ir = SchemaIr { models: vec![serde_json::from_value(post).unwrap()], ..SchemaIr::default() };
    ir.behavior.declarations.push(Declaration {
        attribute: "outside.tag".into(), model: "Post".into(), field: None, field_type: None, arguments: BTreeMap::new(),
        positional: vec!["blue".into()], location: SourceLocation { file: "schema.prisma".into(), line: 3, column: 2 }, lowered: false,
    });
    ir
}

// The artifact has one pass before and one pass after the pass of the library, and `after` sets both sides.
// A compiled pass is a recording closure here: the artifact with the real feature passes is a `feature-check` build.
#[test]
fn the_passes_of_the_artifact_and_of_a_library_run_in_one_order() {
    let manifest = installed("", "0.2");
    let ordered = manifest.with_file_name("ordered.toml");
    fs::write(&ordered, fs::read_to_string(&manifest).unwrap().replacen("[[passes]]\n", "[[passes]]\nafter = [\"acme.static.prep\"]\n", 1)).unwrap();
    let compiled: Manifest = toml::from_str(&format!(
        "id = \"acme.static\"\nversion = \"1.0.0\"\nhost_contract = \"{HOST_CONTRACT_VERSION}\"\nschema_contract = \"{SCHEMA_CONTRACT_VERSION}\"\n\
         languages = [\"python\"]\ndatabases = [\"postgres\"]\n\
         [[passes]]\nid = \"acme.static.prep\"\nphase = \"declaration\"\nrust = \"acme_static::prep\"\neffects = [\"models.static_prep\"]\n\
         [[passes]]\nid = \"acme.static.late\"\nphase = \"declaration\"\nrust = \"acme_static::late\"\nafter = [\"acme.outside.tag\"]\neffects = [\"models.static_late\"]\n"
    )).unwrap();
    let order = ["acme.static.prep".to_owned(), "acme.static.late".to_owned()];
    let plan = orm_core::loader::plan(&[(ID.to_owned(), ordered)], &["outside".to_owned()].into(), &[], &[compiled], &order).unwrap().unwrap();

    // Each compiled pass records whether the library pass has run before it.
    let seen = RefCell::new(vec![]);
    let run = |ir: &mut SchemaIr, pass: &str| {
        seen.borrow_mut().push((pass.to_owned(), ir.models[0].fields.iter().any(|f| f.name == "tag")));
        Ok(true)
    };
    let mut ir = unlowered();
    plan.lower(&mut ir, None, &run).unwrap();

    assert_eq!(*seen.borrow(), [("acme.static.prep".to_owned(), false), ("acme.static.late".to_owned(), true)]);
    let behavior = &ir.behavior;
    assert_eq!(behavior.completed_passes, order);
    assert_eq!(behavior.packages[ID].passes, PASSES);
    assert_eq!(behavior.extensions.get("acme.static").map(String::as_str), Some("1.0.0"));
    assert!(behavior.declarations.iter().all(|d| d.lowered));
    assert_eq!(ir.models[0].fields.iter().find(|f| f.name == "tag").unwrap().default, Some("blue".into()));
}

// The hooks of the library: the manifest declares `exports`, so the loader resolves `init`, `drop_state` and the table.

fn open(manifest: &Path) -> Library { plan(manifest).library(ID).unwrap() }

fn text(value: &str) -> Value { Value::Text(value.into()) }
fn arg(name: &str, value: Value) -> Arg { Arg { name: name.into(), value } }

#[test]
fn init_gets_the_extension_data_and_a_hook_reads_the_state() {
    let library = open(&installed("", "0.2"));
    let state = library.init(&serde_json::json!({"prefix": "x"})).unwrap();
    assert_eq!(state.call("outside.prefix", &[arg("value", text("hello"))], &Context::default()), Ok(text("xhello")));
    let bare = library.init(&serde_json::Value::Null).unwrap();
    assert_eq!(bare.call("outside.prefix", &[arg("value", text("hello"))], &Context::default()), Ok(text("hello")));
    assert!(state.hook("outside.echo").is_some() && state.hook("outside.gone").is_none());
}

#[test]
fn every_value_tag_makes_a_round_trip_through_a_hook() {
    let state = open(&installed("", "0.2")).init(&serde_json::Value::Null).unwrap();
    let values = [
        Value::Null, Value::Bool(true), Value::Bool(false), Value::Int(-7), Value::Float(1.5), Value::Decimal("12.340".into()), text("héllo"), text(""),
        Value::Bytes(vec![0, 1, 255]), Value::DateTime(-1_000_000), Value::Uuid([7; 16]), Value::Json("{\"a\":[1]}".into()), Value::Date(-3),
    ];
    for value in values {
        assert_eq!(state.call("outside.echo", &[arg("value", value.clone())], &Context::default()), Ok(value.clone()), "{value:?}");
    }
    let add = |a, b| state.call("outside.add", &[arg("a", a), arg("b", b)], &Context::default());
    assert_eq!(add(Value::Int(2), Value::Int(3)), Ok(Value::Int(5)));
    assert_eq!(add(Value::Float(0.5), Value::Float(1.0)), Ok(Value::Float(1.5)));
    assert!(add(Value::Int(2), Value::Float(3.0)).unwrap_err().contains("cannot add"));
}

#[test]
fn a_hook_reads_the_request_context() {
    let state = open(&installed("", "0.2")).init(&serde_json::Value::Null).unwrap();
    let context = Context { values: vec![arg("tenant", text("acme")), arg("n", Value::Int(1))] };
    assert_eq!(state.call("outside.context", &[], &context), Ok(Value::Json("{\"n\":2,\"tenant\":\"acme\"}".into())));
    assert_eq!(state.call("outside.context", &[], &Context::default()), Ok(Value::Json("{\"n\":0,\"tenant\":null}".into())));
}

#[test]
fn a_hook_error_is_its_own_text_and_a_panic_or_an_init_failure_names_the_extension() {
    let library = open(&installed("", "0.2"));
    let state = library.init(&serde_json::Value::Null).unwrap();
    // The text of a hook that returns `Err` is the whole error: the caller adds the model and the field.
    assert_eq!(state.call("outside.fail", &[], &Context::default()), Err("the hook failed".to_owned()));
    let error = state.call("outside.panic", &[], &Context::default()).unwrap_err();
    assert!(error.contains(ID) && error.contains("panicked") && error.contains("the hook panicked"), "{error}");
    assert!(state.call("outside.prefix", &[], &Context::default()).unwrap_err().contains("missing argument value"));
    assert!(state.call("outside.gone", &[], &Context::default()).unwrap_err().contains("no hook outside.gone"));
    // The library still works after a panic.
    assert!(state.call("outside.drops", &[], &Context::default()).is_ok());
    let error = library.init(&serde_json::json!({"prefix": 3})).err().unwrap();
    assert!(error.contains(ID) && error.contains("prefix must be a string"), "{error}");
    let error = library.init(&serde_json::json!({"panic": true})).err().unwrap();
    assert!(error.contains(ID) && error.contains("init panicked") && error.contains("init asked to panic"), "{error}");
}

#[test]
fn a_hook_of_point_two_returns_nothing_and_refuses_a_value_that_is_not_text() {
    let state = open(&installed("", "0.2")).init(&serde_json::Value::Null).unwrap();
    assert_eq!(state.call("outside.audit", &[arg("value", text("hello"))], &Context::default()), Ok(Value::Null));
    let error = state.call("outside.audit", &[arg("value", Value::Int(1))], &Context::default()).unwrap_err();
    assert_eq!(error, "value must be text, got Int(1)");
}

#[test]
fn drop_state_runs_once_when_the_state_drops() {
    // A copy of the library at its own path has its own counter.
    let library = open(&package("drops", "", &manifest("0.2")));
    let drops = |state: &orm_core::loader::State| state.call("outside.drops", &[], &Context::default()).unwrap();
    let first = library.init(&serde_json::Value::Null).unwrap();
    assert_eq!(drops(&first), Value::Int(0));
    let second = library.init(&serde_json::Value::Null).unwrap();
    drop(first);
    assert_eq!(drops(&second), Value::Int(1));
    drop(second);
    let third = library.init(&serde_json::Value::Null).unwrap();
    assert_eq!(drops(&third), Value::Int(2));
}

/// A package of the default build, and `missing.toml` beside it: a manifest that declares one export more than the library has a hook for.
fn with_a_missing_export(name: &str) -> (PathBuf, PathBuf) {
    let good = package(name, "", &manifest("0.2"));
    let bad = good.with_file_name("missing.toml");
    let extra = "[[exports]]\nid = \"outside.missing\"\npoint = 1\ninput = \"string\"\noutput = \"string\"\n\n[python]";
    fs::write(&bad, fs::read_to_string(&good).unwrap().replace("[python]", extra)).unwrap();
    (good, bad)
}

fn refuses_the_missing_export(error: &str) {
    assert!(error.contains(ID) && error.contains("the export outside.missing") && error.contains("no hook of that name"), "{error}");
}

#[test]
fn an_export_without_a_hook_fails_the_first_open_of_a_library() {
    let (_, bad) = with_a_missing_export("table-fresh");
    refuses_the_missing_export(&plan(&bad).library(ID).err().unwrap());
}

#[test]
fn an_export_without_a_hook_fails_when_the_library_is_already_loaded() {
    let (good, bad) = with_a_missing_export("table-loaded");
    open(&good);
    refuses_the_missing_export(&plan(&bad).library(ID).err().unwrap());
}

#[test]
fn a_manifest_without_exports_has_no_library_to_open() {
    let bare = package("bare", "", &without_exports(&manifest("0.2")));
    let error = plan(&bare).library(ID).err().unwrap();
    assert!(error.contains(ID) && error.contains("declares no exports"), "{error}");
}

// A library of slice 1 has the three symbols of ABI 0.1 and no `init`, no `drop_state` and no hook table.

#[test]
fn a_library_of_slice_one_lowers_a_schema_without_a_hook_table() {
    let text = without_exports(&manifest("0.1")).replace("schema_contract = \"3.1\"", "schema_contract = \"3.0\"");
    let path = package("pass-only", "pass-only", &text);
    let ir = compile("pass-only", SCHEMA, &[(ID, &path)]).unwrap();
    assert!(ir.models[0].fields.iter().any(|f| f.name == "tag"));
}

#[test]
fn a_manifest_that_declares_exports_for_a_library_of_slice_one_names_the_missing_table() {
    let path = package("pass-only-exports", "pass-only", &manifest("0.1"));
    let error = plan(&path).library(ID).err().unwrap();
    assert!(error.contains(ID) && error.contains("has no `orm_ext_hooks`") && error.contains("the manifest declares exports"), "{error}");
}

// A schema with a hook record: the compiled pass `acme.static.bind` binds the hook and sets the `extension_data` of the library.

fn bind_plan(manifest: &Path) -> Plan {
    let compiled: Manifest = toml::from_str(&format!(
        "id = \"acme.static\"\nversion = \"1.0.0\"\nhost_contract = \"{HOST_CONTRACT_VERSION}\"\nschema_contract = \"{SCHEMA_CONTRACT_VERSION}\"\n\
         languages = [\"python\"]\ndatabases = [\"postgres\"]\n\
         [[passes]]\nid = \"acme.static.bind\"\nphase = \"declaration\"\nrust = \"acme_static::bind\"\neffects = [\"models.static_bind\"]\n"
    )).unwrap();
    orm_core::loader::plan(&[(ID.to_owned(), manifest.to_path_buf())], &["outside".to_owned()].into(), &[], &[compiled], &["acme.static.bind".to_owned()]).unwrap().unwrap()
}

/// A hook of the library at `point` on the field `field` of `Post`.
fn binding(point: u8, field: &str, export: &str) -> HookBinding {
    HookBinding { source: ID.into(), point, model: "Post".into(), field: Some(field.into()), export: export.into(), dependencies: vec![], reads_context: false }
}

fn lower_with(plan: &Plan, hook: &HookBinding) -> Result<SchemaIr, String> {
    let run = |ir: &mut SchemaIr, _: &str| {
        ir.behavior.hooks.push(hook.clone());
        ir.behavior.extension_data.insert(ID.to_owned(), serde_json::json!({"prefix": "x"}));
        Ok(true)
    };
    let mut ir = unlowered();
    plan.lower(&mut ir, None, &run).map(|_| ir)
}

#[test]
fn a_hook_of_the_library_lowers_with_the_new_minor_and_runs_with_its_extension_data() {
    let plan = bind_plan(&installed("", "0.2"));
    let ir = lower_with(&plan, &binding(1, "tag", "outside.prefix")).unwrap();
    assert_eq!((ir.behavior.hooks.len(), ir.behavior.schema_contract_minor), (1, FIELD_ADAPTER_OPTIONS_MINOR));
    let state = plan.library(ID).unwrap().init(&ir.behavior.extension_data[ID]).unwrap();
    assert_eq!(state.call("outside.prefix", &[arg("value", text("hello"))], &Context::default()), Ok(text("xhello")));
}

#[test]
fn an_expression_record_sets_minor_4_also_after_generate_removed_its_hook() {
    let plan = bind_plan(&installed("", "0.2"));
    let ir = lower_with(&plan, &binding(7, "tag", "acme.outside.region")).unwrap();
    assert!(ir.behavior.hooks.is_empty(), "a hook that reads no context is evaluated at generate");
    let tag = ir.models[0].fields.iter().find(|f| f.name == "tag").unwrap();
    assert!(matches!(tag.client_default, Some(ClientDefaultIr::Expr(_))), "{:?}", tag.client_default);
    assert_eq!((ir.behavior.schema_contract, ir.behavior.schema_contract_minor), (orm_contracts::extension::SCHEMA_CONTRACT, EXPRESSION_RECORDS_MINOR));
    let ir = lower_with(&plan, &HookBinding { reads_context: true, ..binding(7, "tag", "acme.outside.region") }).unwrap();
    assert_eq!((ir.behavior.hooks.len(), ir.behavior.schema_contract_minor), (1, EXPRESSION_RECORDS_MINOR));
}

#[test]
fn lowering_refuses_a_hook_that_the_manifest_of_the_library_does_not_allow() {
    let plan = bind_plan(&installed("", "0.2"));
    for (hook, message) in [
        (binding(1, "tag", "outside.gone"), "is not in the manifest of acme.outside"),
        (binding(2, "tag", "outside.prefix"), "is bound at point 2, and the manifest says point 1"),
        (binding(1, "id", "outside.prefix"), "takes string but the field is int"),
    ] {
        let error = lower_with(&plan, &hook).unwrap_err();
        assert!(error.contains(message), "{error}");
    }
}

const FIELDS: &str = "model Order {\n id Int @id\n price Money @outside.money\n memo String @outside.note(\"remember\")\n}\n";

#[test]
fn an_outside_attribute_owns_a_field_type_and_takes_a_field_out_of_the_model() {
    let manifest = installed("", "0.2");
    let path = scratch("schema-fields").join("schema.prisma");
    fs::write(&path, FIELDS).unwrap();
    let _ = fs::remove_file(orm_core::identity::manifest_path(&path));
    let installed = Installed { extensions: [(ID.to_owned(), manifest)].into(), ..Installed::new() };
    let error = compile_file_with(&path, &installed).unwrap_err();
    assert!(error.contains("orm identities"), "an attribute with identities = true needs the identity file: {error}");
    orm_core::dsl::generate_identities_with(&path, &[], &[], &installed).unwrap();
    assert!(orm_core::identity::manifest_path(&path).is_file());

    let ir = compile_file_with(&path, &installed).unwrap();
    assert!(ir.identities.is_some());
    let names: Vec<&str> = ir.models[0].fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["id", "price_amount", "price_currency"], "Money becomes two columns and the note is no column");
    assert_eq!(ir.behavior.extension_data[ID]["notes"]["Order.memo"], "remember");
}

// `Schema::from_ir_with_packages` opens the library of each package that has bindings and resolves them into `Model.hooks`.

/// `Post { id, tag }` with the bindings `hooks`, the extension data of `data`, and the stamp of the library.
fn hooked(hooks: Vec<HookBinding>, data: serde_json::Value) -> SchemaIr {
    let id = FieldIr { primary_key: true, ..FieldIr::plain("id", ColType::Int) };
    let post = serde_json::json!({ "name": "Post", "table": "post", "fields": [id, FieldIr::plain("tag", ColType::Text)] });
    let mut ir = SchemaIr { models: vec![serde_json::from_value(post).unwrap()], ..SchemaIr::default() };
    ir.behavior.hooks = hooks;
    ir.behavior.extension_data.insert(ID.to_owned(), data);
    ir.behavior.schema_contract = orm_contracts::extension::SCHEMA_CONTRACT;
    ir.behavior.schema_contract_minor = SCHEMA_CONTRACT_MINOR;
    ir.behavior.packages.insert(ID.to_owned(), orm_contracts::extension::PackageStamp { version: "0.1.0".into(), languages: vec!["python".into(), "typescript".into()], databases: vec!["postgres".into(), "sqlite".into()], ..Default::default() });
    ir
}

fn packages(manifest: &Path) -> BTreeMap<String, PathBuf> { [(ID.to_owned(), manifest.parent().unwrap().to_path_buf())].into() }

#[test]
fn define_resolves_the_bindings_of_a_library_into_the_hook_plan_of_the_model() {
    let manifest = installed("", "0.2");
    let ir = hooked(vec![binding(1, "tag", "outside.prefix"), binding(2, "tag", "outside.audit")], serde_json::json!({"prefix": "x"}));
    let schema = orm_core::schema::Schema::from_ir_with_packages(ir, &packages(&manifest)).unwrap();
    let plan = schema.model(0).hooks.as_deref().expect("the model has a plan");
    assert_eq!((plan.transforms.len(), plan.validators.len(), plan.records.len()), (1, 1, 0));
    let (position, hook) = &plan.transforms[0];
    assert_eq!(*position, 1);
    let mut value = text("hello");
    hook.rewrite(&mut value, &Context::default()).unwrap();
    assert_eq!(value, text("xhello"), "init got the extension data");
}

#[test]
fn a_hook_binding_without_a_package_directory_fails_define() {
    let ir = hooked(vec![binding(1, "tag", "outside.prefix")], serde_json::Value::Null);
    let error = orm_core::schema::Schema::from_ir_with_packages(ir, &BTreeMap::new()).err().expect("no directory");
    assert!(error.contains(ID) && error.contains("no package directory"), "{error}");
}

// Code generation, migrations and compile-time checks build a schema without any extension library.
#[test]
fn a_schema_with_hook_bindings_builds_unbound_without_a_package_directory() {
    let ir = hooked(vec![binding(1, "tag", "outside.prefix")], serde_json::Value::Null);
    let schema = orm_core::schema::Schema::from_ir_unbound(ir).unwrap();
    assert!(schema.models.iter().all(|m| m.hooks.is_none()), "no binding is resolved");
    let (_, checked) = orm_core::dsl::check(hooked(vec![binding(1, "tag", "outside.prefix")], serde_json::Value::Null)).unwrap();
    assert!(checked.models.iter().all(|m| m.hooks.is_none()));
}

#[test]
fn a_binding_to_a_missing_export_and_a_failing_init_fail_define_with_the_id() {
    let manifest = installed("", "0.2");
    let error = orm_core::schema::Schema::from_ir_with_packages(hooked(vec![binding(1, "tag", "outside.gone")], serde_json::Value::Null), &packages(&manifest)).err().expect("no export");
    assert!(error.contains("Post.tag") && error.contains(ID) && error.contains("outside.gone"), "{error}");
    let error = orm_core::schema::Schema::from_ir_with_packages(hooked(vec![binding(1, "tag", "outside.prefix")], serde_json::json!({"fail": true})), &packages(&manifest)).err().expect("init fails");
    assert!(error.contains(ID) && error.contains("init asked to fail"), "{error}");
}

// `define()` validates the IR first, so only a direct call reaches the check of `bind` itself.
#[test]
fn bind_refuses_a_binding_of_an_extension_that_did_not_run_as_a_library() {
    let manifest = installed("", "0.2");
    let hooks = || hooked(vec![binding(1, "tag", "outside.prefix")], serde_json::Value::Null);
    let mut requirements = hooks().behavior;
    requirements.packages.clear();
    let mut schema = orm_core::schema::Schema::from_ir_unbound(hooks()).unwrap();
    let index = [("Post".to_owned(), 0)].into();
    let error = orm_core::hooks::bind(&mut schema.models, &index, &requirements, &packages(&manifest)).unwrap_err();
    assert!(error.contains("Post.tag") && error.contains(ID) && error.contains("did not run as a library"), "{error}");
    assert!(schema.models[0].hooks.is_none(), "a refused binding leaves no plan");
}

#[test]
fn a_binding_of_an_unknown_field_fails_define() {
    let manifest = installed("", "0.2");
    let error = orm_core::schema::Schema::from_ir_with_packages(hooked(vec![binding(1, "nope", "outside.prefix")], serde_json::Value::Null), &packages(&manifest)).err().expect("no field");
    assert!(error.contains("Post.nope") && error.contains("outside.prefix"), "{error}");
}

#[test]
fn a_library_with_the_wrong_abi_fails_define_with_both_versions() {
    let manifest = installed("abi-newer", "0.2");
    let error = orm_core::schema::Schema::from_ir_with_packages(hooked(vec![binding(1, "tag", "outside.prefix")], serde_json::Value::Null), &packages(&manifest)).err().expect("abi");
    assert!(error.contains(ID) && error.contains("ABI 0.3") && error.contains("ABI 0.2"), "{error}");
}

// Hook 4: a point-4 binding plus a write assignment give a field the value of a hook on update.

fn actor_ir(binding_ok: bool) -> SchemaIr {
    let mut hook = binding(4, "tag", "acme.outside.actor");
    hook.reads_context = true;
    let mut ir = hooked(if binding_ok { vec![hook] } else { vec![] }, serde_json::Value::Null);
    ir.behavior.write_assignments.push(orm_contracts::writes::WriteAssignment {
        source: ID.into(), model: "Post".into(), field: "tag".into(), on: vec![orm_contracts::writes::WriteKind::Update],
        value: orm_contracts::writes::AssignValue::Hook { export: "acme.outside.actor".into() }, upsert: Default::default(),
    });
    ir
}

#[test]
fn a_point_four_binding_resolves_into_the_assign_list_and_reads_the_context() {
    let manifest = installed("", "0.2");
    let schema = orm_core::schema::Schema::from_ir_with_packages(actor_ir(true), &packages(&manifest)).unwrap();
    let plan = schema.model(0).hooks.as_deref().expect("the model has a plan");
    assert_eq!((plan.assign.len(), plan.no_upsert, plan.reads_context()), (1, false, true));
    let hook = plan.assigner(1).expect("the tag field has a hook");
    let context = Context { values: vec![arg("user", text("ada"))] };
    assert_eq!(hook.produce(&context), Ok(text("ada")));
    assert!(hook.produce(&Context::default()).unwrap_err().contains("no user in the request scope"));
}

#[test]
fn a_hook_assignment_without_its_point_four_binding_fails_define() {
    let manifest = installed("", "0.2");
    let error = orm_core::schema::Schema::from_ir_with_packages(actor_ir(false), &packages(&manifest)).err().expect("no binding");
    assert!(error.contains("Post.tag") && error.contains("acme.outside.actor") && error.contains("no point-4 binding"), "{error}");
}

#[test]
fn a_point_four_hook_that_returns_the_wrong_type_fails_define() {
    let manifest = installed("", "0.2");
    let mut ir = actor_ir(true);
    ir.models[0].fields[1] = FieldIr::plain("tag", ColType::Int);
    let error = orm_core::schema::Schema::from_ir_with_packages(ir, &packages(&manifest)).err().expect("type");
    assert!(error.contains("Post.tag") && error.contains("acme.outside.actor"), "{error}");
}

// Hook 5: a point-5 binding on a result field gives a computed field that the library fills on read.

fn initials_ir(mutate: impl FnOnce(&mut SchemaIr)) -> SchemaIr {
    let id = FieldIr { primary_key: true, ..FieldIr::plain("id", ColType::Int) };
    let fields = [id, FieldIr::plain("first", ColType::Text), FieldIr::plain("last", ColType::Text), FieldIr::plain("initials", ColType::Text)];
    let person = serde_json::json!({ "name": "Person", "table": "person", "fields": fields });
    let mut ir = hooked(vec![], serde_json::Value::Null);
    ir.models = vec![serde_json::from_value(person).unwrap()];
    ir.behavior.hooks.push(HookBinding {
        source: ID.into(), point: 5, model: "Person".into(), field: Some("initials".into()), export: "acme.outside.initials".into(),
        dependencies: vec!["first".into(), "last".into()], reads_context: false,
    });
    ir.behavior.result_fields.push(orm_contracts::extension::ResultDeclaration { model: "Person".into(), field: "initials".into(), dependencies: vec!["first".into(), "last".into()] });
    mutate(&mut ir);
    ir
}

#[test]
fn a_point_five_binding_resolves_into_the_computed_list_of_the_hook_plan() {
    let manifest = installed("", "0.2");
    let schema = orm_core::schema::Schema::from_ir_with_packages(initials_ir(|_| {}), &packages(&manifest)).unwrap();
    let model = schema.model(0);
    let plan = model.hooks.as_deref().expect("the model has a plan");
    assert_eq!(plan.computed.len(), 1);
    let (position, hook, dependencies) = &plan.computed[0];
    assert_eq!((*position, dependencies.as_slice()), (3, &[1, 2][..]));
    assert!(model.computed(3) && !model.computed(1) && model.dependencies(3) == [1, 2]);
    assert_eq!(hook.compute(&[arg("first", text("ada")), arg("last", text("lovelace"))], &Context::default()), Ok(text("AL")));
}

/// Lowers the `Person` schema of `initials_ir(mutate)` through the pass hook of the plan, which validates it.
fn lower_initials(mutate: impl FnOnce(&mut SchemaIr)) -> Result<SchemaIr, String> {
    let plan = bind_plan(&installed("", "0.2"));
    let seed = std::cell::RefCell::new(Some(initials_ir(mutate)));
    let run = |ir: &mut SchemaIr, _: &str| {
        let seed = seed.borrow_mut().take().expect("one run");
        ir.models = seed.models;
        ir.behavior.hooks = seed.behavior.hooks;
        ir.behavior.result_fields = seed.behavior.result_fields;
        Ok(true)
    };
    let mut ir = unlowered();
    plan.lower(&mut ir, None, &run).map(|_| ir)
}

#[test]
fn a_point_five_binding_on_a_declared_result_field_lowers() {
    let ir = lower_initials(|_| {}).unwrap();
    assert_eq!(ir.behavior.result_fields.len(), 1);
}

#[test]
fn a_computed_field_that_depends_on_a_computed_field_fails() {
    let error = lower_initials(|ir| {
        ir.behavior.result_fields.push(orm_contracts::extension::ResultDeclaration { model: "Person".into(), field: "last".into(), dependencies: vec!["first".into()] });
        ir.behavior.hooks.push(HookBinding {
            source: ID.into(), point: 5, model: "Person".into(), field: Some("last".into()), export: "acme.outside.initials".into(),
            dependencies: vec!["first".into()], reads_context: false,
        });
    }).unwrap_err();
    assert!(error.contains("Person.initials") && error.contains("depends on the computed field last"), "{error}");
}

#[test]
fn a_point_five_binding_without_a_result_field_or_with_the_wrong_type_fails() {
    let error = lower_initials(|ir| ir.behavior.result_fields.clear()).unwrap_err();
    assert!(error.contains("Person.initials") && error.contains("result_fields"), "{error}");
    let error = lower_initials(|ir| ir.models[0].fields[3] = FieldIr::plain("initials", ColType::Int)).unwrap_err();
    assert!(error.contains("Person.initials") && error.contains("acme.outside.initials"), "{error}");
}

// Hooks 6 and 7: an expression hook gives the expression of a write assignment or of an insert default.

/// `Post { id, tag, note }`: `tag` has a write assignment from point 6 and `note` has an insert default from point 7.
fn expression_ir(reads_context: bool, mutate: impl FnOnce(&mut SchemaIr)) -> SchemaIr {
    let (write, default) = if reads_context { ("acme.outside.editor", "acme.outside.owner") } else { ("acme.outside.label", "acme.outside.region") };
    let mut ir = hooked(vec![], serde_json::Value::Null);
    let note = FieldIr { client_default: Some(orm_contracts::ir::ClientDefaultIr::Hook { export: default.into() }), ..FieldIr::plain("note", ColType::Text) };
    ir.models[0].fields.push(note);
    for (point, field, export) in [(6, "tag", write), (7, "note", default)] {
        ir.behavior.hooks.push(HookBinding { reads_context, ..binding(point, field, export) });
    }
    ir.behavior.write_assignments.push(orm_contracts::writes::WriteAssignment {
        source: ID.into(), model: "Post".into(), field: "tag".into(), on: vec![orm_contracts::writes::WriteKind::Update],
        value: orm_contracts::writes::AssignValue::ExprHook { export: write.into() }, upsert: Default::default(),
    });
    mutate(&mut ir);
    ir
}

/// Lowers `seed` through the pass hook of the plan, which validates it and evaluates the hooks that read no context.
fn lower_expressions(seed: SchemaIr) -> Result<SchemaIr, String> {
    let plan = bind_plan(&installed("", "0.2"));
    let seed = RefCell::new(Some(seed));
    let run = |ir: &mut SchemaIr, _: &str| {
        let seed = seed.borrow_mut().take().expect("one run");
        ir.models = seed.models;
        ir.behavior.hooks = seed.behavior.hooks;
        ir.behavior.write_assignments = seed.behavior.write_assignments;
        Ok(true)
    };
    let mut ir = unlowered();
    plan.lower(&mut ir, None, &run)?;
    // The test binary has no compiled `acme.static` extension for `define()` to find.
    ir.behavior.extensions.clear();
    ir.behavior.completed_passes.clear();
    Ok(ir)
}

#[test]
fn an_expression_hook_that_reads_no_context_becomes_a_stored_expression_at_lowering() {
    use orm_contracts::{ir::ClientDefaultIr, writes::AssignValue};
    let ir = lower_expressions(expression_ir(false, |_| {})).unwrap();
    assert!(ir.behavior.hooks.is_empty(), "the bindings are gone: {:?}", ir.behavior.hooks);
    let guarded = serde_json::json!({"t": "text", "value": "guarded"});
    assert_eq!(ir.behavior.write_assignments[0].value, AssignValue::Expr(guarded));
    assert!(matches!(&ir.models[0].fields[2].client_default, Some(ClientDefaultIr::Expr(e)) if *e == serde_json::json!({"t": "text", "value": "eu"})));
    // `define()` needs no package directory and loads no library for a stored expression.
    let schema = orm_core::schema::Schema::from_ir_with_packages(ir, &BTreeMap::new()).unwrap();
    assert!(schema.model(0).hooks.is_none());
    assert!(schema.model(0).assignments[0].expr.is_some());
}

#[test]
fn an_expression_hook_that_reads_the_context_stays_a_record_and_runs_on_each_statement() {
    use orm_contracts::{ir::ClientDefaultIr, writes::AssignValue};
    let ir = lower_expressions(expression_ir(true, |_| {})).unwrap();
    assert_eq!(ir.behavior.hooks.len(), 2);
    assert!(matches!(ir.behavior.write_assignments[0].value, AssignValue::ExprHook { .. }));
    assert!(matches!(ir.models[0].fields[2].client_default, Some(ClientDefaultIr::Hook { .. })));
    let error = orm_core::schema::Schema::from_ir_with_packages(ir, &BTreeMap::new()).err().expect("no directory");
    assert!(error.contains(ID) && error.contains("no package directory"), "{error}");
    let manifest = installed("", "0.2");
    let ir = lower_expressions(expression_ir(true, |_| {})).unwrap();
    let schema = orm_core::schema::Schema::from_ir_with_packages(ir, &packages(&manifest)).unwrap();
    let plan = schema.model(0).hooks.as_deref().expect("the model has a plan");
    assert_eq!((plan.expr_assign.len(), plan.expr_default.len(), plan.reads_context()), (1, 1, true));
    let context = Context { values: vec![arg("user", text("ada"))] };
    assert_eq!(plan.expr_assigner(1).unwrap().expression(&context), Ok(serde_json::json!({"t": "text", "value": "by:ada"})));
    assert_eq!(plan.expr_defaulter(2).unwrap().expression(&context), Ok(serde_json::json!({"t": "text", "value": "by:ada"})));
    assert!(plan.expr_assigner(1).unwrap().expression(&Context::default()).unwrap_err().contains("no user in the request scope"));
}

#[test]
fn an_expression_record_without_its_binding_or_with_a_transform_fails_at_lowering() {
    let error = lower_expressions(expression_ir(false, |ir| ir.behavior.hooks.retain(|h| h.point != 6))).unwrap_err();
    assert!(error.contains("Post.tag") && error.contains("acme.outside.label") && error.contains("no point-6 binding"), "{error}");
    let error = lower_expressions(expression_ir(false, |ir| ir.behavior.hooks.retain(|h| h.point != 7))).unwrap_err();
    assert!(error.contains("Post.note") && error.contains("acme.outside.region") && error.contains("no point-7 binding"), "{error}");
    let error = lower_expressions(expression_ir(false, |ir| ir.behavior.hooks.push(binding(1, "note", "outside.prefix")))).unwrap_err();
    assert!(error.contains("Post.note") && error.contains("takes no transform or validator"), "{error}");
}

#[test]
fn an_expression_hook_binds_only_at_the_point_of_its_export() {
    let plan = bind_plan(&installed("", "0.2"));
    let error = lower_with(&plan, &binding(6, "tag", "outside.prefix")).unwrap_err();
    assert!(error.contains("is bound at point 6, and the manifest says point 1"), "{error}");
    let error = lower_with(&plan, &binding(7, "tag", "outside.prefix")).unwrap_err();
    assert!(error.contains("is bound at point 7, and the manifest says point 1"), "{error}");
}
