//! Extensions that ship their passes as native libraries: select, check, load and run them.
//! It names no extension. The passes of libraries and the passes compiled into the artifact run in one order.
use sha2::{Digest, Sha256};
use orm_contracts::abi::{owns_bytes, AbiVersionFn, Arg, Buffer, Context, DropStateFn, FreeFn, HookFn, HooksFn, InitFn, OrmArg, OrmContext, OrmValue, RunPassFn, Value, STATUS_ERROR, STATUS_OK, STATUS_PANIC, SYMBOL_ABI_VERSION, SYMBOL_DROP_STATE, SYMBOL_FREE, SYMBOL_HOOKS, SYMBOL_INIT, SYMBOL_RUN_PASS};
use orm_contracts::extension::{attribute_namespace, PackageStamp, ABI_VERSION, SCHEMA_CONTRACT, SCHEMA_CONTRACT_VERSION};
use orm_contracts::ir::SchemaIr;
use orm_contracts::version::ContractVersion;
use orm_contracts::resolve::resolve;
use crate::behavior::{capture_storage, validate_declarations, validate_requirements, Manifest, Pass};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::ffi::c_void;
use std::sync::{Arc, Mutex, OnceLock};

/// The file that describes an extension package, beside its library.
pub const MANIFEST_FILE: &str = "orm-extension.toml";

/// A native library after the ABI check. The library stays loaded for the life of the process.
struct Native {
    library: libloading::Library,
    run: RunPassFn,
    free: FreeFn,
    abi: ContractVersion,
    /// The optional symbols of ABI 0.2; resolved the first time a manifest that declares `exports` opens the library.
    symbols: OnceLock<HookSymbols>,
}

/// The `init`, `drop_state` and hook table of a library, not the `exports` of a manifest.
struct HookSymbols {
    init: Option<InitFn>,
    drop_state: Option<DropStateFn>,
    hooks: Vec<(String, HookFn)>,
}

impl HookSymbols {
    fn find(&self, name: &str) -> Option<HookFn> { self.hooks.iter().find(|(n, _)| n == name).map(|(_, f)| *f) }
}

/// The error for a library that lacks `symbol` (a NUL-terminated name); `why` is empty or ends the clause that says why the library needs it.
fn no_symbol(id: &str, path: &Path, symbol: &[u8], why: &str) -> String {
    format!("extension {id}: {} has no `{}`{why}; build it with orm_contracts::extension!", path.display(), String::from_utf8_lossy(&symbol[..symbol.len() - 1]))
}

/// The error of a pass, an `init` or a hook (`what`) that ended with `status` and `text`.
fn call_error(id: &str, what: &str, status: i32, text: &str) -> String {
    if status == STATUS_PANIC { format!("extension {id}: {what} panicked: {text}") } else { format!("extension {id}: {what}: {text}") }
}

/// The error for an extension whose manifest declares no `exports`: it has no hooks to open.
fn no_exports(id: &str) -> String { format!("extension {id}: the manifest declares no exports") }

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
        ir.behavior.stamp_minor();
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
        native.resolve_hooks(d, &path)?;
        return Ok(native.clone());
    }
    // SAFETY: loading a library runs its initializers; the user chose to install this extension.
    let library = unsafe { libloading::Library::new(&path) }.map_err(|e| format!("extension {id}: cannot load {}: {e}", path.display()))?;
    // SAFETY: the symbol types are the ABI of orm_contracts::abi; the ABI version is checked below before any other call.
    let (abi, run, free) = unsafe {
        let abi = *library.get::<AbiVersionFn>(SYMBOL_ABI_VERSION).map_err(|_| no_symbol(id, &path, SYMBOL_ABI_VERSION, ""))?;
        let run = *library.get::<RunPassFn>(SYMBOL_RUN_PASS).map_err(|_| no_symbol(id, &path, SYMBOL_RUN_PASS, ""))?;
        let free = *library.get::<FreeFn>(SYMBOL_FREE).map_err(|_| no_symbol(id, &path, SYMBOL_FREE, ""))?;
        (abi, run, free)
    };
    // SAFETY: `abi` takes no argument and reads nothing.
    let reported = ContractVersion::from_bits(unsafe { abi() });
    reported.check("ABI", ABI_VERSION, id)?;
    let native = Arc::new(Native { library, run, free, abi: reported, symbols: OnceLock::new() });
    native.check_manifest(d)?;
    native.resolve_hooks(d, &path)?;
    cache.insert(path, native.clone());
    Ok(native)
}

