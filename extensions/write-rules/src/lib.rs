//! Lowers the schema's model and field declarations to neutral write rules.

use monorm::extension::Declaration;
use monorm::ir::{SchemaIr, TriggerEvent};
use monorm::writes::WriteRule;
use serde_json::json;

const SOURCE: &str = "monorm.write-rules";

fn rule(ir: &SchemaIr, declaration: &Declaration) -> Result<WriteRule, String> {
    let args = &declaration.arguments;
    let (name, operations, fields, changed) = match declaration.field.as_deref() {
        Some(field) => (
            format!("read_only_{field}"),
            json!([TriggerEvent::Update]),
            json!([field]),
            true,
        ),
        None => {
            let operation = args.get("operation");
            (
                "protect".into(),
                operation.map_or_else(
                    || json!(["insert", "update", "delete"]),
                    |op| op["operations"].clone(),
                ),
                operation.map_or_else(|| json!([]), |op| op["fields"].clone()),
                false,
            )
        }
    };
    let mut rule: WriteRule = serde_json::from_value(json!({
        "source": SOURCE, "model": declaration.model, "name": name,
        "operations": operations, "fields": fields, "changed": changed,
        "layer": args.get("layer").cloned().unwrap_or_else(|| json!("both")),
        "action": args.get("action").cloned().unwrap_or_else(|| json!("refuse")),
        "skippable": args.get("skippable").cloned().unwrap_or(serde_json::Value::Bool(false)),
        "condition": args.get("condition"),
        "declaration": { "attribute": declaration.attribute, "field": declaration.field },
    }))
    .map_err(|error| monorm::tracking::error(declaration, error))?;
    rule.operations.sort();
    let model = ir
        .behavior
        .storage
        .as_ref()
        .map_or(&ir.models, |storage| &storage.models)
        .iter()
        .chain(&ir.models)
        .find(|model| model.name == rule.model);
    let column = |field: &str| {
        model
            .and_then(|m| m.fields.iter().find(|f| f.name == field))
            .map_or_else(|| field.to_owned(), |f| f.column.clone())
    };
    let columns = |fields: &[String]| fields.iter().map(|field| column(field)).collect::<Vec<_>>();
    if let Some(field) = &declaration.field {
        let column = column(field);
        let tag = if column
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            column
        } else {
            fingerprint(column.as_bytes())
        };
        rule.name = format!("read_only_{tag}");
    } else {
        let mut condition = rule.condition.clone();
        if let Some(condition) = &mut condition {
            for change in &mut condition.changes {
                change.fields = columns(&change.fields);
                change.exclude = columns(&change.exclude);
            }
        }
        let key = json!([
            rule.operations,
            columns(&rule.fields),
            condition,
            rule.layer,
            rule.action,
            rule.skippable
        ]);
        rule.name = format!("protect_{}", fingerprint(key.to_string().as_bytes()));
    }
    Ok(rule)
}

fn fingerprint(bytes: &[u8]) -> String {
    let hash = bytes.iter().fold(0xcbf29ce484222325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    });
    format!("{hash:016x}")
}

pub fn prepare(ir: &mut SchemaIr) -> Result<(), String> {
    for declaration in ir.behavior.declarations.iter().filter(|d| {
        !d.lowered
            && matches!(
                d.attribute.as_str(),
                "write_rules.protect" | "write_rules.read_only"
            )
    }) {
        let rule = rule(ir, declaration)?;
        if ir
            .behavior
            .write_rules
            .iter()
            .any(|current| current.model == rule.model && current.name == rule.name)
        {
            return Err(monorm::tracking::error(
                declaration,
                "this write rule is declared twice",
            ));
        }
        ir.behavior.write_rules.push(rule);
    }
    Ok(())
}

monorm::extension!(("monorm.write-rules.prepare", prepare));

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(dialect: &str, declarations: serde_json::Value) -> SchemaIr {
        let mut ir: SchemaIr = serde_json::from_value(json!({
            "dialect": dialect,
            "models": [{"name": "Entry", "table": "entries", "fields": [
                {"name": "id", "column": "id", "type": "int", "primary_key": true},
                {"name": "title", "column": "title", "type": "string"}
            ]}]
        }))
        .unwrap();
        ir.behavior.declarations = serde_json::from_value(declarations).unwrap();
        ir
    }

    fn protect(arguments: serde_json::Value) -> serde_json::Value {
        json!({"attribute": "write_rules.protect", "model": "Entry", "arguments": arguments, "positional": [],
            "location": {"file": "schema.prisma", "line": 1, "column": 1}})
    }

    fn ddl(mut ir: SchemaIr) -> Result<String, String> {
        prepare(&mut ir)?;
        ir.behavior.declarations.clear();
        ir.stamp_expression_records();
        let (ir, schema) = monorm_core::dsl::check(ir)?;
        assert_eq!(ir.behavior.schema_contract_minor, 10);
        Ok(monorm_core::migrate::create_all(&schema)?.join("\n"))
    }

    #[test]
    fn default_and_field_rules_generate_before_triggers() {
        for dialect in ["postgres", "sqlite"] {
            let ir = schema(
                dialect,
                json!([protect(json!({})), {
                    "attribute": "write_rules.read_only", "model": "Entry", "field": "title",
                    "arguments": {}, "positional": [],
                    "location": {"file": "schema.prisma", "line": 2, "column": 1}
                }]),
            );
            let sql = ddl(ir).unwrap();
            match dialect {
                "postgres" => {
                    assert!(sql.contains("BEFORE INSERT OR UPDATE OR DELETE"), "{sql}");
                    assert!(sql.contains("ERRCODE = 'MW001'"), "{sql}");
                    assert!(
                        sql.contains("OLD.\"title\" IS DISTINCT FROM NEW.\"title\""),
                        "{sql}"
                    );
                }
                _ => {
                    assert_eq!(sql.matches("CREATE TRIGGER").count(), 4, "{sql}");
                    assert!(sql.contains("RAISE(ABORT"), "{sql}");
                    assert!(sql.contains("OLD.\"title\" IS NOT NEW.\"title\""), "{sql}");
                }
            }
        }
    }

    #[test]
    fn conditional_update_of_can_drop_rows_in_the_database() {
        let sql = ddl(schema("postgres", json!([protect(json!({
            "layer": "db", "action": "drop", "operation": {"operations": ["update"], "fields": ["title"]},
            "condition": {"changes": [{"kind": "all_change", "fields": ["title"]}]}
        }))]))).unwrap();
        assert!(sql.contains("BEFORE UPDATE OF \"title\""), "{sql}");
        assert!(
            sql.contains("OLD.\"title\" IS DISTINCT FROM NEW.\"title\""),
            "{sql}"
        );
        assert!(sql.contains("RETURN NULL"), "{sql}");
    }

    #[test]
    fn invalid_layers_fail_at_schema_check() {
        for (args, message) in [
            (
                json!({"layer": "app", "condition": {"sql": "true"}}),
                "condition requires layer: db",
            ),
            (json!({"action": "drop"}), "action: drop requires layer: db"),
            (
                json!({"layer": "db", "skippable": true}),
                "a skippable database-only rule requires PostgreSQL",
            ),
        ] {
            let error = ddl(schema("sqlite", json!([protect(args)]))).unwrap_err();
            assert!(error.contains(message), "{error}");
        }
        let sql = ddl(schema(
            "sqlite",
            json!([protect(json!({"skippable": true}))]),
        ))
        .unwrap();
        assert!(!sql.contains("CREATE TRIGGER"), "{sql}");
    }
}
