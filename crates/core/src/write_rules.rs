//! Validates neutral write rules and builds their database triggers.

use crate::dialect::Dialect;
use crate::dsl::reads_row;
use crate::ir::{ChangeIr, ChangeKind, ModelIr, SchemaIr, TriggerEvent, TriggerIr};
use crate::migrate::model::object_name;
use monorm::sql::literal;
use monorm::writes::{
    fingerprint, WriteAction, WriteLayer, WriteRule, WRITE_PROTECTED_PREFIX, WRITE_PROTECTED_SQLSTATE,
};
use serde::{Deserialize, Serialize};

pub(crate) fn prepare(ir: &mut SchemaIr) -> Result<(), String> {
    for rule in &ir.behavior.write_rules {
        if let Some(view) = ir
            .behavior
            .model_views
            .iter()
            .find(|v| v.model == rule.model)
        {
            return Err(format!(
                "{}: a proxy cannot declare a write rule; protect the root model {}",
                rule.model, view.root
            ));
        }
        let models = ir
            .behavior
            .storage
            .as_mut()
            .map_or(&mut ir.models, |storage| &mut storage.models);
        let model = models
            .iter_mut()
            .find(|m| m.name == rule.model)
            .ok_or_else(|| {
                format!(
                    "{}: write rule {} names an unknown storage model",
                    rule.model, rule.name
                )
            })?;
        if !model.managed && rule.layer.db() {
            return Err(format!("extension {} adds database write protection for unmanaged model {}", rule.source, model.name));
        }
        validate(ir.dialect, model, rule)?;
        if rule.layer.db() && !(ir.dialect == Dialect::Sqlite && rule.skippable) {
            for trigger in triggers(ir.dialect, model, rule, &ir.bypass_roles)? {
                model.push_trigger(trigger)?;
            }
        }
    }
    Ok(())
}

fn validate(dialect: Dialect, model: &ModelIr, rule: &WriteRule) -> Result<(), String> {
    use TriggerEvent::{Delete, Insert, Truncate, Update};
    let error = |reason: &str| {
        Err(format!(
            "{}: write rule {}: {reason}",
            model.name, rule.name
        ))
    };
    if rule.name.is_empty()
        || !rule
            .name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return error("the name must contain only letters, digits and underscores");
    }
    if model.sql_view.is_some() {
        return error("a write rule needs a storage table");
    }
    if rule.operations.is_empty() || rule.operations.contains(&Truncate) {
        return error("operation needs insert, update, update_of(...) or delete");
    }
    if !rule.fields.is_empty() && !rule.operations.contains(&Update) {
        return error("fields need an update operation");
    }
    for field in &rule.fields {
        if !model.fields.iter().any(|f| f.name == *field) {
            return error(&format!("unknown field {field}"));
        }
    }
    if rule.changed
        && (rule.fields.is_empty() || rule.operations.iter().any(|operation| *operation != Update))
    {
        return error("value-change protection needs an update operation and at least one field");
    }
    if rule.layer.app() && rule.condition.is_some() {
        return error("condition requires layer: db");
    }
    if rule.layer.app() && rule.action == WriteAction::Drop {
        return error("action: drop requires layer: db");
    }
    if dialect == Dialect::Sqlite && rule.layer == WriteLayer::Db && rule.skippable {
        return error("a skippable database-only rule requires PostgreSQL");
    }
    if let Some(cond) = &rule.condition {
        if !cond.changes.is_empty() && rule.operations.iter().any(|e| *e != Update) {
            return error("change helpers require update or update_of(...)");
        }
        if let Some(sql) = &cond.sql {
            if (rule.operations.contains(&Insert) && reads_row(sql, "old"))
                || (rule.operations.contains(&Delete) && reads_row(sql, "new"))
            {
                return error("the condition reads a row unavailable for one of its operations");
            }
        }
    }
    Ok(())
}

/// Metadata retained by the database in a generated trigger body. Field references
/// are stored as column names so pull can assign fresh logical names to them.
#[derive(Deserialize, Serialize)]
pub(crate) struct DeclarationMarker {
    pub rule: WriteRule,
    pub roles: Vec<String>,
}

const MARKER: &str = "/* monorm write rule ";

impl DeclarationMarker {
    pub fn read(body: &str) -> Option<Self> {
        let (json, _) = body.trim_start().strip_prefix(MARKER)?.split_once(" */")?;
        serde_json::from_str(json).ok()
    }

