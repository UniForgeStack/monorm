//! An extension built in its own workspace (`tests/outside-extension`) runs through the loader.
//! Each ABI feature is one build; each installed package is a copy of a build with its own manifest.
use orm_contracts::extension::{Declaration, Manifest, SourceLocation, HOST_CONTRACT_VERSION, SCHEMA_CONTRACT_VERSION};
use orm_core::dsl::{compile_file_with, Installed};
use orm_core::ir::{ColType, FieldIr, SchemaIr};
use std::{cell::RefCell, collections::BTreeMap, fs, path::{Path, PathBuf}, process::Command, sync::Mutex};

const ID: &str = "acme.outside";
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
    fs::read_to_string(root().join("orm-extension.toml")).unwrap().replace("abi = \"0.1\"", &format!("abi = \"{abi}\""))
}

/// Builds the library with `feature` and keeps a copy: every build writes the same file.
fn build(feature: &str) -> PathBuf {
    let target = scratch("target");
    let mut build = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    build.args(["build", "--offline", "--quiet", "--manifest-path"]).arg(root().join("Cargo.toml")).arg("--target-dir").arg(&target);
    if !feature.is_empty() { build.args(["--features", feature]); }
    assert!(build.status().unwrap().success(), "the outside extension did not build");
    let name = format!("{}outside_extension{}", std::env::consts::DLL_PREFIX, std::env::consts::DLL_SUFFIX);
    let kept = scratch(&format!("library-{}", variant(feature))).join(&name);
    fs::copy(target.join("debug").join(&name), &kept).unwrap();
    kept
}

/// The installed package of a variant: a copy of the library that `feature` builds, and a manifest that says `abi`.
/// Each variant has its own directory, so the loader opens its own copy of the library.
fn installed(feature: &str, abi: &str) -> PathBuf {
    let mut builds = BUILDS.lock().unwrap_or_else(|e| e.into_inner());
    let key = (feature.to_owned(), abi.to_owned());
    if let Some(path) = builds.packages.get(&key) { return path.clone(); }
    let library = builds.libraries.entry(feature.to_owned()).or_insert_with(|| build(feature)).clone();
    let dir = scratch(&format!("{}-{abi}", variant(feature)));
    fs::copy(&library, dir.join(library.file_name().unwrap())).unwrap();
    let path = dir.join("orm-extension.toml");
    fs::write(&path, manifest(abi)).unwrap();
    builds.packages.insert(key, path.clone());
    path
}

fn compile(name: &str, schema: &str, extensions: &[(&str, &Path)]) -> Result<SchemaIr, String> {
    let path = scratch(&format!("schema-{name}")).join("schema.prisma");
    fs::write(&path, schema).unwrap();
    let installed = Installed { extensions: extensions.iter().map(|(id, path)| ((*id).to_owned(), path.to_path_buf())).collect(), ..Installed::new() };
    compile_file_with(&path, &installed)
}

#[test]
fn an_outside_library_lowers_a_schema_that_uses_its_attribute() {
    let manifest = installed("", "0.1");
    let ir = compile("lowers", SCHEMA, &[(ID, &manifest)]).unwrap();
    let tag = ir.models[0].fields.iter().find(|f| f.name == "tag").expect("the pass adds the field");
    assert_eq!(tag.default, Some("blue".into()));
    let stamp = &ir.behavior.packages[ID];
    assert_eq!((stamp.version.as_str(), stamp.passes.as_slice(), stamp.attributes.as_slice()), ("0.1.0", ["acme.outside.tag".to_owned()].as_slice(), ["outside.tag".to_owned()].as_slice()));
}

#[test]
fn the_generator_key_loads_a_library_that_no_attribute_selects() {
    let manifest = installed("", "0.1");
    let schema = "generator client {\n provider = \"orm\"\n extensions = [\"acme.outside\"]\n}\nmodel Post {\n id Int @id\n}\n";
    assert!(compile("named", schema, &[(ID, &manifest)]).unwrap().behavior.packages.contains_key(ID));
    assert!(compile("unused", "model Post {\n id Int @id\n}\n", &[(ID, &manifest)]).unwrap().behavior.packages.is_empty());
}

