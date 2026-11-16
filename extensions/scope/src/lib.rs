//! `@@scope.by(shop: shop_id, mode: livemode)`: each key names a scope value and the column that holds it.
//! Every key adds a read default (`<column> = scope.<key>`) and a scoped field: the insert fill and the write check.
//! `unscoped("<key>")` turns off both for one key.
//! On PostgreSQL the same keys also give the table row-level security: the policy `monorm_scope`, a fill trigger and
//! a read-only policy for each exempt role. A level table of a composed model joins to its parent.
use monorm::{
    dialect::Dialect,
    extension::{Declaration, ReadDefault},
    ir::{ColType, ModelIr, PolicyCommand, PolicyIr, RlsIr, SchemaIr, TriggerEvent, TriggerIr, TriggerTiming},
    sql::{column_type, ident, literal},
    tracking::{composed_levels, error},
    writes::{PublishedScopeKey, ScopedField},
};
use std::collections::{BTreeMap, BTreeSet};

/// The `source` of every record this extension emits.
const SOURCE: &str = "monorm.scope";
/// The one policy of a scoped table. PostgreSQL ORs permissive policies, so every key and the bypass sit in this one.
pub const POLICY: &str = "monorm_scope";
/// The trigger that fills a NULL scope column from the setting. It has no `skippable` name: no block switches it off.
pub const FILL: &str = "monorm_scope_fill";
/// The policy name of an exempt role is this prefix and the role name.
pub const EXEMPT_PREFIX: &str = "monorm_scope_exempt_";

/// One scope key and the column of a physical table that holds it.
#[derive(Clone)]
struct Key {
    name: String,
    column: String,
    sql_type: String,
}

/// `(SELECT NULLIF(current_setting('scope.<key>', true), '')::<type>)`: the sub-select makes the read run once per statement.
fn setting(key: &Key) -> String {
    format!("(SELECT NULLIF(current_setting({}, true), '')::{})", literal(&format!("scope.{}", key.name)), key.sql_type)
}

/// `current_user` is a member of one of the roles. The catalog lookup keeps a role that does not exist from raising an error:
/// `pg_has_role` with a name fails for an unknown role, and the policy runs for every statement.
fn member_of(roles: &[String]) -> String {
    let names = roles.iter().map(|r| literal(r)).collect::<Vec<_>>().join(", ");
    format!("(SELECT EXISTS (SELECT 1 FROM pg_roles WHERE rolname IN ({names}) AND pg_has_role(current_user, oid, 'member')))")
}

/// The term of one key: the column equals the setting, or an open `unscoped` bypass of a bypass role.
fn term(key: &Key, bypass_roles: &[String]) -> String {
    let equal = format!("{} = {}", ident(&key.column), setting(key));
    if bypass_roles.is_empty() { return format!("({equal})"); }
    let open = format!("(SELECT {} = ANY(NULLIF(current_setting('monorm.bypass', true), '')::text[]))", literal(&key.name));
    format!("({equal} OR ({open} AND {}))", member_of(bypass_roles))
}

fn expression(keys: &[Key], bypass_roles: &[String]) -> String {
    keys.iter().map(|k| term(k, bypass_roles)).collect::<Vec<_>>().join(" AND ")
}

fn policy(expression: String) -> PolicyIr {
    PolicyIr { name: POLICY.into(), command: PolicyCommand::All, using: Some(expression.clone()), with_check: Some(expression), ..Default::default() }
}

/// A read-only policy for each exempt role; the migration creates it only when the role exists.
fn exempt_policies(roles: &[String]) -> Vec<PolicyIr> {
    roles.iter().map(|r| PolicyIr {
        name: format!("{EXEMPT_PREFIX}{r}"), command: PolicyCommand::Select, roles: vec![r.clone()],
        using: Some("true".into()), if_roles_exist: true, ..Default::default()
    }).collect()
}

/// The fill trigger: for each key, a NULL column takes the setting.
fn fill(keys: &[Key]) -> TriggerIr {
    let steps: String = keys.iter().map(|k| format!("IF NEW.{c} IS NULL THEN NEW.{c} := {s}; END IF; ", c = ident(&k.column), s = setting(k))).collect();
    TriggerIr { name: FILL.into(), timing: TriggerTiming::Before, events: vec![TriggerEvent::Insert], body: Some(format!("BEGIN {steps}RETURN NEW; END;")), ..Default::default() }
}

