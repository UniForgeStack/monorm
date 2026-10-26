//! Extensions that ship their passes as native libraries: select, check, load and run them.
//! It names no extension. The passes of libraries and the passes compiled into the artifact run in one order.
use sha2::{Digest, Sha256};
use orm_contracts::abi::{AbiVersionFn, Buffer, FreeFn, RunPassFn, SYMBOL_ABI_VERSION, SYMBOL_FREE, SYMBOL_RUN_PASS, STATUS_OK, STATUS_PANIC};
use orm_contracts::extension::{attribute_namespace, PackageStamp, ABI_VERSION, SCHEMA_CONTRACT, SCHEMA_CONTRACT_VERSION};
use orm_contracts::ir::SchemaIr;
use orm_contracts::version::ContractVersion;
use orm_contracts::resolve::resolve;
use crate::behavior::{capture_storage, validate_declarations, validate_requirements, Manifest, Pass};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The file that describes an extension package, beside its library.
pub const MANIFEST_FILE: &str = "orm-extension.toml";

/// A native library after the ABI check. The library stays loaded for the life of the process.
struct Native {
    _library: libloading::Library,
    run: RunPassFn,
    free: FreeFn,
    abi: ContractVersion,
}

static LIBRARIES: Mutex<BTreeMap<PathBuf, Arc<Native>>> = Mutex::new(BTreeMap::new());

/// An installed extension that the schema selected.
pub struct Dynamic {
    pub manifest: Manifest,
    /// SHA-256 of the raw manifest bytes, independent of the parsed contract shape.
    pub manifest_hash: String,
    dir: PathBuf,
    path: PathBuf,
}

/// The selected libraries and the one run order of all passes.
pub struct Plan {
    /// The manifests of the artifact and of the selected libraries.
    pub manifests: Vec<Manifest>,
    dynamic: Vec<Dynamic>,
    order: Vec<Pass>,
    /// The pass order of the artifact alone: the lowering state of an IR keeps it.
    static_order: Vec<String>,
}

/// The manifest fields that every release reads in one way. A later release can add fields, so
/// this struct ignores the unknown ones: the loader checks the versions and the namespaces first,
/// and parses the whole manifest only for a selected package.
#[derive(Deserialize)]
struct Header {
    id: String,
    schema_contract: Option<ContractVersion>,
    abi: Option<ContractVersion>,
    #[serde(default)]
    attributes: Vec<HeaderAttribute>,
}

#[derive(Deserialize)]
struct HeaderAttribute { name: String }

impl Header {
    /// The version errors of `Manifest::check_contracts`, from the header fields alone.
    fn check_versions(&self) -> Result<(), String> {
        if let Some(schema) = self.schema_contract { schema.check("schema contract", SCHEMA_CONTRACT_VERSION, &self.id)?; }
        if let Some(abi) = self.abi { abi.check("ABI", ABI_VERSION, &self.id)?; }
        Ok(())
    }

    fn uses(&self, namespaces: &BTreeSet<String>) -> bool {
        self.attributes.iter().any(|a| namespaces.contains(attribute_namespace(&a.name)))
    }
}

/// The text of the manifest file `path` of extension `id`, and its header.
fn read_header(id: &str, path: &Path) -> Result<(String, Header), String> {
    let failed = |e: &dyn std::fmt::Display| format!("extension {id}: {}: {e}", path.display());
    let text = std::fs::read_to_string(path).map_err(|e| failed(&e))?;
    let header = toml::from_str(&text).map_err(|e| failed(&e))?;
    Ok((text, header))
}

fn parse_manifest(id: &str, path: &Path, text: &str, header: &Header) -> Result<Dynamic, String> {
    if header.id != id {
        return Err(format!("extension {id}: {} describes {}; fix the extension id or the manifest path", path.display(), header.id));
    }
    let manifest: Manifest = toml::from_str(text).map_err(|e| {
        // A later minor can add a field. The version error tells the user what to do; an unknown field does not.
        header.check_versions().err().unwrap_or_else(|| format!("extension {id}: {}: {e}", path.display()))
    })?;
    let dir = path.parent().map_or_else(PathBuf::new, Path::to_path_buf);
    Ok(Dynamic { manifest, manifest_hash: format!("{:x}", Sha256::digest(text.as_bytes())), dir, path: path.to_path_buf() })
}