#[test]
fn a_pass_error_names_the_extension_and_the_pass() {
    let manifest = installed("", "0.1");
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
    let manifest = installed("", "0.1");
    let rival = scratch("rival").join("orm-extension.toml");
    fs::write(&rival, "id = \"acme.rival\"\nversion = \"1.0.0\"\nschema_contract = \"2.0\"\nabi = \"0.1\"\nlibrary = \"none\"\nlanguages = [\"python\"]\ndatabases = [\"sqlite\"]\n[[passes]]\nid = \"acme.rival.tag\"\nphase = \"declaration\"\neffects = [\"models.outside\"]\n").unwrap();
    let schema = "generator client {\n extensions = [\"acme.outside\", \"acme.rival\"]\n}\nmodel Post {\n id Int @id\n}\n";
    let error = compile("conflict", schema, &[(ID, &manifest), ("acme.rival", &rival)]).unwrap_err();
    assert!(error.contains("models.outside") && error.contains(ID) && error.contains("acme.rival"), "{error}");
}

#[test]
fn a_broken_manifest_of_another_package_fails_only_a_compile_that_it_can_own() {
    let manifest = installed("", "0.1");
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
    let manifest = installed("abi-major", "0.1");
    let error = compile("lib-major", SCHEMA, &[(ID, &manifest)]).unwrap_err();
    assert!(error.contains(ID) && error.contains("ABI 1.0") && error.contains("ABI 0.1") && error.contains("major") && error.contains("install a release of"), "{error}");
}

#[test]
fn a_library_with_a_higher_abi_minor_asks_for_an_update() {
    let manifest = installed("abi-newer", "0.1");
    let error = compile("lib-minor", SCHEMA, &[(ID, &manifest)]).unwrap_err();
    assert!(error.contains(ID) && error.contains("ABI 0.2") && error.contains("ABI 0.1") && error.contains("update the ORM"), "{error}");
}

#[test]
fn a_manifest_of_another_abi_fails_before_any_library_loads() {
    for (abi, advice) in [("1.0", "install a release of"), ("0.2", "update the ORM")] {
        let path = scratch(&format!("manifest-{abi}")).join("orm-extension.toml");
        fs::write(&path, manifest(abi).replace("library = \"outside_extension\"", "library = \"gone\"")).unwrap();
        let error = compile(&format!("manifest-{abi}"), SCHEMA, &[(ID, &path)]).unwrap_err();
        assert!(error.contains(ID) && error.contains(&format!("ABI {abi}")) && error.contains("ABI 0.1") && error.contains(advice) && !error.contains("gone"), "{error}");
    }
}

#[test]
fn a_manifest_that_disagrees_with_the_library_abi_fails() {
    let manifest = installed("", "0.0");
    let error = compile("disagree", SCHEMA, &[(ID, &manifest)]).unwrap_err();
    assert!(error.contains(ID) && error.contains("the library reports ABI 0.1") && error.contains("the manifest says 0.0"), "{error}");
}

#[test]
fn a_manifest_that_disagrees_still_fails_when_the_library_is_already_loaded() {
    let manifest = installed("", "0.1");
    compile("disagree-first", SCHEMA, &[(ID, &manifest)]).unwrap();
    let older = manifest.with_file_name("older.toml");
    fs::write(&older, fs::read_to_string(&manifest).unwrap().replace("abi = \"0.1\"", "abi = \"0.0\"")).unwrap();
    let error = compile("disagree-loaded", SCHEMA, &[(ID, &older)]).unwrap_err();
    assert!(error.contains("the library reports ABI 0.1") && error.contains("the manifest says 0.0"), "{error}");
}

#[test]
fn lowering_a_lowered_schema_again_keeps_one_field() {
    let manifest = installed("", "0.1");
    let mut ir = compile("again", SCHEMA, &[(ID, &manifest)]).unwrap();
    let artifact = orm_core::behavior::artifact();
    let plan = orm_core::loader::plan(&[(ID.to_owned(), manifest.clone())], &["outside".to_owned()].into(), &[], orm_core::behavior::manifests(), &artifact.passes).unwrap().unwrap();
    orm_core::behavior::prepare_with(&mut ir, None, Some(&plan)).unwrap();
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
    let manifest = installed("", "0.1");
    let ordered = manifest.with_file_name("ordered.toml");
    fs::write(&ordered, fs::read_to_string(&manifest).unwrap().replace("[[passes]]\n", "[[passes]]\nafter = [\"acme.static.prep\"]\n")).unwrap();
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
    assert_eq!(behavior.packages[ID].passes, ["acme.outside.tag"]);
    assert_eq!(behavior.extensions.get("acme.static").map(String::as_str), Some("1.0.0"));
    assert!(behavior.declarations.iter().all(|d| d.lowered));
    assert_eq!(ir.models[0].fields.iter().find(|f| f.name == "tag").unwrap().default, Some("blue".into()));
}
