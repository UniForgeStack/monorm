//! `@@soft_delete.deleted_at`: the model gets a nullable `deleted_at` field, and the ORM
//! soft-deletes rows instead of deleting them.
//! Database mode adds row triggers, so a raw `DELETE` soft-deletes too
//! and the soft delete cascades to soft-delete children.
use monorm::{dialect::Dialect, extension::{views_keeping, Declaration, ReadDefault}, ir::{ColType, Expr, FieldIr, ModelIr, OnDelete, SchemaIr, TriggerEvent, TriggerIr, TriggerTiming}, sql::ident, tracking::{error, Mode, Tracked}, writes::{AssignValue, DeleteRewrite, WriteAssignment, WriteKind}};

/// The read default that hides soft-deleted rows; `without_defaults("deleted")` bypasses it.
const SOURCE: &str = "monorm.soft-delete";
const DEFAULT: &str = "deleted";
const TRACKED: Tracked = Tracked { attribute: "soft_delete.deleted_at", field: "deleted_at" };
/// On a relation field: a soft delete of the parent keeps the child rows. Read by `check`, not by the tracking code.
const KEEP_ROWS: &str = "soft_delete.keep_child_rows";

/// A soft-delete field: declared on a model, or copied to a proxy of its storage owner.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SoftDelete {
    model: String,
    field: String,
    mode: Mode,
    /// The database rule that the triggers of this model replace (the `rule` argument).
    rule: Option<String>,
}

/// Runs before the logical passes, so a proxy of the model copies the field too.
pub fn add_fields(ir: &mut SchemaIr) -> Result<(), String> {
    TRACKED.add_fields_besides(ir, FieldIr::plain(TRACKED.field, ColType::DateTime), &[KEEP_ROWS])
}

/// The field a delete-time assignment of this model increments, if any: the version bump of a soft delete.
fn bumped<'a>(assignments: &'a [WriteAssignment], model: &str) -> Option<&'a str> {
    assignments.iter().find(|a| a.model == model && a.value == AssignValue::Increment && a.on.contains(&WriteKind::DeleteRewrite)).map(|a| a.field.as_str())
}

/// A user filter that only repeats the `"deleted"` default hides the rows `all_with_deleted()` and the other members must reach.
/// The core has resolved the filter into a tree; a string is the string form that IR from other tools can carry.
fn repeats_default(filter: &serde_json::Value, field: &str) -> bool {
    if let Some(text) = filter.as_str() {
        let mut text: String = text.split_whitespace().collect();
        while text.starts_with('(') && text.ends_with(')') { text = text[1..text.len() - 1].to_owned(); }
        return text == format!("{field}==null") || text == format!("{field}isnull");
    }
    *filter == serde_json::json!({"t": "is_null", "item": {"t": "col", "path": [], "name": field}, "neg": false})
}

/// The declaration that wrote this filter, for the error position: any declaration whose argument is the filter, the model's own first.
fn written_by<'a>(ir: &'a SchemaIr, model: &str, filter: &serde_json::Value) -> Option<&'a Declaration> {
    let writes = |d: &&Declaration| d.positional.iter().chain(d.arguments.values()).any(|v| v == filter);
    let own = ir.behavior.declarations.iter().filter(|d| d.model == model);
    own.clone().find(writes).or_else(|| ir.behavior.declarations.iter().filter(|d| d.model != model).find(writes))
}

/// Runs after the behavior passes, so the read defaults of every other extension exist.
pub fn check(ir: &mut SchemaIr) -> Result<(), String> {
    for rewrite in ir.behavior.delete_rewrites.iter().filter(|d| d.source == SOURCE) {
        for d in ir.behavior.read_defaults.iter().filter(|d| d.model == rewrite.model && d.source != SOURCE) {
            let Some(filter) = d.filter.as_ref().filter(|f| repeats_default(f, &rewrite.field)) else { continue };
            let message = format!("{} already hides soft-deleted rows by default; remove this filter, it would also hide them from all_with_deleted(), deleted_only(), undelete() and hard_delete()", rewrite.model);
            return Err(match written_by(ir, &rewrite.model, filter) {
                Some(declaration) if declaration.model != rewrite.model => error(declaration, format!("{message} (this declaration is on {}, which {} inherits)", declaration.model, rewrite.model)),
                Some(declaration) => error(declaration, message),
                None => message,
            });
        }
    }
    lost_children(ir)
}

/// The models of the batch, then the physical models of earlier batches that have no logical model.
fn all_models(ir: &SchemaIr) -> Vec<&ModelIr> {
    let physical = ir.behavior.storage.iter().flat_map(|s| &s.models).filter(|m| !ir.models.iter().any(|l| l.name == m.name));
    ir.models.iter().chain(physical).collect()
}

