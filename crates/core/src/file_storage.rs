//! File fields declared by the file-storage extension, resolved to positions for the code
//! generators. The reference codec and the upload belong to the language packages.
use crate::ir::SchemaIr;

#[derive(Clone, Debug)]
pub struct PreparedFileField {
    pub position: usize,
    pub storage: String,
    pub nullable: bool,
}

pub fn prepare(ir: &SchemaIr) -> Result<Vec<Vec<PreparedFileField>>, String> {
    crate::behavior::validate_field_adapters(ir)?;
    let mut prepared = vec![vec![]; ir.models.len()];
    for file in &ir.behavior.file_fields {
        let model = ir.models.iter().position(|m| m.name == file.model)
            .ok_or_else(|| format!("unknown file model {}", file.model))?;
        let position = ir.models[model].fields.iter().position(|f| f.name == file.field)
            .ok_or_else(|| format!("unknown file field {}", file.field))?;
        let field = &ir.models[model].fields[position];
        if field.primary_key || field.default.is_some() || field.default_now
            || field.default_sql.is_some() || field.auto_increment {
            return Err(format!("{}.{}: file fields cannot be keys or have defaults", file.model, file.field));
        }
        crate::behavior::check_file_methods(ir, &ir.models[model], &file.field)?;
        prepared[model].push(PreparedFileField {
            position, storage: file.storage.clone(), nullable: field.nullable,
        });
    }
    // A view writes the table of its root, so its rows get the same file checks.
    for view in &ir.behavior.model_views {
        let (Some(model), Some(owner)) = (ir.models.iter().position(|m| m.name == view.model), ir.models.iter().position(|m| m.name == view.root)) else { continue };
        for file in prepared[owner].clone() {
            let name = &ir.models[owner].fields[file.position].name;
            let Some(position) = ir.models[model].fields.iter().position(|f| f.name == *name) else { continue };
            crate::behavior::check_file_methods(ir, &ir.models[model], name)?;
            prepared[model].push(PreparedFileField { position, nullable: ir.models[model].fields[position].nullable, ..file });
        }
    }
    Ok(prepared)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::behavior::{FieldAdapter, FileField, FILE_REFERENCE_ADAPTER, SCHEMA_CONTRACT};

    fn schema() -> SchemaIr {
        let mut ir = crate::dsl::compile("model Report {\n id Int @id\n file Json?\n}", None).unwrap();
        ir.behavior.schema_contract = SCHEMA_CONTRACT;
        ir.behavior.field_adapters.push(FieldAdapter { model: "Report".into(), field: "file".into(), adapter: FILE_REFERENCE_ADAPTER.into() });
        ir.behavior.file_fields.push(FileField { model: "Report".into(), field: "file".into(), storage: "reports".into(), reference_contract: 1 });
        ir
    }

    #[test]
    fn definition_resolves_positions() {
        let schema = crate::schema::Schema::from_ir(schema()).unwrap();
        assert_eq!(schema.models[0].file_fields[0].position, 1);
    }

    #[test]
    fn generated_types_and_methods_are_selected_only() {
        let ir = schema();
        let schema = crate::schema::Schema::from_ir(serde_json::from_value(serde_json::to_value(&ir).unwrap()).unwrap()).unwrap();
        let py = crate::codegen::python::generate(&ir, &schema, "report.prisma").unwrap();
        assert!(py.module.contains("_install_file_model"));
        assert!(py.stub.contains("Reference | None | Upload"));
        assert!(py.stub.contains("file_signed_url"));
        let ts = crate::codegen::typescript::generate(&ir, &schema, "report.prisma", "orm").unwrap();
        assert!(ts.contains("installModel"));
        assert!(ts.contains("fileSignedUrl"));
        assert!(ts.contains("Reference | null | Upload"));
        let ir = crate::dsl::compile("model Plain {\n id Int @id\n data Json?\n}", None).unwrap();
        let schema = crate::schema::Schema::from_ir(serde_json::from_value(serde_json::to_value(&ir).unwrap()).unwrap()).unwrap();
        assert!(!crate::codegen::python::generate(&ir, &schema, "plain").unwrap().module.contains("orm_file_storage"));
        assert!(!crate::codegen::typescript::generate(&ir, &schema, "plain", "orm").unwrap().contains("@orm/file-storage"));
    }

    #[test]
    fn direct_metadata_cannot_bypass_default_rejection() {
        let mut ir = schema();
        ir.models[0].fields[1].default = Some(serde_json::json!({}));
        assert!(crate::schema::Schema::from_ir(ir).err().unwrap().contains("defaults"));
    }
}
