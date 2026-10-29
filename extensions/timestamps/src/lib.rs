//! `@@timestamps.updated_at`: the model gets an `updated_at` field that the ORM sets on each
//! update. Database mode adds a row trigger so other writers set it too.
use monorm::{dialect::Dialect, ir::{ColType, FieldIr, ModelIr, SchemaIr, TriggerIr}, tracking::{self, error, Tracked}, writes::{AssignValue, WriteAssignment, WriteKind}};

const SOURCE: &str = "monorm.timestamps";
const TRACKED: Tracked = Tracked { attribute: "timestamps.updated_at", field: "updated_at" };

/// Who keeps the column current for writers outside the ORM.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// A row trigger, so every writer gets the behavior.
    Database,
    /// The ORM only; no DDL.
    Application,
}

impl Mode {
    fn parse(value: Option<&serde_json::Value>) -> Result<Self, String> {
        match value.map(|v| v.as_str()) {
            None | Some(Some("database")) => Ok(Self::Database),
            Some(Some("application")) => Ok(Self::Application),
            _ => Err("mode must be \"database\" or \"application\"".into()),
        }
    }
}

/// Runs before the logical passes, so a proxy of the model copies the field too.
pub fn add_fields(ir: &mut SchemaIr) -> Result<(), String> {
    TRACKED.add_fields(ir, FieldIr { nullable: false, default_now: true, ..FieldIr::plain(TRACKED.field, ColType::DateTime) })
}

const SQLITE_NOW: &str = "strftime('%Y-%m-%d %H:%M:%f','now')";

fn ident(s: &str) -> String { format!("\"{}\"", s.replace('"', "\"\"")) }

/// The trigger that sets `column` on an update that leaves it unchanged, so a written value
/// wins. SQLite can't assign `NEW`, so its trigger updates the row after the update.
fn trigger(dialect: Dialect, model: &ModelIr, column: &str) -> Result<TriggerIr, String> {
    let key: Vec<_> = model.fields.iter().filter(|f| f.primary_key).map(|f| format!("{c} = NEW.{c}", c = ident(&f.column))).collect();
    if key.is_empty() { return Err(format!("{} has no primary key", model.name)); }
    let c = ident(column);
    let value = match dialect {
        Dialect::Postgres => serde_json::json!({"timing": "before", "when": format!("NEW.{c} IS NOT DISTINCT FROM OLD.{c}"),
            "body": format!("BEGIN NEW.{c} := now(); RETURN NEW; END;")}),
        // The `WHEN` guard stops the trigger from firing itself under `recursive_triggers`: the
        // write it makes carries the current time, so the second pass sees a changed value.
        Dialect::Sqlite => serde_json::json!({"timing": "after", "when": format!("NEW.{c} IS OLD.{c} AND NEW.{c} IS NOT {SQLITE_NOW}"),
            "body": format!("UPDATE {} SET {c} = {SQLITE_NOW} WHERE {}", ident(&model.table), key.join(" AND "))}),
    };
    let mut value = value;
    value["name"] = format!("updated_at_{column}").into();
    value["events"] = serde_json::json!(["update"]);
    serde_json::from_value(value).map_err(|e| e.to_string())
}