/// A soft delete reaches a child only through the cascade trigger or the ORM, and both skip a child without soft delete:
/// its rows stay and point to a parent that is hidden. A cascade from a soft-delete table into a table without soft delete
/// fails the schema, unless the relation field carries `@soft_delete.keep_child_rows`.
/// The check works on tables, so it covers every level (each soft-delete child is a parent in its turn),
/// proxies (same table as their root) and storage owners.
fn lost_children(ir: &SchemaIr) -> Result<(), String> {
    let models = all_models(ir);
    let root = |name: &str| ir.behavior.model_views.iter().find(|v| v.model == name).map_or(name.to_owned(), |v| v.root.clone());
    let opt_outs: Vec<&Declaration> = ir.behavior.declarations.iter().filter(|d| d.attribute == KEEP_ROWS).collect();
    for d in &opt_outs {
        if !d.arguments.is_empty() || !d.positional.is_empty() { return Err(error(d, "takes no argument")); }
        let Some(field) = &d.field else { return Err(d.error_with("@@", "write it on the relation field that cascades, not on the model")) };
        let relation = models.iter().find(|m| m.name == d.model).is_some_and(|m| m.relations.iter().any(|r| &r.name == field && r.foreign_key));
        if !relation { return Err(error(d, format!("{}.{field} is no relation field with a foreign key; write the attribute on the relation field, for example on {}.author", d.model, d.model))); }
    }
    let table = |name: &str| models.iter().find(|m| m.name == name).map(|m| m.table.as_str());
    let soft: Vec<&str> = ir.behavior.delete_rewrites.iter().filter(|d| d.source == SOURCE).filter_map(|d| table(&d.model)).collect();
    for child in models.iter().filter(|m| !soft.contains(&m.table.as_str())) {
        let owner = root(&child.name);
        for r in child.relations.iter().filter(|r| r.foreign_key && r.on_delete == Some(OnDelete::Cascade)) {
            if !table(&r.target).is_some_and(|t| soft.contains(&t)) { continue; }
            if opt_outs.iter().any(|d| (d.model == child.name || d.model == owner) && d.field.as_deref() == Some(r.name.as_str())) { continue; }
            let (parent, child_name, relation) = (&r.target, &child.name, &r.name);
            let message = format!("{parent} is soft-deleted, but {child_name}.{relation} cascades deletes (onDelete: Cascade) and {child_name} has no soft delete, so a soft delete of {parent} keeps the {child_name} rows; \
                fix: add @@{} to {child_name}, or add @{KEEP_ROWS} to the relation field {child_name}.{relation} to keep the {child_name} rows on a soft delete", TRACKED.attribute);
            let parent_root = root(parent);
            return Err(match ir.behavior.declarations.iter().find(|d| d.attribute == TRACKED.attribute && d.field.is_none() && (&d.model == parent || d.model == parent_root)) {
                Some(d) => d.error_with("@@", message),
                None => message,
            });
        }
    }
    Ok(())
}

fn column<'m>(model: &'m ModelIr, field: &str) -> Result<&'m str, String> {
    model.fields.iter().find(|f| f.name == field).map(|f| f.column.as_str()).ok_or_else(|| format!("{} has no field {field}", model.name))
}

/// `, "v" = "v" + 1` for a model with a version field, else nothing.
fn bump(version: Option<&str>) -> String {
    version.map(|v| { let v = ident(v); format!(", {v} = {v} + 1") }).unwrap_or_default()
}

/// `BEFORE DELETE` of a live row: set the field and cancel the delete. A row that is
/// already soft-deleted is deleted. On PostgreSQL a delete inside a trigger (a foreign-key
/// cascade included) is a hard delete: a cancelled cascade delete runs no foreign-key check
/// and would leave an orphan.
fn delete_trigger(dialect: Dialect, model: &ModelIr, field: &str, version: Option<&str>) -> Result<TriggerIr, String> {
    let keys = model.key_fields();
    if keys.is_empty() { return Err(format!("{} has no primary key", model.name)); }
    let pk = keys.iter().map(|k| Ok(format!("{0} = OLD.{0}", ident(column(model, k)?)))).collect::<Result<Vec<_>, String>>()?.join(" AND ");
    let (c, t) = (ident(column(model, field)?), ident(&model.table));
    let bump = bump(version.map(|v| column(model, v)).transpose()?);
    let (body, when) = match dialect {
        Dialect::Postgres => (format!("BEGIN UPDATE {t} SET {c} = now(){bump} WHERE {pk}; RETURN NULL; END;"), format!("OLD.{c} IS NULL AND pg_trigger_depth() = 0")),
        Dialect::Sqlite => (format!("UPDATE {t} SET {c} = strftime('%Y-%m-%d %H:%M:%f', 'now'){bump} WHERE {pk}; SELECT RAISE(IGNORE)"), format!("OLD.{c} IS NULL")),
    };
    Ok(TriggerIr { name: "soft_delete".into(), timing: TriggerTiming::Before, events: vec![TriggerEvent::Delete], when: Some(when), body: Some(body), ..Default::default() })
}

type Child = (String, String, Option<String>, Vec<(String, String)>);

/// `AFTER UPDATE OF` the field, when a row becomes soft-deleted: soft-delete the live rows
/// of each soft-delete child whose relation cascades deletes. A child is
/// `(table, soft-delete column, version column, [(foreign key column, referenced parent column)])`.
fn cascade_trigger(dialect: Dialect, parent: &ModelIr, field: &str, children: &[Child]) -> Result<TriggerIr, String> {
    let c = ident(column(parent, field)?);
    let statements: Vec<String> = children.iter().map(|(table, child, version, keys)| {
        let bump = bump(version.as_deref());
        let (table, child) = (ident(table), ident(child));
        let link: Vec<String> = keys.iter().map(|(key, to)| format!("{} = NEW.{}", ident(key), ident(to))).collect();
        format!("UPDATE {table} SET {child} = NEW.{c}{bump} WHERE {} AND {child} IS NULL", link.join(" AND "))
    }).collect();
    let body = match dialect {
        Dialect::Postgres => format!("BEGIN {}; RETURN NULL; END;", statements.join("; ")),
        Dialect::Sqlite => statements.join("; "),
    };
    Ok(TriggerIr { name: "soft_delete_cascade".into(), timing: TriggerTiming::After, events: vec![TriggerEvent::Update], update_of: vec![field.into()],
        when: Some(format!("OLD.{c} IS NULL AND NEW.{c} IS NOT NULL")), body: Some(body), ..Default::default() })
}

