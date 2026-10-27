//! `@@locking.version`: the model gets a `version Int @default(0)` field. ORM updates increment
//! it, and instance writes check it. No DDL beyond the column.
use orm_contracts::{ir::{ColType, FieldIr, SchemaIr}, tracking::{self, error, Tracked}, writes::{AssignValue, InstanceGuard, WriteAssignment, WriteKind}};

const SOURCE: &str = "orm.locking";
const TRACKED: Tracked = Tracked { attribute: "locking.version", field: "version" };

/// Runs before the logical passes, so a proxy of the model copies the field too.
pub fn add_fields(ir: &mut SchemaIr) -> Result<(), String> {
    TRACKED.add_fields(ir, FieldIr { nullable: false, default: Some(serde_json::json!(0)), ..FieldIr::plain(TRACKED.field, ColType::Int) })
}

pub fn prepare(ir: &mut SchemaIr) -> Result<(), String> {
    // The records this pass finds: declared models, then proxies that share the column.
    let mut versions: Vec<WriteAssignment> = vec![];
    // The records of earlier definition batches: a proxy may take its root from one of them.
    let earlier: Vec<WriteAssignment> = ir.behavior.write_assignments.iter().filter(|a| a.source == SOURCE).cloned().collect();
    for d in &TRACKED.declarations(ir)? {
        let name = TRACKED.field_name(d)?;
        let upsert = tracking::upsert(d)?;
        let model = ir.models.iter().find(|m| m.name == d.model).ok_or_else(|| error(d, "unknown model"))?;
        if !model.fields.iter().any(|f| f.name == name) { return Err(error(d, "unknown field")); }
        if versions.iter().any(|a| a.model == d.model) { return Err(error(d, format!("{} has more than one version field", d.model))); }
        versions.push(WriteAssignment { source: SOURCE.into(), model: d.model.clone(), field: name,
            on: vec![WriteKind::Update, WriteKind::Upsert, WriteKind::DeleteRewrite], value: AssignValue::Increment, upsert });
    }
    // A view that keeps the field takes the records of its root storage owner.
    for view in &ir.behavior.model_views {
        if versions.iter().chain(&earlier).any(|a| a.model == view.model) { continue; }
        let Some(root) = versions.iter().chain(&earlier).find(|a| a.model == view.root).cloned() else { continue };
        if !ir.models.iter().find(|m| m.name == view.model).is_some_and(|m| m.fields.iter().any(|f| f.name == root.field)) {
            return Err(format!("{}: proxy must keep the version field {}", view.model, root.field));
        }
        versions.push(WriteAssignment { model: view.model.clone(), ..root });
    }
    for a in versions {
        ir.behavior.instance_guards.push(InstanceGuard { source: SOURCE.into(), model: a.model.clone(), field: a.field.clone() });
        ir.behavior.write_assignments.push(a);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use orm_contracts::writes::UpsertDefault;

    /// A model with a declaration and without the field: the input of `add_fields`.
    fn declared(positional: serde_json::Value, arguments: serde_json::Value, field: Option<&str>) -> SchemaIr {
        serde_json::from_value(serde_json::json!({
            "models": [{"name": "Post", "table": "posts", "fields": [
                {"name": "id", "column": "id", "type": "int", "primary_key": true}
            ]}],
            "behavior": {"schema_contract": 3, "declarations": [
                {"attribute": "locking.version", "model": "Post", "field": field, "positional": positional, "arguments": arguments, "location": {"file": "schema.prisma", "line": 3, "column": 3}}
            ]}
        })).unwrap()
    }

    /// The state after `add_fields`.
    fn schema() -> SchemaIr {
        let mut ir = declared(serde_json::json!([]), serde_json::json!({}), None);
        orm_contracts::extension::capture_storage(&mut ir).unwrap();
        add_fields(&mut ir).unwrap();
        ir.behavior.storage = None;
        ir
    }

    fn added(positional: serde_json::Value) -> Result<SchemaIr, String> {
        let mut ir = declared(positional, serde_json::json!({}), None);
        orm_contracts::extension::capture_storage(&mut ir)?;
        add_fields(&mut ir)?;
        Ok(ir)
    }

    #[test]
    fn map_sets_the_column_of_the_version_field() {
        let mut ir = declared(serde_json::json!([]), serde_json::json!({"map": "rev"}), None);
        orm_contracts::extension::capture_storage(&mut ir).unwrap();
        add_fields(&mut ir).unwrap();
        for model in [&ir.models[0], &ir.behavior.storage.as_ref().unwrap().models[0]] {
            let f = model.fields.last().unwrap();
            assert_eq!((f.name.as_str(), f.column.as_str()), ("version", "rev"));
        }
    }

    #[test]
    fn the_model_attribute_adds_the_version_field_to_logical_and_physical_models() {
        let ir = added(serde_json::json!([])).unwrap();
        for model in [&ir.models[0], &ir.behavior.storage.as_ref().unwrap().models[0]] {
            let f = model.fields.last().unwrap();
            assert_eq!((f.name.as_str(), f.ty, f.nullable, f.default.clone()), ("version", ColType::Int, false, Some(serde_json::json!(0))));
        }
        let effects = orm_contracts::extension::pass_effects(&declared(serde_json::json!([]), serde_json::json!({}), None), add_fields).unwrap();
        assert_eq!(effects.into_iter().collect::<Vec<_>>(), ["behavior.storage", "models"]);
        assert_eq!(added(serde_json::json!(["rev"])).unwrap().models[0].fields.last().unwrap().name, "rev");
    }

    #[test]
    fn upsert_pick_keeps_the_version_in_the_default_upsert_list() {
        for (arguments, expected) in [(serde_json::json!({}), UpsertDefault::Drop), (serde_json::json!({"upsert": "pick"}), UpsertDefault::Pick)] {
            let mut ir = declared(serde_json::json!([]), arguments, None);
            orm_contracts::extension::capture_storage(&mut ir).unwrap();
            add_fields(&mut ir).unwrap();
            prepare(&mut ir).unwrap();
            assert_eq!(ir.behavior.write_assignments[0].upsert, expected);
        }
        let mut ir = declared(serde_json::json!([]), serde_json::json!({"upsert": "merge"}), None);
        orm_contracts::extension::capture_storage(&mut ir).unwrap();
        add_fields(&mut ir).unwrap();
        assert!(prepare(&mut ir).unwrap_err().ends_with("upsert must be \"drop\" or \"pick\""));
    }

    #[test]
    fn the_field_attribute_fails_with_the_replacement_line() {
        for (field, line) in [("version", "@@locking.version"), ("rev", "@@locking.version(\"rev\")")] {
            let mut ir = declared(serde_json::json!([]), serde_json::json!({}), Some(field));
            orm_contracts::extension::capture_storage(&mut ir).unwrap();
            let err = add_fields(&mut ir).unwrap_err();
            assert_eq!(err, format!("schema.prisma:3:3: @locking.version: a field attribute is no longer supported; delete the field Post.{field} and write {line} in the model block"));
        }
    }

    #[test]
    fn a_field_with_the_same_name_is_an_error_that_names_both() {
        let mut ir = declared(serde_json::json!([]), serde_json::json!({}), None);
        ir.models[0].fields.push(FieldIr::plain("version", ColType::Int));
        orm_contracts::extension::capture_storage(&mut ir).unwrap();
        let err = add_fields(&mut ir).unwrap_err();
        assert_eq!(err, "schema.prisma:3:3: @locking.version: adds the field Post.version, which already exists; remove that field or name another field with @@locking.version(\"other_name\")");
    }

    #[test]
    fn preparation_records_the_version_field_once_and_adds_no_ddl() {
        let effects = orm_contracts::extension::pass_effects(&schema(), prepare).unwrap();
        assert_eq!(effects.into_iter().collect::<Vec<_>>(), ["behavior.instance_guards", "behavior.write_assignments"]);
        let mut ir = schema();
        prepare(&mut ir).unwrap();
        assert_eq!(ir.behavior.instance_guards, [InstanceGuard { source: "orm.locking".into(), model: "Post".into(), field: "version".into() }]);
    }

    #[test]
    fn the_version_field_is_incremented_on_every_write_kind_and_guards_instances() {
        let mut ir = schema();
        prepare(&mut ir).unwrap();
        let a = &ir.behavior.write_assignments;
        assert_eq!(a.len(), 1);
        assert_eq!((a[0].source.as_str(), a[0].model.as_str(), a[0].field.as_str(), a[0].value.clone(), a[0].upsert), ("orm.locking", "Post", "version", AssignValue::Increment, UpsertDefault::Drop));
        assert_eq!(a[0].on, [WriteKind::Update, WriteKind::Upsert, WriteKind::DeleteRewrite]);
        assert_eq!(ir.behavior.instance_guards, [InstanceGuard { source: "orm.locking".into(), model: "Post".into(), field: "version".into() }]);
    }

    #[test]
    fn a_proxy_that_keeps_the_field_gets_the_records_of_its_root() {
        let mut ir = schema();
        let mut view: orm_contracts::ir::ModelIr = serde_json::from_value(serde_json::to_value(&ir.models[0]).unwrap()).unwrap();
        view.name = "PostView".into();
        ir.models.push(view);
        ir.behavior.model_views.push(orm_contracts::extension::ModelView { model: "PostView".into(), source: "Post".into(), root: "Post".into(), row_filter: None, insert_table: None });
        prepare(&mut ir).unwrap();
        let models: Vec<_> = ir.behavior.write_assignments.iter().map(|a| a.model.as_str()).collect();
        assert_eq!(models, ["Post", "PostView"]);
        assert_eq!(ir.behavior.instance_guards.iter().map(|g| g.model.as_str()).collect::<Vec<_>>(), ["Post", "PostView"]);
    }

    #[test]
    fn a_proxy_in_a_later_batch_gets_the_records_of_a_root_in_the_context() {
        let mut ir = schema();
        prepare(&mut ir).unwrap();
        ir.behavior.declarations.clear();
        let mut view: orm_contracts::ir::ModelIr = serde_json::from_value(serde_json::to_value(&ir.models[0]).unwrap()).unwrap();
        view.name = "PostView".into();
        ir.models.push(view);
        ir.behavior.model_views.push(orm_contracts::extension::ModelView { model: "PostView".into(), source: "Post".into(), root: "Post".into(), row_filter: None, insert_table: None });
        prepare(&mut ir).unwrap();
        prepare(&mut ir).unwrap();
        let models: Vec<_> = ir.behavior.write_assignments.iter().map(|a| a.model.as_str()).collect();
        assert_eq!(models, ["Post", "PostView"]);
        assert_eq!(ir.behavior.instance_guards.iter().map(|g| g.model.as_str()).collect::<Vec<_>>(), ["Post", "PostView"]);
    }

    #[test]
    fn a_proxy_must_keep_the_version_field() {
        let mut ir = schema();
        let mut view: orm_contracts::ir::ModelIr = serde_json::from_value(serde_json::to_value(&ir.models[0]).unwrap()).unwrap();
        view.name = "PostView".into();
        view.fields.retain(|f| f.name != "version");
        ir.models.push(view);
        ir.behavior.model_views.push(orm_contracts::extension::ModelView { model: "PostView".into(), source: "Post".into(), root: "Post".into(), row_filter: None, insert_table: None });
        assert_eq!(prepare(&mut ir).unwrap_err(), "PostView: proxy must keep the version field version");
    }

    #[test]
    fn a_model_has_one_version_field() {
        let mut ir = schema();
        let mut again = ir.behavior.declarations[0].clone();
        again.positional = vec![serde_json::json!("rev")];
        ir.behavior.declarations.push(again);
        ir.models[0].fields.push(FieldIr::plain("rev", ColType::Int));
        assert_eq!(prepare(&mut ir).unwrap_err(), "schema.prisma:3:3: @locking.version: Post has more than one version field");
    }
}

// The package entry point; Rust callers keep the rlib API.
orm_contracts::extension!(("orm.locking.fields", add_fields), ("orm.locking.prepare", prepare));
