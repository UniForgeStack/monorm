//! `@locking.version`: ORM updates increment the field, and instance writes check it.
//! No DDL.
use orm_contracts::{extension::Declaration, ir::{ColType, SchemaIr}, writes::{AssignValue, InstanceGuard, UpsertDefault, WriteAssignment, WriteKind}};

const SOURCE: &str = "orm.locking";

fn error(d: &Declaration, message: impl std::fmt::Display) -> String {
    format!("{}:{}:{}: @{}: {message}", d.location.file, d.location.line, d.location.column, d.attribute)
}

pub fn prepare(ir: &mut SchemaIr) -> Result<(), String> {
    // The (model, field) pairs this pass finds: declared models, then proxies that share the column.
    let mut versions: Vec<(String, String)> = vec![];
    // The pairs of earlier definition batches: a proxy may take its root from one of them.
    let earlier: Vec<(String, String)> = ir.behavior.instance_guards.iter().filter(|g| g.source == SOURCE).map(|g| (g.model.clone(), g.field.clone())).collect();
    let declarations: Vec<_> = ir.behavior.declarations.iter().filter(|d| !d.lowered && d.attribute.starts_with("locking.")).cloned().collect();
    for d in &declarations {
        if d.attribute != "locking.version" { return Err(error(d, "unknown locking attribute")); }
        let name = d.field.clone().ok_or_else(|| error(d, "is a field attribute"))?;
        let model = ir.models.iter().find(|m| m.name == d.model).ok_or_else(|| error(d, "unknown model"))?;
        let field = model.fields.iter().find(|f| f.name == name).ok_or_else(|| error(d, "unknown field"))?;
        if !matches!(field.ty, ColType::Int | ColType::BigInt) || field.array || field.nullable || field.primary_key {
            return Err(error(d, format!("{}.{name} must be a required Int or BigInt field", d.model)));
        }
        if model.primary_key.contains(&name) { return Err(error(d, format!("{}.{name} must not be part of the @@id", d.model))); }
        if versions.iter().any(|(m, _)| *m == d.model) { return Err(error(d, format!("{} has more than one version field", d.model))); }
        versions.push((d.model.clone(), name));
    }
    // A proxy that keeps the field takes the records of its root storage owner.
    for proxy in &ir.behavior.proxy_models {
        if versions.iter().chain(&earlier).any(|(m, _)| *m == proxy.model) { continue; }
        let root = versions.iter().chain(&earlier).find(|(m, _)| *m == proxy.storage_owner).map(|(_, f)| f.clone());
        if let Some(source) = root {
            if !ir.models.iter().find(|m| m.name == proxy.model).is_some_and(|m| m.fields.iter().any(|f| f.name == source)) {
                return Err(format!("{}: proxy must keep the version field {source}", proxy.model));
            }
            versions.push((proxy.model.clone(), source));
        }
    }
    for (model, field) in versions {
        ir.behavior.write_assignments.push(WriteAssignment { source: SOURCE.into(), model: model.clone(), field: field.clone(),
            on: vec![WriteKind::Update, WriteKind::Upsert, WriteKind::DeleteRewrite], value: AssignValue::Increment, upsert: UpsertDefault::Drop });
        ir.behavior.instance_guards.push(InstanceGuard { source: SOURCE.into(), model, field });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(ty: &str) -> SchemaIr {
        serde_json::from_value(serde_json::json!({
            "models": [{"name": "Post", "table": "posts", "fields": [
                {"name": "id", "column": "id", "type": "int", "primary_key": true},
                {"name": "version", "column": "version", "type": ty, "default": 0}
            ]}],
            "behavior": {"schema_contract": 2, "declarations": [
                {"attribute": "locking.version", "model": "Post", "field": "version", "positional": [], "arguments": {}, "location": {"file": "schema.prisma", "line": 3, "column": 3}}
            ]}
        })).unwrap()
    }

    #[test]
    fn preparation_records_the_version_field_once_and_adds_no_ddl() {
        let effects = orm_contracts::extension::pass_effects(&schema("int"), prepare).unwrap();
        assert_eq!(effects.into_iter().collect::<Vec<_>>(), ["behavior.instance_guards", "behavior.write_assignments"]);
        let mut ir = schema("big_int");
        prepare(&mut ir).unwrap();
        assert_eq!(ir.behavior.instance_guards, [InstanceGuard { source: "orm.locking".into(), model: "Post".into(), field: "version".into() }]);
    }

    #[test]
    fn the_version_field_is_incremented_on_every_write_kind_and_guards_instances() {
        let mut ir = schema("int");
        prepare(&mut ir).unwrap();
        let a = &ir.behavior.write_assignments;
        assert_eq!(a.len(), 1);
        assert_eq!((a[0].source.as_str(), a[0].model.as_str(), a[0].field.as_str(), a[0].value, a[0].upsert), ("orm.locking", "Post", "version", AssignValue::Increment, UpsertDefault::Drop));
        assert_eq!(a[0].on, [WriteKind::Update, WriteKind::Upsert, WriteKind::DeleteRewrite]);
        assert_eq!(ir.behavior.instance_guards, [InstanceGuard { source: "orm.locking".into(), model: "Post".into(), field: "version".into() }]);
    }

    #[test]
    fn a_proxy_that_keeps_the_field_gets_the_records_of_its_root() {
        let mut ir = schema("int");
        let mut view: orm_contracts::ir::ModelIr = serde_json::from_value(serde_json::to_value(&ir.models[0]).unwrap()).unwrap();
        view.name = "PostView".into();
        ir.models.push(view);
        ir.behavior.proxy_models.push(orm_contracts::extension::ProxyModel { model: "PostView".into(), parent: "Post".into(), storage_owner: "Post".into(), ..Default::default() });
        prepare(&mut ir).unwrap();
        let models: Vec<_> = ir.behavior.write_assignments.iter().map(|a| a.model.as_str()).collect();
        assert_eq!(models, ["Post", "PostView"]);
        assert_eq!(ir.behavior.instance_guards.iter().map(|g| g.model.as_str()).collect::<Vec<_>>(), ["Post", "PostView"]);
    }

    #[test]
    fn a_proxy_in_a_later_batch_gets_the_records_of_a_root_in_the_context() {
        let mut ir = schema("int");
        prepare(&mut ir).unwrap();
        ir.behavior.declarations.clear();
        let mut view: orm_contracts::ir::ModelIr = serde_json::from_value(serde_json::to_value(&ir.models[0]).unwrap()).unwrap();
        view.name = "PostView".into();
        ir.models.push(view);
        ir.behavior.proxy_models.push(orm_contracts::extension::ProxyModel { model: "PostView".into(), parent: "Post".into(), storage_owner: "Post".into(), ..Default::default() });
        prepare(&mut ir).unwrap();
        prepare(&mut ir).unwrap();
        let models: Vec<_> = ir.behavior.write_assignments.iter().map(|a| a.model.as_str()).collect();
        assert_eq!(models, ["Post", "PostView"]);
        assert_eq!(ir.behavior.instance_guards.iter().map(|g| g.model.as_str()).collect::<Vec<_>>(), ["Post", "PostView"]);
    }

    #[test]
    fn a_proxy_must_keep_the_version_field() {
        let mut ir = schema("int");
        let mut view: orm_contracts::ir::ModelIr = serde_json::from_value(serde_json::to_value(&ir.models[0]).unwrap()).unwrap();
        view.name = "PostView".into();
        view.fields.retain(|f| f.name != "version");
        ir.models.push(view);
        ir.behavior.proxy_models.push(orm_contracts::extension::ProxyModel { model: "PostView".into(), parent: "Post".into(), storage_owner: "Post".into(), ..Default::default() });
        assert_eq!(prepare(&mut ir).unwrap_err(), "PostView: proxy must keep the version field version");
    }

    #[test]
    fn a_model_has_one_version_field() {
        let mut ir = schema("int");
        let again = ir.behavior.declarations[0].clone();
        ir.behavior.declarations.push(again);
        assert_eq!(prepare(&mut ir).unwrap_err(), "schema.prisma:3:3: @locking.version: Post has more than one version field");
    }

    #[test]
    fn a_version_field_is_an_integer() {
        let err = prepare(&mut schema("string")).unwrap_err();
        assert_eq!(err, "schema.prisma:3:3: @locking.version: Post.version must be a required Int or BigInt field");
    }

    #[test]
    fn a_version_field_in_a_composite_id_is_rejected() {
        let mut ir = schema("int");
        ir.models[0].primary_key = vec!["id".into(), "version".into()];
        let err = prepare(&mut ir).unwrap_err();
        assert_eq!(err, "schema.prisma:3:3: @locking.version: Post.version must not be part of the @@id");
    }
}
