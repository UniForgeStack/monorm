//! `@@scope.by(shop: shop_id, mode: livemode)`: each key names a scope value and the column that holds it.
//! Every key adds a read default (`<column> = scope.<key>`) and a scoped field: the insert fill and the write check.
//! `unscoped("<key>")` turns off both for one key.
use monorm::{extension::{Declaration, ReadDefault}, ir::SchemaIr, tracking::error, writes::ScopedField};
use std::collections::BTreeSet;

/// The `source` of every record this extension emits.
const SOURCE: &str = "monorm.scope";

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

/// Adds the read default and the scoped field of every `@@scope.by` key.
pub fn prepare(ir: &mut SchemaIr) -> Result<(), String> {
    let mut read_defaults = vec![];
    let mut scoped = vec![];
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    for d in ir.behavior.declarations.iter().filter(|d| d.attribute == "scope.by") {
        for (key, field) in declared(ir, d)? {
            if !seen.insert((d.model.clone(), key.clone())) { return Err(error(d, format!("{}: the scope key {key} is declared twice", d.model))); }
            let filter = serde_json::json!({"t": "cmp", "op": "eq", "l": {"t": "col", "path": [], "name": field}, "r": {"t": "scope", "name": key}});
            read_defaults.push(ReadDefault { source: SOURCE.into(), name: key.clone(), model: d.model.clone(), filter: Some(filter), ..Default::default() });
            scoped.push(ScopedField { source: SOURCE.into(), model: d.model.clone(), field, key });
        }
    }
    ir.behavior.read_defaults.extend(read_defaults);
    ir.behavior.scoped_fields.extend(scoped);
    Ok(())
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
}

monorm::extension!(("monorm.scope.prepare", prepare));