fn secure(model: &mut ModelIr, policy: PolicyIr, exempt: Vec<PolicyIr>) -> Result<(), String> {
    model.rls = RlsIr { enabled: true, forced: true };
    for p in std::iter::once(policy).chain(exempt) { model.push_policy(p)?; }
    Ok(())
}

/// The physical model `name`: the storage snapshot first, then the logical list.
fn model_of<'a>(ir: &'a SchemaIr, name: &str) -> Option<&'a ModelIr> {
    ir.behavior.storage.iter().flat_map(|s| &s.models).chain(&ir.models).find(|m| m.name == name)
}

fn declared(ir: &SchemaIr, d: &Declaration) -> Result<Vec<(String, String)>, String> {
    if d.field.is_some() { return Err(error(d, "@@scope.by goes on a model, not on a field")); }
    if !d.positional.is_empty() { return Err(error(d, format!("{}: @@scope.by takes keys, not positional values: @@scope.by(shop: shop_id)", d.model))); }
    if d.arguments.is_empty() { return Err(error(d, format!("{}: @@scope.by needs at least one key: @@scope.by(shop: shop_id)", d.model))); }
    let model = ir.models.iter().find(|m| m.name == d.model).ok_or_else(|| error(d, format!("unknown model {}", d.model)))?;
    let mut out = vec![];
    for (key, value) in &d.arguments {
        let Some(field) = value.as_str() else { return Err(error(d, format!("{}: @@scope.by({key}:) names a field of the model: {key}: {key}_id", d.model))) };
        if model.fields.iter().all(|f| f.name != field) { return Err(error(d, format!("{} has no field {field}, named by @@scope.by({key}:)", d.model))); }
        out.push((key.clone(), field.to_owned()));
    }
    Ok(out)
}

/// The model that holds the column of `field` on `model`: a composed model reads an inherited field from its owner.
fn owner_of(ir: &SchemaIr, model: &str, field: &str) -> (String, Option<String>) {
    match ir.behavior.field_storage.iter().find(|f| f.model == model && f.field == field) {
        Some(f) => (f.owner.clone(), Some(f.column.clone())),
        None => (model.to_owned(), None),
    }
}

/// The policy of each level table of a composed model: its row is visible when the row of its parent is.
/// A proxy has no table of its own, so it gets none.
fn level_policies(ir: &mut SchemaIr, exempt: &[String]) -> Result<(), String> {
    loop {
        let mut added = false;
        for link in ir.behavior.owner_links.clone() {
            let Some(parent) = model_of(ir, &link.parent).filter(|m| m.policies.iter().any(|p| p.name == POLICY)) else { continue };
            if model_of(ir, &link.child).is_none_or(|m| m.policies.iter().any(|p| p.name == POLICY)) { continue; }
            let column = |m: &ModelIr, field: &str| m.fields.iter().find(|f| f.name == field).map(|f| f.column.clone());
            let child = model_of(ir, &link.child).ok_or("unknown level table")?;
            let (Some(parent_column), Some(child_column)) = (column(parent, &link.parent_key), column(child, &link.child_key)) else {
                return Err(format!("{}: the key {} of the composition has no column", link.child, link.child_key));
            };
            let on = format!("EXISTS (SELECT 1 FROM {} r WHERE r.{} = {}.{})", ident(&parent.table), ident(&parent_column), ident(&child.table), ident(&child_column));
            for model in ir.models_named_mut(&link.child) { secure(model, policy(on.clone()), exempt_policies(exempt))?; }
            added = true;
        }
        if !added { return Ok(()); }
    }
}

