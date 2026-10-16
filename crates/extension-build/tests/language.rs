//! Extension language code: the build embeds it, records its members and type-checks it.
use sha2::{Digest, Sha256};
use std::{fs, path::{Path, PathBuf}, process::{Command, Output}};

fn copy(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() { copy(&entry.path(), &to.join(entry.file_name())); }
        else { fs::copy(entry.path(), to.join(entry.file_name())).unwrap(); }
    }
}

struct Case { host: PathBuf, base: PathBuf, fixture: PathBuf }

impl Case {
    fn new(name: &str) -> Self {
        let host = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap().to_owned();
        let base = host.join("target/extension-proof-tests").join(format!("language-{name}-{}", std::process::id()));
        let fixture = base.join("fixture");
        copy(&host.join("crates/extension-build/tests/fixtures/language"), &fixture);
        let manifest = fixture.join("Cargo.toml");
        let text = fs::read_to_string(&manifest).unwrap().replace("../../../../contracts", host.join("crates/contracts").to_str().unwrap());
        fs::write(manifest, text).unwrap();
        Self { host, base, fixture }
    }

    fn patch(&self, file: &str, from: &str, to: &str) {
        let path = self.fixture.join(file);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains(from), "{file} lacks {from:?}");
        fs::write(path, text.replace(from, to)).unwrap();
    }

    fn build(&self) -> Output {
        let config = serde_json::json!({"host":self.host,"output":self.base.join("out"),"offline":true,"prepare_only":true,
            "dependencies":{"language":{"package":"example-language","path":self.fixture}}});
        let path = self.base.join("config.json");
        fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
        Command::new(env!("CARGO_BIN_EXE_orm-extension-build")).arg(path).output().unwrap()
    }

    fn failure(&self) -> String {
        let result = self.build();
        assert!(!result.status.success(), "the build must fail");
        String::from_utf8_lossy(&result.stderr).into_owned()
    }
}

impl Drop for Case { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.base); } }

fn sha(files: &[&Path], root: &Path) -> String {
    let mut sha = Sha256::new();
    for file in files {
        let name = file.strip_prefix(root).unwrap().to_str().unwrap();
        let content = fs::read_to_string(file).unwrap();
        sha.update((name.len() as u64).to_le_bytes());
        sha.update(name.as_bytes());
        sha.update((content.len() as u64).to_le_bytes());
        sha.update(content.as_bytes());
    }
    sha.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn build_embeds_sources_and_records_members() {
    let case = Case::new("ok");
    let result = case.build();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    let artifact: serde_json::Value = serde_json::from_slice(&fs::read(case.base.join("out/artifact.json")).unwrap()).unwrap();
    let records = artifact["language_members"].as_array().unwrap();
    assert_eq!(records.len(), 2);
    let python = &records[0];
    assert_eq!((python["extension"].as_str(), python["language"].as_str(), python["version"].as_str()), (Some("example.language"), Some("python"), Some("1.0.0")));
    assert_eq!(python["query_set_members"], serde_json::json!(["everything"]));
    assert_eq!(python["instance_members"], serde_json::json!(["label", "me"]));
    let python_dir = case.fixture.join("python/example_language");
    assert_eq!(python["hash"], sha(&[&python_dir.join("__init__.py")], &python_dir));
    let typescript = &records[1];
    assert_eq!(typescript["language"], "typescript");
    assert_eq!(typescript["query_set_members"], serde_json::json!(["everything"]));
    assert_eq!(typescript["instance_members"], serde_json::json!(["label"]));
    let ts_dir = case.fixture.join("typescript/src");
    assert_eq!(typescript["hash"], sha(&[&ts_dir.join("index.ts")], &ts_dir));
    let composition = fs::read_to_string(case.base.join("out/composition.rs")).unwrap();
    assert!(composition.contains("language_sources") && composition.contains("class TaggedQuerySet") && composition.contains("taggedQuerySet"));
}

#[test]
fn a_python_type_error_stops_the_build() {
    let case = Case::new("py-error");
    case.patch("python/example_language/__init__.py", "return str(self.pk)", "return 1");
    let error = case.failure();
    assert!(error.contains("example.language (python) mypy: type check failed") && error.contains("[return-value]"), "{error}");
}

#[test]
fn a_typescript_type_error_stops_the_build() {
    let case = Case::new("ts-error");
    case.patch("typescript/src/index.ts", "return String(this.pk);", "return 1;");
    let error = case.failure();
    assert!(error.contains("example.language (typescript) tsc: type check failed") && error.contains("TS2322"), "{error}");
}

#[test]
fn an_incompatible_override_stops_the_build() {
    let case = Case::new("override");
    case.patch("Cargo.toml", "instance = \"TaggedInstance\"\n\n[package.metadata.orm-extension.typescript]", "instance = \"TaggedInstance\"\noverrides = [\"without_defaults\"]\n\n[package.metadata.orm-extension.typescript]");
    case.patch("python/example_language/__init__.py", "    def everything(self) -> Self:", "    def without_defaults(self, extra: int) -> Self:\n        return self\n\n    def everything(self) -> Self:");
    let error = case.failure();
    assert!(error.contains("(python) mypy: type check failed") && error.contains("without_defaults"), "{error}");
}

#[test]
fn an_override_must_name_a_member() {
    let case = Case::new("unknown-override");
    case.patch("Cargo.toml", "instance = \"TaggedInstance\"\n\n[package.metadata.orm-extension.typescript]", "instance = \"TaggedInstance\"\noverrides = [\"nothing\"]\n\n[package.metadata.orm-extension.typescript]");
    assert!(case.failure().contains("override nothing is not a member"));
}

#[test]
fn a_section_needs_its_language_and_the_contract() {
    let case = Case::new("languages");
    case.patch("Cargo.toml", "languages = [\"python\", \"typescript\"]", "languages = [\"python\"]");
    assert!(case.failure().contains("typescript language section requires typescript in languages"));
    let case = Case::new("contract");
    case.patch("Cargo.toml", "language_contract = 1", "language_contract = 2");
    assert!(case.failure().contains("incompatible language contract"));
}
