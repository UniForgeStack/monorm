//! Shared by the tracking extensions (timestamps, soft-delete, locking): a model attribute adds
//! the field that it tracks, and a field attribute fails with the model attribute to write instead.
use crate::{extension::{attribute_namespace, Declaration}, ir::{ColType, FieldIr, SchemaIr}, writes::UpsertDefault};

/// The prefix of every tracking error: `file:line:column: @attribute: message`.
pub fn error(d: &Declaration, message: impl std::fmt::Display) -> String {
    format!("{}:{}:{}: @{}: {message}", d.location.file, d.location.line, d.location.column, d.attribute)
}

fn is_snake_case(s: &str) -> bool {
    s.starts_with(|c: char| c.is_ascii_lowercase()) && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// `editedAt` as `edited_at`; `None` when the result is not snake_case.
fn snake_case(s: &str) -> Option<String> {
    let mut out = String::new();
    for c in s.trim_start_matches('_').chars() {
        if c.is_ascii_uppercase() && !out.is_empty() && !out.ends_with('_') { out.push('_'); }
        out.push(c.to_ascii_lowercase());
    }
    is_snake_case(&out).then_some(out)
}

/// The `upsert` argument: whether the default `on_conflict(update=True)` column list keeps the field.
pub fn upsert(d: &Declaration) -> Result<UpsertDefault, String> {
    match d.arguments.get("upsert").map(|v| v.as_str()) {
        None | Some(Some("drop")) => Ok(UpsertDefault::Drop),
        Some(Some("pick")) => Ok(UpsertDefault::Pick),
        _ => Err(error(d, "upsert must be \"drop\" or \"pick\"")),
    }
}

/// A tracking model attribute and the default name of the field it adds.
#[derive(Clone, Copy, Debug)]
pub struct Tracked {
    pub attribute: &'static str,
    pub field: &'static str,
}

impl Tracked {
    /// The declarations of this attribute in the batch. Another attribute of its namespace fails.
    pub fn declarations(&self, ir: &SchemaIr) -> Result<Vec<Declaration>, String> {
        let own = attribute_namespace(self.attribute);
        let declarations: Vec<_> = ir.behavior.declarations.iter().filter(|d| !d.lowered && attribute_namespace(&d.attribute) == own).cloned().collect();
        match declarations.iter().find(|d| d.attribute != self.attribute) {
            Some(d) => Err(error(d, format!("unknown {own} attribute"))),
            None => Ok(declarations),
        }
    }

    /// The snake_case name of the added field: the positional argument, else the default.
    pub fn field_name(&self, d: &Declaration) -> Result<String, String> {
        match d.positional.as_slice() {
            [] => Ok(self.field.to_owned()),
            [name] => name.as_str().filter(|s| is_snake_case(s)).map(str::to_owned).ok_or_else(|| error(d, "the field name must be a snake_case string")),
            _ => Err(error(d, "takes at most one positional argument, the field name")),
        }
    }

    /// The column of the added field: the `map` argument, else the field name.
    pub fn column(&self, d: &Declaration, name: &str) -> Result<String, String> {
        match d.arguments.get("map").map(|v| v.as_str()) {
            None => Ok(name.to_owned()),
            Some(Some(column)) if !column.is_empty() => Ok(column.to_owned()),
            _ => Err(error(d, "map must be a string")),
        }
    }

    /// Add the field of each declaration to its model. `template` holds the field with the default name; the declared name and column replace its own.
    pub fn add_fields(&self, ir: &mut SchemaIr, template: FieldIr) -> Result<(), String> {
        for d in &self.declarations(ir)? {
            self.reject_field_form(ir, d, &template)?;
            let name = self.field_name(d)?;
            let column = self.column(d, &name)?;
            add_field(ir, d, &FieldIr { name, column, ..template.clone() })?;
        }
        Ok(())
    }

    /// A field attribute (`field T @attr`) fails; the message prints the model attribute to write instead.
    /// The line keeps the column of the old field, so the next migration leaves the live column alone.
    fn reject_field_form(&self, ir: &SchemaIr, d: &Declaration, template: &FieldIr) -> Result<(), String> {
        let Some(field) = &d.field else { return Ok(()) };
        let old = ir.models.iter().find(|m| m.name == d.model).and_then(|m| m.fields.iter().find(|f| &f.name == field));
        let column = old.map_or(field.as_str(), |f| f.column.as_str());
        let name = if is_snake_case(field) { field.clone() } else { snake_case(field).unwrap_or_else(|| self.field.to_owned()) };
        let mut arguments = vec![];
        if name != self.field { arguments.push(serde_json::Value::from(name.as_str()).to_string()); }
        if column != name { arguments.push(format!("map: {}", serde_json::Value::from(column))); }
        if let Some(mode) = d.arguments.get("mode") { arguments.push(format!("mode: {mode}")); }
        let line = if arguments.is_empty() { format!("@@{}", d.attribute) } else { format!("@@{}({})", d.attribute, arguments.join(", ")) };
        let note = match old {
            Some(f) if f.ty == ColType::BigInt && template.ty == ColType::Int => "; the added field has the type Int, not BigInt; change the type by hand",
            _ => "",
        };
        Err(error(d, format!("a field attribute is no longer supported; delete the field {}.{field} and write {line} in the model block{note}", d.model)))
    }
}

/// Append `field` to the logical and the physical model of the declaration.
fn add_field(ir: &mut SchemaIr, d: &Declaration, field: &FieldIr) -> Result<(), String> {
    let mut lists = vec![&mut ir.models];
    if let Some(storage) = &mut ir.behavior.storage { lists.push(&mut storage.models); }
    for list in lists {
        let model = list.iter_mut().find(|m| m.name == d.model).ok_or_else(|| error(d, format!("{} has no model of its own to add the field to", d.model)))?;
        let hint = format!("remove that field or name another field with @@{}(\"other_name\")", d.attribute);
        if model.fields.iter().any(|f| f.name == field.name) || model.relations.iter().any(|r| r.name == field.name) {
            return Err(error(d, format!("adds the field {}.{}, which already exists; {hint}", d.model, field.name)));
        }
        if let Some(owner) = model.fields.iter().find(|f| f.column == field.column) {
            return Err(error(d, format!("adds the field {}.{}, but the field {}.{} already uses the column {}; {hint}", d.model, field.name, d.model, owner.name, field.column)));
        }
        model.fields.push(field.clone());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const TRACKED: Tracked = Tracked { attribute: "timestamps.updated_at", field: "updated_at" };

    /// A model `Post` with `fields` and one declaration of each of `attributes`.
    fn schema(fields: serde_json::Value, attributes: &[&str]) -> SchemaIr {
        let declarations: Vec<_> = attributes.iter().map(|attribute| json!({
            "attribute": attribute, "model": "Post", "field": null, "positional": [], "arguments": {}, "location": {"file": "schema.prisma", "line": 4, "column": 3}})).collect();
        let mut ir: SchemaIr = serde_json::from_value(json!({
            "models": [{"name": "Post", "table": "posts", "fields": fields}],
            "behavior": {"schema_contract": 5, "declarations": declarations}
        })).unwrap();
        crate::extension::capture_storage(&mut ir).unwrap();
        ir
    }

    #[test]
    fn a_name_clash_and_a_column_clash_name_the_field_that_is_in_the_way() {
        let id = json!({"name": "id", "column": "id", "type": "int", "primary_key": true});
        let mut ir = schema(json!([id, {"name": "updated_at", "column": "touched", "type": "int"}]), &["timestamps.updated_at"]);
        let err = TRACKED.add_fields(&mut ir, FieldIr::plain("updated_at", ColType::Int)).unwrap_err();
        assert_eq!(err, "schema.prisma:4:3: @timestamps.updated_at: adds the field Post.updated_at, which already exists; remove that field or name another field with @@timestamps.updated_at(\"other_name\")");
        let mut ir = schema(json!([id, {"name": "touched", "column": "updated_at", "type": "int"}]), &["timestamps.updated_at"]);
        let err = TRACKED.add_fields(&mut ir, FieldIr::plain("updated_at", ColType::Int)).unwrap_err();
        assert_eq!(err, "schema.prisma:4:3: @timestamps.updated_at: adds the field Post.updated_at, but the field Post.touched already uses the column updated_at; remove that field or name another field with @@timestamps.updated_at(\"other_name\")");
    }

    #[test]
    fn a_field_attribute_fails_with_the_line_to_write_that_keeps_the_name_and_the_column() {
        let prefix = "schema.prisma:4:3: @timestamps.updated_at: a field attribute is no longer supported; delete the field Post.";
        // (old field, its column, arguments, the line to write)
        let rows = [
            ("updated_at", "updated_at", json!({}), "@@timestamps.updated_at"),
            ("edited_at", "edited_at", json!({}), "@@timestamps.updated_at(\"edited_at\")"),
            ("edited_at", "edited_at", json!({"mode": "application"}), "@@timestamps.updated_at(\"edited_at\", mode: \"application\")"),
            ("updated_at", "_updated", json!({}), "@@timestamps.updated_at(map: \"_updated\")"),
            ("edited_at", "_edited", json!({"mode": "database"}), "@@timestamps.updated_at(\"edited_at\", map: \"_edited\", mode: \"database\")"),
            ("editedAt", "editedAt", json!({}), "@@timestamps.updated_at(\"edited_at\", map: \"editedAt\")"),
            ("_updated_at", "_updated_at", json!({"mode": "database"}), "@@timestamps.updated_at(map: \"_updated_at\", mode: \"database\")"),
        ];
        for (field, column, arguments, line) in rows {
            let mut ir = schema(json!([{"name": field, "column": column, "type": "date_time"}]), &["timestamps.updated_at"]);
            ir.behavior.declarations[0].field = Some(field.into());
            ir.behavior.declarations[0].arguments = serde_json::from_value(arguments).unwrap();
            let err = TRACKED.add_fields(&mut ir, FieldIr::plain("updated_at", ColType::DateTime)).unwrap_err();
            assert_eq!(err, format!("{prefix}{field} and write {line} in the model block"));
        }
    }

    #[test]
    fn a_field_attribute_without_the_old_field_in_the_schema_keeps_the_field_name_as_the_column() {
        let mut ir = schema(json!([]), &["timestamps.updated_at"]);
        ir.behavior.declarations[0].field = Some("editedAt".into());
        let err = TRACKED.add_fields(&mut ir, FieldIr::plain("updated_at", ColType::DateTime)).unwrap_err();
        assert_eq!(err, "schema.prisma:4:3: @timestamps.updated_at: a field attribute is no longer supported; delete the field Post.editedAt and write @@timestamps.updated_at(\"edited_at\", map: \"editedAt\") in the model block");
    }

    #[test]
    fn a_field_attribute_on_a_big_int_version_notes_that_the_added_field_is_int() {
        const VERSION: Tracked = Tracked { attribute: "locking.version", field: "version" };
        let mut ir = schema(json!([{"name": "version", "column": "rev", "type": "big_int"}]), &["locking.version"]);
        ir.behavior.declarations[0].attribute = "locking.version".into();
        ir.behavior.declarations[0].field = Some("version".into());
        let err = VERSION.add_fields(&mut ir, FieldIr::plain("version", ColType::Int)).unwrap_err();
        assert_eq!(err, "schema.prisma:4:3: @locking.version: a field attribute is no longer supported; delete the field Post.version and write @@locking.version(map: \"rev\") in the model block; the added field has the type Int, not BigInt; change the type by hand");
    }

    #[test]
    fn map_sets_the_column_of_the_added_field() {
        let id = json!({"name": "id", "column": "id", "type": "int", "primary_key": true});
        let mut ir = schema(json!([id]), &["timestamps.updated_at"]);
        ir.behavior.declarations[0].positional = vec![json!("edited_at")];
        ir.behavior.declarations[0].arguments.insert("map".into(), json!("_edited"));
        TRACKED.add_fields(&mut ir, FieldIr::plain("updated_at", ColType::Int)).unwrap();
        for model in [&ir.models[0], &ir.behavior.storage.as_ref().unwrap().models[0]] {
            let f = model.fields.last().unwrap();
            assert_eq!((f.name.as_str(), f.column.as_str()), ("edited_at", "_edited"));
        }
        let mut ir = schema(json!([id]), &["timestamps.updated_at"]);
        ir.behavior.declarations[0].arguments.insert("map".into(), json!(3));
        let err = TRACKED.add_fields(&mut ir, FieldIr::plain("updated_at", ColType::Int)).unwrap_err();
        assert_eq!(err, "schema.prisma:4:3: @timestamps.updated_at: map must be a string");
    }

    #[test]
    fn declarations_take_the_attribute_and_fail_on_another_of_its_namespace() {
        let mut ir = schema(json!([]), &["timestamps.updated_at", "locking.version", "timestamps.updated_at"]);
        assert_eq!(TRACKED.declarations(&ir).unwrap().len(), 2);
        ir.behavior.declarations[0].lowered = true;
        assert_eq!(TRACKED.declarations(&ir).unwrap().len(), 1);
        let ir = schema(json!([]), &["timestamps.created_at"]);
        assert_eq!(TRACKED.declarations(&ir).unwrap_err(), "schema.prisma:4:3: @timestamps.created_at: unknown timestamps attribute");
    }

    #[test]
    fn upsert_is_drop_or_pick() {
        let declared = |value: Option<serde_json::Value>| {
            let mut d = schema(json!([]), &["timestamps.updated_at"]).behavior.declarations.remove(0);
            if let Some(value) = value { d.arguments.insert("upsert".into(), value); }
            d
        };
        assert_eq!(upsert(&declared(None)), Ok(UpsertDefault::Drop));
        assert_eq!(upsert(&declared(Some(json!("drop")))), Ok(UpsertDefault::Drop));
        assert_eq!(upsert(&declared(Some(json!("pick")))), Ok(UpsertDefault::Pick));
        assert_eq!(upsert(&declared(Some(json!("merge")))), Err("schema.prisma:4:3: @timestamps.updated_at: upsert must be \"drop\" or \"pick\"".into()));
    }
}
