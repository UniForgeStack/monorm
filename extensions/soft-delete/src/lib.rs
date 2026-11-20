//! `@@soft_delete.deleted_at`: the model gets a nullable `deleted_at` field, and the ORM
//! soft-deletes rows instead of deleting them.
//! Database mode adds row triggers, so a raw `DELETE` soft-deletes too
//! and the soft delete cascades to soft-delete children.
use monorm::{dialect::Dialect, extension::{views_keeping, Declaration, ReadDefault}, ir::{ColType, Expr, FieldIr, ModelIr, OnDelete, SchemaIr, TriggerEvent, TriggerIr, TriggerTiming}, sql::ident, tracking::{self, composed_root, error, Mode, Tracked}, writes::{AssignValue, DeleteRewrite, WriteAssignment, WriteKind}};

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
fn keeps_rows(ir: &SchemaIr, child: &str, relation: &str) -> bool {
    let root = ir.behavior.model_views.iter().find(|v| v.model == child).map_or(child, |v| v.root.as_str());
    ir.behavior.declarations.iter().any(|d| d.attribute == KEEP_ROWS && (d.model == child || d.model == root) && d.field.as_deref() == Some(relation))
}

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
    // A level of a composed model follows the soft delete of its root: the root row holds the flag for every level.
    let soft_roots: Vec<&str> = ir.behavior.delete_rewrites.iter().filter(|d| d.source == SOURCE).map(|d| d.model.as_str()).collect();
    let follows_root = |name: &str| tracking::composed_root(ir, name).is_some_and(|r| soft_roots.contains(&r.as_str()));
    for child in models.iter().filter(|m| !soft.contains(&m.table.as_str()) && !follows_root(&m.name)) {
        for r in child.relations.iter().filter(|r| r.foreign_key && r.on_delete == Some(OnDelete::Cascade)) {
            if !table(&r.target).is_some_and(|t| soft.contains(&t)) { continue; }
            // The link of a level to its parent is the composition itself.
            if ir.behavior.owner_links.iter().any(|l| l.child == child.name && l.parent == r.target && r.from == [l.child_key.clone()]) { continue; }
            if keeps_rows(ir, &child.name, &r.name) { continue; }
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
    // An owning relation from a soft-delete owner into a table without soft delete: the sweep has nothing to stamp, so the owned row stays when its last owner is soft-deleted.
    for owner in models.iter().filter(|m| soft.contains(&m.table.as_str())) {
        for r in owner.relations.iter().filter(|r| r.owns && r.foreign_key) {
            let Some(target) = models.iter().find(|m| m.name == r.target) else { continue };
            if soft.contains(&target.table.as_str()) || follows_root(&target.name) || keeps_rows(ir, &owner.name, &r.name) { continue; }
            let (owner_name, target_name, relation) = (&owner.name, &target.name, &r.name);
            return Err(format!("{owner_name} is soft-deleted, but {owner_name}.{relation} owns {target_name} and {target_name} has no soft delete, so the last soft delete of an owner keeps the {target_name} rows; \
                fix: add @@{} to {target_name}, or add @{KEEP_ROWS} to the relation field {owner_name}.{relation} to keep the {target_name} rows on a soft delete", TRACKED.attribute));
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

/// A table that a soft delete of a parent reaches. `keys` pairs a foreign key column with the referenced parent column.
/// A composed level keeps no soft-delete column of its own: `table` is the root table, and `via` names the level table
/// that holds the foreign key, as `(level table, its key column, the root key column)`.
struct Child { table: String, soft: String, version: Option<String>, keys: Vec<(String, String)>, via: Option<(String, String, String)>, kept: bool }

/// `AFTER UPDATE OF` the field, when a row becomes soft-deleted: soft-delete the live rows
/// of each soft-delete child whose relation cascades deletes.
fn cascade_trigger(dialect: Dialect, parent: &ModelIr, field: &str, children: &[Child]) -> Result<TriggerIr, String> {
    let c = ident(column(parent, field)?);
    let statements = children.iter().map(|child| child.statement(&format!("NEW.{c}"), &format!("{} IS NULL", ident(&child.soft)))).collect::<Vec<_>>();
    Ok(TriggerIr { name: "soft_delete_cascade".into(), timing: TriggerTiming::After, events: vec![TriggerEvent::Update], update_of: vec![field.into()],
        when: Some(format!("OLD.{c} IS NULL AND NEW.{c} IS NOT NULL")), body: Some(trigger_body(dialect, &statements)), ..Default::default() })
}

/// `AFTER UPDATE OF` the field, when a row is undeleted: undelete the rows its soft delete cascaded to,
/// those that carry the stamp of the parent. A row that was soft-deleted on its own, with another stamp, stays deleted.
/// A relation with `@soft_delete.keep_child_rows` has no revive.
fn revive_trigger(dialect: Dialect, parent: &ModelIr, field: &str, children: &[Child]) -> Result<Option<TriggerIr>, String> {
    let c = ident(column(parent, field)?);
    let statements = children.iter().filter(|child| !child.kept).map(|child| child.statement("NULL", &format!("{} = OLD.{c}", ident(&child.soft)))).collect::<Vec<_>>();
    if statements.is_empty() { return Ok(None); }
    Ok(Some(TriggerIr { name: "soft_delete_revive".into(), timing: TriggerTiming::After, events: vec![TriggerEvent::Update], update_of: vec![field.into()],
        when: Some(format!("OLD.{c} IS NOT NULL AND NEW.{c} IS NULL")), body: Some(trigger_body(dialect, &statements)), ..Default::default() }))
}

impl Child {
    /// `UPDATE child SET soft = <value> WHERE <link to the parent row> AND <guard>`.
    fn statement(&self, value: &str, guard: &str) -> String {
        let Child { table, soft, version, keys, via, .. } = self;
        let (table, soft) = (ident(table), ident(soft));
        let bump = bump(version.as_deref());
        let link: Vec<String> = keys.iter().map(|(key, to)| format!("{} = NEW.{}", ident(key), ident(to))).collect();
        let matched = match via {
            None => link.join(" AND "),
            Some((level, level_key, root_key)) => format!("{} IN (SELECT {} FROM {} WHERE {})", ident(root_key), ident(level_key), ident(level), link.join(" AND ")),
        };
        format!("UPDATE {table} SET {soft} = {value}{bump} WHERE {matched} AND {guard}")
    }
}

/// The row is live: its soft-delete field is `NULL`.
fn live(field: &str) -> serde_json::Value { Expr::is_null(Expr::col(field), false).to_json() }

/// An owning relation (`owns`) into a soft-delete target. The guard of the target reads every one of them,
/// and the triggers of the owner act on its own.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Owning {
    /// The owner model, its table, its soft-delete column (none: the owner has no soft delete) and its key columns.
    model: String,
    table: String,
    soft: Option<String>,
    keys: Vec<String>,
    /// `(owner column, target column)` for each key field of the relation.
    pairs: Vec<(String, String)>,
    target_table: String,
    target_soft: String,
    target_version: Option<String>,
}

/// Every owning relation into a soft-delete target, over the models of this batch and of earlier ones.
fn owning_relations(ir: &SchemaIr, roles: &[SoftDelete]) -> Result<Vec<Owning>, String> {
    let models = all_models(ir);
    let mut out = vec![];
    for m in &models {
        for r in m.relations.iter().filter(|r| r.owns && r.foreign_key) {
            let Some(target) = models.iter().find(|t| t.name == r.target) else { continue };
            // A composed level has no soft-delete column of its own: the root holds it, and owning into a level is not a target here.
            let Some(role) = roles.iter().find(|s| s.model == target.name) else { continue };
            let soft = roles.iter().find(|s| s.model == m.name).map(|s| column(m, &s.field).map(str::to_owned)).transpose()?;
            let pairs = r.from.iter().zip(&r.to).map(|(f, t)| Ok((column(m, f)?.to_owned(), column(target, t)?.to_owned()))).collect::<Result<Vec<_>, String>>()?;
            let keys = m.key_fields().into_iter().map(|k| column(m, k).map(str::to_owned)).collect::<Result<Vec<_>, String>>()?;
            let target_version = bumped(&ir.behavior.write_assignments, &target.name).map(|f| column(target, f).map(str::to_owned)).transpose()?;
            out.push(Owning { model: m.name.clone(), table: m.table.clone(), soft, keys, pairs, target_table: target.table.clone(), target_soft: column(target, &role.field)?.to_owned(), target_version });
        }
    }
    Ok(out)
}

/// The guard of the table `target`: `NOT EXISTS` for each owning column into it, true when no live row still owns the target row.
/// The row of the target table that the statement names is `target.<key>`. An owner that has no soft delete is always live.
/// A row of the target table never owns itself. `spare` leaves one more owner row out of the arms of its table: the row
/// that a revive trigger reads as `NEW`, given as its table and key columns.
fn guard(all: &[Owning], target: &str, spare: Option<(&str, &[String])>) -> Vec<String> {
    let t = ident(target);
    let mut arms: Vec<&Owning> = vec![];
    for o in all.iter().filter(|o| o.target_table == target) {
        if !arms.iter().any(|a| a.table == o.table && a.pairs == o.pairs) { arms.push(o); }
    }
    arms.iter().enumerate().map(|(i, a)| {
        let alias = format!("o{i}");
        let mut conds: Vec<String> = a.pairs.iter().map(|(from, to)| format!("{alias}.{} = {t}.{}", ident(from), ident(to))).collect();
        if let Some(soft) = &a.soft { conds.push(format!("{alias}.{} IS NULL", ident(soft))); }
        if a.table == target {
            conds.push(format!("NOT ({})", a.pairs.iter().map(|(_, to)| format!("{alias}.{0} = {t}.{0}", ident(to))).collect::<Vec<_>>().join(" AND ")));
        }
        if let Some((_, keys)) = spare.filter(|(table, _)| *table == a.table) {
            conds.push(format!("NOT ({})", keys.iter().map(|k| format!("{alias}.{0} = NEW.{0}", ident(k))).collect::<Vec<_>>().join(" AND ")));
        }
        format!("NOT EXISTS (SELECT 1 FROM {} AS {alias} WHERE {})", ident(&a.table), conds.join(" AND "))
    }).collect()
}

/// `PERFORM 1 FROM <target> WHERE <key> = <row>.<fk> FOR NO KEY UPDATE`: on PostgreSQL a trigger takes the lock in its own statement,
/// so the statement after it reads what a concurrent co-owner delete has committed. SQLite has one writer and needs no lock.
fn lock_target(dialect: Dialect, o: &Owning, row: &str) -> Option<String> {
    (dialect == Dialect::Postgres).then(|| {
        let m = o.pairs.iter().map(|(from, to)| format!("{} = {row}.{}", ident(to), ident(from))).collect::<Vec<_>>().join(" AND ");
        format!("PERFORM 1 FROM {} WHERE {m} FOR NO KEY UPDATE", ident(&o.target_table))
    })
}

/// `AFTER UPDATE OF` the field, when an owner becomes soft-deleted: soft-delete the target of each owning relation that
/// no live owner holds any more, with the owner's stamp. It reads `OLD`, so a key move in the same statement releases the old target.
fn sweep_trigger(dialect: Dialect, all: &[Owning], rels: &[&Owning], field: &str) -> TriggerIr {
    let c = ident(field);
    let mut statements = vec![];
    for o in rels {
        statements.extend(lock_target(dialect, o, "OLD"));
        let (t, ts) = (ident(&o.target_table), ident(&o.target_soft));
        let m = o.pairs.iter().map(|(from, to)| format!("{} = OLD.{}", ident(to), ident(from))).collect::<Vec<_>>().join(" AND ");
        let mut conds = vec![m, format!("{ts} IS NULL")];
        conds.extend(guard(all, &o.target_table, None));
        statements.push(format!("UPDATE {t} SET {ts} = NEW.{c}{} WHERE {}", bump(o.target_version.as_deref()), conds.join(" AND ")));
    }
    TriggerIr { name: "soft_delete_sweep".into(), timing: TriggerTiming::After, events: vec![TriggerEvent::Update], update_of: vec![field.into()],
        when: Some(format!("OLD.{c} IS NULL AND NEW.{c} IS NOT NULL")), body: Some(trigger_body(dialect, &statements)), ..Default::default() }
}

/// `AFTER UPDATE OF` the field, when an owner is undeleted: revive the target that its delete stamped (the same stamp),
/// and the target that is deleted while this owner is its only live owner.
fn owned_revive_trigger(dialect: Dialect, all: &[Owning], rels: &[&Owning], field: &str) -> TriggerIr {
    let c = ident(field);
    let mut statements = vec![];
    for o in rels {
        statements.extend(lock_target(dialect, o, "NEW"));
        let (t, ts) = (ident(&o.target_table), ident(&o.target_soft));
        let m = o.pairs.iter().map(|(from, to)| format!("{} = NEW.{}", ident(to), ident(from))).collect::<Vec<_>>().join(" AND ");
        let others = guard(all, &o.target_table, Some((&o.table, &o.keys)));
        let alone = if others.is_empty() { String::new() } else { format!(" OR ({})", others.join(" AND ")) };
        statements.push(format!("UPDATE {t} SET {ts} = NULL{} WHERE {m} AND {ts} IS NOT NULL AND ({ts} = OLD.{c}{alone})", bump(o.target_version.as_deref())));
    }
    TriggerIr { name: "soft_delete_owned_revive".into(), timing: TriggerTiming::After, events: vec![TriggerEvent::Update], update_of: vec![field.into()],
        when: Some(format!("OLD.{c} IS NOT NULL AND NEW.{c} IS NULL")), body: Some(trigger_body(dialect, &statements)), ..Default::default() }
}

/// The statements as a trigger body: a PostgreSQL function body, or the bare statements of a SQLite trigger.
fn trigger_body(dialect: Dialect, statements: &[String]) -> String {
    match dialect {
        Dialect::Postgres => format!("BEGIN {}; RETURN NULL; END;", statements.join("; ")),
        Dialect::Sqlite => statements.join("; "),
    }
}

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
        // The levels below a composed root read the root's flag through the join to the root.
        let levels: Vec<String> = tracking::composed_levels(ir, &s.model).iter().map(|l| l.child.clone()).collect();
        for child in levels {
            if roles.iter().any(|r| r.model == child) || ir.behavior.read_defaults.iter().any(|d| d.source == SOURCE && d.model == child) { continue; }
            ir.behavior.read_defaults.push(ReadDefault { source: SOURCE.into(), name: DEFAULT.into(), model: child,
                filter: Some(live(&s.field)), ..Default::default() });
        }
    }
    ir.behavior.delete_rewrites.retain(|d| d.source != SOURCE);
    for s in &roles {
        ir.behavior.delete_rewrites.push(DeleteRewrite { source: SOURCE.into(), model: s.model.clone(), field: s.field.clone(), guard: live(&s.field) });
    }
    let dialect = ir.dialect;
    for s in roles.iter().filter(|s| s.mode == Mode::Database && added.contains(&s.model)) {
        // Children: soft-delete models, and the levels of a composed model with a soft-delete root, with a cascading foreign key to this model.
        let mut children = vec![];
        for child in &ir.models {
            let composed = composed_root(ir, &child.name);
            let role_model = composed.as_deref().unwrap_or(&child.name);
            let Some(child_field) = roles.iter().find(|c| c.model == role_model) else { continue };
            let Some(soft_model) = ir.models.iter().find(|m| m.name == role_model) else { continue };
            for r in child.relations.iter().filter(|r| r.foreign_key && r.target == s.model && r.on_delete == Some(OnDelete::Cascade)) {
                // The link of a level to its parent is the composition itself: the root's soft delete covers every level.
                if ir.behavior.owner_links.iter().any(|l| l.child == child.name && l.parent == r.target && r.from == [l.child_key.clone()]) { continue; }
                let version = bumped(&ir.behavior.write_assignments, role_model).map(|f| column(soft_model, f).map(str::to_owned)).transpose()?;
                let mut keys = vec![];
                let mut holders = std::collections::BTreeSet::new();
                for f in &r.from {
                    keys.push(column(child, f)?.to_owned());
                    holders.insert(ir.behavior.field_storage.iter().find(|m| m.model == child.name && &m.field == f).map_or(child.name.as_str(), |m| m.owner.as_str()));
                }
                let via = match (composed.is_some(), holders.iter().next()) {
                    (true, Some(&holder)) if holder != role_model => {
                        if holders.len() > 1 { return Err(format!("{}.{}: the foreign key fields of a soft-deleted cascade must sit in one level table", child.name, r.name)); }
                        let level = ir.behavior.storage.iter().flat_map(|st| &st.models).chain(&ir.models).find(|m| m.name == holder).ok_or_else(|| format!("unknown composition level {holder}"))?;
                        let key = |m: &ModelIr| m.key_fields().first().map(|f| column(m, f).map(str::to_owned)).unwrap_or_else(|| Err(format!("{} has no primary key", m.name)));
                        Some((level.table.clone(), key(level)?, key(soft_model)?))
                    }
                    _ => None,
                };
                let kept = keeps_rows(ir, &child.name, &r.name);
                children.push((Child { table: soft_model.table.clone(), soft: column(soft_model, &child_field.field)?.to_owned(), version, keys: vec![], via, kept }, keys, r.to.clone()));
            }
        }
        let version = bumped(&ir.behavior.write_assignments, &s.model).map(str::to_owned);
        for model in ir.models_named_mut(&s.model) {
            let version = version.as_deref();
            let mut triggers = vec![delete_trigger(dialect, model, &s.field, version)?];
            if !children.is_empty() {
                let resolved = children.iter().map(|(child, keys, to)| {
                    let pairs = keys.iter().zip(to).map(|(k, to)| Ok((k.clone(), column(model, to)?.to_owned()))).collect::<Result<Vec<_>, String>>()?;
                    Ok(Child { table: child.table.clone(), soft: child.soft.clone(), version: child.version.clone(), keys: pairs, via: child.via.clone(), kept: child.kept })
                }).collect::<Result<Vec<_>, String>>()?;
                triggers.push(cascade_trigger(dialect, model, &s.field, &resolved)?);
                triggers.extend(revive_trigger(dialect, model, &s.field, &resolved)?);
            }
            for mut t in triggers {
                t.replaces_rule = s.rule.clone();
                model.push_trigger(t)?;
            }
        }
    }
    owning_triggers(ir, &roles, &added)
}

/// The sweep and the owned revive of every database-mode owner. The guard of a target reads each owning column into it,
/// so a new owning column changes the triggers of every owner of that target, including the owners of earlier batches:
/// their triggers are made again, and the migration diff replaces the functions.
/// A model of an earlier batch is a database-mode owner when it carries the `soft_delete` trigger.
fn owning_triggers(ir: &mut SchemaIr, roles: &[SoftDelete], added: &[String]) -> Result<(), String> {
    let all = owning_relations(ir, roles)?;
    let dialect = ir.dialect;
    let mut owners: Vec<(String, String)> = vec![];
    for o in &all {
        let Some(field) = roles.iter().find(|s| s.model == o.model && o.soft.is_some()).map(|s| s.field.clone()) else { continue };
        let database = match roles.iter().find(|s| s.model == o.model) {
            Some(s) if added.contains(&s.model) => s.mode == Mode::Database,
            _ => all_models(ir).iter().any(|m| m.name == o.model && m.triggers.iter().any(|t| t.name == "soft_delete")),
        };
        if database && !owners.iter().any(|(m, _)| m == &o.model) { owners.push((o.model.clone(), field)); }
    }
    for (name, field) in owners {
        let rels: Vec<&Owning> = all.iter().filter(|o| o.model == name).collect();
        let triggers = [sweep_trigger(dialect, &all, &rels, &field), owned_revive_trigger(dialect, &all, &rels, &field)];
        for model in ir.models_named_mut(&name) {
            model.triggers.retain(|t| triggers.iter().all(|n| n.name != t.name));
            for t in &triggers { model.push_trigger(t.clone())?; }
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
            assert_eq!(names, ["soft_delete", "soft_delete_cascade", "soft_delete_revive"]);
            assert_eq!(author.triggers[0].when.as_deref(), Some("OLD.\"deleted\" IS NULL AND pg_trigger_depth() = 0"));
            assert_eq!(author.triggers[0].body.as_deref(), Some("BEGIN UPDATE \"authors\" SET \"deleted\" = now(), \"rev\" = \"rev\" + 1 WHERE \"id\" = OLD.\"id\"; RETURN NULL; END;"));
            // Note has no soft delete, so only Book is soft-deleted with its author.
            assert_eq!(author.triggers[1].body.as_deref(), Some("BEGIN UPDATE \"books\" SET \"deleted\" = NEW.\"deleted\", \"lock\" = \"lock\" + 1 WHERE \"writer\" = NEW.\"id\" AND \"deleted\" IS NULL; RETURN NULL; END;"));
            // The revive undeletes the rows that carry the stamp of the parent.
            let revive = &author.triggers[2];
            assert_eq!(revive.when.as_deref(), Some("OLD.\"deleted\" IS NOT NULL AND NEW.\"deleted\" IS NULL"));
            assert_eq!(revive.body.as_deref(), Some("BEGIN UPDATE \"books\" SET \"deleted\" = NULL, \"lock\" = \"lock\" + 1 WHERE \"writer\" = NEW.\"id\" AND \"deleted\" = OLD.\"deleted\"; RETURN NULL; END;"));
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
    fn keep_child_rows_gives_the_relation_no_revive() {
        let mut ir = bare("postgres", "database");
        ir.behavior.declarations.push(keep_rows("Book", "author", 12));
        ir.behavior.declarations.push(keep_rows("Note", "author", 13));
        prepared(&mut ir).unwrap();
        let names: Vec<_> = ir.models[0].triggers.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["soft_delete", "soft_delete_cascade"]);
    }

    #[test]
    fn sqlite_revive_cascade_undeletes_the_rows_with_the_stamp_of_the_parent() {
        let mut ir = schema("sqlite", "database");
        prepared(&mut ir).unwrap();
        let db = rusqlite::Connection::open_in_memory().unwrap();
        for statement in migration(&ir) { db.execute_batch(&statement).unwrap(); }
        db.execute_batch("INSERT INTO authors (id, rev) VALUES (1, 0); INSERT INTO books (id, writer, lock) VALUES (1, 1, 0), (2, 1, 0), (3, 1, 0);
            UPDATE books SET deleted = '2020-01-01 00:00:00.000' WHERE id = 3").unwrap();
        db.execute("DELETE FROM authors WHERE id = 1", []).unwrap();
        let live = |sql: &str| db.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(live("SELECT count(*) FROM books WHERE deleted IS NOT NULL"), 3);
        db.execute("UPDATE authors SET deleted = NULL WHERE id = 1", []).unwrap();
        // Books 1 and 2 follow the stamp of the author; book 3 was deleted on its own and stays deleted.
        assert_eq!(live("SELECT count(*) FROM books WHERE deleted IS NULL AND id IN (1, 2)"), 2);
        assert_eq!(live("SELECT count(*) FROM books WHERE id = 3 AND deleted = '2020-01-01 00:00:00.000'"), 1);
        assert_eq!(live("SELECT lock FROM books WHERE id = 1"), 2);
    }

    #[test]
    fn an_owning_relation_into_a_table_without_soft_delete_fails_the_schema_with_both_fixes() {
        let mut ir = owning("postgres", 1);
        ir.behavior.declarations.retain(|d| d.model != "Display");
        let err = checked(ir.clone()).unwrap_err();
        assert_eq!(err, "B1 is soft-deleted, but B1.display owns Display and Display has no soft delete, so the last soft delete of an owner keeps the Display rows; \
            fix: add @@soft_delete.deleted_at to Display, or add @soft_delete.keep_child_rows to the relation field B1.display to keep the Display rows on a soft delete");
        ir.behavior.declarations.push(keep_rows("B1", "display", 30));
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

    /// A target `Display` with a version field and `owners` models `B1..Bn`, each with one owning column `display_id` into it. Every model has soft delete in database mode.
    fn owning(dialect: &str, owners: usize) -> SchemaIr {
        let key = serde_json::json!({"name": "id", "column": "id", "type": "int", "primary_key": true});
        let mut models = vec![serde_json::json!({"name": "Display", "table": "displays", "fields": [key, {"name": "version", "column": "rev", "type": "int"}]})];
        for k in 1..=owners {
            models.push(serde_json::json!({"name": format!("B{k}"), "table": format!("b{k}"), "fields": [key, {"name": "display_id", "column": "display_id", "type": "int"}],
                "relations": [{"name": "display", "kind": "one", "target": "Display", "from": "display_id", "to": "id", "foreign_key": true, "owns": true}]}));
        }
        let mut ir: SchemaIr = serde_json::from_value(serde_json::json!({"dialect": dialect, "models": models,
            "behavior": {"schema_contract": 5, "write_assignments": [{"source": "monorm.locking", "model": "Display", "field": "version", "on": ["update", "delete_rewrite"], "value": "increment"}]}})).unwrap();
        let line = std::cell::Cell::new(2);
        let named = |model: &str| { line.set(line.get() + 1); at(model, line.get(), serde_json::json!(["deleted"]), serde_json::json!({"mode": "database"}), None) };
        ir.behavior.declarations = std::iter::once("Display".to_owned()).chain((1..=owners).map(|k| format!("B{k}"))).map(|m| named(&m)).collect();
        monorm::extension::capture_storage(&mut ir).unwrap();
        add_fields(&mut ir).unwrap();
        ir.behavior.storage = None;
        ir
    }

    fn trigger<'a>(ir: &'a SchemaIr, model: &str, name: &str) -> &'a TriggerIr {
        ir.models.iter().find(|m| m.name == model).unwrap().triggers.iter().find(|t| t.name == name).unwrap_or_else(|| panic!("{model} has no trigger {name}"))
    }

    #[test]
    fn one_owning_column_makes_a_sweep_with_a_lock_and_an_owned_revive() {
        let mut ir = owning("postgres", 1);
        prepared(&mut ir).unwrap();
        let sweep = trigger(&ir, "B1", "soft_delete_sweep");
        assert_eq!((sweep.timing, sweep.events.clone(), sweep.update_of.clone()), (TriggerTiming::After, vec![TriggerEvent::Update], vec!["deleted".to_owned()]));
        assert_eq!(sweep.when.as_deref(), Some(r#"OLD."deleted" IS NULL AND NEW."deleted" IS NOT NULL"#));
        assert_eq!(sweep.body.as_deref(), Some(concat!(
            r#"BEGIN PERFORM 1 FROM "displays" WHERE "id" = OLD."display_id" FOR NO KEY UPDATE; "#,
            r#"UPDATE "displays" SET "deleted" = NEW."deleted", "rev" = "rev" + 1 WHERE "id" = OLD."display_id" AND "deleted" IS NULL "#,
            r#"AND NOT EXISTS (SELECT 1 FROM "b1" AS o0 WHERE o0."display_id" = "displays"."id" AND o0."deleted" IS NULL); RETURN NULL; END;"#)));
        let revive = trigger(&ir, "B1", "soft_delete_owned_revive");
        assert_eq!(revive.when.as_deref(), Some(r#"OLD."deleted" IS NOT NULL AND NEW."deleted" IS NULL"#));
        assert_eq!(revive.body.as_deref(), Some(concat!(
            r#"BEGIN PERFORM 1 FROM "displays" WHERE "id" = NEW."display_id" FOR NO KEY UPDATE; "#,
            r#"UPDATE "displays" SET "deleted" = NULL, "rev" = "rev" + 1 WHERE "id" = NEW."display_id" AND "deleted" IS NOT NULL "#,
            r#"AND ("deleted" = OLD."deleted" OR (NOT EXISTS (SELECT 1 FROM "b1" AS o0 WHERE o0."display_id" = "displays"."id" AND o0."deleted" IS NULL AND NOT (o0."id" = NEW."id")))); RETURN NULL; END;"#)));
        // The target is no owner: it has no owning trigger.
        assert!(ir.models[0].triggers.iter().all(|t| !t.name.contains("sweep") && !t.name.contains("owned")));
    }

    #[test]
    fn sqlite_has_no_lock() {
        let mut ir = owning("sqlite", 1);
        prepared(&mut ir).unwrap();
        let body = trigger(&ir, "B1", "soft_delete_sweep").body.clone().unwrap();
        assert!(body.starts_with(r#"UPDATE "displays" SET"#) && !body.contains("PERFORM") && !body.contains("FOR NO KEY"), "{body}");
    }

    #[test]
    fn four_owning_columns_give_every_owner_one_arm_each() {
        let mut ir = owning("postgres", 4);
        prepared(&mut ir).unwrap();
        for k in 1..=4 {
            let body = trigger(&ir, &format!("B{k}"), "soft_delete_sweep").body.clone().unwrap();
            assert_eq!(body.matches("NOT EXISTS").count(), 4, "{body}");
            for arm in 1..=4 { assert!(body.contains(&format!(r#"FROM "b{arm}" AS o{}"#, arm - 1)), "{body}"); }
            // The statement itself names only the owner's own key.
            assert!(body.contains(r#"WHERE "id" = OLD."display_id" AND "deleted" IS NULL AND NOT EXISTS"#), "{body}");
        }
    }

    #[test]
    fn a_new_owning_column_changes_the_function_of_every_owner_of_the_target() {
        let (mut four, mut five) = (owning("postgres", 4), owning("postgres", 5));
        prepared(&mut four).unwrap();
        prepared(&mut five).unwrap();
        let (before, after) = (migration(&four), migration(&five));
        let functions = |sql: &[String], k: usize| sql.iter().filter(|s| s.contains("soft_delete_sweep") && s.contains(&format!(r#""b{k}""#))).cloned().collect::<Vec<_>>();
        for k in 1..=4 {
            let (old, new) = (functions(&before, k), functions(&after, k));
            assert!(!old.is_empty() && old != new, "B{k}: the sweep of an old owner stays when a fifth column appears");
            assert!(new.iter().any(|s| s.contains(r#"FROM "b5" AS o4"#)), "B{k}: {new:?}");
        }
    }

    /// A second batch adds the fifth owner: the models of the first batch are lowered and keep their records, and their triggers are made again.
    #[test]
    fn a_later_batch_replaces_the_sweep_of_the_owners_of_an_earlier_batch() {
        let mut first = owning("postgres", 4);
        prepared(&mut first).unwrap();
        for d in &mut first.behavior.declarations { d.lowered = true; }
        first.behavior.lowered_models = first.models.iter().map(|m| m.name.clone()).collect();
        let five = owning("postgres", 5);
        first.models.push(five.models.iter().find(|m| m.name == "B5").unwrap().clone());
        first.behavior.declarations.extend(five.behavior.declarations.iter().filter(|d| d.model == "B5").cloned());
        prepared(&mut first).unwrap();
        for k in 1..=5 {
            let body = trigger(&first, &format!("B{k}"), "soft_delete_sweep").body.clone().unwrap();
            assert_eq!(body.matches("NOT EXISTS").count(), 5, "B{k}: {body}");
        }
        // The physical copy of an earlier owner holds the new function too: the migration reads it.
        let physical = first.behavior.storage.as_ref().unwrap().models.iter().find(|m| m.name == "B1").unwrap();
        assert_eq!(physical.triggers.iter().filter(|t| t.name == "soft_delete_sweep").count(), 1);
        assert_eq!(physical.triggers.iter().find(|t| t.name == "soft_delete_sweep").unwrap().body.as_deref().unwrap().matches("NOT EXISTS").count(), 5);
    }

    #[test]
    fn a_self_owning_column_excludes_the_target_row_itself() {
        let mut ir: SchemaIr = serde_json::from_value(serde_json::json!({"dialect": "postgres", "models": [{"name": "Node", "table": "nodes", "fields": [
            {"name": "id", "column": "id", "type": "int", "primary_key": true}, {"name": "parent_id", "column": "parent_id", "type": "int", "nullable": true}],
            "relations": [{"name": "parent", "kind": "one", "target": "Node", "from": "parent_id", "to": "id", "foreign_key": true, "owns": true}]}],
            "behavior": {"schema_contract": 5}})).unwrap();
        ir.behavior.declarations = vec![at("Node", 3, serde_json::json!(["deleted"]), serde_json::json!({"mode": "database"}), None)];
        monorm::extension::capture_storage(&mut ir).unwrap();
        add_fields(&mut ir).unwrap();
        ir.behavior.storage = None;
        prepared(&mut ir).unwrap();
        let body = trigger(&ir, "Node", "soft_delete_sweep").body.clone().unwrap();
        assert!(body.contains(r#"AND NOT (o0."id" = "nodes"."id")"#), "{body}");
    }

    #[test]
    fn application_mode_adds_no_owning_trigger() {
        let mut ir = owning("postgres", 2);
        for d in &mut ir.behavior.declarations { d.arguments.insert("mode".into(), serde_json::json!("application")); }
        prepared(&mut ir).unwrap();
        assert!(ir.models.iter().all(|m| m.triggers.is_empty()));
    }

    #[test]
    fn sqlite_sweep_and_revive_end_to_end() {
        let mut ir = owning("sqlite", 2);
        prepared(&mut ir).unwrap();
        let db = rusqlite::Connection::open_in_memory().unwrap();
        for statement in migration(&ir) { db.execute_batch(&statement).unwrap(); }
        db.execute_batch("INSERT INTO displays (id, rev) VALUES (1, 0), (2, 0); INSERT INTO b1 (id, display_id) VALUES (1, 1), (2, 2); INSERT INTO b2 (id, display_id) VALUES (1, 1)").unwrap();
        let count = |sql: &str| db.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
        // Two owners of display 1: the first delete keeps it, the second stamps it.
        db.execute("DELETE FROM b1 WHERE id = 1", []).unwrap();
        assert_eq!(count("SELECT count(*) FROM displays WHERE deleted IS NOT NULL"), 0);
        db.execute("DELETE FROM b2 WHERE id = 1", []).unwrap();
        assert_eq!(count("SELECT count(*) FROM displays WHERE id = 1 AND deleted IS NOT NULL AND deleted = (SELECT deleted FROM b2 WHERE id = 1)"), 1);
        assert_eq!(count("SELECT rev FROM displays WHERE id = 1"), 1);
        // Undelete of b2 revives the display with its stamp.
        db.execute("UPDATE b2 SET deleted = NULL WHERE id = 1", []).unwrap();
        assert_eq!(count("SELECT count(*) FROM displays WHERE id = 1 AND deleted IS NULL"), 1);
        assert_eq!(count("SELECT rev FROM displays WHERE id = 1"), 2);
        // One statement archives every owner of display 2 and one key move: the old target is released.
        db.execute("UPDATE b1 SET display_id = 1, deleted = strftime('%Y-%m-%d %H:%M:%f', 'now') WHERE id = 2", []).unwrap();
        assert_eq!(count("SELECT count(*) FROM displays WHERE id = 2 AND deleted IS NOT NULL"), 1);
        assert_eq!(count("SELECT count(*) FROM displays WHERE id = 1 AND deleted IS NULL"), 1);
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