/// The row is live: its soft-delete field is `NULL`.
fn live(field: &str) -> serde_json::Value { Expr::is_null(Expr::col(field), false).to_json() }

pub fn prepare(ir: &mut SchemaIr) -> Result<(), String> {
    // Models of an earlier batch keep their records; their mode is unknown (Application is a placeholder), and only the models of this batch use it.
    let mut roles: Vec<SoftDelete> = ir.behavior.delete_rewrites.iter().filter(|d| d.source == SOURCE)
        .map(|d| SoftDelete { model: d.model.clone(), field: d.field.clone(), mode: Mode::Application, rule: None }).collect();
    let mut added = vec![];
    for d in &TRACKED.declarations_besides(ir, &[KEEP_ROWS])? {
        let name = TRACKED.field_name(d)?;
        let mode = Mode::parse(d.arguments.get("mode")).map_err(|e| error(d, e))?;
        let model = ir.models.iter().find(|m| m.name == d.model).ok_or_else(|| error(d, "unknown model"))?;
        if !model.fields.iter().any(|f| f.name == name) { return Err(error(d, "unknown field")); }
        if roles.iter().any(|s| s.model == d.model) { return Err(error(d, format!("{} has more than one soft-delete field", d.model))); }
        let rule = match d.arguments.get("rule") {
            None => None,
            Some(_) if ir.dialect == Dialect::Sqlite => return Err(error(d, "rule names a PostgreSQL rule; SQLite has no rules to replace")),
            Some(r) if mode == Mode::Database => Some(r.as_str().ok_or_else(|| error(d, "rule must be a string"))?.to_owned()),
            Some(_) => return Err(error(d, "rule names a database rule that the triggers replace; it needs mode \"database\"")),
        };
        roles.push(SoftDelete { model: d.model.clone(), field: name, mode, rule });
        added.push(d.model.clone());
    }
    // A proxy follows the shared storage of its root; its logical rows need the same ORM
    // update on delete. Database triggers are generated only for declarations.
    for source in roles.clone() {
        let has = |model: &str| roles.iter().any(|s| s.model == model);
        let split = views_keeping(ir, &source.model, &source.field, has);
        if let Some(view) = split.lacking.first() { return Err(format!("{view}: proxy must keep the soft-delete field {}", source.field)); }
        roles.extend(split.keeping.into_iter().map(|model| SoftDelete { model, field: source.field.clone(), mode: source.mode, rule: None }));
    }
    ir.behavior.read_defaults.retain(|d| d.source != SOURCE);
    for s in &roles {
        ir.behavior.read_defaults.push(ReadDefault { source: SOURCE.into(), name: DEFAULT.into(), model: s.model.clone(),
            filter: Some(live(&s.field)), ..Default::default() });
    }
    ir.behavior.delete_rewrites.retain(|d| d.source != SOURCE);
    for s in &roles {
        ir.behavior.delete_rewrites.push(DeleteRewrite { source: SOURCE.into(), model: s.model.clone(), field: s.field.clone(), guard: live(&s.field) });
    }
    let dialect = ir.dialect;
    for s in roles.iter().filter(|s| s.mode == Mode::Database && added.contains(&s.model)) {
        // Children: soft-delete models with a cascading foreign key to this model.
        let mut children = vec![];
        for child in &ir.models {
            let Some(child_field) = roles.iter().find(|c| c.model == child.name) else { continue };
            for r in child.relations.iter().filter(|r| r.foreign_key && r.target == s.model && r.on_delete == Some(OnDelete::Cascade)) {
                let keys = r.from.iter().map(|f| Ok(column(child, f)?.to_owned())).collect::<Result<Vec<_>, String>>()?;
                let version = bumped(&ir.behavior.write_assignments, &child.name).map(|f| column(child, f).map(str::to_owned)).transpose()?;
                children.push((child.table.clone(), column(child, &child_field.field)?.to_owned(), version, keys, r.to.clone()));
            }
        }
        let version = bumped(&ir.behavior.write_assignments, &s.model).map(str::to_owned);
        for model in ir.models_named_mut(&s.model) {
            let version = version.as_deref();
            let mut triggers = vec![delete_trigger(dialect, model, &s.field, version)?];
            if !children.is_empty() {
                let resolved = children.iter().map(|(t, c, v, keys, to)| {
                    let pairs = keys.iter().zip(to).map(|(k, to)| Ok((k.clone(), column(model, to)?.to_owned()))).collect::<Result<Vec<_>, String>>()?;
                    Ok((t.clone(), c.clone(), v.clone(), pairs))
                }).collect::<Result<Vec<_>, String>>()?;
                triggers.push(cascade_trigger(dialect, model, &s.field, &resolved)?);
            }
            for mut t in triggers {
                t.replaces_rule = s.rule.clone();
                model.push_trigger(t)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use monorm::extension::Declaration;

    fn at(model: &str, line: u32, positional: serde_json::Value, arguments: serde_json::Value, field: Option<&str>) -> Declaration {
        serde_json::from_value(serde_json::json!({"attribute": "soft_delete.deleted_at", "model": model, "field": field, "positional": positional, "arguments": arguments, "location": {"file": "schema.prisma", "line": line, "column": 3}})).unwrap()
    }

    /// The input of `add_fields`: Author, Book and Note without a soft-delete field, with `declarations` and a version record on Author and Book.
    fn declared(dialect: &str, declarations: Vec<Declaration>) -> SchemaIr {
        let lock = |model: &str| serde_json::json!({"source": "monorm.locking", "model": model, "field": "version", "on": ["update", "upsert", "delete_rewrite"], "value": "increment"});
        let mut ir: SchemaIr = serde_json::from_value(serde_json::json!({
            "dialect": dialect,
            "models": [
                {"name": "Author", "table": "authors", "fields": [
                    {"name": "id", "column": "id", "type": "int", "primary_key": true},
                    {"name": "version", "column": "rev", "type": "int"}
                ]},
                {"name": "Book", "table": "books", "fields": [
                    {"name": "id", "column": "id", "type": "int", "primary_key": true},
                    {"name": "author_id", "column": "writer", "type": "int"},
                    {"name": "version", "column": "lock", "type": "int"}
                ], "relations": [{"name": "author", "kind": "one", "target": "Author", "from": "author_id", "to": "id", "foreign_key": true, "on_delete": "cascade"}]},
                {"name": "Note", "table": "notes", "fields": [
                    {"name": "id", "column": "id", "type": "int", "primary_key": true},
                    {"name": "author_id", "column": "author_id", "type": "int"}
                ], "relations": [{"name": "author", "kind": "one", "target": "Author", "from": "author_id", "to": "id", "foreign_key": true, "on_delete": "cascade"}]}
            ],
            "behavior": {"schema_contract": 5, "write_assignments": [lock("Author"), lock("Book")]}
        })).unwrap();
        ir.behavior.declarations = declarations;
        ir
    }

    /// The state after `add_fields`: Author and Book declare `@@soft_delete.deleted_at("deleted")` and have the field `deleted`; Note has no soft delete.
    fn schema(dialect: &str, mode: &str) -> SchemaIr {
        let mut ir = bare(dialect, mode);
        ir.behavior.declarations.push(keep_rows("Note", "author", 12));
        ir
    }

    /// `@soft_delete.keep_child_rows` on the relation field `field` of `model`.
    fn keep_rows(model: &str, field: &str, line: u32) -> Declaration {
        serde_json::from_value(serde_json::json!({"attribute": "soft_delete.keep_child_rows", "model": model, "field": field, "positional": [], "arguments": {}, "location": {"file": "schema.prisma", "line": line, "column": 3}})).unwrap()
    }

    /// `schema` without the opt-out: Note cascades from a soft-delete model and has no soft delete.
    fn bare(dialect: &str, mode: &str) -> SchemaIr {
        let named = |model: &str, line: u32| at(model, line, serde_json::json!(["deleted"]), serde_json::json!({"mode": mode}), None);
        let mut ir = declared(dialect, vec![named("Author", 3), named("Book", 9)]);
        monorm::extension::capture_storage(&mut ir).unwrap();
        add_fields(&mut ir).unwrap();
        ir.behavior.storage = None;
        ir
    }

    fn added(positional: serde_json::Value) -> Result<SchemaIr, String> {
        let mut ir = declared("postgres", vec![at("Author", 3, positional, serde_json::json!({}), None)]);
        monorm::extension::capture_storage(&mut ir)?;
        add_fields(&mut ir)?;
        Ok(ir)
    }

    fn prepared(ir: &mut SchemaIr) -> Result<(), String> {
        monorm::extension::capture_storage(ir)?;
        prepare(ir)
    }

    fn migration(ir: &SchemaIr) -> Vec<String> {
        let storage = serde_json::json!({"dialect": ir.dialect, "models": ir.behavior.storage.as_ref().unwrap().models});
        let schema = monorm_core::schema::Schema::from_ir(serde_json::from_value(storage).unwrap()).unwrap();
        monorm_core::migrate::plan(&schema, &Default::default()).unwrap().up.into_iter().map(|s| s.sql).collect()
    }

    /// The wire shape of the live filter, byte for byte.
    #[test]
    fn the_live_filter_keeps_its_json() {
        let wire = serde_json::json!({"t": "is_null", "item": {"t": "col", "path": [], "name": "deleted_at"}, "neg": false});
        assert_eq!(live("deleted_at").to_string(), wire.to_string());
    }

    #[test]
    fn the_model_attribute_adds_a_nullable_deleted_at_field_to_logical_and_physical_models() {
        let ir = added(serde_json::json!([])).unwrap();
        for model in [&ir.models[0], &ir.behavior.storage.as_ref().unwrap().models[0]] {
            let f = model.fields.last().unwrap();
            assert_eq!((f.name.as_str(), f.column.as_str(), f.ty, f.nullable, f.default_now), ("deleted_at", "deleted_at", ColType::DateTime, true, false));
        }
        let ir = added(serde_json::json!(["removed_at"])).unwrap();
        assert_eq!(ir.models[0].fields.last().unwrap().name, "removed_at");
        let err = added(serde_json::json!(["removedAt"])).unwrap_err();
        assert_eq!(err, "schema.prisma:3:3: @soft_delete.deleted_at: the field name must be a snake_case string");
    }

    #[test]
    fn map_sets_the_column_of_the_deleted_at_field() {
        let mut ir = declared("postgres", vec![at("Author", 3, serde_json::json!([]), serde_json::json!({"map": "_deleted_at"}), None)]);
        monorm::extension::capture_storage(&mut ir).unwrap();
        add_fields(&mut ir).unwrap();
        for model in [&ir.models[0], &ir.behavior.storage.as_ref().unwrap().models[0]] {
            let f = model.fields.last().unwrap();
            assert_eq!((f.name.as_str(), f.column.as_str()), ("deleted_at", "_deleted_at"));
        }
    }

    #[test]
    fn the_field_attribute_fails_with_the_replacement_line() {
        let cases = [
            ("deleted", serde_json::json!({}), "@@soft_delete.deleted_at(\"deleted\")"),
            ("deleted_at", serde_json::json!({"mode": "application"}), "@@soft_delete.deleted_at(mode: \"application\")"),
            ("removed_at", serde_json::json!({"mode": "database"}), "@@soft_delete.deleted_at(\"removed_at\", mode: \"database\")"),
            ("deleted_at", serde_json::json!({}), "@@soft_delete.deleted_at"),
        ];
        for (field, arguments, line) in cases {
            let mut ir = declared("postgres", vec![at("Author", 3, serde_json::json!([]), arguments, Some(field))]);
            monorm::extension::capture_storage(&mut ir).unwrap();
            let err = add_fields(&mut ir).unwrap_err();
            assert_eq!(err, format!("schema.prisma:3:3: @soft_delete.deleted_at: a field attribute is no longer supported; delete the field Author.{field} and write {line} in the model block"));
        }
    }

    #[test]
    fn a_field_with_the_same_name_is_an_error_that_names_both() {
        let mut ir = declared("postgres", vec![at("Author", 3, serde_json::json!([]), serde_json::json!({}), None)]);
        ir.models[0].fields.push(FieldIr::plain("deleted_at", ColType::DateTime));
        monorm::extension::capture_storage(&mut ir).unwrap();
        let err = add_fields(&mut ir).unwrap_err();
        assert_eq!(err, "schema.prisma:3:3: @soft_delete.deleted_at: adds the field Author.deleted_at, which already exists; remove that field or name another field with @@soft_delete.deleted_at(\"other_name\")");
    }

    #[test]
    fn preparation_changes_only_its_declared_effects_once() {
        let effects = monorm::extension::pass_effects(&schema("postgres", "database"), prepare).unwrap();
        assert_eq!(effects.into_iter().collect::<Vec<_>>(), ["behavior.delete_rewrites", "behavior.read_defaults", "behavior.storage", "models"]);
        let effects = monorm::extension::pass_effects(&schema("postgres", "application"), prepare).unwrap();
        assert_eq!(effects.into_iter().collect::<Vec<_>>(), ["behavior.delete_rewrites", "behavior.read_defaults"]);
    }

    #[test]
    fn each_soft_delete_model_gets_a_deleted_read_default() {
        let mut ir = schema("postgres", "application");
        prepared(&mut ir).unwrap();
        let defaults: Vec<_> = ir.behavior.read_defaults.iter().map(|d| (d.source.as_str(), d.name.as_str(), d.model.as_str())).collect();
        assert_eq!(defaults, [("monorm.soft-delete", "deleted", "Author"), ("monorm.soft-delete", "deleted", "Book")]);
        let filter = ir.behavior.read_defaults[1].filter.clone().unwrap();
        assert_eq!(filter, serde_json::json!({"t": "is_null", "item": {"t": "col", "path": [], "name": "deleted"}, "neg": false}));
    }

    #[test]
    fn each_soft_delete_model_gets_a_delete_rewrite_with_the_default_filter() {
        for mode in ["application", "database"] {
            let mut ir = schema("postgres", mode);
            prepared(&mut ir).unwrap();
            let rewrites: Vec<_> = ir.behavior.delete_rewrites.iter().map(|d| (d.source.as_str(), d.model.as_str(), d.field.as_str())).collect();
            assert_eq!(rewrites, [("monorm.soft-delete", "Author", "deleted"), ("monorm.soft-delete", "Book", "deleted")]);
            assert_eq!(Some(&ir.behavior.delete_rewrites[1].guard), ir.behavior.read_defaults[1].filter.as_ref());
        }
    }

    #[test]
    fn a_proxy_gets_the_delete_rewrite_of_its_root() {
        let mut ir = schema("postgres", "application");
        monorm::extension::capture_storage(&mut ir).unwrap();
        ir.behavior.lowered_models.push("AuthorView".into());
        let mut view: ModelIr = ir.models[0].clone();
        view.name = "AuthorView".into();
        ir.models.push(view);
        ir.behavior.model_views.push(monorm::extension::ModelView { model: "AuthorView".into(), source: "Author".into(), root: "Author".into(), row_filter: None, insert_table: None });
        prepared(&mut ir).unwrap();
        assert!(ir.behavior.delete_rewrites.iter().any(|d| d.model == "AuthorView" && d.field == "deleted"));
    }

    #[test]
    fn a_model_has_one_soft_delete_field() {
        let mut ir = schema("postgres", "application");
        let again = ir.behavior.declarations[0].clone();
        ir.behavior.declarations.push(again);
        assert_eq!(prepared(&mut ir).unwrap_err(), "schema.prisma:3:3: @soft_delete.deleted_at: Author has more than one soft-delete field");
    }

    #[test]
    fn a_rule_needs_postgres() {
        for (dialect, want) in [("postgres", Ok(())), ("sqlite", Err("schema.prisma:3:3: @soft_delete.deleted_at: rule names a PostgreSQL rule; SQLite has no rules to replace".to_string()))] {
            let mut ir = declared(dialect, vec![at("Author", 3, serde_json::json!(["deleted"]), serde_json::json!({"mode": "database", "rule": "soft_delete"}), None)]);
            monorm::extension::capture_storage(&mut ir).unwrap();
            add_fields(&mut ir).unwrap();
            assert_eq!(prepare(&mut ir), want, "{dialect}");
        }
    }

    #[test]
    fn a_proxy_must_keep_the_soft_delete_field() {
        let mut ir = schema("postgres", "application");
        monorm::extension::capture_storage(&mut ir).unwrap();
        ir.behavior.lowered_models.push("AuthorView".into());
        let mut view: ModelIr = ir.models[0].clone();
        view.name = "AuthorView".into();
        view.fields.retain(|f| f.name != "deleted");
        ir.models.push(view);
        ir.behavior.model_views.push(monorm::extension::ModelView { model: "AuthorView".into(), source: "Author".into(), root: "Author".into(), row_filter: None, insert_table: None });
        assert_eq!(prepared(&mut ir).unwrap_err(), "AuthorView: proxy must keep the soft-delete field deleted");
    }

    #[test]
    fn a_leftover_filter_on_the_deleted_field_is_rejected() {
        let tree = serde_json::json!({"t": "is_null", "item": {"t": "col", "path": [], "name": "deleted"}, "neg": false});
        let message = "Author already hides soft-deleted rows by default; remove this filter, it would also hide them from all_with_deleted(), deleted_only(), undelete() and hard_delete()";
        // Another extension's read default on the model, and the declaration that wrote it.
        let given = |filter: serde_json::Value, declared: bool| {
            let mut ir = schema("postgres", "application");
            if declared {
                ir.behavior.declarations.push(serde_json::from_value(serde_json::json!({"attribute": "other.filter", "model": "Author", "positional": ["live", filter], "arguments": {},
                    "location": {"file": "schema.prisma", "line": 6, "column": 1}})).unwrap());
            }
            prepared(&mut ir).unwrap();
            ir.behavior.read_defaults.push(ReadDefault { source: "other.ext".into(), name: "live".into(), model: "Author".into(), filter: Some(filter), ..Default::default() });
            ir
        };
        for filter in [serde_json::json!("deleted == null"), serde_json::json!("( deleted==null )"), serde_json::json!("deleted is null"), tree.clone()] {
            let err = check(&mut given(filter.clone(), true)).unwrap_err();
            assert_eq!(err, format!("schema.prisma:6:1: @other.filter: {message}"));
            assert_eq!(check(&mut given(filter, false)).unwrap_err(), message, "no declaration, no position");
        }
        let mut ir = given(serde_json::json!({"t": "and", "items": [tree, {"t": "cmp", "op": "gt", "l": {"t": "col", "path": [], "name": "id"}, "r": {"t": "int", "value": 0}}]}), true);
        check(&mut ir).unwrap();
        ir.behavior.read_defaults.last_mut().unwrap().model = "Note".into();
        ir.behavior.read_defaults.last_mut().unwrap().filter = Some(serde_json::json!("deleted == null"));
        check(&mut ir).unwrap();
        // A filter that the model inherits: the declaration is on the parent, and the error says so.
        let mut ir = given(tree.clone(), false);
        ir.behavior.declarations.push(serde_json::from_value(serde_json::json!({"attribute": "other.filter", "model": "Note", "positional": ["live", tree], "arguments": {},
            "location": {"file": "schema.prisma", "line": 2, "column": 1}})).unwrap());
        assert_eq!(check(&mut ir).unwrap_err(), format!("schema.prisma:2:1: @other.filter: {message} (this declaration is on Note, which Author inherits)"));
    }

    #[test]
    fn the_filter_of_a_model_without_soft_delete_and_the_own_read_default_pass() {
        let mut ir = schema("postgres", "application");
        prepared(&mut ir).unwrap();
        check(&mut ir).unwrap();
        assert!(monorm::extension::pass_effects(&ir, check).unwrap().is_empty());
    }

    #[test]
    fn triggers_bump_the_version_the_write_assignments_name_not_a_locking_declaration() {
        let mut ir = schema("postgres", "database");
        ir.behavior.write_assignments.retain(|a| a.model != "Book");
        ir.behavior.declarations.push(serde_json::from_value(serde_json::json!({"attribute": "locking.version", "model": "Book", "field": "version", "positional": [], "arguments": {}, "location": {"file": "schema.prisma", "line": 4, "column": 3}})).unwrap());
        prepared(&mut ir).unwrap();
        let cascade = ir.models[0].triggers[1].body.as_deref().unwrap();
        assert!(!cascade.contains("\"lock\""), "{cascade}");
        let mut ir = schema("postgres", "database");
        ir.behavior.write_assignments.iter_mut().filter(|a| a.model == "Author").for_each(|a| a.on = vec![WriteKind::Update]);
        prepared(&mut ir).unwrap();
        assert!(!ir.models[0].triggers[0].body.as_deref().unwrap().contains("\"rev\""));
    }

    #[test]
    fn to_one_references_to_a_soft_delete_model_are_typed_nullable() {
        use monorm_core::codegen::{python, typescript, Options};
        let mut ir = schema("postgres", "application");
        prepared(&mut ir).unwrap();
        ir.behavior.declarations.clear();
        ir.behavior.storage = None;
        let value = serde_json::to_value(&ir).unwrap();
        let ir: monorm_core::ir::SchemaIr = serde_json::from_value(value.clone()).unwrap();
        let core = monorm_core::schema::Schema::from_ir(serde_json::from_value(value).unwrap()).unwrap();
        let py = python::generate_with(&ir, &core, "schema.prisma", &Options::default()).unwrap();
        assert_eq!(py.stub.matches("author: f.BelongsTo[Author | None,").count(), 2, "{}", py.stub);
        let ts = typescript::generate_with(&ir, &core, "schema.prisma", "@ufstack/monorm", &Options::default()).unwrap();
        assert_eq!(ts.matches(r#"Hop<"author", "opt", AuthorSpec>], true>"#).count(), 2, "{ts}");
    }

    #[test]
    fn database_mode_adds_delete_and_cascade_triggers() {
        let mut ir = schema("postgres", "database");
        prepared(&mut ir).unwrap();
        assert_eq!(ir.behavior.delete_rewrites[1].model, "Book");
        for author in [&ir.models[0], &ir.behavior.storage.as_ref().unwrap().models[0]] {
            let names: Vec<_> = author.triggers.iter().map(|t| t.name.as_str()).collect();
            assert_eq!(names, ["soft_delete", "soft_delete_cascade"]);
            assert_eq!(author.triggers[0].when.as_deref(), Some("OLD.\"deleted\" IS NULL AND pg_trigger_depth() = 0"));
            assert_eq!(author.triggers[0].body.as_deref(), Some("BEGIN UPDATE \"authors\" SET \"deleted\" = now(), \"rev\" = \"rev\" + 1 WHERE \"id\" = OLD.\"id\"; RETURN NULL; END;"));
            // Note has no soft delete, so only Book is soft-deleted with its author.
            assert_eq!(author.triggers[1].body.as_deref(), Some("BEGIN UPDATE \"books\" SET \"deleted\" = NEW.\"deleted\", \"lock\" = \"lock\" + 1 WHERE \"writer\" = NEW.\"id\" AND \"deleted\" IS NULL; RETURN NULL; END;"));
        }
        assert_eq!(ir.models[1].triggers.len(), 1);
        let up = migration(&ir);
        assert!(up.contains(&"CREATE TRIGGER \"soft_delete\" BEFORE DELETE ON \"authors\" FOR EACH ROW WHEN (OLD.\"deleted\" IS NULL AND pg_trigger_depth() = 0) EXECUTE FUNCTION \"authors_soft_delete\"()".to_owned()), "{up:?}");
        assert!(up.contains(&"CREATE TRIGGER \"soft_delete_cascade\" AFTER UPDATE OF \"deleted\" ON \"authors\" FOR EACH ROW WHEN (OLD.\"deleted\" IS NULL AND NEW.\"deleted\" IS NOT NULL) EXECUTE FUNCTION \"authors_soft_delete_cascade\"()".to_owned()), "{up:?}");
    }

    #[test]
    fn application_mode_adds_no_ddl() {
        let mut ir = schema("sqlite", "application");
        prepared(&mut ir).unwrap();
        assert_eq!(ir.behavior.delete_rewrites.len(), 2);
        assert!(ir.behavior.storage.as_ref().unwrap().models.iter().all(|m| m.triggers.is_empty()));
    }

    #[test]
    fn proxies_inherit_soft_delete_role_without_duplicate_triggers() {
        for mode in ["application", "database"] {
            let mut ir = schema("postgres", mode);
            monorm::extension::capture_storage(&mut ir).unwrap();
            ir.behavior.lowered_models.push("AuthorView".into());
            let mut view: ModelIr = ir.models[0].clone();
            view.name = "AuthorView".into();
            ir.models.push(view);
            ir.behavior.model_views.push(monorm::extension::ModelView { model: "AuthorView".into(), source: "Author".into(), root: "Author".into(), row_filter: None, insert_table: None });
            prepared(&mut ir).unwrap();
            let rewrite = ir.behavior.delete_rewrites.iter().find(|d| d.model == "AuthorView").expect("proxy inherits root soft delete");
            assert_eq!(rewrite.field, "deleted");
            assert!(ir.models[3].triggers.is_empty());
            assert_eq!(ir.behavior.storage.as_ref().unwrap().models.iter().filter(|m| m.table == "authors").count(), 1);
        }
    }

    #[test]
    fn wrong_declarations_fail_with_their_location() {
        let err = prepared(&mut schema("postgres", "later")).unwrap_err();
        assert!(err.ends_with("mode must be \"database\" or \"application\""), "{err}");
    }

    const LOST: &str = "schema.prisma:3:3: @@soft_delete.deleted_at: Author is soft-deleted, but Note.author cascades deletes (onDelete: Cascade) and Note has no soft delete, so a soft delete of Author keeps the Note rows; fix: add @@soft_delete.deleted_at to Note, or add @soft_delete.keep_child_rows to the relation field Note.author to keep the Note rows on a soft delete";

    fn checked(mut ir: SchemaIr) -> Result<(), String> {
        prepared(&mut ir)?;
        check(&mut ir)
    }

    fn model(ir: &mut SchemaIr, value: serde_json::Value) { ir.models.push(serde_json::from_value(value).unwrap()); }

    #[test]
    fn a_cascade_to_a_child_without_soft_delete_fails_the_schema_with_both_fixes() {
        for mode in ["application", "database"] {
            assert_eq!(checked(bare("postgres", mode)).unwrap_err(), LOST, "{mode}");
        }
        assert_eq!(checked(bare("sqlite", "database")).unwrap_err(), LOST);
        checked(schema("postgres", "database")).unwrap();
        checked(schema("postgres", "application")).unwrap();
    }

    #[test]
    fn the_check_follows_cascades_through_several_levels() {
        // Author (soft) <- Book (soft) <- Page (no soft delete): the soft delete reaches Book and stops there.
        let mut ir = schema("postgres", "database");
        model(&mut ir, serde_json::json!({"name": "Page", "table": "pages", "fields": [
            {"name": "id", "column": "id", "type": "int", "primary_key": true}, {"name": "book_id", "column": "book_id", "type": "int"}],
            "relations": [{"name": "book", "kind": "one", "target": "Book", "from": "book_id", "to": "id", "foreign_key": true, "on_delete": "cascade"}]}));
        let err = checked(ir.clone()).unwrap_err();
        assert!(err.starts_with("schema.prisma:9:3: @@soft_delete.deleted_at: Book is soft-deleted, but Page.book cascades deletes"), "{err}");
        ir.behavior.declarations.push(keep_rows("Page", "book", 20));
        checked(ir.clone()).unwrap();
        // A child that the opt-out keeps is never deleted by a soft delete, so its own cascades lose nothing.
        model(&mut ir, serde_json::json!({"name": "Mark", "table": "marks", "fields": [
            {"name": "id", "column": "id", "type": "int", "primary_key": true}, {"name": "note_id", "column": "note_id", "type": "int"}],
            "relations": [{"name": "note", "kind": "one", "target": "Note", "from": "note_id", "to": "id", "foreign_key": true, "on_delete": "cascade"}]}));
        checked(ir).unwrap();
    }

    #[test]
    fn a_relation_that_does_not_cascade_is_no_loss() {
        let mut ir = bare("postgres", "database");
        for m in ir.models.iter_mut().filter(|m| m.name == "Note") { m.relations[0].on_delete = Some(OnDelete::SetNull); }
        checked(ir).unwrap();
    }

    #[test]
    fn proxies_and_storage_owners_are_checked_through_their_table() {
        let view = |ir: &mut SchemaIr, source: &str| {
            let mut view: ModelIr = ir.models.iter().find(|m| m.name == source).unwrap().clone();
            view.name = format!("{source}View");
            ir.behavior.lowered_models.push(view.name.clone());
            ir.behavior.model_views.push(monorm::extension::ModelView { model: view.name.clone(), source: source.into(), root: source.into(), row_filter: None, insert_table: None });
            ir.models.push(view);
        };
        // A relation to the proxy of a soft-delete model reaches the soft-delete table.
        let mut ir = bare("postgres", "application");
        view(&mut ir, "Author");
        ir.models.iter_mut().find(|m| m.name == "Note").unwrap().relations[0].target = "AuthorView".into();
        assert_eq!(checked(ir).unwrap_err(), LOST.replace("Author is", "AuthorView is").replace("of Author keeps", "of AuthorView keeps"));
        // A proxy of a child without soft delete is the same table: the opt-out of its root counts, and one error names the root.
        let mut ir = schema("postgres", "application");
        view(&mut ir, "Note");
        checked(ir).unwrap();
        let mut ir = bare("postgres", "application");
        view(&mut ir, "Note");
        assert_eq!(checked(ir).unwrap_err(), LOST);
        // A proxy of a soft-delete child keeps the field, so it is no lost child.
        let mut ir = schema("postgres", "database");
        view(&mut ir, "Book");
        checked(ir).unwrap();
    }

    #[test]
    fn the_opt_out_belongs_on_a_cascading_relation_and_takes_no_argument() {
        let mut ir = schema("postgres", "application");
        ir.behavior.declarations.push(keep_rows("Note", "author_id", 13));
        assert_eq!(checked(ir).unwrap_err(), "schema.prisma:13:3: @soft_delete.keep_child_rows: Note.author_id is no relation field with a foreign key; write the attribute on the relation field, for example on Note.author");
        let mut ir = schema("postgres", "application");
        let mut d = keep_rows("Note", "author", 13);
        d.arguments.insert("x".into(), serde_json::json!(1));
        ir.behavior.declarations.push(d);
        assert_eq!(checked(ir).unwrap_err(), "schema.prisma:13:3: @soft_delete.keep_child_rows: takes no argument");
        let mut ir = schema("postgres", "application");
        ir.behavior.declarations.push(serde_json::from_value(serde_json::json!({"attribute": "soft_delete.keep_child_rows", "model": "Note", "positional": [], "arguments": {}, "location": {"file": "schema.prisma", "line": 14, "column": 1}})).unwrap());
        assert_eq!(checked(ir).unwrap_err(), "schema.prisma:14:1: @@soft_delete.keep_child_rows: write it on the relation field that cascades, not on the model");
    }

    #[test]
    fn sqlite_delete_soft_deletes_cascades_and_purges_a_deleted_row() {
        let mut ir = schema("sqlite", "database");
        prepared(&mut ir).unwrap();
        let db = rusqlite::Connection::open_in_memory().unwrap();
        for statement in migration(&ir) { db.execute_batch(&statement).unwrap(); }
        db.execute_batch("INSERT INTO authors (id, rev) VALUES (1, 0), (2, 0); INSERT INTO books (id, writer, lock) VALUES (1, 1, 0), (2, 2, 0); INSERT INTO notes (id, author_id) VALUES (1, 1)").unwrap();
        let deleted = |db: &rusqlite::Connection, sql: &str| db.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(db.execute("DELETE FROM authors WHERE id = 1", []).unwrap(), 0);
        assert_eq!(deleted(&db, "SELECT count(*) FROM authors WHERE deleted IS NOT NULL"), 1);
        assert_eq!(deleted(&db, "SELECT count(*) FROM books WHERE deleted IS NOT NULL AND writer = 1"), 1);
        assert_eq!(deleted(&db, "SELECT count(*) FROM books WHERE deleted IS NULL"), 1);
        assert_eq!(deleted(&db, "SELECT count(*) FROM notes"), 1);
        assert_eq!(deleted(&db, "SELECT rev FROM authors WHERE id = 1"), 1);
        assert_eq!(deleted(&db, "SELECT lock FROM books WHERE id = 1"), 1);
        assert_eq!(deleted(&db, "SELECT lock FROM books WHERE id = 2"), 0);
        db.execute_batch("PRAGMA foreign_keys = ON").unwrap();
        assert_eq!(db.execute("DELETE FROM authors WHERE id = 1", []).unwrap(), 1);
        assert_eq!(deleted(&db, "SELECT count(*) FROM authors"), 1);
    }
}

// The package entry point; Rust callers keep the rlib API.
monorm::extension!(("monorm.soft-delete.fields", add_fields), ("monorm.soft-delete.prepare", prepare), ("monorm.soft-delete.check", check));
