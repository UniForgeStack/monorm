//! Field adapter records resolved to field positions for the code generators.
//! The adapter id, its runtime and its options stay opaque here: the manifest of the extension names the runtime.
use crate::ir::SchemaIr;

#[derive(Clone, Debug)]
pub struct PreparedAdapter {
    pub position: usize,
    pub adapter: String,
    pub options: serde_json::Value,
    pub nullable: bool,
}

/// The adapters of each model, in model order.
pub fn prepare(ir: &SchemaIr) -> Result<Vec<Vec<PreparedAdapter>>, String> {
    crate::behavior::validate_field_adapters(ir)?;
    crate::file_storage::check(ir)?;
    let mut prepared = vec![vec![]; ir.models.len()];
    for record in &ir.behavior.field_adapters {
        let model = ir.models.iter().position(|m| m.name == record.model)
            .ok_or_else(|| format!("unknown field adapter model {}", record.model))?;
        let position = ir.models[model].fields.iter().position(|f| f.name == record.field)
            .ok_or_else(|| format!("{}.{}: unknown adapter field", record.model, record.field))?;
        prepared[model].push(PreparedAdapter {
            position, adapter: record.adapter.clone(), options: record.options.clone(), nullable: ir.models[model].fields[position].nullable,
        });
    }
    Ok(prepared)
}
