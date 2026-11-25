//! The built-in resources on the auth and admin tables: roles with their permission names, the role membership of users,
//! the permissions, and the read-only audit log. They follow the permission strategy of the site like every resource.

use monorm_core::ir::RelKind;
use monorm_core::schema::Schema;
use serde_json::{json, Value};

use super::{concrete};
use crate::site::Queryset;
use crate::site_config::{ResourceConfig};

/// The navigation group of the built-in pages (the audit log, the roles, the permissions and the users). A page that the config gives another group keeps it.
pub(super) const GROUP: &str = "Users and access";

/// What `apply` added that the build finishes after the routes exist.
#[derive(Default)]
pub(super) struct Added {
    /// The index of the `Role` resource whose `permissions` picker lists the permission names of the site.
    pub role: Option<usize>,
}

fn has(schema: &Schema, name: &str) -> bool {
    schema.model_idx(name).is_ok_and(|i| concrete(schema.model(i)))
}

fn grouped(config: &mut ResourceConfig) {
    config.group.get_or_insert_with(|| GROUP.to_owned());
}

fn position(configs: &mut Vec<ResourceConfig>, model: &str) -> usize {
    match configs.iter().position(|c| c.model == model) {
        Some(i) => i,
        None => {
            configs.push(ResourceConfig::of(model));
            configs.len() - 1
        }
    }
}

/// Adds the built-in resources that the schema has a model for, and sets what they need. A page that the config already has keeps its settings.
pub(super) fn apply(schema: &Schema, configs: &mut Vec<ResourceConfig>) -> Added {
    let mut added = Added::default();
    if has(schema, "AdminLogEntry") {
        let i = position(configs, "AdminLogEntry");
        configs[i].allow = Some(vec!["read".to_owned()]);
        configs[i].bulk_delete = false;
        // The History link of a detail page opens this list filtered on these two fields, so a list with its own filters keeps them and gains these.
        for name in ["resource", "record_key"] {
            if !configs[i].list.filters.iter().any(|f| f == name) {
                configs[i].list.filters.push(name.to_owned());
            }
        }
        grouped(&mut configs[i]);
    }
    if !(has(schema, "Role") && has(schema, "Permission") && has(schema, "RolePermission")) {
        return added;
    }
    let p = position(configs, "Permission");
    grouped(&mut configs[p]);
    if configs[p].list.search.is_none() {
        // `label` leads the unique index of the table, so it is the one search field that an index serves.
        configs[p].list.search = Some(vec!["label".to_owned()]);
    }
    let r = position(configs, "Role");
    grouped(&mut configs[r]);
    if configs[r].list.search.is_none() {
        configs[r].list.search = Some(vec!["name".to_owned()]);
    }
    if !configs[r].relations.contains_key("permissions") {
        configs[r].relations.insert("permissions".to_owned(), crate::relations::picker("name", Some(json!({"t": "const", "value": true}))));
        added.role = Some(r);
    }
    membership(schema, configs);
    added
}

/// The page of the join model of `User.roles`: a picker for the user and one for the role.
fn membership(schema: &Schema, configs: &mut Vec<ResourceConfig>) {
    let Some(role) = schema.model_idx("Role").ok() else { return };
    for m in &schema.models {
        // `Permission.roles` has the same shape; only a model with a `username` is the user.
        if !m.ir.fields.iter().any(|f| f.name == "username") {
            continue;
        }
        let Some(rel) = m.ir.relations.iter().find(|r| r.name == "roles" && r.kind == RelKind::Many && r.through.is_some()) else { continue };
        if !matches!(m.relation("roles"), Ok((_, t)) if t == role) || !concrete(m) {
            continue;
        }
        let Some(through) = &rel.through else { continue };
        if !has(schema, &through.model) {
            continue;
        }
        let join = schema.model(schema.model_idx(&through.model).expect("checked by has"));
        let picker_of = |field: &str| join.ir.relations.iter().find(|r| r.kind == RelKind::One && r.from == [field]).map(|r| r.name.clone());
        let user_name = m.ir.name.clone();
        let j = position(configs, &through.model);
        grouped(&mut configs[j]);
        if let Some(name) = picker_of(&through.target) {
            configs[j].relations.entry(name).or_insert_with(|| crate::relations::picker("name", None));
        }
        let user_listed = configs.iter().any(|c| c.model == user_name);
        if let (Some(name), true) = (picker_of(&through.source), user_listed) {
            let u = position(configs, &user_name);
            grouped(&mut configs[u]);
            if configs[u].list.search.is_none() {
                configs[u].list.search = Some(vec!["username".to_owned()]);
            }
            configs[j].relations.entry(name).or_insert_with(|| crate::relations::picker("username", None));
        }
    }
}

/// The filter that offers the permissions of `names` (`<label>.<codename>`) and no other row.
pub(super) fn names_filter(names: &[String]) -> Queryset {
    let mut labels: Vec<(&str, Vec<&str>)> = Vec::new();
    for n in names {
        let Some((label, codename)) = n.split_once('.') else { continue };
        match labels.iter_mut().find(|(l, _)| *l == label) {
            Some((_, codenames)) => codenames.push(codename),
            None => labels.push((label, vec![codename])),
        }
    }
    let col = |name: &str| json!({"t": "col", "path": [], "name": name});
    let alternatives: Vec<Value> = labels
        .iter()
        .map(|(label, codenames)| {
            let values: Vec<Value> = codenames.iter().map(|c| json!({"t": "text", "value": c})).collect();
            json!({"t": "and", "items": [
                {"t": "cmp", "op": "eq", "l": col("label"), "r": {"t": "text", "value": label}},
                {"t": "in", "item": col("codename"), "values": values},
            ]})
        })
        .collect();
    let filter = match alternatives.len() {
        0 => json!({"t": "const", "value": false}),
        1 => alternatives.into_iter().next().expect("one"),
        _ => json!({"t": "or", "items": alternatives}),
    };
    Queryset { filter: Some(filter), slots: Vec::new(), function: None }
}
