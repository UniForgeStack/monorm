//! Build frontend for fixed native composition. Cargo owns package resolution.
use orm_contracts::extension::{Manifest, Pass};
use std::collections::BTreeMap;

#[derive(Debug)]
pub struct Composition {
    pub manifests: Vec<Manifest>,
    pub passes: Vec<Pass>,
    /// Language sources of the manifests; the build fills it after `resolve`.
    pub languages: Vec<language::Embedded>,
}

impl Composition {
    pub fn resolve(manifests: Vec<Manifest>) -> Result<Self, String> {
        if let Some(m) = manifests.iter().find(|m| m.is_dynamic()) {
            return Err(format!("{}: a library cannot be compiled into an artifact; the loader runs it", m.id));
        }
        let resolved = orm_contracts::resolve::resolve(manifests, &language::validate_section)?;
        Ok(Self { manifests: resolved.manifests, passes: resolved.passes, languages: vec![] })
    }

    pub fn pass_ids(&self) -> Vec<&str> { self.passes.iter().map(|p| p.id.as_str()).collect() }

    /// The generated code uses calls checked by rustc, never an execution callback list.
    pub fn source(&self) -> Result<String, String> {
        self.source_with_native(None)
    }

    pub fn artifact(&self, native: Option<&native::Sources>) -> orm_contracts::extension::Artifact {
        let versions: BTreeMap<_, _> = self.manifests.iter().map(|m| (&m.id, &m.version)).collect();
        let artifact = orm_contracts::extension::Artifact {
            passes: self.pass_ids().into_iter().map(str::to_owned).collect(),
            capabilities: vec!["schema-transformations".into(), "physical-schema".into()],
            extensions: versions.iter().map(|(k,v)| ((*k).clone(), (*v).clone())).collect(),
            requires: self.manifests.iter().filter(|m| !m.requires.is_empty()).map(|m| (m.id.clone(), m.requires.clone())).collect(),
            exports: self.manifests.iter().flat_map(|m| m.exports.iter().map(|e| e.id.clone())).collect(),
            ..Default::default()
        };
        let mut artifact = artifact;
        artifact.language_members = self.languages.iter().map(|l| l.members.clone()).collect();
        if let Some(native) = native {
            artifact.specializations = native.specializations.clone();
            artifact.capabilities.extend(["native-string-values", "native-string-records", "native-string-results"].into_iter().map(str::to_owned));
        }
        artifact
    }