pub fn prepare(ir: &mut SchemaIr) -> Result<(), String> {
    for d in &TRACKED.declarations(ir)? {
        let name = TRACKED.field_name(d)?;
        let mode = Mode::parse(d.arguments.get("mode")).map_err(|e| error(d, e))?;
        let upsert = tracking::upsert(d)?;
        let model = ir.models.iter().find(|m| m.name == d.model).ok_or_else(|| error(d, "unknown model"))?;
        let field = model.fields.iter().find(|f| f.name == name).ok_or_else(|| error(d, "unknown field"))?;
        if ir.behavior.write_assignments.iter().any(|a| a.source == SOURCE && a.model == d.model && a.field == name) { return Err(error(d, "is given twice")); }
        if mode == Mode::Database {
            let column = field.column.clone();
            let models = ir.models.iter_mut().chain(ir.behavior.storage.iter_mut().flat_map(|s| s.models.iter_mut()));
            for model in models.filter(|m| m.name == d.model) {
                let trigger = trigger(ir.dialect, model, &column)?;
                if model.triggers.iter().any(|t| t.name == trigger.name) { return Err(error(d, format!("{} already has a trigger {}", d.model, trigger.name))); }
                model.triggers.push(trigger);
            }
        }
        // A proxy that shares the table but hides the field would leave the column unset on its writes.
        let proxies: Vec<String> = ir.behavior.model_views.iter().filter(|v| v.root == d.model).map(|v| v.model.clone()).collect();
        let mut targets = vec![d.model.clone()];
        if mode == Mode::Application {
            for proxy in proxies {
                if !ir.models.iter().find(|m| m.name == proxy).is_some_and(|m| m.fields.iter().any(|f| f.name == name)) {
                    return Err(error(d, format!("{proxy} excludes {name}, so its writes cannot set it; keep the field in the proxy or use mode \"database\"")));
                }
                targets.push(proxy);
            }
        }
        for model in targets {
            ir.behavior.write_assignments.push(WriteAssignment { source: SOURCE.into(), model, field: name.clone(),
                on: vec![WriteKind::Update, WriteKind::Upsert, WriteKind::DeleteRewrite], value: AssignValue::Now, upsert });
        }
    }
    // A proxy defined after its root's batch: the root has a record from that batch, and a database-mode root has a trigger.
    let roots: Vec<WriteAssignment> = ir.behavior.write_assignments.iter().filter(|a| a.source == SOURCE).cloned().collect();
    for root in roots {
        let Some(column) = ir.models.iter().find(|m| m.name == root.model).and_then(|m| m.fields.iter().find(|f| f.name == root.field)).map(|f| f.column.clone()) else { continue };
        let trigger = format!("updated_at_{column}");
        if ir.models.iter().find(|m| m.name == root.model).is_some_and(|m| m.triggers.iter().any(|t| t.name == trigger)) { continue; }
        let proxies: Vec<String> = ir.behavior.model_views.iter().filter(|v| v.root == root.model).map(|v| v.model.clone()).collect();
        for proxy in proxies {
            if ir.behavior.write_assignments.iter().any(|a| a.source == SOURCE && a.model == proxy && a.field == root.field) { continue; }
            if !ir.models.iter().find(|m| m.name == proxy).is_some_and(|m| m.fields.iter().any(|f| f.name == root.field)) {
                return Err(format!("{proxy}: proxy must keep the field {}, so its writes can set it", root.field));
            }
            ir.behavior.write_assignments.push(WriteAssignment { model: proxy, ..root.clone() });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use monorm::writes::UpsertDefault;

    /// A model with a declaration and without the field: the input of `add_fields`.
    fn declared(dialect: &str, positional: serde_json::Value, arguments: serde_json::Value, field: Option<&str>) -> SchemaIr {
        serde_json::from_value(serde_json::json!({
            "dialect": dialect,
            "models": [{"name": "Post", "table": "posts", "fields": [
                {"name": "id", "column": "id", "type": "int", "primary_key": true}
            ]}],
            "behavior": {"schema_contract": 5, "declarations": [
                {"attribute": "timestamps.updated_at", "model": "Post", "field": field, "positional": positional, "arguments": arguments, "location": {"file": "schema.prisma", "line": 4, "column": 3}}
            ]}
        })).unwrap()
    }

    /// The state after `add_fields`: the field `changed` exists.
    fn schema(dialect: &str, mode: Option<&str>) -> SchemaIr {
        let arguments = mode.map(|m| serde_json::json!({"mode": m})).unwrap_or_else(|| serde_json::json!({}));
        let mut ir = declared(dialect, serde_json::json!(["changed"]), arguments, None);
        monorm::extension::capture_storage(&mut ir).unwrap();
        add_fields(&mut ir).unwrap();
        ir.behavior.storage = None;
        ir
    }

    fn prepared(ir: &mut SchemaIr) -> Result<(), String> {
        monorm::extension::capture_storage(ir)?;
        prepare(ir)
    }

    fn added(positional: serde_json::Value) -> Result<SchemaIr, String> {
        let mut ir = declared("postgres", positional, serde_json::json!({}), None);
        monorm::extension::capture_storage(&mut ir)?;
        add_fields(&mut ir)?;
        Ok(ir)
    }

    #[test]
    fn the_model_attribute_adds_the_updated_at_field_to_logical_and_physical_models() {
        let ir = added(serde_json::json!([])).unwrap();
        for model in [&ir.models[0], &ir.behavior.storage.as_ref().unwrap().models[0]] {
            let f = model.fields.last().unwrap();
            assert_eq!((f.name.as_str(), f.column.as_str(), f.ty, f.nullable, f.default_now), ("updated_at", "updated_at", ColType::DateTime, false, true));
        }
        let effects = monorm::extension::pass_effects(&declared("postgres", serde_json::json!([]), serde_json::json!({}), None), add_fields).unwrap();
        assert_eq!(effects.into_iter().collect::<Vec<_>>(), ["behavior.storage", "models"]);
    }

    #[test]
    fn map_sets_the_column_of_the_updated_at_field() {
        let mut ir = declared("postgres", serde_json::json!([]), serde_json::json!({"map": "editedAt"}), None);
        monorm::extension::capture_storage(&mut ir).unwrap();
        add_fields(&mut ir).unwrap();
        for model in [&ir.models[0], &ir.behavior.storage.as_ref().unwrap().models[0]] {
            let f = model.fields.last().unwrap();
            assert_eq!((f.name.as_str(), f.column.as_str()), ("updated_at", "editedAt"));
        }
    }

    #[test]
    fn a_positional_name_names_the_field() {
        let ir = added(serde_json::json!(["edited_at"])).unwrap();
        assert_eq!(ir.models[0].fields.last().unwrap().name, "edited_at");
        let err = added(serde_json::json!(["editedAt"])).unwrap_err();
        assert_eq!(err, "schema.prisma:4:3: @timestamps.updated_at: the field name must be a snake_case string");
    }

    #[test]
    fn upsert_pick_keeps_the_field_in_the_default_upsert_list() {
        for (arguments, expected) in [(serde_json::json!({}), UpsertDefault::Drop), (serde_json::json!({"upsert": "drop"}), UpsertDefault::Drop), (serde_json::json!({"upsert": "pick"}), UpsertDefault::Pick)] {
            let mut ir = declared("postgres", serde_json::json!(["changed"]), arguments, None);
            monorm::extension::capture_storage(&mut ir).unwrap();
            add_fields(&mut ir).unwrap();
            prepare(&mut ir).unwrap();
            assert_eq!(ir.behavior.write_assignments[0].upsert, expected);
        }
        let mut ir = declared("postgres", serde_json::json!([]), serde_json::json!({"upsert": "merge"}), None);
        monorm::extension::capture_storage(&mut ir).unwrap();
        add_fields(&mut ir).unwrap();
        assert!(prepare(&mut ir).unwrap_err().ends_with("upsert must be \"drop\" or \"pick\""));
    }

    #[test]
    fn the_field_attribute_fails_with_the_replacement_line() {
        let cases = [
            ("changed", serde_json::json!({}), "@@timestamps.updated_at(\"changed\")"),
            ("updated_at", serde_json::json!({"mode": "application"}), "@@timestamps.updated_at(mode: \"application\")"),
            ("edited_at", serde_json::json!({"mode": "database"}), "@@timestamps.updated_at(\"edited_at\", mode: \"database\")"),
            ("updated_at", serde_json::json!({}), "@@timestamps.updated_at"),
        ];
        for (field, arguments, line) in cases {
            let mut ir = declared("postgres", serde_json::json!([]), arguments, Some(field));
            monorm::extension::capture_storage(&mut ir).unwrap();
            let err = add_fields(&mut ir).unwrap_err();
            assert_eq!(err, format!("schema.prisma:4:3: @timestamps.updated_at: a field attribute is no longer supported; delete the field Post.{field} and write {line} in the model block"));
        }
    }

    #[test]
    fn a_field_with_the_same_name_is_an_error_that_names_both() {
        let mut ir = declared("postgres", serde_json::json!([]), serde_json::json!({}), None);
        ir.models[0].fields.push(monorm::ir::FieldIr::plain("updated_at", ColType::DateTime));
        monorm::extension::capture_storage(&mut ir).unwrap();
        let err = add_fields(&mut ir).unwrap_err();
        assert_eq!(err, "schema.prisma:4:3: @timestamps.updated_at: adds the field Post.updated_at, which already exists; remove that field or name another field with @@timestamps.updated_at(\"other_name\")");
    }

    #[test]
    fn preparation_changes_only_its_declared_effects_once() {
        let effects = monorm::extension::pass_effects(&schema("postgres", None), prepare).unwrap();
        assert_eq!(effects.into_iter().collect::<Vec<_>>(), ["behavior.storage", "behavior.write_assignments", "models"]);
        let effects = monorm::extension::pass_effects(&schema("postgres", Some("application")), prepare).unwrap();
        assert_eq!(effects.into_iter().collect::<Vec<_>>(), ["behavior.write_assignments"]);
    }

    #[test]
    fn database_mode_adds_the_trigger_to_logical_and_physical_models() {
        let mut ir = schema("postgres", None);
        prepared(&mut ir).unwrap();
        assert_eq!(ir.behavior.write_assignments.len(), 1);
        for model in [&ir.models[0], &ir.behavior.storage.as_ref().unwrap().models[0]] {
            let t = &model.triggers[0];
            assert_eq!((t.name.as_str(), t.body.as_deref()), ("updated_at_changed", Some("BEGIN NEW.\"changed\" := now(); RETURN NEW; END;")));
            assert_eq!(serde_json::to_value(t.timing).unwrap(), "before");
            assert_eq!(t.when.as_deref(), Some("NEW.\"changed\" IS NOT DISTINCT FROM OLD.\"changed\""));
        }
        let mut ir = schema("sqlite", None);
        prepared(&mut ir).unwrap();
        let t = &ir.behavior.storage.as_ref().unwrap().models[0].triggers[0];
        assert_eq!(serde_json::to_value(t.timing).unwrap(), "after");
        assert_eq!(t.when.as_deref(), Some("NEW.\"changed\" IS OLD.\"changed\" AND NEW.\"changed\" IS NOT strftime('%Y-%m-%d %H:%M:%f','now')"));
        assert_eq!(t.body.as_deref(), Some("UPDATE \"posts\" SET \"changed\" = strftime('%Y-%m-%d %H:%M:%f','now') WHERE \"id\" = NEW.\"id\""));
    }

    #[test]
    fn the_sqlite_trigger_matches_the_whole_composite_key() {
        let mut ir = schema("sqlite", None);
        ir.models[0].fields.insert(0, serde_json::from_value(serde_json::json!({"name": "shop", "column": "shop", "type": "int", "primary_key": true})).unwrap());
        prepared(&mut ir).unwrap();
        let t = &ir.behavior.storage.as_ref().unwrap().models[0].triggers[0];
        assert_eq!(t.body.as_deref(), Some("UPDATE \"posts\" SET \"changed\" = strftime('%Y-%m-%d %H:%M:%f','now') WHERE \"shop\" = NEW.\"shop\" AND \"id\" = NEW.\"id\""));
    }

    fn with_proxy(mode: &str, keeps_field: bool) -> SchemaIr {
        let mut ir = schema("postgres", Some(mode));
        let mut view: ModelIr = serde_json::from_value(serde_json::to_value(&ir.models[0]).unwrap()).unwrap();
        view.name = "PostView".into();
        if !keeps_field { view.fields.retain(|f| f.name != "changed"); }
        ir.models.push(view);
        ir.behavior.model_views.push(monorm::extension::ModelView { model: "PostView".into(), source: "Post".into(), root: "Post".into(), row_filter: None, insert_table: None });
        ir
    }

    #[test]
    fn the_field_is_assigned_on_updates_upserts_and_delete_rewrites() {
        for mode in ["application", "database"] {
            let mut ir = schema("postgres", Some(mode));
            prepared(&mut ir).unwrap();
            let a = &ir.behavior.write_assignments;
            assert_eq!(a.len(), 1);
            assert_eq!((a[0].source.as_str(), a[0].model.as_str(), a[0].field.as_str(), a[0].value.clone(), a[0].upsert), ("monorm.timestamps", "Post", "changed", AssignValue::Now, UpsertDefault::Drop));
            assert_eq!(a[0].on, [WriteKind::Update, WriteKind::Upsert, WriteKind::DeleteRewrite]);
        }
    }

    #[test]
    fn application_mode_copies_the_assignment_to_each_proxy() {
        let mut ir = with_proxy("application", true);
        prepared(&mut ir).unwrap();
        let models: Vec<_> = ir.behavior.write_assignments.iter().map(|a| a.model.as_str()).collect();
        assert_eq!(models, ["Post", "PostView"]);
        let mut ir = with_proxy("database", true);
        prepared(&mut ir).unwrap();
        assert_eq!(ir.behavior.write_assignments.len(), 1);
    }

    #[test]
    fn a_proxy_in_a_later_batch_gets_the_assignment_of_an_application_root_only() {
        for (mode, expected) in [("application", vec!["Post", "PostView"]), ("database", vec!["Post"])] {
            let mut ir = schema("postgres", Some(mode));
            prepared(&mut ir).unwrap();
            ir.behavior.declarations.clear();
            let mut view: ModelIr = serde_json::from_value(serde_json::to_value(&ir.models[0]).unwrap()).unwrap();
            view.name = "PostView".into();
            ir.models.push(view);
            ir.behavior.model_views.push(monorm::extension::ModelView { model: "PostView".into(), source: "Post".into(), root: "Post".into(), row_filter: None, insert_table: None });
            prepare(&mut ir).unwrap();
            prepare(&mut ir).unwrap();
            let models: Vec<_> = ir.behavior.write_assignments.iter().map(|a| a.model.as_str()).collect();
            assert_eq!(models, expected, "{mode}");
        }
    }

    #[test]
    fn application_mode_rejects_a_proxy_that_excludes_the_field() {
        let err = prepared(&mut with_proxy("application", false)).unwrap_err();
        assert!(err.starts_with("schema.prisma:4:3: @timestamps.updated_at: PostView excludes changed"), "{err}");
        prepared(&mut with_proxy("database", false)).unwrap();
    }

    #[test]
    fn application_mode_adds_no_ddl() {
        let mut ir = schema("sqlite", Some("application"));
        prepared(&mut ir).unwrap();
        assert!(ir.models[0].triggers.is_empty() && ir.behavior.storage.as_ref().unwrap().models[0].triggers.is_empty());
    }

    #[test]
    fn wrong_declarations_fail_with_their_location() {
        let err = prepared(&mut schema("postgres", Some("trigger"))).unwrap_err();
        assert!(err.ends_with("mode must be \"database\" or \"application\""), "{err}");
    }

    /// The migration of the physical schema, as the host plans it.
    fn migration(ir: &SchemaIr) -> Vec<String> {
        let storage = serde_json::json!({"dialect": ir.dialect, "models": ir.behavior.storage.as_ref().unwrap().models});
        let schema = monorm_core::schema::Schema::from_ir(serde_json::from_value(storage).unwrap()).unwrap();
        let plan = monorm_core::migrate::plan(&schema, &Default::default()).unwrap();
        plan.up.into_iter().map(|s| s.sql).collect()
    }

    #[test]
    fn postgres_migration_creates_the_trigger_function_and_trigger() {
        let mut ir = schema("postgres", None);
        prepared(&mut ir).unwrap();
        let up = migration(&ir);
        assert!(up.iter().any(|s| s.starts_with("CREATE OR REPLACE FUNCTION \"posts_updated_at_changed\"() RETURNS trigger")), "{up:?}");
        assert!(up.contains(&"CREATE TRIGGER \"updated_at_changed\" BEFORE UPDATE ON \"posts\" FOR EACH ROW WHEN (NEW.\"changed\" IS NOT DISTINCT FROM OLD.\"changed\") EXECUTE FUNCTION \"posts_updated_at_changed\"()".to_owned()), "{up:?}");
    }

    #[test]
    fn sqlite_trigger_sets_the_field_for_other_writers_only() {
        let mut ir = schema("sqlite", None);
        prepared(&mut ir).unwrap();
        let db = rusqlite::Connection::open_in_memory().unwrap();
        for statement in migration(&ir) { db.execute_batch(&statement).unwrap(); }
        db.execute_batch("INSERT INTO posts (id, changed) VALUES (1, '2000-01-01 00:00:00')").unwrap();
        let changed = |db: &rusqlite::Connection| db.query_row("SELECT changed FROM posts", [], |r| r.get::<_, String>(0)).unwrap();
        db.execute_batch("UPDATE posts SET id = 1").unwrap();
        assert_ne!(changed(&db), "2000-01-01 00:00:00");
        db.execute_batch("UPDATE posts SET changed = '2001-01-01 00:00:00'").unwrap();
        assert_eq!(changed(&db), "2001-01-01 00:00:00");
    }

    #[test]
    fn sqlite_trigger_survives_recursive_triggers_and_keeps_milliseconds() {
        let mut ir = schema("sqlite", None);
        prepared(&mut ir).unwrap();
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("PRAGMA recursive_triggers = ON").unwrap();
        for statement in migration(&ir) { db.execute_batch(&statement).unwrap(); }
        db.execute_batch("INSERT INTO posts (id, changed) VALUES (1, '2000-01-01 00:00:00')").unwrap();
        for i in 0..2000 { db.execute_batch("UPDATE posts SET id = 1").unwrap_or_else(|e| panic!("update {i}: {e}")); }
        let value: String = db.query_row("SELECT changed FROM posts", [], |r| r.get(0)).unwrap();
        assert!(value.len() == 23 && value.as_bytes()[19] == b'.', "{value}");
    }
}

// The package entry point; Rust callers keep the rlib API.
monorm::extension!(("monorm.timestamps.fields", add_fields), ("monorm.timestamps.prepare", prepare));