/// Reads `manifest`, the file that `id` names.
pub fn read_manifest(id: &str, manifest: &Path) -> Result<Dynamic, String> {
    let (text, header) = read_header(id, manifest)?;
    parse_manifest(id, manifest, &text, &header)
}

/// What only a selected library must satisfy; an unrelated installed package never fails a compile.
fn check_library(d: &Dynamic) -> Result<(), String> {
    if d.manifest.is_dynamic() { return Ok(()); }
    Err(format!("extension {}: {} has no `abi`, so it has no library to load; add `abi` and `library`", d.manifest.id, d.path.display()))
}

/// Chooses the extensions of `available` that the schema uses (an attribute namespace in `used`) or names
/// in `generator { extensions }`, then checks them with `statics`, the manifests of the artifact, and
/// `static_order`, their pass order. A named id of `statics` selects nothing: the artifact has it.
/// `None` when no library is selected: the artifact alone lowers the schema.
pub fn plan(available: &[(String, PathBuf)], used: &BTreeSet<String>, named: &[String], statics: &[Manifest], static_order: &[String]) -> Result<Option<Plan>, String> {
    let mut seen: BTreeMap<&str, &Path> = BTreeMap::new();
    let mut selected: Vec<Dynamic> = vec![];
    let mut unreadable: Vec<String> = vec![];
    for (id, path) in available {
        if let Some(first) = seen.insert(id, path) {
            return Err(format!("extension {id} is installed twice: {} and {}; uninstall one of them", first.display(), path.display()));
        }
        let wanted = named.iter().any(|n| n == id);
        // An unrelated package only has to show its attribute names: a broken or newer one never fails a compile.
        match read_header(id, path) {
            Ok((text, header)) if wanted || header.uses(used) => {
                let dynamic = parse_manifest(id, path, &text, &header)?;
                check_library(&dynamic)?;
                selected.push(dynamic);
            }
            Ok(_) => {}
            Err(error) if wanted => return Err(error),
            Err(error) => unreadable.push(error),
        }
    }
    if let Some(missing) = named.iter().find(|n| !available.iter().any(|(id, _)| id == *n) && !statics.iter().any(|m| m.id == **n)) {
        return Err(format!("generator names extension {missing}, which is not installed; install its package or pass --extension {missing}=<path to {MANIFEST_FILE}>"));
    }
    if !unreadable.is_empty() {
        let owned: BTreeSet<&str> = statics.iter().chain(selected.iter().map(|d| &d.manifest))
            .flat_map(|m| &m.attributes).map(|a| attribute_namespace(&a.name)).collect();
        // A package that does not parse can own the namespace: the user sees why nothing owns it.
        if let Some(namespace) = used.iter().find(|n| !owned.contains(n.as_str())) {
            return Err(format!("no installed extension owns the attribute namespace {namespace}; these installed manifests do not parse, and one of them can be the owner: {}", unreadable.join("; ")));
        }
    }
    if selected.is_empty() { return Ok(None); }
    let all: Vec<Manifest> = statics.iter().cloned().chain(selected.iter().map(|d| d.manifest.clone())).collect();
    let resolved = resolve(all, &|_, _, _| Ok(()))?;
    Ok(Some(Plan { manifests: resolved.manifests, dynamic: selected, order: resolved.passes, static_order: static_order.to_vec() }))
}

impl Plan {
    /// Loads each library and checks its ABI version, so a mismatch fails before any pass runs.
    fn load(&self) -> Result<BTreeMap<&str, Arc<Native>>, String> {
        self.dynamic.iter().map(|d| Ok((d.manifest.id.as_str(), open(d)?))).collect()
    }

