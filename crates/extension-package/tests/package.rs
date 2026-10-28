//! Run the builder as an extension author does and read the files it writes.
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Output},
};

const KEYS: [(&str, &str); 3] = [
    ("darwin-arm64", "libmonorm_demo.dylib"),
    ("linux-x64", "libmonorm_demo.so"),
    ("win32-x64", "monorm_demo.dll"),
];

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "monorm-extension-package-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn crate_dir(root: &Path, library: &str) -> PathBuf {
    let dir = root.join("demo");
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"monorm_demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[lib]\ncrate-type = [\"cdylib\"]\n[workspace]\n",
    )
    .unwrap();
    fs::write(dir.join("src/lib.rs"), "").unwrap();
    fs::write(
        dir.join("monorm-extension.toml"),
        format!(
            "id = \"acme.demo\"\nversion = \"0.1.0\"\nabi = \"0.2\"\nlibrary = \"{library}\"\nschema_contract = 5\nlanguage_contract = 1\nlanguages = [\"python\"]\ndatabases = [\"postgres\"]\n"
        ),
    )
    .unwrap();
    dir
}

fn tsc(root: &Path) -> PathBuf {
    let path = root.join("tsc");
    fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

fn libraries(root: &Path, skip: Option<&str>) -> PathBuf {
    let dir = root.join("libraries");
    for (key, name) in KEYS {
        if Some(key) != skip {
            fs::create_dir_all(dir.join(key)).unwrap();
            fs::write(dir.join(key).join(name), key).unwrap();
        }
    }
    dir
}

fn builder(args: &[&dyn AsRef<std::ffi::OsStr>]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_monorm-extension-package"));
    for arg in args {
        command.arg(arg.as_ref());
    }
    command.output().unwrap()
}

fn assemble(root: &Path, dir: &Path, libraries: &Path) -> Output {
    builder(&[
        &"assemble",
        &dir,
        &"--libraries",
        &libraries,
        &"--out",
        &root.join("out"),
        &"--tsc",
        &tsc(root),
    ])
}

fn wheel_names(wheel: &Path) -> (Vec<String>, String, String) {
    let mut archive = zip_reader(wheel);
    let names = archive.names();
    let record = archive.read("acme_demo-0.1.0.dist-info/RECORD");
    let wheel_file = archive.read("acme_demo-0.1.0.dist-info/WHEEL");
    (names, record, wheel_file)
}

/// A minimal reader through the Python standard library; the crate has no zip dependency.
struct Zip(PathBuf);

fn zip_reader(path: &Path) -> Zip {
    Zip(path.to_owned())
}

impl Zip {
    fn python(&self, code: &str, arg: &str) -> String {
        let out = Command::new(std::env::var_os("PYTHON").unwrap_or_else(|| "python3".into()))
            .args(["-I", "-c", code])
            .arg(&self.0)
            .arg(arg)
            .output()
            .unwrap();
        assert!(out.status.success());
        let mut text = String::new();
        out.stdout.as_slice().read_to_string(&mut text).unwrap();
        text
    }
    fn names(&self) -> Vec<String> {
        let text = self.python(
            "import sys,zipfile;print('\\n'.join(zipfile.ZipFile(sys.argv[1]).namelist()))",
            "",
        );
        text.lines().map(str::to_owned).collect()
    }
    fn read(&mut self, name: &str) -> String {
        self.python("import sys,zipfile;print(zipfile.ZipFile(sys.argv[1]).read(sys.argv[2]).decode(),end='')", name)
    }
}

/// The stub compiler writes no `.js`, so read the TypeScript entry that it would compile.
fn manifest_hash(npm: &Path) -> String {
    let entry = fs::read_to_string(npm.join("index.ts")).unwrap();
    let line = entry
        .lines()
        .find_map(|l| l.strip_prefix("export const manifestHash = "))
        .unwrap();
    line.trim_end_matches(';').trim_matches('"').to_owned()
}

#[test]
fn assemble_writes_one_wheel_and_one_npm_package_with_the_three_libraries() {
    let root = scratch("assemble");
    let dir = crate_dir(&root, "monorm_demo");
    let output = assemble(&root, &dir, &libraries(&root, None));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let wheel = root.join("out/acme_demo-0.1.0-py3-none-macosx_11_0_arm64.manylinux_2_28_x86_64.win_amd64.whl");
    let (names, record, wheel_file) = wheel_names(&wheel);
    assert!(wheel_file.contains("Tag: py3-none-macosx_11_0_arm64.manylinux_2_28_x86_64.win_amd64"));
    assert!(wheel_file.contains("Root-Is-Purelib: false"));
    for (_, name) in KEYS {
        assert!(names.contains(&format!("acme_demo/{name}")), "{names:?}");
        assert!(
            record.contains(&format!("acme_demo/{name},sha256=")),
            "{record}"
        );
    }
    let package: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(root.join("out/npm/package.json")).unwrap())
            .unwrap();
    let files: Vec<_> = package["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f.as_str().unwrap())
        .collect();
    for (_, name) in KEYS {
        assert!(files.contains(&name), "{files:?}");
        assert!(root.join("out/npm").join(name).is_file());
    }
    assert!(package.get("os").is_none() && package.get("cpu").is_none());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn assemble_names_the_missing_platform() {
    let root = scratch("missing");
    let dir = crate_dir(&root, "monorm_demo");
    let output = assemble(&root, &dir, &libraries(&root, Some("win32-x64")));
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("win32-x64") && error.contains("monorm_demo.dll"),
        "{error}"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_library_with_an_extension_cannot_be_assembled() {
    let root = scratch("extension");
    let dir = crate_dir(&root, "libmonorm_demo.so");
    let output = assemble(&root, &dir, &libraries(&root, None));
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no file extension"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn package_and_assemble_stamp_the_same_manifest_hash() {
    let root = scratch("hash");
    let dir = crate_dir(&root, "monorm_demo");
    let output = assemble(&root, &dir, &libraries(&root, None));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let assembled = manifest_hash(&root.join("out/npm"));
    let single = root.join("single");
    let output = builder(&[&"package", &dir, &"--out", &single, &"--tsc", &tsc(&root)]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(manifest_hash(&single.join("npm")), assembled);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn library_copies_the_host_library_under_its_platform_key() {
    let root = scratch("library");
    let dir = crate_dir(&root, "monorm_demo");
    let out = root.join("built");
    let output = builder(&[&"library", &dir, &"--out", &out]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let built: Vec<_> = fs::read_dir(&out)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    let [platform] = built.as_slice() else {
        panic!("{built:?}")
    };
    let key = platform.file_name().unwrap().to_str().unwrap();
    let (_, name) = KEYS.iter().find(|k| k.0 == key).unwrap();
    assert!(platform.join(name).is_file());
    fs::remove_dir_all(root).unwrap();
}

const RUNTIMES: &str = "[python]\nsource = \"python\"\ndependencies = [\"opendal>=0.47,<0.48\", \"monorm>=0.1,<0.2\"]\n[typescript]\nsource = \"typescript\"\ndependencies = { opendal = \"^0.47.0\", \"@ufstack/monorm\" = \"^0.1.0\" }\nexports = { \"./model\" = \"model.js\" }\n";

/// A crate with runtime sources and `RUNTIMES` (or another tail) after the header keys.
fn crate_with_runtimes(root: &Path, tail: &str) -> PathBuf {
    let dir = crate_dir(root, "monorm_demo");
    let manifest = dir.join("monorm-extension.toml");
    let header = fs::read_to_string(&manifest).unwrap();
    fs::write(&manifest, format!("{header}languages = [\"python\", \"typescript\"]\n{tail}").replace("languages = [\"python\"]\ndatabases", "databases")).unwrap();
    fs::create_dir_all(dir.join("python")).unwrap();
    fs::write(dir.join("python/__init__.py"), "").unwrap();
    fs::create_dir_all(dir.join("typescript")).unwrap();
    fs::write(dir.join("typescript/index.ts"), "export {};\n").unwrap();
    fs::write(dir.join("typescript/model.ts"), "export const model = 1;\n").unwrap();
    dir
}

#[test]
fn dependencies_and_subpath_exports_reach_the_wheel_and_the_npm_package() {
    let root = scratch("keys");
    let dir = crate_with_runtimes(&root, RUNTIMES);
    let output = assemble(&root, &dir, &libraries(&root, None));
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let wheel = root.join("out/acme_demo-0.1.0-py3-none-macosx_11_0_arm64.manylinux_2_28_x86_64.win_amd64.whl");
    let metadata = zip_reader(&wheel).read("acme_demo-0.1.0.dist-info/METADATA");
    let requires: Vec<_> = metadata.lines().filter(|l| l.starts_with("Requires-Dist: ")).collect();
    assert_eq!(requires, ["Requires-Dist: opendal>=0.47,<0.48", "Requires-Dist: monorm>=0.1,<0.2"]);
    let package: serde_json::Value = serde_json::from_str(&fs::read_to_string(root.join("out/npm/package.json")).unwrap()).unwrap();
    assert_eq!(package["dependencies"], serde_json::json!({"opendal": "^0.47.0", "@ufstack/monorm": "^0.1.0"}));
    assert_eq!(package["exports"]["."]["default"], "./index.js");
    assert_eq!(package["exports"]["./model"], serde_json::json!({"types": "./model.d.ts", "default": "./model.js"}));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_package_without_the_keys_has_no_requires_dist_and_no_dependencies() {
    let root = scratch("nokeys");
    let dir = crate_with_runtimes(&root, "[python]\nsource = \"python\"\n[typescript]\nsource = \"typescript\"\n");
    let output = assemble(&root, &dir, &libraries(&root, None));
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let wheel = root.join("out/acme_demo-0.1.0-py3-none-macosx_11_0_arm64.manylinux_2_28_x86_64.win_amd64.whl");
    assert!(!zip_reader(&wheel).read("acme_demo-0.1.0.dist-info/METADATA").contains("Requires-Dist"));
    let package: serde_json::Value = serde_json::from_str(&fs::read_to_string(root.join("out/npm/package.json")).unwrap()).unwrap();
    assert!(package.get("dependencies").is_none());
    assert_eq!(package["exports"].as_object().unwrap().len(), 1);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_wrong_dependency_shape_or_export_is_an_error() {
    let cases = [
        ("[python]\nsource = \"python\"\ndependencies = { opendal = \"1\" }\n", "python.dependencies must be a list"),
        ("[typescript]\nsource = \"typescript\"\ndependencies = [\"opendal\"]\n", "typescript.dependencies must be a table"),
        ("[python]\nsource = \"python\"\nexports = { \"./a\" = \"a.js\" }\n", "python.exports is not allowed"),
        ("[typescript]\nsource = \"typescript\"\nexports = { \"model\" = \"model.js\" }\n", "use a subpath like"),
        ("[typescript]\nsource = \"typescript\"\nexports = { \"./m\" = \"../model.js\" }\n", "use a subpath like"),
        ("[typescript]\nsource = \"typescript\"\nexports = { \"./gone\" = \"gone.js\" }\n", "no source gone.ts"),
    ];
    for (index, (tail, message)) in cases.into_iter().enumerate() {
        let root = scratch(&format!("bad{index}"));
        let dir = crate_with_runtimes(&root, tail);
        let output = assemble(&root, &dir, &libraries(&root, None));
        assert!(!output.status.success(), "{tail}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains(message), "{tail}: {error}");
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn a_monorm_id_gives_the_ufstack_npm_name_and_a_monorm_dist() {
    // The id, then the npm name, the Python dist and the Python module that it gives.
    let cases = [
        ("monorm.inherit", "@ufstack/monorm-inherit", "monorm-inherit", "monorm_inherit"),
        (
            "monorm.query-defaults",
            "@ufstack/monorm-query-defaults",
            "monorm-query-defaults",
            "monorm_query_defaults",
        ),
        (
            "monorm.soft-delete",
            "@ufstack/monorm-soft-delete",
            "monorm-soft-delete",
            "monorm_soft_delete",
        ),
        ("acme.demo", "@acme/demo", "acme-demo", "acme_demo"),
    ];
    for (index, (id, npm, dist, module)) in cases.into_iter().enumerate() {
        let root = scratch(&format!("namespace{index}"));
        let dir = crate_dir(&root, "monorm_demo");
        let manifest = dir.join("monorm-extension.toml");
        let text = fs::read_to_string(&manifest).unwrap().replace("acme.demo", id);
        fs::write(&manifest, text).unwrap();
        let output = assemble(&root, &dir, &libraries(&root, None));
        assert!(output.status.success(), "{id}: {}", String::from_utf8_lossy(&output.stderr));
        let mut wheel = zip_reader(&root.join(format!("out/{module}-0.1.0-py3-none-macosx_11_0_arm64.manylinux_2_28_x86_64.win_amd64.whl")));
        let metadata = wheel.read(&format!("{module}-0.1.0.dist-info/METADATA"));
        assert!(metadata.contains(&format!("Name: {dist}\n")), "{id}: {metadata}");
        assert!(wheel.names().contains(&format!("{module}/py.typed")), "{id}");
        let package: serde_json::Value = serde_json::from_str(&fs::read_to_string(root.join("out/npm/package.json")).unwrap()).unwrap();
        assert_eq!(package["name"], npm, "{id}");
        fs::remove_dir_all(root).unwrap();
    }
}
