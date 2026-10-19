//! `@soft_delete.deleted_at`: the ORM soft-deletes rows instead of deleting them.
//! Database mode adds row triggers, so a raw `DELETE` soft-deletes too and the soft delete
//! cascades to soft-delete children.
use orm_contracts::{dialect::Dialect, extension::{Declaration, ReadDefault}, ir::{ColType, ModelIr, OnDelete, SchemaIr, TriggerIr}, writes::{AssignValue, DeleteRewrite, WriteAssignment, WriteKind}};

/// The read default that hides soft-deleted rows; `without_defaults("deleted")` bypasses it.
const SOURCE: &str = "orm.soft-delete";
const DEFAULT: &str = "deleted";

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

/// A soft-delete field: declared on a model, or copied to a proxy of its storage owner.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SoftDelete {
    model: String,
    field: String,
    mode: Mode,
}

fn error(d: &Declaration, message: impl std::fmt::Display) -> String {
    format!("{}:{}:{}: @{}: {message}", d.location.file, d.location.line, d.location.column, d.attribute)
}

/// The field a delete-time assignment of this model increments, if any: the version bump of a soft delete.
fn bumped<'a>(assignments: &'a [WriteAssignment], model: &str) -> Option<&'a str> {
    assignments.iter().find(|a| a.model == model && a.value == AssignValue::Increment && a.on.contains(&WriteKind::DeleteRewrite)).map(|a| a.field.as_str())
}

/// A user filter that only repeats the `"deleted"` default hides the rows `all_with_deleted()` and the other members must reach.
fn repeats_default(filter: &str, field: &str) -> bool {
    let mut text: String = filter.split_whitespace().collect();
    while text.starts_with('(') && text.ends_with(')') { text = text[1..text.len() - 1].to_owned(); }
    text == format!("{field}==null") || text == format!("{field}isnull")
}

fn reject_leftover_filters(ir: &SchemaIr, roles: &[SoftDelete]) -> Result<(), String> {
    for s in roles {
        for d in ir.behavior.declarations.iter().filter(|d| d.model == s.model && d.field.is_none()) {
            let filter = match d.attribute.as_str() {
                "query.filter" => d.positional.get(1).and_then(|v| v.as_str()),
                "query.defaults" => d.arguments.get("filter").and_then(|v| v.as_str()),
                _ => None,
            };
            if filter.is_some_and(|f| repeats_default(f, &s.field)) {
                return Err(error(d, format!("{} already hides soft-deleted rows by default; remove this filter, it would also hide them from all_with_deleted(), deleted_only(), undelete() and hard_delete()", s.model)));
            }
        }
    }
    Ok(())
}

fn ident(s: &str) -> String { format!("\"{}\"", s.replace('"', "\"\"")) }

fn column<'m>(model: &'m ModelIr, field: &str) -> Result<&'m str, String> {
    model.fields.iter().find(|f| f.name == field).map(|f| f.column.as_str()).ok_or_else(|| format!("{} has no field {field}", model.name))
}

fn trigger(name: &str, value: serde_json::Value) -> Result<TriggerIr, String> {
    let mut value = value;
    value["name"] = name.into();
    serde_json::from_value(value).map_err(|e| e.to_string())
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
        Dialect::Sqlite => (format!("UPDATE {t} SET {c} = CURRENT_TIMESTAMP{bump} WHERE {pk}; SELECT RAISE(IGNORE)"), format!("OLD.{c} IS NULL")),
    };
    trigger("soft_delete", serde_json::json!({"timing": "before", "events": ["delete"], "when": when, "body": body}))
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
    trigger("soft_delete_cascade", serde_json::json!({"timing": "after", "events": ["update"], "update_of": [field],
        "when": format!("OLD.{c} IS NULL AND NEW.{c} IS NOT NULL"), "body": body}))
}