    /// Runs every pass of the artifact and of the libraries in the resolved order.
    /// `run_static` runs a compiled pass by id and tells whether the artifact has it.
    pub fn lower(&self, ir: &mut SchemaIr, language: Option<&str>, run_static: &dyn Fn(&mut SchemaIr, &str) -> Result<bool, String>) -> Result<(), String> {
        if ir.behavior.schema_contract != 0 && ir.behavior.schema_contract != SCHEMA_CONTRACT {
            return Err(format!("schema contract {} unavailable (artifact {}); rebuild native artifact", ir.behavior.schema_contract, SCHEMA_CONTRACT));
        }
        validate_declarations(ir, &self.manifests, language)?;
        if !ir.behavior.completed_passes.is_empty() && ir.behavior.completed_passes != self.static_order {
            return Err("incompatible lowering state; rebuild schema and native artifact".into());
        }
        let natives = self.load()?;
        let owner_of: BTreeMap<&str, &str> = self.dynamic.iter()
            .flat_map(|d| d.manifest.passes.iter().map(move |p| (p.id.as_str(), d.manifest.id.as_str()))).collect();
        capture_storage(ir)?;
        let previous: Vec<_> = ir.behavior.declarations.iter().filter(|d| d.lowered).cloned().collect();
        ir.behavior.declarations.retain(|d| !d.lowered);
        ir.behavior.completed_passes.clear();
        for d in &self.dynamic {
            let attributes = d.manifest.attributes.iter().map(|a| a.name.clone()).collect();
            ir.behavior.packages.insert(d.manifest.id.clone(), PackageStamp { version: d.manifest.version.clone(), manifest_hash: d.manifest_hash.clone(), passes: vec![], attributes,
                dependencies: d.manifest.dependencies.clone(), languages: d.manifest.languages.clone(), databases: d.manifest.databases.clone(),
                language_members: d.manifest.language_sections().map(|(l, s)| (l.to_owned(), s.clone())).collect(),
            });
        }
        for pass in &self.order {
            match owner_of.get(pass.id.as_str()) {
                Some(owner) => {
                    *ir = natives[owner].call(owner, &pass.id, ir)?;
                    ir.behavior.packages.get_mut(*owner).expect("stamped above").passes.push(pass.id.clone());
                }
                None => {
                    if !run_static(ir, &pass.id)? { return Err(format!("pass {} is not compiled into this artifact; rebuild with its extension", pass.id)); }
                }
            }
        }
        ir.behavior.completed_passes = self.static_order.clone();
        for declaration in &mut ir.behavior.declarations { declaration.lowered = true; }
        ir.behavior.declarations.extend(previous);
        ir.behavior.lowered_models = ir.models.iter().map(|m| m.name.clone()).collect();
        ir.behavior.schema_contract = SCHEMA_CONTRACT;
        ir.behavior.extensions = self.manifests.iter().filter(|m| !m.is_dynamic()).map(|m| (m.id.clone(), m.version.clone())).collect();
        validate_requirements(ir, &self.manifests, language)
    }
}

fn library_path(d: &Dynamic) -> PathBuf {
    let name = d.manifest.library.as_deref().unwrap_or_default();
    let file = if Path::new(name).extension().is_some() { name.to_owned() }
        else { format!("{}{name}{}", std::env::consts::DLL_PREFIX, std::env::consts::DLL_SUFFIX) };
    d.dir.join(file)
}