impl Native {
    /// For a manifest that declares `exports`: resolves `orm_ext_init`, `orm_ext_drop_state` and `orm_ext_hooks` once,
    /// then checks on every open that the hook table has a hook for each export. A library that a cache hit returns is checked again.
    fn resolve_hooks(&self, d: &Dynamic, path: &Path) -> Result<(), String> {
        if d.manifest.exports.is_empty() { return Ok(()); }
        let id = &d.manifest.id;
        let symbols = match self.symbols.get() {
            Some(symbols) => symbols,
            None => { let read = self.read_symbols(id, path)?; self.symbols.get_or_init(|| read) }
        };
        match d.manifest.exports.iter().find(|e| symbols.find(&e.id).is_none()) {
            Some(export) => Err(format!("extension {id}: the manifest declares the export {}, but the hook table of {} has no hook of that name; fix the export id or add the hook to `extension!`", export.id, path.display())),
            None => Ok(()),
        }
    }

    fn read_symbols(&self, id: &str, path: &Path) -> Result<HookSymbols, String> {
        let missing = |symbol: &[u8]| no_symbol(id, path, symbol, ", and the manifest declares exports");
        // SAFETY: the symbol types are the ABI of orm_contracts::abi; the ABI version was checked before this call.
        let (init, drop_state, table) = unsafe {
            let init = self.library.get::<InitFn>(SYMBOL_INIT).ok().map(|s| *s);
            let drop_state = self.library.get::<DropStateFn>(SYMBOL_DROP_STATE).ok().map(|s| *s);
            let hooks = *self.library.get::<HooksFn>(SYMBOL_HOOKS).map_err(|_| missing(SYMBOL_HOOKS))?;
            (init, drop_state, hooks())
        };
        if init.is_some() && drop_state.is_none() { return Err(missing(SYMBOL_DROP_STATE)); }
        let mut hooks = vec![];
        if !table.is_null() {
            // SAFETY: the table and its entries are immutable statics of the library, which stays loaded.
            let table = unsafe { &*table };
            let entries = if table.len == 0 { &[][..] } else { unsafe { std::slice::from_raw_parts(table.entries, table.len) } };
            for entry in entries {
                // SAFETY: the name is a static string of the library.
                let name = unsafe { std::slice::from_raw_parts(entry.name_ptr, entry.name_len) };
                let name = std::str::from_utf8(name).map_err(|_| format!("extension {id}: a hook name is not UTF-8"))?;
                hooks.push((name.to_owned(), entry.call));
            }
        }
        Ok(HookSymbols { init, drop_state, hooks })
    }

    /// The symbols of a library that a manifest with `exports` opened.
    fn symbols(&self, id: &str) -> Result<&HookSymbols, String> {
        self.symbols.get().ok_or_else(|| no_exports(id))
    }

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
        Err(call_error(id, &format!("pass {pass}"), status, &String::from_utf8_lossy(&text)))
    }
}

/// An opened library of a manifest that declares `exports`: its hooks and its state.
#[derive(Clone)]
pub struct Library { native: Arc<Native>, id: String, exports: Vec<orm_contracts::extension::Export> }

/// The state that `orm_ext_init` returned. Dropping it calls `orm_ext_drop_state` once.
pub struct State { native: Arc<Native>, id: String, raw: *mut c_void }

/// A hook that `State::hook` resolved by name. It holds the library, so the pointer stays valid, and only the states of that library call it.
#[derive(Clone)]
pub struct Hook { native: Arc<Native>, call: HookFn, name: String }

