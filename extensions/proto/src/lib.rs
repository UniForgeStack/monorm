//! Compile `@proto.message("pkg.Msg")` declarations into binding metadata for a Bytes column.
use monorm::{
    extension::{validate_field_adapters, views_keeping, FieldAdapter},
    ir::{ColType, ConstraintIr, FieldIr, ModelIr, SchemaIr},
};

const ADAPTER: &str = "proto.message.v1";

/// The full name of a message: dotted identifiers, at least a package and a name (`pkg.sub.Message`).
fn message_name_ok(name: &str) -> bool {
    let parts: Vec<&str> = name.split('.').collect();
    parts.len() >= 2
        && parts.iter().all(|p| {
            let mut chars = p.chars();
            chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

/// The reason a proto field cannot sit in a key, an index, a constraint, a default or a relation.
fn refusal(model: &ModelIr, field: &FieldIr) -> Option<&'static str> {
    if field.ty != ColType::Bytes || field.enum_name.is_some() {
        return Some("a Bytes field is required");
    }
    if field.array {
        return Some("a list of proto fields is not supported");
    }
    if field.nested.is_some() {
        return Some("a proto field inside a nested type is not supported");
    }
    if field.primary_key || model.primary_key.contains(&field.name) {
        return Some("a proto field cannot be a key");
    }
    if field.unique || field.index || model.indexes.iter().any(|i| i.columns.iter().any(|c| c.field.as_deref() == Some(&field.name)) || i.include.contains(&field.name)) {
        return Some("a proto field cannot have @index or @unique: the encoding of a message is not canonical");
    }
    let named_by_constraint = model.constraints.iter().any(|c| match c {
        ConstraintIr::Unique { fields, .. } => fields.contains(&field.name),
        ConstraintIr::Check { expr, .. } => expr.contains(&field.column),
        _ => false,
    });
    if named_by_constraint {
        return Some("a proto field cannot have @unique or @check");
    }
    if field.check.is_some() {
        return Some("a proto field cannot have @check");
    }
    if field.has_server_default() || field.default_sql.is_some() || field.client_default.is_some() {
        return Some("a proto field cannot have a default");
    }
    if model.relations.iter().any(|r| r.from.contains(&field.column) || r.to.contains(&field.column)) {
        return Some("a proto field cannot be a key of a relation");
    }
    None
}

/// A view writes the table of its root, so its rows get the proto fields of the root.
/// The copy is idempotent: a view field that has a record keeps it.
fn copy_adapters_to_views(ir: &mut SchemaIr) {
    let owners: Vec<FieldAdapter> = ir.behavior.field_adapters.iter().filter(|a| a.adapter == ADAPTER).cloned().collect();
    let mut copies = Vec::new();
    for owner in owners {
        let has = |model: &str| ir.behavior.field_adapters.iter().any(|a| a.model == model && a.field == owner.field);
        for view in views_keeping(ir, &owner.model, &owner.field, has).keeping {
            copies.push(FieldAdapter { model: view, ..owner.clone() });
        }
    }
    ir.behavior.field_adapters.extend(copies);
}

pub fn lower(ir: &mut SchemaIr) -> Result<(), String> {
    // Validate the entire batch before mutation, so a failed definition is atomic.
    validate_field_adapters(ir)?;
    let mut contributions: Vec<FieldAdapter> = Vec::new();
    for declaration in &ir.behavior.declarations {
        if declaration.attribute != "proto.message" || declaration.lowered {
            continue;
        }
        let fail = |message: &str| declaration.error_with("@", message);
        let name = declaration.field.as_ref().ok_or_else(|| fail("field declaration required"))?;
        let model = ir.models.iter().find(|m| m.name == declaration.model).ok_or_else(|| fail("unknown model"))?;
        let field = model.fields.iter().find(|f| f.name == *name).ok_or_else(|| fail("unknown field"))?;
        if let Some(reason) = refusal(model, field) {
            return Err(fail(reason));
        }
        let message = declaration
            .positional
            .first()
            .and_then(|s| s.as_str())
            .filter(|s| message_name_ok(s))
            .ok_or_else(|| fail("the full message name is required, for example \"pkg.Message\""))?;
        let taken = |a: &FieldAdapter| a.model == model.name && a.field == *name;
        if ir.behavior.field_adapters.iter().any(taken) || contributions.iter().any(taken) {
            return Err(fail("duplicate field adapter"));
        }
        contributions.push(FieldAdapter {
            model: model.name.clone(),
            field: name.clone(),
            adapter: ADAPTER.into(),
            options: serde_json::json!({"message": message}),
            null_only: true,
        });
    }
    ir.behavior.field_adapters.extend(contributions);
    copy_adapters_to_views(ir);
    validate_field_adapters(ir)
}

// The package entry point; Rust callers keep the rlib API.
monorm::extension!(("monorm.proto.fields", lower));

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema(ty: &str) -> SchemaIr {
        serde_json::from_value(json!({"models":[{"name":"Doc","table":"docs","fields":[
            {"name":"id","column":"id","type":"int","primary_key":true},
            {"name":"body","column":"body","type":ty,"nullable":true}]}],
            "behavior":{"declarations":[{"attribute":"proto.message","model":"Doc","field":"body","arguments":{},"positional":["acme.Note"],"location":{"file":"schema.prisma","line":3,"column":9}}]}})).unwrap()
    }

    /// Declared effect `behavior.field_adapters.proto`; the pass changes that section only.
    #[test]
    fn lowering_changes_only_its_declared_effects_once() {
        let effects = monorm::extension::pass_effects(&schema("bytes"), lower).unwrap();
        assert_eq!(effects.into_iter().collect::<Vec<_>>(), ["behavior.field_adapters"]);
    }

    #[test]
    fn a_record_names_the_message_and_allows_null_checks_only() {
        let mut ir = schema("bytes");
        lower(&mut ir).unwrap();
        let record = &ir.behavior.field_adapters[0];
        assert_eq!((record.adapter.as_str(), record.null_only), (ADAPTER, true));
        assert_eq!(record.options, json!({"message": "acme.Note"}));
        ir.behavior.declarations[0].lowered = true;
        lower(&mut ir).unwrap();
        assert_eq!(ir.behavior.field_adapters.len(), 1);
    }

    #[test]
    fn a_view_gets_the_record_of_its_root_once() {
        let mut ir = schema("bytes");
        let mut view: ModelIr = ir.models[0].clone();
        view.name = "Draft".into();
        ir.models.push(view);
        ir.behavior.model_views.push(monorm::extension::ModelView { model: "Draft".into(), source: "Doc".into(), root: "Doc".into(), row_filter: None, insert_table: None });
        lower(&mut ir).unwrap();
        ir.behavior.declarations[0].lowered = true;
        lower(&mut ir).unwrap();
        let models: Vec<_> = ir.behavior.field_adapters.iter().map(|a| a.model.as_str()).collect();
        assert_eq!(models, ["Doc", "Draft"]);
    }

    #[test]
    fn a_field_that_is_not_bytes_is_refused_without_mutation() {
        for ty in ["string", "json", "int"] {
            let mut ir = schema(ty);
            assert!(lower(&mut ir).unwrap_err().contains("a Bytes field is required"), "{ty}");
            assert!(ir.behavior.field_adapters.is_empty());
        }
    }

    #[test]
    fn a_bad_message_name_is_refused() {
        for name in ["Note", "", "acme.", "acme..Note", "1acme.Note", "acme.No te"] {
            let mut ir = schema("bytes");
            ir.behavior.declarations[0].positional = vec![json!(name)];
            assert!(lower(&mut ir).unwrap_err().contains("full message name"), "{name:?}");
        }
        let mut ir = schema("bytes");
        ir.behavior.declarations[0].positional = vec![json!("acme.sub.Outer.Inner")];
        lower(&mut ir).unwrap();
    }

    #[test]
    fn a_duplicate_declaration_is_refused() {
        let mut ir = schema("bytes");
        let again = ir.behavior.declarations[0].clone();
        ir.behavior.declarations.push(again);
        assert!(lower(&mut ir).unwrap_err().contains("duplicate field adapter"));
        assert!(ir.behavior.field_adapters.is_empty());
    }

    #[test]
    fn a_key_an_index_a_check_a_default_a_list_or_a_nested_field_is_refused() {
        let cases: [(&str, fn(&mut ModelIr), &str); 12] = [
            ("field key", |m| m.fields[1].primary_key = true, "cannot be a key"),
            ("model key", |m| m.primary_key = vec!["id".into(), "body".into()], "cannot be a key"),
            ("unique field", |m| m.fields[1].unique = true, "@index or @unique"),
            ("index field", |m| m.fields[1].index = true, "@index or @unique"),
            ("model index", |m| m.indexes.push(serde_json::from_value(json!({"columns": [{"field": "body"}]})).unwrap()), "@index or @unique"),
            ("unique constraint", |m| m.constraints.push(ConstraintIr::Unique { name: None, fields: vec!["body".into()], nulls_not_distinct: false, deferrable: None }), "@unique or @check"),
            ("check constraint", |m| m.constraints.push(ConstraintIr::Check { name: None, expr: "length(body) > 0".into() }), "@unique or @check"),
            ("field check", |m| m.fields[1].check = Some("length(body) > 0".into()), "@check"),
            ("default", |m| m.fields[1].default = Some(json!("x")), "cannot have a default"),
            ("default sql", |m| m.fields[1].default_sql = Some("'\\x'::bytea".into()), "cannot have a default"),
            ("list", |m| m.fields[1].array = true, "list of proto fields"),
            ("nested", |m| m.fields[1].nested = Some(serde_json::from_value(json!({"type": "Inner", "storage": "json", "field": "inner"})).unwrap()), "nested type"),
        ];
        for (label, edit, expected) in cases {
            let mut ir = schema("bytes");
            edit(&mut ir.models[0]);
            let error = lower(&mut ir).unwrap_err();
            assert!(error.contains(expected), "{label}: {error}");
            assert!(ir.behavior.field_adapters.is_empty(), "{label}");
        }
    }
}