fn open(d: &Dynamic) -> Result<Arc<Native>, String> {
    let id = &d.manifest.id;
    let path = library_path(d);
    let path = path.canonicalize().map_err(|e| format!("extension {id}: library {}: {e}; build or reinstall the package", path.display()))?;
    let mut cache = LIBRARIES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(native) = cache.get(&path) {
        native.check_manifest(d)?;
        return Ok(native.clone());
    }
    // SAFETY: loading a library runs its initializers; the user chose to install this extension.
    let library = unsafe { libloading::Library::new(&path) }.map_err(|e| format!("extension {id}: cannot load {}: {e}", path.display()))?;
    let symbol = |name: &[u8]| format!("extension {id}: {} has no `{}`; build it with orm_contracts::extension!", path.display(), String::from_utf8_lossy(&name[..name.len() - 1]));
    // SAFETY: the symbol types are the ABI of orm_contracts::abi; ABI 0.1 is checked below before any other call.
    let (abi, run, free) = unsafe {
        let abi = *library.get::<AbiVersionFn>(SYMBOL_ABI_VERSION).map_err(|_| symbol(SYMBOL_ABI_VERSION))?;
        let run = *library.get::<RunPassFn>(SYMBOL_RUN_PASS).map_err(|_| symbol(SYMBOL_RUN_PASS))?;
        let free = *library.get::<FreeFn>(SYMBOL_FREE).map_err(|_| symbol(SYMBOL_FREE))?;
        (abi, run, free)
    };
    // SAFETY: `abi` takes no argument and reads nothing.
    let reported = ContractVersion::from_bits(unsafe { abi() });
    reported.check("ABI", ABI_VERSION, id)?;
    let native = Arc::new(Native { _library: library, run, free, abi: reported });
    native.check_manifest(d)?;
    cache.insert(path, native.clone());
    Ok(native)
}

impl Native {
    fn check_manifest(&self, d: &Dynamic) -> Result<(), String> {
        if Some(self.abi) == d.manifest.abi { return Ok(()); }
        Err(format!("extension {}: the library reports ABI {}, the manifest says {}; rebuild the library or fix the manifest", d.manifest.id, self.abi, d.manifest.abi.map_or_else(String::new, |v| v.to_string())))
    }