// SAFETY: a state is `Send + Sync` by the ABI (see `orm_contracts::abi`): the host may call the hooks of one state
// from several threads at once and drop it on another thread. `raw` is the only field that is not `Send + Sync` by itself.
unsafe impl Send for State {}
// SAFETY: see `Send`.
unsafe impl Sync for State {}

impl Plan {
    /// Opens the library of the selected extension `id`. A manifest without `exports` has no hooks to open.
    pub fn library(&self, id: &str) -> Result<Library, String> {
        let d = self.dynamic.iter().find(|d| d.manifest.id == id).ok_or_else(|| format!("extension {id} is not a selected library"))?;
        if d.manifest.exports.is_empty() { return Err(no_exports(id)); }
        Ok(Library { native: open(d)?, id: id.to_owned(), exports: d.manifest.exports.clone() })
    }
}

/// Opens the library of the package installed in `dir`. `define()` calls this for each package that has hook bindings.
/// It reads the manifest and checks the library against it (`check_library`, then the ABI and the hook table in `open`).
/// `hooks::bind` then checks each bound export.
pub fn open_package(id: &str, dir: &Path) -> Result<Library, String> {
    let d = read_manifest(id, &dir.join(MANIFEST_FILE))?;
    check_library(&d)?;
    if d.manifest.exports.is_empty() { return Err(no_exports(id)); }
    Ok(Library { native: open(&d)?, id: id.to_owned(), exports: d.manifest.exports.clone() })
}

impl Library {
    /// The manifest entry of the export `id`.
    pub fn export(&self, id: &str) -> Option<&orm_contracts::extension::Export> { self.exports.iter().find(|e| e.id == id) }

    /// Calls `orm_ext_init` with the JSON of the `extension_data` entry. A library without `init` gets no state.
    pub fn init(&self, data: &serde_json::Value) -> Result<State, String> {
        let symbols = self.native.symbols(&self.id)?;
        let mut raw: *mut c_void = std::ptr::null_mut();
        if let Some(init) = symbols.init {
            let json = serde_json::to_vec(data).map_err(|e| e.to_string())?;
            let mut error = Buffer { ptr: std::ptr::null_mut(), len: 0 };
            // SAFETY: the range and the out pointers are valid for the call; the buffer is freed once, by its owner.
            let (status, text) = unsafe {
                let status = init(json.as_ptr(), json.len(), &mut raw, &mut error);
                let text = if error.ptr.is_null() { vec![] } else { std::slice::from_raw_parts(error.ptr, error.len).to_vec() };
                (self.native.free)(error.ptr, error.len);
                (status, text)
            };
            if status != STATUS_OK { return Err(call_error(&self.id, "init", status, &String::from_utf8_lossy(&text))); }
        }
        Ok(State { native: self.native.clone(), id: self.id.clone(), raw })
    }
}

impl State {
    /// Looks up the hook `name` of the library once, so that a write does not search the table again.
    pub fn hook(&self, name: &str) -> Option<Hook> {
        let call = self.native.symbols(&self.id).ok()?.find(name)?;
        Some(Hook { native: self.native.clone(), call, name: name.to_owned() })
    }

    /// Calls the hook `name` with the arguments and the request context.
    pub fn call(&self, name: &str, args: &[Arg], context: &Context) -> Result<Value, String> {
        let hook = self.hook(name).ok_or_else(|| format!("extension {}: no hook {name}", self.id))?;
        self.call_hook(&hook, args, context)
    }

