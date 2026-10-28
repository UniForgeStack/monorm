//! Compile namespaced file declarations into provider-independent binding metadata.
use monorm_contracts::{
    extension::{check_adapter_methods, expand_adapter_template, validate_field_adapters, FieldAdapter},
    ir::{ColType, SchemaIr},
};

const ADAPTER: &str = "file-storage.reference.v1";

/// The method names that the language runtime adds for `field`, in the Python and TypeScript forms.
fn method_names(field: &str) -> Vec<String> {
    ["{field}_signed_url", "{field}_open"]
        .iter()
        .map(|t| expand_adapter_template(t, "python", field))
        .chain(["{field}SignedUrl", "{field}Open"].iter().map(|t| expand_adapter_template(t, "typescript", field)))
        .collect()
}

/// A view writes the table of its root, so its rows get the file fields of the root.
/// The copy is idempotent: a view field that has a record keeps it.
fn copy_to_views(ir: &mut SchemaIr) -> Result<(), String> {
    let mut copies = Vec::new();
    for view in &ir.behavior.model_views {
        let Some(model) = ir.models.iter().find(|m| m.name == view.model) else { continue };
        for owner in ir.behavior.field_adapters.iter().filter(|a| a.adapter == ADAPTER && a.model == view.root) {
            if !model.fields.iter().any(|f| f.name == owner.field) { continue; }
            if ir.behavior.field_adapters.iter().any(|a| a.model == view.model && a.field == owner.field) { continue; }
            check_adapter_methods(model, &method_names(&owner.field))?;
            copies.push(FieldAdapter { model: view.model.clone(), ..owner.clone() });
        }
    }
    ir.behavior.field_adapters.extend(copies);
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
                "file fields cannot be keys or have defaults",
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
                .any(|f: &FieldAdapter| f.model == model.name && f.field == *name)
        {
            return Err(fail("duplicate file-field adapter"));
        }
        check_adapter_methods(model, &method_names(name)).map_err(|e| fail(&e))?;
        contributions.push(FieldAdapter {
            model: model.name.clone(),
            field: name.clone(),
            adapter: ADAPTER.into(),
            options: serde_json::json!({"storage": storage}),
        });
    }
    ir.behavior.field_adapters.extend(contributions);
    copy_to_views(ir)?;
    validate_field_adapters(ir)
}

// The package entry point; Rust callers keep the rlib API.
monorm_contracts::extension!(("monorm.file-storage.fields", lower));

#[cfg(test)]
mod tests {
    use super::*;
    fn schema(ty: &str) -> SchemaIr {
        serde_json::from_value(serde_json::json!({"models":[{"name":"Report","table":"reports","fields":[{"name":"id","column":"id","type":"int","primary_key":true},{"name":"file","column":"file","type":ty}]}],"behavior":{"declarations":[{"attribute":"storage.file","model":"Report","field":"file","arguments":{"storage":"reports"},"positional":[],"location":{"file":"schema.prisma","line":3,"column":9}}]}})).unwrap()
    }
    /// Declared effect `behavior.field_adapters`.
    #[test]
    fn lowering_changes_only_its_declared_effects_once() {
        let effects = monorm_contracts::extension::pass_effects(&schema("json"), lower).unwrap();
        assert_eq!(effects.into_iter().collect::<Vec<_>>(), ["behavior.field_adapters"]);
    }
    #[test]
    fn json_metadata_without_provider_configuration() {
        let mut ir = schema("json");
        lower(&mut ir).unwrap();
        assert_eq!(ir.behavior.field_adapters[0].adapter, ADAPTER);
        assert_eq!(ir.behavior.field_adapters[0].options, serde_json::json!({"storage": "reports"}));
        ir.behavior.declarations[0].lowered = true;
        lower(&mut ir).unwrap();
        assert_eq!(ir.behavior.field_adapters.len(), 1);
    }
    #[test]
    fn a_view_gets_the_records_of_its_root_once() {
        let mut ir = schema("json");
        let mut view: monorm_contracts::ir::ModelIr = serde_json::from_value(serde_json::to_value(&ir.models[0]).unwrap()).unwrap();
        view.name = "Draft".into();
        ir.models.push(view);
        ir.behavior.model_views.push(monorm_contracts::extension::ModelView { model: "Draft".into(), source: "Report".into(), root: "Report".into(), row_filter: None, insert_table: None });
        lower(&mut ir).unwrap();
        ir.behavior.declarations[0].lowered = true;
        lower(&mut ir).unwrap();
        let adapters: Vec<_> = ir.behavior.field_adapters.iter().map(|a| (a.model.as_str(), a.options.clone())).collect();
        assert_eq!(adapters, [("Report", serde_json::json!({"storage": "reports"})), ("Draft", serde_json::json!({"storage": "reports"}))]);
    }
    #[test]
    fn invalid_shape_and_collision_fail_without_mutation() {
        let mut invalid = schema("string");
        assert!(lower(&mut invalid).is_err());
        assert!(invalid.behavior.field_adapters.is_empty());
        let mut collision = schema("json");
        let mut field = collision.models[0].fields[0].clone();
        field.name = "file_open".into();
        collision.models[0].fields.push(field);
        assert!(lower(&mut collision).unwrap_err().contains("collides"));
        assert!(collision.behavior.field_adapters.is_empty());
    }
    #[test]
    fn a_key_or_a_default_is_rejected() {
        for case in 0..3 {
            let mut ir = schema("json");
            let field = &mut ir.models[0].fields[1];
            match case { 0 => field.primary_key = true, 1 => field.default_now = true, _ => field.auto_increment = true }
            assert!(lower(&mut ir).unwrap_err().contains("cannot be keys or have defaults"));
            assert!(ir.behavior.field_adapters.is_empty());
        }
    }
}