    fn call(&self, id: &str, pass: &str, ir: &SchemaIr) -> Result<SchemaIr, String> {
        let json = serde_json::to_vec(ir).map_err(|e| e.to_string())?;
        let mut out = Buffer { ptr: std::ptr::null_mut(), len: 0 };
        // SAFETY: the ranges are valid for the call and `out` is writable; the buffer is freed once, by its owner.
        let (status, text) = unsafe {
            let status = (self.run)(pass.as_ptr(), pass.len(), json.as_ptr(), json.len(), &mut out);
            let text = if out.ptr.is_null() { vec![] } else { std::slice::from_raw_parts(out.ptr, out.len).to_vec() };
            (self.free)(out.ptr, out.len);
            (status, text)
        };
        if status == STATUS_OK {
            return serde_json::from_slice(&text).map_err(|e| format!("extension {id}: pass {pass} returned an IR that does not parse: {e}"));
        }
        let text = String::from_utf8_lossy(&text);
        Err(if status == STATUS_PANIC { format!("extension {id}: pass {pass} panicked: {text}") } else { format!("extension {id}: pass {pass}: {text}") })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn install(dir: &Path, id: &str, body: &str) -> (String, PathBuf) {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(MANIFEST_FILE);
        std::fs::write(&path, format!("id = \"{id}\"\nversion = \"1.0.0\"\nschema_contract = \"2.0\"\nabi = \"0.1\"\nlibrary = \"none\"\nlanguages = [\"python\"]\ndatabases = [\"sqlite\"]\n{body}")).unwrap();
        (id.to_owned(), path)
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("orm-loader-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        dir
    }

    fn select(available: &[(String, PathBuf)], used: &BTreeSet<String>, named: &[String]) -> Result<Option<Plan>, String> {
        plan(available, used, named, &[], &[])
    }

    fn used(namespace: &str) -> BTreeSet<String> { [namespace.to_owned()].into() }

    const TAG: &str = "[[attributes]]\nname = \"acme.tag\"\ntarget = \"model\"\n";
    const KIT: &str = "[[attributes]]\nname = \"kit.x\"\ntarget = \"model\"\n";

    #[test]
    fn it_selects_an_extension_by_the_namespace_that_the_schema_uses() {
        let dir = scratch("select");
        let available = [install(&dir.join("a"), "acme.audit", TAG), install(&dir.join("b"), "other.kit", KIT)];
        let plan = select(&available, &used("acme"), &[]).unwrap().unwrap();
        assert_eq!(plan.manifests.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["acme.audit"]);
        assert!(select(&available, &BTreeSet::new(), &[]).unwrap().is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_generator_key_selects_an_extension_without_an_attribute() {
        let dir = scratch("named");
        let available = [install(&dir, "acme.audit", TAG)];
        assert!(select(&available, &BTreeSet::new(), &["acme.audit".into()]).unwrap().is_some());
        let error = select(&available, &BTreeSet::new(), &["acme.gone".into()]).err().unwrap();
        assert!(error.contains("acme.gone") && error.contains("not installed") && error.contains("--extension"), "{error}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_generator_key_may_name_an_extension_that_the_artifact_has() {
        let compiled: Manifest = toml::from_str("id = \"acme.compiled\"\nversion = \"1.0.0\"\nschema_contract = \"2.0\"\nhost_contract = \"1.0\"\nlanguages = []\ndatabases = []\n").unwrap();
        assert!(plan(&[], &BTreeSet::new(), &["acme.compiled".into()], &[compiled], &[]).unwrap().is_none());
    }

    #[test]
    fn two_installed_packages_with_one_id_name_both_paths() {
        let dir = scratch("twice");
        let available = [install(&dir.join("a"), "acme.audit", TAG), install(&dir.join("b"), "acme.audit", TAG)];
        let error = select(&available, &BTreeSet::new(), &[]).err().unwrap();
        assert!(error.contains("installed twice") && error.contains("/a/") && error.contains("/b/"), "{error}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_manifest_must_match_its_id_and_describe_a_library() {
        let dir = scratch("manifest");
        let (_, path) = install(&dir, "acme.audit", TAG);
        assert!(read_manifest("acme.other", &path).err().unwrap().contains("describes acme.audit"));
        std::fs::write(&path, "id = \"acme.audit\"\nversion = \"1\"\nschema_contract = 2\nhost_contract = 1\nlanguages = []\ndatabases = []\n").unwrap();
        assert!(check_library(&read_manifest("acme.audit", &path).unwrap()).err().unwrap().contains("no `abi`"));
        assert!(read_manifest("acme.audit", &dir.join("gone.toml")).err().unwrap().contains("gone.toml"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_wrong_id_fails_only_for_a_package_that_the_compile_selects() {
        let dir = scratch("id");
        let (_, path) = install(&dir, "acme.audit", TAG);
        let available = [("acme.other".to_owned(), path)];
        assert!(select(&available, &BTreeSet::new(), &[]).unwrap().is_none());
        for selected in [select(&available, &used("acme"), &[]), select(&available, &BTreeSet::new(), &["acme.other".into()])] {
            assert!(selected.err().unwrap().contains("describes acme.audit"));
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn two_selected_extensions_with_one_attribute_conflict() {
        let dir = scratch("conflict");
        let available = [install(&dir.join("a"), "acme.audit", TAG), install(&dir.join("b"), "acme.rival", TAG)];
        let error = select(&available, &used("acme"), &[]).err().unwrap();
        assert!(error.contains("acme.tag") && error.contains("acme.audit") && error.contains("acme.rival"), "{error}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_unrelated_package_that_is_no_library_never_fails_a_compile() {
        let dir = scratch("unrelated");
        let (_, other) = install(&dir.join("b"), "other.kit", KIT);
        std::fs::write(&other, format!("id = \"other.kit\"\nversion = \"1\"\nschema_contract = \"2.0\"\nhost_contract = \"1.0\"\nlanguages = []\ndatabases = []\n{KIT}")).unwrap();
        let available = [install(&dir.join("a"), "acme.audit", TAG), ("other.kit".to_owned(), other.clone())];
        assert!(select(&available, &used("acme"), &[]).unwrap().is_some());
        let error = select(&available, &used("kit"), &[]).err().unwrap();
        assert!(error.contains("no `abi`"), "{error}");
        let error = select(&available, &BTreeSet::new(), &["other.kit".into()]).err().unwrap();
        assert!(error.contains("no `abi`"), "{error}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_manifest_of_a_later_release_fails_only_for_a_package_that_the_compile_selects() {
        let dir = scratch("later");
        let (_, newer) = install(&dir.join("b"), "other.kit", &format!("future_field = true\n{KIT}"));
        std::fs::write(&newer, std::fs::read_to_string(&newer).unwrap().replace("schema_contract = \"2.0\"", "schema_contract = \"2.1\"")).unwrap();
        let available = [install(&dir.join("a"), "acme.audit", TAG), ("other.kit".to_owned(), newer)];
        assert!(select(&available, &used("acme"), &[]).unwrap().is_some());
        for selected in [select(&available, &used("kit"), &[]), select(&available, &BTreeSet::new(), &["other.kit".into()])] {
            let error = selected.err().unwrap();
            assert!(error.contains("other.kit") && error.contains("schema contract 2.1") && error.contains("newer than this engine") && error.contains("update the ORM"), "{error}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_unknown_field_without_a_newer_version_is_a_manifest_error() {
        let dir = scratch("unknown");
        let (id, path) = install(&dir, "acme.audit", &format!("future_field = true\n{TAG}"));
        let error = select(&[(id, path)], &used("acme"), &[]).err().unwrap();
        assert!(error.contains("future_field"), "{error}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_manifest_that_does_not_parse_is_named_only_when_no_other_extension_owns_the_namespace() {
        let dir = scratch("broken");
        let (_, broken) = install(&dir.join("b"), "other.kit", KIT);
        std::fs::write(&broken, "id = \"other.kit\"\nversion = [").unwrap();
        let available = [install(&dir.join("a"), "acme.audit", TAG), ("other.kit".to_owned(), broken.clone())];
        assert!(select(&available, &used("acme"), &[]).unwrap().is_some());
        assert!(select(&available, &BTreeSet::new(), &[]).unwrap().is_none());
        let error = select(&available, &used("kit"), &[]).err().unwrap();
        assert!(error.contains("namespace kit") && error.contains("do not parse") && error.contains(&broken.display().to_string()), "{error}");
        let error = select(&available, &BTreeSet::new(), &["other.kit".into()]).err().unwrap();
        assert!(error.contains(&broken.display().to_string()) && !error.contains("namespace"), "{error}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_library_file_names_the_path() {
        let dir = scratch("library");
        let (id, path) = install(&dir, "acme.audit", TAG);
        let plan = select(&[(id, path)], &BTreeSet::new(), &["acme.audit".into()]).unwrap().unwrap();
        let error = plan.load().err().unwrap();
        assert!(error.contains("acme.audit") && (error.contains("libnone") || error.contains("none.dll")), "{error}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn lowering_state_of_another_artifact_fails_before_a_library_loads() {
        let dir = scratch("state");
        let (id, path) = install(&dir, "acme.audit", TAG);
        let order = ["static.pass".to_owned()];
        let plan = plan(&[(id, path)], &BTreeSet::new(), &["acme.audit".into()], &[], &order).unwrap().unwrap();
        let lower = |completed: &[&str]| {
            let mut ir = SchemaIr::default();
            ir.behavior.completed_passes = completed.iter().map(|p| (*p).to_owned()).collect();
            plan.lower(&mut ir, None, &|_, _| Ok(false)).err().unwrap()
        };
        assert!(lower(&["other.pass"]).contains("incompatible lowering state"));
        for completed in [&[][..], &["static.pass"][..]] {
            let error = lower(completed);
            assert!(error.contains("libnone") || error.contains("none.dll"), "{error}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