    /// Calls a resolved hook. A hook that fails with its own text gives that text and nothing else: the caller names the place.
    pub fn call_hook(&self, hook: &Hook, args: &[Arg], context: &Context) -> Result<Value, String> {
        if !Arc::ptr_eq(&self.native, &hook.native) {
            return Err(format!("extension {}: the hook {} belongs to another library", self.id, hook.name));
        }
        let raw_args: Vec<OrmArg> = args.iter().map(Arg::borrow).collect();
        let raw_context: Vec<OrmArg> = context.values.iter().map(Arg::borrow).collect();
        let ctx = OrmContext { args: raw_context.as_ptr(), len: raw_context.len() };
        let mut out = OrmValue::NULL;
        // SAFETY: the arguments live through the call and `out` is writable; the state is the one that `init` returned.
        let status = unsafe { (hook.call)(self.raw, raw_args.as_ptr(), raw_args.len(), &ctx, &mut out) };
        // SAFETY: the library wrote `out` on every status. It is copied here; only a tag that `owns_bytes` holds a buffer, and it is freed once.
        let value = unsafe {
            let value = Value::from_raw(&out);
            if owns_bytes(out.tag) { (self.native.free)(out.ptr as *mut u8, out.len); }
            value
        };
        let what = || format!("hook {}", hook.name);
        match (status, value) {
            (STATUS_OK, Ok(value)) => Ok(value),
            (STATUS_OK, Err(e)) => Err(format!("extension {}: {} returned a value that does not read: {e}", self.id, what())),
            (STATUS_ERROR, Ok(Value::Text(text))) => Err(text),
            (status, value) => {
                let text = match value {
                    Ok(Value::Text(text)) => text,
                    Ok(other) => format!("{other:?}"),
                    Err(e) => format!("the value that came with status {status} does not read: {e}"),
                };
                Err(call_error(&self.id, &what(), status, &text))
            }
        }
    }
}

