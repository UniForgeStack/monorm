//! Behavioral extension setup. Database catalogs remain in `ext`.
pub use orm_contracts::extension::*;

#[cfg(feature = "composition")]
mod composition { include!(env!("ORM_CORE_COMPOSITION")); }

/// Every schema preparation checks the artifact; build it once per process.
pub fn artifact() -> &'static Artifact {
    static ARTIFACT: std::sync::OnceLock<Artifact> = std::sync::OnceLock::new();
    ARTIFACT.get_or_init(build_artifact)
}

fn build_artifact() -> Artifact {
    #[cfg(feature = "composition")]
    let mut artifact = composition::artifact();
    #[cfg(not(feature = "composition"))]
    let mut artifact = Artifact::default();
    let compiled = [
        (cfg!(feature = "reference-loading"), "reference-loading"),
        (cfg!(feature = "model-composition"), "model-composition"),
        (cfg!(feature = "proxy-models"), "proxy-models"),
        (cfg!(feature = "generic-relations"), "generic-relations"),
        (cfg!(feature = "query-defaults"), QUERY_DEFAULTS),
    ];
    for (_, capability) in compiled.into_iter().filter(|(on, _)| *on) {
        if !artifact.capabilities.iter().any(|c| c == capability) { artifact.capabilities.push(capability.into()); }
    }
    // The loader is in every build, and a library pass can record storage.
    if !artifact.capabilities.iter().any(|c| c == "physical-schema") { artifact.capabilities.push("physical-schema".into()); }
    artifact
}

/// The manifests compiled into this artifact; none without composition.
pub fn manifests() -> &'static [Manifest] {
    #[cfg(feature = "composition")]
    { composition::manifests() }
    #[cfg(not(feature = "composition"))]
    { &[] }
}

/// The language code of each extension of this artifact for `language`: dependencies first,
/// so a later extension's overrides wrap the earlier ones.
pub fn extension_code(language: &str) -> Vec<crate::codegen::ExtensionCode> {
    let manifests = manifests();
    let mut order: Vec<&Manifest> = Vec::new();
    fn visit<'a>(all: &'a [Manifest], m: &'a Manifest, order: &mut Vec<&'a Manifest>) {
        if order.iter().any(|o| o.id == m.id) { return; }
        for dep in m.dependencies.keys() {
            if let Some(d) = all.iter().find(|o| &o.id == dep) { visit(all, d, order); }
        }
        order.push(m);
    }
    let mut sorted: Vec<&Manifest> = manifests.iter().collect();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));
    for m in sorted { visit(manifests, m, &mut order); }
    #[cfg(feature = "composition")]
    let sources = composition::language_sources();
    #[cfg(not(feature = "composition"))]
    let sources: &[(&str, &str, &str, &str)] = &[];
    order.into_iter().filter_map(|m| {
        let members = artifact().language_members.iter().find(|r| r.extension == m.id && r.language == language)?.clone();
        let files = sources.iter().filter(|(e, l, _, _)| *e == m.id && *l == language).map(|(_, _, f, c)| (f.to_string(), c.to_string())).collect();
        Some(crate::codegen::ExtensionCode { members, attributes: m.attributes.iter().map(|a| a.name.clone()).collect(), files })
    }).collect()
}

pub fn prepare(ir: &mut crate::ir::SchemaIr, language: Option<&str>) -> Result<(), String> {
    prepare_with(ir, language, None)
}

/// [`prepare`] with the plan of the installed extension libraries that the schema selected.
/// The plan runs their passes and the compiled passes in one order.
pub fn prepare_with(ir: &mut crate::ir::SchemaIr, language: Option<&str>, plan: Option<&crate::loader::Plan>) -> Result<(), String> {
    if ir.behavior.storage.is_none() && (!ir.behavior.field_storage.is_empty() || !ir.behavior.owner_links.is_empty()) { return Err("storage ownership contributions require an explicit physical schema".into()); }
    if let Some(plan) = plan {
        plan.lower(ir, language, &run_static_pass)?;
        #[cfg(feature = "composition")]
        { composition::bind(ir)?; }
    } else {
        #[cfg(feature = "composition")]
        { composition::prepare(ir, language)?; }
        #[cfg(not(feature = "composition"))]
        { validate_declarations(ir, &[], language)?; }
    }
    check_requirements(ir, artifact())
}

/// Runs the compiled pass `id`; false when this artifact has no such pass.
fn run_static_pass(ir: &mut crate::ir::SchemaIr, id: &str) -> Result<bool, String> {
    #[cfg(feature = "composition")]
    { composition::run_pass(ir, id) }
    #[cfg(not(feature = "composition"))]
    { let _ = (ir, id); Ok(false) }
}

#[cfg(feature = "composition")]
pub use composition::{bind, NativeModel};
