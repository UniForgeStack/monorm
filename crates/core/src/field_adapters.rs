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

#[cfg(test)]
mod tests {
    use crate::behavior::{FieldAdapter, SCHEMA_CONTRACT};

    #[test]
    fn definition_resolves_positions_and_options() {
        let mut ir = crate::dsl::compile("model Report {\n id Int @id\n file Json?\n}", None).unwrap();
        ir.behavior.schema_contract = SCHEMA_CONTRACT;
        ir.behavior.field_adapters.push(FieldAdapter { model: "Report".into(), field: "file".into(), adapter: "acme.reference.v1".into(), options: serde_json::json!({"storage": "reports"}) });
        let schema = crate::schema::Schema::from_ir(ir).unwrap();
        let adapter = schema.models[0].adapter_for("file").unwrap();
        assert_eq!(adapter.position, 1);
        assert_eq!(adapter.options, serde_json::json!({"storage": "reports"}));
        assert!(schema.models[0].adapter_for("id").is_none());
    }
}