impl Drop for State {
    fn drop(&mut self) {
        if self.raw.is_null() { return; }
        if let Some(drop_state) = self.native.symbols.get().and_then(|s| s.drop_state) {
            // SAFETY: `raw` is the state that `init` of this library returned, and it drops once.
            unsafe { drop_state(self.raw) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orm_contracts::abi::TAG_INT;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn install(dir: &Path, id: &str, body: &str) -> (String, PathBuf) {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(MANIFEST_FILE);
        std::fs::write(&path, format!("id = \"{id}\"\nversion = \"1.0.0\"\nschema_contract = \"3.0\"\nabi = \"0.2\"\nlibrary = \"none\"\nlanguages = [\"python\"]\ndatabases = [\"sqlite\"]\n{body}")).unwrap();
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

    fn this_library() -> libloading::Library {
        #[cfg(unix)]
        { libloading::os::unix::Library::this().into() }
        #[cfg(windows)]
        { libloading::os::windows::Library::this().expect("the process has a module handle").into() }
    }

    unsafe extern "C" fn no_run(_: *const u8, _: usize, _: *const u8, _: usize, _: *mut Buffer) -> i32 { STATUS_OK }

    unsafe extern "C" fn real_free(ptr: *mut u8, len: usize) {
        // SAFETY: the tests give it only the buffers that `Value::leak` made.
        unsafe { orm_contracts::abi::free(ptr, len) }
    }

    /// A hook that writes `$value` to `out` and returns `$status`.
    macro_rules! hook {
        ($status:expr, $value:expr) => {{
            unsafe extern "C" fn hook(_: *const c_void, _: *const OrmArg, _: usize, _: *const OrmContext, out: *mut OrmValue) -> i32 {
                // SAFETY: `State::call` passes a writable `out`.
                unsafe { *out = $value };
                $status
            }
            hook as HookFn
        }};
    }

    /// A library that is no file: its `free` and its hooks are functions of the test binary.
    fn fake(free: FreeFn, hooks: &[(&str, HookFn)]) -> Arc<Native> {
        let hooks = hooks.iter().map(|(name, hook)| ((*name).to_owned(), *hook)).collect();
        Arc::new(Native { library: this_library(), run: no_run, free, abi: ABI_VERSION, symbols: OnceLock::from(HookSymbols { init: None, drop_state: None, hooks }) })
    }

    fn call(native: &Arc<Native>, name: &str) -> Result<Value, String> {
        State { native: native.clone(), id: "x".into(), raw: std::ptr::null_mut() }.call(name, &[], &Context::default())
    }

    #[test]
    fn a_scalar_value_holds_no_buffer_to_free() {
        static FREED: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn free(_: *mut u8, _: usize) { FREED.fetch_add(1, Ordering::SeqCst); }
        // A library may leave any bytes in the `ptr` and `len` of a scalar.
        let native = fake(free, &[("x.int", hook!(STATUS_OK, OrmValue { tag: TAG_INT, int: 7, float: 0.0, ptr: 0x10 as *const u8, len: 99 }))]);
        assert_eq!(call(&native, "x.int"), Ok(Value::Int(7)));
        assert_eq!(FREED.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_text_value_is_freed_once_with_its_own_range() {
        static FREED: Mutex<Vec<usize>> = Mutex::new(Vec::new());
        unsafe extern "C" fn free(ptr: *mut u8, len: usize) {
            FREED.lock().unwrap().push(len);
            // SAFETY: `Value::leak` made the buffer.
            unsafe { orm_contracts::abi::free(ptr, len) }
        }
        let native = fake(free, &[("x.text", hook!(STATUS_OK, Value::Text("hello".into()).leak()))]);
        assert_eq!(call(&native, "x.text"), Ok(Value::Text("hello".into())));
        assert_eq!(*FREED.lock().unwrap(), [5]);
    }

    #[test]
    fn a_failed_hook_keeps_its_name_and_its_status_when_its_value_does_not_read() {
        let native = fake(real_free, &[
            ("x.error", hook!(STATUS_ERROR, OrmValue { tag: 99, ..OrmValue::NULL })),
            ("x.panic", hook!(STATUS_PANIC, OrmValue { tag: 99, ..OrmValue::NULL })),
            ("x.ok", hook!(STATUS_OK, OrmValue { tag: 99, ..OrmValue::NULL })),
            ("x.boom", hook!(STATUS_ERROR, Value::Text("boom".into()).leak())),
            ("x.crash", hook!(STATUS_PANIC, Value::Text("boom".into()).leak())),
        ]);
        let reason = "the value that came with status";
        assert_eq!(call(&native, "x.error").unwrap_err(), format!("extension x: hook x.error: {reason} 1 does not read: unknown value tag 99"));
        assert_eq!(call(&native, "x.panic").unwrap_err(), format!("extension x: hook x.panic panicked: {reason} 2 does not read: unknown value tag 99"));
        assert_eq!(call(&native, "x.ok").unwrap_err(), "extension x: hook x.ok returned a value that does not read: unknown value tag 99");
        assert_eq!(call(&native, "x.boom").unwrap_err(), "boom");
        assert_eq!(call(&native, "x.crash").unwrap_err(), "extension x: hook x.crash panicked: boom");
        assert_eq!(call(&native, "x.gone").unwrap_err(), "extension x: no hook x.gone");
    }

    #[test]
    fn a_resolved_hook_runs_only_on_a_state_of_its_own_library() {
        let first = fake(real_free, &[("x.ok", hook!(STATUS_OK, Value::Text("one".into()).leak()))]);
        let second = fake(real_free, &[("x.ok", hook!(STATUS_OK, Value::Text("two".into()).leak()))]);
        let state = |native: &Arc<Native>| State { native: native.clone(), id: "x".into(), raw: std::ptr::null_mut() };
        let hook = state(&first).hook("x.ok").expect("the table has the hook");
        assert!(state(&first).hook("x.gone").is_none());
        assert_eq!(state(&first).call_hook(&hook, &[], &Context::default()), Ok(Value::Text("one".into())));
        let error = state(&second).call_hook(&hook, &[], &Context::default()).unwrap_err();
        assert_eq!(error, "extension x: the hook x.ok belongs to another library");
    }

    #[test]
    fn a_state_may_cross_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<State>();
        assert_send_sync::<Library>();
    }

    fn manifest_with_exports(dir: &Path, ids: &[&str]) -> Dynamic {
        let exports: String = ids.iter().map(|id| format!("[[exports]]\nid = \"{id}\"\npoint = 1\ninput = \"string\"\noutput = \"string\"\n")).collect();
        let (id, path) = install(dir, "acme.audit", &exports);
        read_manifest(&id, &path).unwrap()
    }

    #[test]
    fn the_hook_table_has_a_hook_for_each_export_of_the_manifest() {
        let dir = scratch("table");
        let native = fake(real_free, &[("a.one", hook!(STATUS_OK, OrmValue::NULL))]);
        let library = Path::new("libacme.so");
        assert_eq!(native.resolve_hooks(&manifest_with_exports(&dir, &["a.one"]), library), Ok(()));
        // The symbols are resolved already, as on the second open of a library: the check runs again.
        let error = native.resolve_hooks(&manifest_with_exports(&dir, &["a.one", "a.gone"]), library).unwrap_err();
        assert!(error.contains("extension acme.audit") && error.contains("the export a.gone") && error.contains("libacme.so") && error.contains("no hook of that name"), "{error}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_manifest_without_exports_has_no_library_to_open() {
        let dir = scratch("noexports");
        let (id, path) = install(&dir, "acme.audit", TAG);
        let plan = select(&[(id, path)], &BTreeSet::new(), &["acme.audit".into()]).unwrap().unwrap();
        // The library `none` does not exist: the refusal comes before any load.
        assert_eq!(plan.library("acme.audit").err().unwrap(), "extension acme.audit: the manifest declares no exports");
        assert_eq!(plan.library("acme.gone").err().unwrap(), "extension acme.gone is not a selected library");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_export_cannot_declare_bytes_as_its_type() {
        // The value of a hook may be bytes at run time, but a column type has no bytes: the manifest refuses it.
        let dir = scratch("bytes");
        let (id, path) = install(&dir, "acme.audit", "[[exports]]\nid = \"a.bytes\"\npoint = 1\ninput = \"bytes\"\noutput = \"string\"\n");
        let error = read_manifest(&id, &path).err().unwrap();
        assert!(error.starts_with("extension acme.audit: ") && error.contains("input = \"bytes\"") && error.contains("unknown variant `bytes`"), "{error}");
        std::fs::remove_dir_all(&dir).ok();
    }

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
        let compiled: Manifest = toml::from_str("id = \"acme.compiled\"\nversion = \"1.0.0\"\nschema_contract = \"3.0\"\nhost_contract = \"1.0\"\nlanguages = []\ndatabases = []\n").unwrap();
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
        std::fs::write(&path, "id = \"acme.audit\"\nversion = \"1\"\nschema_contract = 3\nhost_contract = 1\nlanguages = []\ndatabases = []\n").unwrap();
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
        std::fs::write(&other, format!("id = \"other.kit\"\nversion = \"1\"\nschema_contract = \"3.0\"\nhost_contract = \"1.0\"\nlanguages = []\ndatabases = []\n{KIT}")).unwrap();
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
        // The later release adds a key to the manifest, or to one of its attributes.
        for (name, extra) in [("later", format!("future_field = true\n{KIT}")), ("later-attribute", format!("{KIT}field_type = \"Money\"\nfuture_key = true\n"))] {
            let dir = scratch(name);
            let (_, newer) = install(&dir.join("b"), "other.kit", &extra);
            std::fs::write(&newer, std::fs::read_to_string(&newer).unwrap().replace("schema_contract = \"3.0\"", "schema_contract = \"3.3\"")).unwrap();
            let available = [install(&dir.join("a"), "acme.audit", TAG), ("other.kit".to_owned(), newer)];
            assert!(select(&available, &used("acme"), &[]).unwrap().is_some());
            for selected in [select(&available, &used("kit"), &[]), select(&available, &BTreeSet::new(), &["other.kit".into()])] {
                let error = selected.err().unwrap();
                assert!(error.contains("other.kit") && error.contains("schema contract 3.3") && error.contains("schema contract 3.2") && error.contains("newer than this engine") && error.contains("update the ORM"), "{error}");
            }
            std::fs::remove_dir_all(&dir).ok();
        }
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
