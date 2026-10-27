//! Compile namespaced file declarations into provider-independent binding metadata.
use orm_contracts::{
    extension::{validate_field_adapters, FieldAdapter, FileField, FILE_REFERENCE_ADAPTER},
    ir::{ColType, SchemaIr},
};

/// A proxy writes the table of its storage owner, so its rows get the file fields of the owner.
/// The copy is idempotent: a proxy field that has a record keeps it.
fn copy_to_proxies(ir: &mut SchemaIr) -> Result<(), String> {
    let mut copies = Vec::new();
    for proxy in &ir.behavior.proxy_models {
        let Some(model) = ir.models.iter().find(|m| m.name == proxy.model) else { continue };
        for file in ir.behavior.file_fields.iter().filter(|f| f.model == proxy.storage_owner) {
            if !model.fields.iter().any(|f| f.name == file.field) { continue; }
            if ir.behavior.field_adapters.iter().any(|a| a.model == proxy.model && a.field == file.field) { continue; }
            orm_contracts::extension::check_adapter_methods(ir, model, &orm_contracts::extension::file_method_names(&file.field))?;
            copies.push(FileField { model: proxy.model.clone(), ..file.clone() });
        }
    }
    for file in copies {
        ir.behavior.field_adapters.push(FieldAdapter {
            model: file.model.clone(),
            field: file.field.clone(),
            adapter: FILE_REFERENCE_ADAPTER.into(),
            options: serde_json::json!({"storage": file.storage}),
        });
        ir.behavior.file_fields.push(file);
    }
    Ok(())
}

pub fn lower(ir: &mut SchemaIr) -> Result<(), String> {
    // Validate the entire batch before mutation, so failed definition is atomic.
    validate_field_adapters(ir)?;
    let mut contributions = Vec::new();
    for declaration in &ir.behavior.declarations {
        if declaration.attribute != "storage.file" || declaration.lowered {
            continue;
        }
        let fail = |message: &str| {
            format!(
                "{}:{}:{}: @storage.file: {message}",
                declaration.location.file, declaration.location.line, declaration.location.column
            )
        };
        let name = declaration
            .field
            .as_ref()
            .ok_or_else(|| fail("field declaration required"))?;
        let model = ir
            .models
            .iter()
            .find(|m| m.name == declaration.model)
            .ok_or_else(|| fail("unknown model"))?;
        let field = model
            .fields
            .iter()
            .find(|f| f.name == *name)
            .ok_or_else(|| fail("unknown field"))?;
        if field.ty != ColType::Json || field.array || field.enum_name.is_some() {
            return Err(fail("scalar Json field required"));
        }
        if field.default.is_some()
            || field.default_sql.is_some()
            || field.default_now
            || field.primary_key
            || field.auto_increment
        {
            return Err(fail(
                "file fields cannot have database defaults or be primary keys",
            ));
        }
        let storage = declaration
            .arguments
            .get("storage")
            .and_then(|s| s.as_str())
            .filter(|s| !s.is_empty() && !s.contains('\0'))
            .ok_or_else(|| fail("nonempty storage identity required"))?;
        if ir
            .behavior
            .field_adapters
            .iter()
            .any(|a| a.model == model.name && a.field == *name)
            || contributions
                .iter()
                .any(|f: &FileField| f.model == model.name && f.field == *name)
        {
            return Err(fail("duplicate file-field adapter"));
        }
        orm_contracts::extension::check_adapter_methods(ir, model, &orm_contracts::extension::file_method_names(name)).map_err(|e| fail(&e))?;
        contributions.push(FileField {
            model: model.name.clone(),
            field: name.clone(),
            storage: storage.to_owned(),
            reference_contract: 1,
        });
    }
    for field in contributions {
        ir.behavior.field_adapters.push(FieldAdapter {
            model: field.model.clone(),
            field: field.field.clone(),
            adapter: FILE_REFERENCE_ADAPTER.into(),
            options: serde_json::json!({"storage": field.storage}),
        });
        ir.behavior.file_fields.push(field);
    }
    copy_to_proxies(ir)?;
    validate_field_adapters(ir)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn schema(ty: &str) -> SchemaIr {
        serde_json::from_value(serde_json::json!({"models":[{"name":"Report","table":"reports","fields":[{"name":"id","column":"id","type":"int","primary_key":true},{"name":"file","column":"file","type":ty}]}],"behavior":{"declarations":[{"attribute":"storage.file","model":"Report","field":"file","arguments":{"storage":"reports"},"positional":[],"location":{"file":"schema.prisma","line":3,"column":9}}]}})).unwrap()
    }
    /// Declared effects `behavior.file_fields` and `behavior.field_adapters`.
    #[test]
    fn lowering_changes_only_its_declared_effects_once() {
        let effects = orm_contracts::extension::pass_effects(&schema("json"), lower).unwrap();
        assert_eq!(effects.into_iter().collect::<Vec<_>>(), ["behavior.field_adapters", "behavior.file_fields"]);
    }
    #[test]
    fn json_metadata_without_provider_configuration() {
        let mut ir = schema("json");
        lower(&mut ir).unwrap();
        assert_eq!(
            ir.behavior.field_adapters[0].adapter,
            FILE_REFERENCE_ADAPTER
        );
        assert_eq!(ir.behavior.file_fields[0].storage, "reports");
        assert_eq!(ir.behavior.field_adapters[0].options, serde_json::json!({"storage": "reports"}));
        ir.behavior.declarations[0].lowered = true;
        lower(&mut ir).unwrap();
        assert_eq!(ir.behavior.file_fields.len(), 1);
    }
    #[test]
    fn a_proxy_gets_the_records_of_its_storage_owner_once() {
        let mut ir = schema("json");
        let mut proxy: orm_contracts::ir::ModelIr = serde_json::from_value(serde_json::to_value(&ir.models[0]).unwrap()).unwrap();
        proxy.name = "Draft".into();
        ir.models.push(proxy);
        ir.behavior.proxy_models.push(orm_contracts::extension::ProxyModel { model: "Draft".into(), parent: "Report".into(), storage_owner: "Report".into(), ..Default::default() });
        lower(&mut ir).unwrap();
        ir.behavior.declarations[0].lowered = true;
        lower(&mut ir).unwrap();
        let adapters: Vec<_> = ir.behavior.field_adapters.iter().map(|a| (a.model.as_str(), a.options.clone())).collect();
        assert_eq!(adapters, [("Report", serde_json::json!({"storage": "reports"})), ("Draft", serde_json::json!({"storage": "reports"}))]);
        assert_eq!(ir.behavior.file_fields.len(), 2);
    }
    #[test]
    fn invalid_shape_and_collision_fail_without_mutation() {
        let mut invalid = schema("string");
        assert!(lower(&mut invalid).is_err());
        assert!(invalid.behavior.file_fields.is_empty());
        let mut collision = schema("json");
        let mut field = collision.models[0].fields[0].clone();
        field.name = "file_open".into();
        collision.models[0].fields.push(field);
        assert!(lower(&mut collision).unwrap_err().contains("collides"));
        assert!(collision.behavior.file_fields.is_empty());
    }
    #[test]
    fn disabled_artifact_rejects_file_contract() {
        let mut ir = schema("json");
        lower(&mut ir).unwrap();
        assert!(
            orm_contracts::extension::check_requirements(&ir, &Default::default())
                .unwrap_err()
                .contains("rebuild")
        );
    }
}
