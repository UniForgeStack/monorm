//! The key and default rule for the file fields that the file-storage extension declares.
//! The reference codec and the upload belong to the language packages.
use crate::ir::SchemaIr;

/// A file field is a value, not a key: no key flag and no database default.
pub fn check(ir: &SchemaIr) -> Result<(), String> {
    for file in &ir.behavior.file_fields {
        let field = ir.models.iter().find(|m| m.name == file.model)
            .ok_or_else(|| format!("unknown file model {}", file.model))?
            .fields.iter().find(|f| f.name == file.field)
            .ok_or_else(|| format!("unknown file field {}", file.field))?;
        if field.primary_key || field.default.is_some() || field.default_now
            || field.default_sql.is_some() || field.auto_increment {
            return Err(format!("{}.{}: file fields cannot be keys or have defaults", file.model, file.field));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::behavior::{FieldAdapter, FileField, FILE_REFERENCE_ADAPTER, SCHEMA_CONTRACT};

    fn schema() -> SchemaIr {
        let mut ir = crate::dsl::compile("model Report {\n id Int @id\n file Json?\n}", None).unwrap();
        ir.behavior.schema_contract = SCHEMA_CONTRACT;
        ir.behavior.field_adapters.push(FieldAdapter { model: "Report".into(), field: "file".into(), adapter: FILE_REFERENCE_ADAPTER.into(), options: serde_json::json!({"storage": "reports"}) });
        ir.behavior.file_fields.push(FileField { model: "Report".into(), field: "file".into(), storage: "reports".into(), reference_contract: 1 });
        ir
    }

    #[test]
    fn definition_resolves_positions_and_options() {
        let schema = crate::schema::Schema::from_ir(schema()).unwrap();
        let adapter = schema.models[0].adapter_for("file").unwrap();
        assert_eq!(adapter.position, 1);
        assert_eq!(adapter.options, serde_json::json!({"storage": "reports"}));
        assert!(schema.models[0].adapter_for("id").is_none());
    }

    #[test]
    fn direct_metadata_cannot_bypass_default_rejection() {
        let mut ir = schema();
        ir.models[0].fields[1].default = Some(serde_json::json!({}));
        assert!(crate::schema::Schema::from_ir(ir).err().unwrap().contains("defaults"));
    }
}