    pub fn source_with_native(&self, native: Option<&native::Sources>) -> Result<String, String> {
        let manifests = serde_json::to_string(&self.manifests).map_err(|e| e.to_string())?;
        let ids = serde_json::to_string(&self.pass_ids()).map_err(|e| e.to_string())?;
        let versions: BTreeMap<_, _> = self.manifests.iter().map(|m| (&m.id, &m.version)).collect();
        let artifact = self.artifact(native);
        let artifact = serde_json::to_string(&artifact).map_err(|e| e.to_string())?;
        let binding = if native.is_some() { "bind(ir)?;" } else { "" };
        let language = language::source(&self.languages)?;
        let rust_paths = self.passes.iter()
            .map(|p| p.rust.as_deref().ok_or_else(|| format!("pass {} has no `rust` path", p.id)))
            .collect::<Result<Vec<_>, _>>()?;
        let mut source = format!(r#"
use crate::ir::SchemaIr;
use crate::behavior::{{Artifact, Manifest, SCHEMA_CONTRACT, validate_declarations, validate_requirements, capture_storage}};
use std::sync::OnceLock;
// The host caches the artifact once per process.
pub fn artifact() -> Artifact {{ serde_json::from_str({artifact:?}).expect("generated artifact") }}
// Every schema preparation reads these constants; parse each once per process.
pub fn manifests() -> &'static [Manifest] {{ static M: OnceLock<Vec<Manifest>> = OnceLock::new(); M.get_or_init(|| serde_json::from_str({manifests:?}).expect("generated manifests")) }}
fn pass_ids() -> &'static [String] {{ static P: OnceLock<Vec<String>> = OnceLock::new(); P.get_or_init(|| serde_json::from_str({ids:?}).expect("generated pass IDs")) }}
pub fn prepare(ir: &mut SchemaIr, language: Option<&str>) -> Result<(), String> {{
    let manifests = manifests();
    // The stamp below would otherwise upgrade an IR built under an older contract.
    if ir.behavior.schema_contract != 0 && ir.behavior.schema_contract != SCHEMA_CONTRACT {{ return Err(format!("schema contract {{}} unavailable (artifact {{}}); rebuild native artifact", ir.behavior.schema_contract, SCHEMA_CONTRACT)); }}
    validate_declarations(ir, manifests, language)?;
    let expected = pass_ids();
    if !ir.behavior.completed_passes.is_empty() && ir.behavior.completed_passes != expected {{ return Err("incompatible lowering state; rebuild schema and native artifact".into()); }}
    if !ir.behavior.completed_passes.is_empty() && ir.behavior.declarations.iter().all(|d| d.lowered) && ir.models.iter().all(|m| ir.behavior.lowered_models.contains(&m.name)) {{
        {binding}
        validate_requirements(ir, manifests, language)?;
        return Ok(());
    }}
"#);
        if !self.passes.is_empty() {
            source.push_str("    capture_storage(ir)?;\n    let previous: Vec<_> = ir.behavior.declarations.iter().filter(|d| d.lowered).cloned().collect();\n    ir.behavior.declarations.retain(|d| !d.lowered);\n    ir.behavior.completed_passes.clear();\n");
        }
        for (p, rust) in self.passes.iter().zip(&rust_paths) {
            source.push_str(&format!("    let () = {}(ir)?;\n    ir.behavior.completed_passes.push({:?}.into());\n", rust, p.id));
        }
        if !self.passes.is_empty() {
            source.push_str("    for declaration in &mut ir.behavior.declarations { declaration.lowered = true; }\n    ir.behavior.declarations.extend(previous);\n");
        }
        source.push_str(&format!(r#"    {binding}
    ir.behavior.lowered_models = ir.models.iter().map(|m| m.name.clone()).collect();
    if !ir.behavior.is_empty() {{
        ir.behavior.schema_contract = SCHEMA_CONTRACT;
        static VERSIONS: OnceLock<std::collections::BTreeMap<String, String>> = OnceLock::new();
        ir.behavior.extensions = VERSIONS.get_or_init(|| serde_json::from_str({:?}).expect("generated versions")).clone();
    }}
    ir.stamp_expression_records();
    validate_requirements(ir, manifests, language)?;
    Ok(())
}}
"#, serde_json::to_string(&versions).map_err(|e| e.to_string())?));
        source.push_str("pub fn run_pass(_ir: &mut SchemaIr, id: &str) -> Result<bool, String> {\n    match id {\n");
        for (p, rust) in self.passes.iter().zip(&rust_paths) {
            source.push_str(&format!("        {:?} => {{ let () = {}(_ir)?; Ok(true) }}\n", p.id, rust));
        }
        source.push_str("        _ => Ok(false),\n    }\n}\n");
        source.push_str(&language);
        if let Some(native) = native {
            source.push_str(&native.core);
        } else {
            source.push_str("#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum NativeModel { None }\nimpl NativeModel { pub fn computed(self) -> &'static [usize] { &[] } pub fn static_hooks(self) -> Option<Box<crate::hooks::HookPlan>> { None } pub fn dependency(self, _field: usize) -> Option<usize> { None } }\npub fn bind(ir: &mut SchemaIr) -> Result<Vec<NativeModel>, String> { Ok(vec![NativeModel::None; ir.models.len()]) }\n");
        }
        Ok(source)
    }
}

pub use orm_contracts::resolve::rust_path;

pub mod language;

pub mod native;

pub mod inputs;