pub fn prepare(ir: &mut SchemaIr) -> Result<(), String> {
    let declarations: Vec<_> = ir.behavior.declarations.iter().filter(|d| !d.lowered && d.attribute.starts_with("soft_delete.")).cloned().collect();
    // Models of an earlier batch keep their records; only the models of this batch need a mode.
    let mut roles: Vec<SoftDelete> = ir.behavior.delete_rewrites.iter().filter(|d| d.source == SOURCE)
        .map(|d| SoftDelete { model: d.model.clone(), field: d.field.clone(), mode: Mode::Application }).collect();
    let mut added = vec![];
    for d in &declarations {
        if d.attribute != "soft_delete.deleted_at" { return Err(error(d, "unknown soft_delete attribute")); }
        let name = d.field.clone().ok_or_else(|| error(d, "is a field attribute"))?;
        let mode = Mode::parse(d.arguments.get("mode")).map_err(|e| error(d, e))?;
        let model = ir.models.iter().find(|m| m.name == d.model).ok_or_else(|| error(d, "unknown model"))?;
        let field = model.fields.iter().find(|f| f.name == name).ok_or_else(|| error(d, "unknown field"))?;
        if field.ty != ColType::DateTime || field.array || !field.nullable { return Err(error(d, format!("{}.{name} must be a nullable DateTime field", d.model))); }
        if roles.iter().any(|s| s.model == d.model) { return Err(error(d, format!("{} has more than one soft-delete field", d.model))); }
        roles.push(SoftDelete { model: d.model.clone(), field: name, mode });
        added.push(d.model.clone());
    }
    // A proxy follows the shared storage of its root; its logical rows need the same ORM
    // update on delete. Database triggers are generated only for declarations.
    for proxy in &ir.behavior.proxy_models {
        if roles.iter().any(|s| s.model == proxy.model) { continue; }
        if let Some(source) = roles.iter().find(|s| s.model == proxy.storage_owner) {
            if !ir.models.iter().find(|m| m.name == proxy.model).is_some_and(|m| m.fields.iter().any(|f| f.name == source.field)) {
                return Err(format!("{}: proxy must keep the soft-delete field {}", proxy.model, source.field));
            }
            roles.push(SoftDelete { model: proxy.model.clone(), field: source.field.clone(), mode: source.mode });
        }
    }
    ir.behavior.read_defaults.retain(|d| d.source != SOURCE);
    for s in &roles {
        let item = serde_json::json!({"t": "col", "path": [], "name": s.field});
        ir.behavior.read_defaults.push(ReadDefault { source: SOURCE.into(), name: DEFAULT.into(), model: s.model.clone(),
            filter: Some(serde_json::json!({"t": "is_null", "item": item, "neg": false})), ..Default::default() });
    }
    reject_leftover_filters(ir, &roles)?;
    ir.behavior.delete_rewrites.retain(|d| d.source != SOURCE);
    for s in &roles {
        let item = serde_json::json!({"t": "col", "path": [], "name": s.field});
        ir.behavior.delete_rewrites.push(DeleteRewrite { source: SOURCE.into(), model: s.model.clone(), field: s.field.clone(),
            guard: serde_json::json!({"t": "is_null", "item": item, "neg": false}) });
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
        let models = ir.models.iter_mut().chain(ir.behavior.storage.iter_mut().flat_map(|st| st.models.iter_mut()));
        for model in models.filter(|m| m.name == s.model) {
            let version = bumped(&ir.behavior.write_assignments, &model.name);
            let mut triggers = vec![delete_trigger(dialect, model, &s.field, version)?];
            if !children.is_empty() {
                let resolved = children.iter().map(|(t, c, v, keys, to)| {
                    let pairs = keys.iter().zip(to).map(|(k, to)| Ok((k.clone(), column(model, to)?.to_owned()))).collect::<Result<Vec<_>, String>>()?;
                    Ok((t.clone(), c.clone(), v.clone(), pairs))
                }).collect::<Result<Vec<_>, String>>()?;
                triggers.push(cascade_trigger(dialect, model, &s.field, &resolved)?);
            }
            for t in triggers {
                if model.triggers.iter().any(|x| x.name == t.name) { return Err(format!("{} already has a trigger {}", model.name, t.name)); }
                model.triggers.push(t);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(dialect: &str, mode: &str, nullable: bool) -> SchemaIr {
        let lock = |model: &str| serde_json::json!({"source": "orm.locking", "model": model, "field": "version", "on": ["update", "upsert", "delete_rewrite"], "value": "increment"});
        let at = |model: &str, line: u32| serde_json::json!({"attribute": "soft_delete.deleted_at", "model": model, "field": "deleted", "positional": [], "arguments": {"mode": mode}, "location": {"file": "schema.prisma", "line": line, "column": 3}});
        serde_json::from_value(serde_json::json!({
            "dialect": dialect,
            "models": [
                {"name": "Author", "table": "authors", "fields": [
                    {"name": "id", "column": "id", "type": "int", "primary_key": true},
                    {"name": "deleted", "column": "deleted_at", "type": "date_time", "nullable": nullable},
                    {"name": "version", "column": "rev", "type": "int"}
                ]},
                {"name": "Book", "table": "books", "fields": [
                    {"name": "id", "column": "id", "type": "int", "primary_key": true},
                    {"name": "author_id", "column": "writer", "type": "int"},
                    {"name": "deleted", "column": "removed_at", "type": "date_time", "nullable": true},
                    {"name": "version", "column": "lock", "type": "int"}
                ], "relations": [{"name": "author", "kind": "one", "target": "Author", "from": "author_id", "to": "id", "foreign_key": true, "on_delete": "cascade"}]},
                {"name": "Note", "table": "notes", "fields": [
                    {"name": "id", "column": "id", "type": "int", "primary_key": true},
                    {"name": "author_id", "column": "author_id", "type": "int"}
                ], "relations": [{"name": "author", "kind": "one", "target": "Author", "from": "author_id", "to": "id", "foreign_key": true, "on_delete": "cascade"}]}
            ],
            "behavior": {"schema_contract": 2, "declarations": [at("Author", 3), at("Book", 9)], "write_assignments": [lock("Author"), lock("Book")]}
        })).unwrap()
    }

    fn prepared(ir: &mut SchemaIr) -> Result<(), String> {
        orm_contracts::extension::capture_storage(ir)?;
        prepare(ir)
    }

    fn migration(ir: &SchemaIr) -> Vec<String> {
        let storage = serde_json::json!({"dialect": ir.dialect, "models": ir.behavior.storage.as_ref().unwrap().models});
        let schema = orm_core::schema::Schema::from_ir(serde_json::from_value(storage).unwrap()).unwrap();
        orm_core::migrate::plan(&schema, &Default::default()).unwrap().up.into_iter().map(|s| s.sql).collect()
    }

    #[test]
    fn preparation_changes_only_its_declared_effects_once() {
        let effects = orm_contracts::extension::pass_effects(&schema("postgres", "database", true), prepare).unwrap();
        assert_eq!(effects.into_iter().collect::<Vec<_>>(), ["behavior.delete_rewrites", "behavior.read_defaults", "behavior.storage", "models"]);
        let effects = orm_contracts::extension::pass_effects(&schema("postgres", "application", true), prepare).unwrap();
        assert_eq!(effects.into_iter().collect::<Vec<_>>(), ["behavior.delete_rewrites", "behavior.read_defaults"]);
    }

    #[test]
    fn each_soft_delete_model_gets_a_deleted_read_default() {
        let mut ir = schema("postgres", "application", true);
        prepared(&mut ir).unwrap();
        let defaults: Vec<_> = ir.behavior.read_defaults.iter().map(|d| (d.source.as_str(), d.name.as_str(), d.model.as_str())).collect();
        assert_eq!(defaults, [("orm.soft-delete", "deleted", "Author"), ("orm.soft-delete", "deleted", "Book")]);
        let filter = ir.behavior.read_defaults[1].filter.clone().unwrap();
        assert_eq!(filter, serde_json::json!({"t": "is_null", "item": {"t": "col", "path": [], "name": "deleted"}, "neg": false}));
    }

    #[test]
    fn each_soft_delete_model_gets_a_delete_rewrite_with_the_default_filter() {
        for mode in ["application", "database"] {
            let mut ir = schema("postgres", mode, true);
            prepared(&mut ir).unwrap();
            let rewrites: Vec<_> = ir.behavior.delete_rewrites.iter().map(|d| (d.source.as_str(), d.model.as_str(), d.field.as_str())).collect();
            assert_eq!(rewrites, [("orm.soft-delete", "Author", "deleted"), ("orm.soft-delete", "Book", "deleted")]);
            assert_eq!(Some(&ir.behavior.delete_rewrites[1].guard), ir.behavior.read_defaults[1].filter.as_ref());
        }
    }

    #[test]
    fn a_proxy_gets_the_delete_rewrite_of_its_root() {
        let mut ir = schema("postgres", "application", true);
        orm_contracts::extension::capture_storage(&mut ir).unwrap();
        ir.behavior.lowered_models.push("AuthorView".into());
        let mut view: ModelIr = serde_json::from_value(serde_json::to_value(&ir.models[0]).unwrap()).unwrap();
        view.name = "AuthorView".into();
        ir.models.push(view);
        ir.behavior.proxy_models.push(orm_contracts::extension::ProxyModel { model: "AuthorView".into(), parent: "Author".into(), storage_owner: "Author".into(), ..Default::default() });
        prepared(&mut ir).unwrap();
        assert!(ir.behavior.delete_rewrites.iter().any(|d| d.model == "AuthorView" && d.field == "deleted"));
    }

    #[test]
    fn a_model_has_one_soft_delete_field() {
        let mut ir = schema("postgres", "application", true);
        let again = ir.behavior.declarations[0].clone();
        ir.behavior.declarations.push(again);
        assert_eq!(prepared(&mut ir).unwrap_err(), "schema.prisma:3:3: @soft_delete.deleted_at: Author has more than one soft-delete field");
    }

    #[test]
    fn a_proxy_must_keep_the_soft_delete_field() {
        let mut ir = schema("postgres", "application", true);
        orm_contracts::extension::capture_storage(&mut ir).unwrap();
        ir.behavior.lowered_models.push("AuthorView".into());
        let mut view: ModelIr = serde_json::from_value(serde_json::to_value(&ir.models[0]).unwrap()).unwrap();
        view.name = "AuthorView".into();
        view.fields.retain(|f| f.name != "deleted");
        ir.models.push(view);
        ir.behavior.proxy_models.push(orm_contracts::extension::ProxyModel { model: "AuthorView".into(), parent: "Author".into(), storage_owner: "Author".into(), ..Default::default() });
        assert_eq!(prepared(&mut ir).unwrap_err(), "AuthorView: proxy must keep the soft-delete field deleted");
    }

    #[test]
    fn a_leftover_filter_on_the_deleted_field_is_rejected() {
        let declare = |attribute: &str, positional: serde_json::Value, arguments: serde_json::Value| serde_json::from_value::<orm_contracts::extension::Declaration>(serde_json::json!({
            "attribute": attribute, "model": "Author", "positional": positional, "arguments": arguments, "location": {"file": "schema.prisma", "line": 6, "column": 1}})).unwrap();
        for filter in ["deleted == null", "( deleted==null )", "deleted is null"] {
            for d in [declare("query.filter", serde_json::json!(["live", filter]), serde_json::json!({})), declare("query.defaults", serde_json::json!([]), serde_json::json!({"filter": filter}))] {
                let mut ir = schema("postgres", "application", true);
                ir.behavior.declarations.push(d);
                let err = prepared(&mut ir).unwrap_err();
                assert!(err.starts_with("schema.prisma:6:1: @query.") && err.contains("Author already hides soft-deleted rows by default; remove this filter"), "{err}");
            }
        }
        let mut ir = schema("postgres", "application", true);
        ir.behavior.declarations.push(declare("query.filter", serde_json::json!(["mine", "deleted == null and id > 0"]), serde_json::json!({})));
        prepared(&mut ir).unwrap();
    }

    #[test]
    fn triggers_bump_the_version_the_write_assignments_name_not_a_locking_declaration() {
        let mut ir = schema("postgres", "database", true);
        ir.behavior.write_assignments.retain(|a| a.model != "Book");
        ir.behavior.declarations.push(serde_json::from_value(serde_json::json!({"attribute": "locking.version", "model": "Book", "field": "version", "positional": [], "arguments": {}, "location": {"file": "schema.prisma", "line": 4, "column": 3}})).unwrap());
        prepared(&mut ir).unwrap();
        let cascade = ir.models[0].triggers[1].body.as_deref().unwrap();
        assert!(!cascade.contains("\"lock\""), "{cascade}");
        let mut ir = schema("postgres", "database", true);
        ir.behavior.write_assignments.iter_mut().filter(|a| a.model == "Author").for_each(|a| a.on = vec![WriteKind::Update]);
        prepared(&mut ir).unwrap();
        assert!(!ir.models[0].triggers[0].body.as_deref().unwrap().contains("\"rev\""));
    }

    #[test]
    fn to_one_references_to_a_soft_delete_model_are_typed_nullable() {
        use orm_core::codegen::{python, typescript, Options};
        let mut ir = schema("postgres", "application", true);
        prepared(&mut ir).unwrap();
        ir.behavior.declarations.clear();
        ir.behavior.storage = None;
        let value = serde_json::to_value(&ir).unwrap();
        let ir: orm_core::ir::SchemaIr = serde_json::from_value(value.clone()).unwrap();
        let core = orm_core::schema::Schema::from_ir(serde_json::from_value(value).unwrap()).unwrap();
        let py = python::generate_with(&ir, &core, "schema.prisma", &Options::default()).unwrap();
        assert_eq!(py.stub.matches("author: f.BelongsTo[Author | None,").count(), 2, "{}", py.stub);
        let ts = typescript::generate_with(&ir, &core, "schema.prisma", "orm", &Options::default()).unwrap();
        assert_eq!(ts.matches(r#"Hop<"author", "opt", AuthorSpec>], true>"#).count(), 2, "{ts}");
    }

    #[test]
    fn database_mode_adds_delete_and_cascade_triggers() {
        let mut ir = schema("postgres", "database", true);
        prepared(&mut ir).unwrap();
        assert_eq!(ir.behavior.delete_rewrites[1].model, "Book");
        for author in [&ir.models[0], &ir.behavior.storage.as_ref().unwrap().models[0]] {
            let names: Vec<_> = author.triggers.iter().map(|t| t.name.as_str()).collect();
            assert_eq!(names, ["soft_delete", "soft_delete_cascade"]);
            assert_eq!(author.triggers[0].when.as_deref(), Some("OLD.\"deleted_at\" IS NULL AND pg_trigger_depth() = 0"));
            assert_eq!(author.triggers[0].body.as_deref(), Some("BEGIN UPDATE \"authors\" SET \"deleted_at\" = now(), \"rev\" = \"rev\" + 1 WHERE \"id\" = OLD.\"id\"; RETURN NULL; END;"));
            // Note has no soft delete, so only Book is soft-deleted with its author.
            assert_eq!(author.triggers[1].body.as_deref(), Some("BEGIN UPDATE \"books\" SET \"removed_at\" = NEW.\"deleted_at\", \"lock\" = \"lock\" + 1 WHERE \"writer\" = NEW.\"id\" AND \"removed_at\" IS NULL; RETURN NULL; END;"));
        }
        assert_eq!(ir.models[1].triggers.len(), 1);
        let up = migration(&ir);
        assert!(up.contains(&"CREATE TRIGGER \"soft_delete\" BEFORE DELETE ON \"authors\" FOR EACH ROW WHEN (OLD.\"deleted_at\" IS NULL AND pg_trigger_depth() = 0) EXECUTE FUNCTION \"authors_soft_delete\"()".to_owned()), "{up:?}");
        assert!(up.contains(&"CREATE TRIGGER \"soft_delete_cascade\" AFTER UPDATE OF \"deleted_at\" ON \"authors\" FOR EACH ROW WHEN (OLD.\"deleted_at\" IS NULL AND NEW.\"deleted_at\" IS NOT NULL) EXECUTE FUNCTION \"authors_soft_delete_cascade\"()".to_owned()), "{up:?}");
    }

    #[test]
    fn application_mode_adds_no_ddl() {
        let mut ir = schema("sqlite", "application", true);
        prepared(&mut ir).unwrap();
        assert_eq!(ir.behavior.delete_rewrites.len(), 2);
        assert!(ir.behavior.storage.as_ref().unwrap().models.iter().all(|m| m.triggers.is_empty()));
    }

    #[test]
    fn proxies_inherit_soft_delete_role_without_duplicate_triggers() {
        for mode in ["application", "database"] {
            let mut ir = schema("postgres", mode, true);
            orm_contracts::extension::capture_storage(&mut ir).unwrap();
            ir.behavior.lowered_models.push("AuthorView".into());
            let mut view: ModelIr = serde_json::from_value(serde_json::to_value(&ir.models[0]).unwrap()).unwrap();
            view.name = "AuthorView".into();
            ir.models.push(view);
            ir.behavior.proxy_models.push(orm_contracts::extension::ProxyModel {
                model: "AuthorView".into(), parent: "Author".into(), storage_owner: "Author".into(), ..Default::default()
            });
            prepared(&mut ir).unwrap();
            let rewrite = ir.behavior.delete_rewrites.iter().find(|d| d.model == "AuthorView").expect("proxy inherits root soft delete");
            assert_eq!(rewrite.field, "deleted");
            assert!(ir.models[3].triggers.is_empty());
            assert_eq!(ir.behavior.storage.as_ref().unwrap().models.iter().filter(|m| m.table == "authors").count(), 1);
        }
    }

    #[test]
    fn wrong_declarations_fail_with_their_location() {
        let err = prepared(&mut schema("postgres", "database", false)).unwrap_err();
        assert_eq!(err, "schema.prisma:3:3: @soft_delete.deleted_at: Author.deleted must be a nullable DateTime field");
        let err = prepared(&mut schema("postgres", "later", true)).unwrap_err();
        assert!(err.ends_with("mode must be \"database\" or \"application\""), "{err}");
    }

    #[test]
    fn sqlite_delete_soft_deletes_cascades_and_purges_a_deleted_row() {
        let mut ir = schema("sqlite", "database", true);
        prepared(&mut ir).unwrap();
        let db = rusqlite::Connection::open_in_memory().unwrap();
        for statement in migration(&ir) { db.execute_batch(&statement).unwrap(); }
        db.execute_batch("INSERT INTO authors (id, rev) VALUES (1, 0), (2, 0); INSERT INTO books (id, writer, lock) VALUES (1, 1, 0), (2, 2, 0); INSERT INTO notes (id, author_id) VALUES (1, 1)").unwrap();
        let deleted = |db: &rusqlite::Connection, sql: &str| db.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(db.execute("DELETE FROM authors WHERE id = 1", []).unwrap(), 0);
        assert_eq!(deleted(&db, "SELECT count(*) FROM authors WHERE deleted_at IS NOT NULL"), 1);
        assert_eq!(deleted(&db, "SELECT count(*) FROM books WHERE removed_at IS NOT NULL AND writer = 1"), 1);
        assert_eq!(deleted(&db, "SELECT count(*) FROM books WHERE removed_at IS NULL"), 1);
        assert_eq!(deleted(&db, "SELECT count(*) FROM notes"), 1);
        assert_eq!(deleted(&db, "SELECT rev FROM authors WHERE id = 1"), 1);
        assert_eq!(deleted(&db, "SELECT lock FROM books WHERE id = 1"), 1);
        assert_eq!(deleted(&db, "SELECT lock FROM books WHERE id = 2"), 0);
        db.execute_batch("PRAGMA foreign_keys = ON").unwrap();
        assert_eq!(db.execute("DELETE FROM authors WHERE id = 1", []).unwrap(), 1);
        assert_eq!(deleted(&db, "SELECT count(*) FROM authors"), 1);
    }
}