/// Adds the read default and the scoped field of every `@@scope.by` key, and on PostgreSQL its row-level security.
pub fn prepare(ir: &mut SchemaIr) -> Result<(), String> {
    let mut read_defaults = vec![];
    let mut scoped = vec![];
    let mut secured: BTreeMap<String, BTreeMap<String, Key>> = BTreeMap::new();
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    let proxies: BTreeSet<String> = ir.behavior.model_views.iter().map(|v| v.model.clone()).collect();
    for d in ir.behavior.declarations.iter().filter(|d| d.attribute == "scope.by" && !d.lowered) {
        for (key, field) in declared(ir, d)? {
            if !seen.insert((d.model.clone(), key.clone())) { return Err(error(d, format!("{}: the scope key {key} is declared twice", d.model))); }
            if ir.dialect == Dialect::Postgres && !proxies.contains(&d.model) {
                let model = ir.models.iter().find(|m| m.name == d.model).ok_or_else(|| error(d, "unknown model"))?;
                let f = model.fields.iter().find(|f| f.name == field).ok_or_else(|| error(d, format!("{} has no field {field}", d.model)))?;
                if f.array || f.ty == ColType::Json { return Err(error(d, format!("{}: the scope field {field} must be a scalar column", d.model))); }
                let (owner, column) = owner_of(ir, &d.model, &field);
                let sql_type = f.db_type.clone().unwrap_or_else(|| column_type(f.ty, f.max_length, false));
                let name = key.clone();
                secured.entry(owner).or_default().insert(key.clone(), Key { name, column: column.unwrap_or_else(|| f.column.clone()), sql_type });
            }
            let filter = serde_json::json!({"t": "cmp", "op": "eq", "l": {"t": "col", "path": [], "name": field}, "r": {"t": "scope", "name": key}});
            // A level below a composed root reads the scope column it inherits from the root.
            let levels: Vec<String> = composed_levels(ir, &d.model).iter().map(|l| l.child.clone()).collect();
            for child in levels {
                read_defaults.push(ReadDefault { source: SOURCE.into(), name: key.clone(), model: child, filter: Some(filter.clone()), ..Default::default() });
            }
            read_defaults.push(ReadDefault { source: SOURCE.into(), name: key.clone(), model: d.model.clone(), filter: Some(filter), ..Default::default() });
            scoped.push(ScopedField { source: SOURCE.into(), model: d.model.clone(), field, key });
        }
    }
    ir.behavior.read_defaults.extend(read_defaults);
    ir.behavior.scoped_fields.extend(scoped);
    if ir.dialect != Dialect::Postgres { return Ok(()); }
    let (bypass, exempt) = (ir.bypass_roles.clone(), ir.exempt_roles.clone());
    let published: BTreeSet<&String> = secured.values().flat_map(|keys| keys.keys()).collect();
    let published: Vec<_> = published.into_iter().map(|key| PublishedScopeKey { source: SOURCE.into(), key: key.clone() }).collect();
    ir.behavior.published_scope_keys.extend(published);
    for (owner, keys) in secured {
        let keys: Vec<Key> = keys.into_values().collect();
        for model in ir.models_named_mut(&owner) {
            secure(model, policy(expression(&keys, &bypass)), exempt_policies(&exempt))?;
            model.push_trigger(fill(&keys))?;
        }
    }
    level_policies(ir, &exempt)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(arguments: serde_json::Value, field: Option<&str>) -> SchemaIr {
        let mut ir: SchemaIr = serde_json::from_value(serde_json::json!({
            "dialect": "postgres",
            "models": [{"name": "Order", "table": "orders", "fields": [
                {"name": "id", "column": "id", "type": "int", "primary_key": true},
                {"name": "shop_id", "column": "shop_id", "type": "int"},
                {"name": "livemode", "column": "livemode", "type": "bool"}
            ]}],
            "behavior": {"schema_contract": 5}
        })).unwrap();
        ir.behavior.declarations = vec![serde_json::from_value(serde_json::json!({"attribute": "scope.by", "model": "Order", "field": field, "positional": [], "arguments": arguments, "location": {"file": "schema.prisma", "line": 4, "column": 3}})).unwrap()];
        ir
    }

    #[test]
    fn a_key_adds_a_read_default_and_a_scoped_field() {
        let mut ir = schema(serde_json::json!({"shop": "shop_id", "mode": "livemode"}), None);
        prepare(&mut ir).unwrap();
        let keys: Vec<_> = ir.behavior.scoped_fields.iter().map(|s| (s.key.as_str(), s.field.as_str())).collect();
        assert_eq!(keys, [("mode", "livemode"), ("shop", "shop_id")]);
        let shop = ir.behavior.read_defaults.iter().find(|d| d.name == "shop").unwrap();
        assert_eq!((shop.source.as_str(), shop.model.as_str()), (SOURCE, "Order"));
        assert_eq!(shop.filter.as_ref().unwrap(), &serde_json::json!({"t": "cmp", "op": "eq", "l": {"t": "col", "path": [], "name": "shop_id"}, "r": {"t": "scope", "name": "shop"}}));
    }

    #[test]
    fn the_filter_parses_as_an_expression() {
        let mut ir = schema(serde_json::json!({"shop": "shop_id"}), None);
        prepare(&mut ir).unwrap();
        let expr: monorm::ir::Expr = serde_json::from_value(ir.behavior.read_defaults[0].filter.clone().unwrap()).unwrap();
        assert!(matches!(expr, monorm::ir::Expr::Cmp { op: monorm::ir::CmpOp::Eq, ref r, .. } if matches!(**r, monorm::ir::Expr::Scope { ref name } if name == "shop")));
    }

    #[test]
    fn a_wrong_declaration_names_its_position() {
        let cases = [
            (serde_json::json!({"shop": "missing"}), None, "Order has no field missing, named by @@scope.by(shop:)"),
            (serde_json::json!({}), None, "@@scope.by needs at least one key"),
            (serde_json::json!({"shop": 7}), None, "@@scope.by(shop:) names a field of the model"),
            (serde_json::json!({"shop": "shop_id"}), Some("shop_id"), "goes on a model, not on a field"),
        ];
        for (arguments, field, message) in cases {
            let err = prepare(&mut schema(arguments, field)).unwrap_err();
            assert!(err.contains("schema.prisma:4") && err.contains(message), "{err}");
        }
    }

    fn secured(roles: serde_json::Value, exempt: serde_json::Value) -> SchemaIr {
        let mut ir = schema(serde_json::json!({"shop": "shop_id"}), None);
        ir.bypass_roles = serde_json::from_value(roles).unwrap();
        ir.exempt_roles = serde_json::from_value(exempt).unwrap();
        ir
    }

    #[test]
    fn postgres_gets_the_policy_the_flags_and_the_fill_trigger() {
        let mut ir = secured(serde_json::json!(["admin"]), serde_json::json!([]));
        prepare(&mut ir).unwrap();
        let m = &ir.models[0];
        assert!(m.rls.enabled && m.rls.forced);
        let p = &m.policies[0];
        let eq = "\"shop_id\" = (SELECT NULLIF(current_setting('scope.shop', true), '')::integer)";
        let open = "(SELECT 'shop' = ANY(NULLIF(current_setting('monorm.bypass', true), '')::text[]))";
        let member = "(SELECT EXISTS (SELECT 1 FROM pg_roles WHERE rolname IN ('admin') AND pg_has_role(current_user, oid, 'member')))";
        let expected = format!("({eq} OR ({open} AND {member}))");
        assert_eq!((p.name.as_str(), p.using.as_deref(), p.with_check.as_deref()), (POLICY, Some(expected.as_str()), Some(expected.as_str())));
        let t = &m.triggers[0];
        assert_eq!(t.name, FILL);
        assert!(t.body.as_deref().unwrap().contains("IF NEW.\"shop_id\" IS NULL THEN NEW.\"shop_id\" := (SELECT NULLIF(current_setting('scope.shop', true), '')::integer)"));
    }

    #[test]
    fn no_bypass_role_leaves_the_bypass_branch_out() {
        let mut ir = secured(serde_json::json!([]), serde_json::json!([]));
        prepare(&mut ir).unwrap();
        let using = ir.models[0].policies[0].using.clone().unwrap();
        assert!(!using.contains("monorm.bypass") && !using.contains(" OR "), "{using}");
    }

    #[test]
    fn each_key_is_one_term_and_each_exempt_role_a_select_policy() {
        let mut ir = secured(serde_json::json!([]), serde_json::json!(["auditor"]));
        ir.behavior.declarations.push(serde_json::from_value(serde_json::json!({"attribute": "scope.by", "model": "Order", "field": null, "positional": [], "arguments": {"mode": "livemode"}, "location": {"file": "schema.prisma", "line": 5, "column": 3}})).unwrap());
        prepare(&mut ir).unwrap();
        let m = &ir.models[0];
        assert_eq!(m.policies.len(), 2);
        let using = m.policies[0].using.clone().unwrap();
        assert!(using.contains("scope.mode") && using.contains("scope.shop") && using.contains(" AND "), "{using}");
        assert_eq!(m.triggers.iter().filter(|t| t.name == FILL).count(), 1);
        let e = &m.policies[1];
        assert_eq!((e.name.as_str(), e.command, e.roles.as_slice(), e.using.as_deref(), e.if_roles_exist), ("monorm_scope_exempt_auditor", PolicyCommand::Select, ["auditor".to_string()].as_slice(), Some("true"), true));
    }

    #[test]
    fn each_level_table_joins_its_direct_parent_and_the_exempt_roles_read_it() {
        let mut ir = secured(serde_json::json!(["admin"]), serde_json::json!(["auditor"]));
        let level = |name: &str, table: &str, column: &str| serde_json::json!({"name": name, "table": table, "fields": [
            {"name": "id", "column": "id", "type": "int", "primary_key": true},
            {"name": column, "column": column, "type": "int"}
        ]});
        ir.models.push(serde_json::from_value(level("Line", "lines", "order_id")).unwrap());
        ir.models.push(serde_json::from_value(level("Note", "notes", "line_id")).unwrap());
        ir.behavior.owner_links = serde_json::from_value(serde_json::json!([
            {"child": "Note", "parent": "Line", "child_key": "line_id", "parent_key": "id"},
            {"child": "Line", "parent": "Order", "child_key": "order_id", "parent_key": "id"}
        ])).unwrap();
        prepare(&mut ir).unwrap();
        for (index, (parent, child, column)) in [("orders", "lines", "order_id"), ("lines", "notes", "line_id")].into_iter().enumerate() {
            let m = &ir.models[index + 1];
            let on = format!("EXISTS (SELECT 1 FROM \"{parent}\" r WHERE r.\"id\" = \"{child}\".\"{column}\")");
            assert!(m.rls.enabled && m.rls.forced, "{child}");
            assert_eq!((m.policies[0].name.as_str(), m.policies[0].using.as_deref(), m.policies[0].with_check.as_deref()), (POLICY, Some(on.as_str()), Some(on.as_str())), "{child}");
            assert_eq!(m.policies[1].name, "monorm_scope_exempt_auditor", "{child}");
        }
    }

    #[test]
    fn sqlite_gets_no_row_security() {
        let mut ir = secured(serde_json::json!(["admin"]), serde_json::json!(["auditor"]));
        ir.dialect = Dialect::Sqlite;
        prepare(&mut ir).unwrap();
        let m = &ir.models[0];
        assert!(m.rls.is_off() && m.policies.is_empty() && m.triggers.is_empty());
        assert_eq!(ir.behavior.scoped_fields.len(), 1);
    }

    #[test]
    fn row_security_changes_only_its_declared_effects_once() {
        let effects = monorm::extension::pass_effects(&secured(serde_json::json!(["admin"]), serde_json::json!(["auditor"])), prepare).unwrap();
        assert_eq!(effects.into_iter().collect::<Vec<_>>(), ["behavior.published_scope_keys", "behavior.read_defaults", "behavior.scoped_fields", "behavior.storage", "models"]);
        let mut ir = secured(serde_json::json!(["admin"]), serde_json::json!([]));
        prepare(&mut ir).unwrap();
        assert_eq!(ir.behavior.published_scope_keys.iter().map(|k| k.key.as_str()).collect::<Vec<_>>(), ["shop"]);
        let mut sqlite = schema(serde_json::json!({"shop": "shop_id"}), None);
        sqlite.dialect = Dialect::Sqlite;
        prepare(&mut sqlite).unwrap();
        assert!(sqlite.behavior.published_scope_keys.is_empty());
    }
}

monorm::extension!(("monorm.scope.prepare", prepare));