    fn comment(model: &ModelIr, rule: &WriteRule, roles: &[String]) -> Result<String, String> {
        let roles = if rule.skippable {
            roles.to_vec()
        } else {
            vec![]
        };
        if rule.declaration.is_none() && roles.is_empty() {
            return Ok(String::new());
        }
        let mut rule = rule.clone();
        rule.model.clear();
        let column = |name: &String| {
            model
                .fields
                .iter()
                .find(|f| f.name == *name)
                .map(|f| f.column.clone())
                .ok_or_else(|| {
                    format!(
                        "{}: write rule {} names unknown field {name}",
                        model.name, rule.name
                    )
                })
        };
        if let Some(d) = &mut rule.declaration {
            d.field = d.field.as_ref().map(column).transpose()?;
        }
        rule.fields = rule.fields.iter().map(column).collect::<Result<_, _>>()?;
        if let Some(condition) = &mut rule.condition {
            for change in &mut condition.changes {
                change.fields = change.fields.iter().map(column).collect::<Result<_, _>>()?;
                change.exclude = change
                    .exclude
                    .iter()
                    .map(column)
                    .collect::<Result<_, _>>()?;
            }
        }
        let json = serde_json::to_string(&Self { rule, roles }).map_err(|e| e.to_string())?;
        Ok(format!("{MARKER}{} */\n", json.replace('*', "\\u002a")))
    }
}

pub(crate) fn skip_name(model: &ModelIr, rule: &WriteRule) -> String {
    // Use only array-safe ASCII bytes, including when the physical table was mapped.
    let fingerprint = fingerprint(model.table.bytes().chain([0]).chain(rule.name.bytes()));
    format!("rule_{fingerprint}")
}

pub(crate) fn sqlite_trigger_name(name: &str, operation: TriggerEvent) -> Option<String> {
    let suffix = match operation {
        TriggerEvent::Insert => "ins",
        TriggerEvent::Update => "upd",
        TriggerEvent::Delete => "del",
        TriggerEvent::Truncate => return None,
    };
    Some(object_name(&[name, suffix]))
}

fn triggers(
    dialect: Dialect,
    model: &ModelIr,
    rule: &WriteRule,
    roles: &[String],
) -> Result<Vec<TriggerIr>, String> {
    use TriggerEvent::Update;
    let mut events = rule.operations.clone();
    events.sort();
    events.dedup();
    let groups = match dialect {
        Dialect::Postgres => vec![(object_name(&[&rule.name]), events)],
        Dialect::Sqlite => events
            .into_iter()
            .filter_map(|event| {
                sqlite_trigger_name(&rule.name, event).map(|name| (name, vec![event]))
            })
            .collect(),
    };
    let comment = DeclarationMarker::comment(model, rule, roles)?;
    Ok(groups.into_iter().map(|(name, events)| {
        let when = rule.condition.as_ref().and_then(|c| c.sql.clone());
        let mut changes = rule.condition.as_ref().map_or_else(Vec::new, |c| c.changes.clone());
        if rule.changed {
            changes.push(ChangeIr { kind: ChangeKind::AnyChange, fields: rule.fields.clone(), exclude: vec![], exclude_auto: false });
        }
        let message = literal(&format!("{WRITE_PROTECTED_PREFIX}{}", rule.name));
        let body = match (dialect, rule.action) {
            (Dialect::Postgres, WriteAction::Refuse) => format!("BEGIN RAISE EXCEPTION USING ERRCODE = '{WRITE_PROTECTED_SQLSTATE}', MESSAGE = {message}; END;"),
            (Dialect::Postgres, WriteAction::Drop) => "BEGIN RETURN NULL; END;".into(),
            (Dialect::Sqlite, WriteAction::Refuse) => format!("SELECT RAISE(ABORT, {message});"),
            (Dialect::Sqlite, WriteAction::Drop) => "SELECT RAISE(IGNORE);".into(),
        };
        TriggerIr {
            name, update_of: if events.contains(&Update) && !rule.changed { rule.fields.clone() } else { vec![] },
            events, when, changes, body: Some(format!("{comment}{body}")),
            guard: rule.skippable,
            skippable: rule.skippable.then(|| skip_name(model, rule)),
            ..Default::default()
        }
    }).collect())
}
