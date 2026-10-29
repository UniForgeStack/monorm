//! Behavioral extension setup. Database catalogs remain in `ext`.
pub use monorm::extension::*;

/// Every schema preparation checks the artifact; build it once per process.
pub fn artifact() -> &'static Artifact {
    static ARTIFACT: std::sync::OnceLock<Artifact> = std::sync::OnceLock::new();
    ARTIFACT.get_or_init(build_artifact)
}

fn build_artifact() -> Artifact {
    let mut artifact = Artifact::default();
    let compiled = [
        (cfg!(feature = "reference-loading"), "reference-loading"),
    ];
    for (_, capability) in compiled.into_iter().filter(|(on, _)| *on) {
        if !artifact.capabilities.iter().any(|c| c == capability) { artifact.capabilities.push(capability.into()); }
    }
    // The loader is in every build, and a library pass can record storage.
    if !artifact.capabilities.iter().any(|c| c == "physical-schema") { artifact.capabilities.push("physical-schema".into()); }
    artifact
}

/// The manifests compiled into this artifact; none.
pub fn manifests() -> &'static [Manifest] { &[] }

pub fn prepare(ir: &mut crate::ir::SchemaIr, language: Option<&str>) -> Result<(), String> {
    prepare_with(ir, language, None)
}

/// [`prepare`] with the plan of the installed extension libraries that the schema selected.
/// The plan runs their passes and the compiled passes in one order.
pub fn prepare_with(ir: &mut crate::ir::SchemaIr, language: Option<&str>, plan: Option<&crate::loader::Plan>) -> Result<(), String> {
    if ir.behavior.storage.is_none() && (!ir.behavior.field_storage.is_empty() || !ir.behavior.owner_links.is_empty()) { return Err("storage ownership contributions require an explicit physical schema".into()); }
    if let Some(plan) = plan {
        plan.lower(ir, language, &run_static_pass)?;
    } else {
        validate_declarations(ir, &[], language)?;
    }
    validate_requirements(ir, manifests(), language)?;
    check_requirements(ir, artifact())
}

/// Runs the compiled pass `id`; false when this artifact has no such pass.
fn run_static_pass(ir: &mut crate::ir::SchemaIr, id: &str) -> Result<bool, String> {
    let _ = (ir, id);
    Ok(false)
}
