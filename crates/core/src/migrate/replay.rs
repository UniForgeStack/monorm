//! The schema state of a migration history: one rule per operation applies it to a [`DbSchema`].
//! There is no snapshot file. The state after migration N is the replay of the operations of
//! migrations 1 to N.

use std::collections::HashMap;

use super::diff::{self, Constraint, Op};
use super::model::{Column, DbSchema, PartitionOf, SqlView, Table};
use super::ops::{Operation, OpKind};

fn table_mut<'a>(db: &'a mut DbSchema, name: &str) -> Result<&'a mut Table, String> {
    db.tables.iter_mut().find(|t| t.name == name).ok_or_else(|| format!("unknown table {name}"))
}

fn column_mut<'a>(db: &'a mut DbSchema, table: &str, column: &str) -> Result<&'a mut Column, String> {
    table_mut(db, table)?.columns.iter_mut().find(|c| c.name == column).ok_or_else(|| format!("unknown column {table}.{column}"))
}

fn rename_everywhere(db: &mut DbSchema, tables: &[(&str, &str)], columns: &[((&str, &str), &str)]) {
    let table_map: HashMap<&str, &str> = tables.iter().copied().collect();
    let col_map: HashMap<(&str, &str), &str> = columns.iter().copied().collect();
    db.tables = db.tables.iter().map(|t| diff::apply_renames(t, &table_map, &col_map)).collect();
}

/// Applies one schema operation to `db`.
pub fn apply(db: &mut DbSchema, op: &Op) -> Result<(), String> {
    match op {
        Op::CreateExtension(e) => {
            if !db.extensions.iter().any(|x| x.name == e.name) { db.extensions.push(e.clone()); }
        }
        Op::DropExtension(e) => db.extensions.retain(|x| x.name != e.name),
        Op::CreateFunction(f) => match db.functions.iter_mut().find(|x| x.key() == f.key()) {
            Some(x) => *x = f.clone(),
            None => db.functions.push(f.clone()),
        },
        Op::DropFunction(f) => db.functions.retain(|x| x.key() != f.key()),
        Op::CreateEnum(e) => match db.enums.iter_mut().find(|x| x.name == e.name) {
            Some(x) => *x = e.clone(),
            None => db.enums.push(e.clone()),
        },
        Op::DropEnum(e) => db.enums.retain(|x| x.name != e.name),
        Op::AddEnumValue { name, value, before } => {
            let e = db.enums.iter_mut().find(|e| &e.name == name).ok_or_else(|| format!("unknown enum type {name}"))?;
            let at = before.as_ref().and_then(|b| e.values.iter().position(|v| v == b)).unwrap_or(e.values.len());
            e.values.insert(at, value.clone());
        }
        Op::RecreateEnum { from, to, .. } => {
            let e = db.enums.iter_mut().find(|e| e.name == from.name).ok_or_else(|| format!("unknown enum type {}", from.name))?;
            *e = to.clone();
        }
        Op::CommentOnType { name, comment } => {
            db.enums.iter_mut().find(|e| &e.name == name).ok_or_else(|| format!("unknown enum type {name}"))?.comment = comment.clone();
        }
        Op::CreateTable { table, foreign_keys } => {
            let mut t = table.clone();
            t.foreign_keys = foreign_keys.clone();
            db.tables.retain(|x| x.name != t.name);
            db.tables.push(t);
        }
        Op::DropTable { table } => {
            if !db.tables.iter().any(|t| &t.name == table) { return Err(format!("unknown table {table}")); }
            db.tables.retain(|t| &t.name != table);
        }
        Op::DetachPartition { table, .. } => table_mut(db, table)?.partition_of = None,
        Op::AttachPartition { table, parent, bound } => {
            table_mut(db, table)?.partition_of = Some(PartitionOf { parent: parent.clone(), bound: bound.clone() });
        }
        Op::RenameTable { from, to } => {
            table_mut(db, from)?;
            rename_everywhere(db, &[(from, to)], &[]);
        }
        Op::RenameColumn { table, from, to } => {
            column_mut(db, table, from)?;
            rename_everywhere(db, &[], &[((table, from), to)]);
        }
        Op::AddColumn { table, column } => table_mut(db, table)?.columns.push(column.clone()),
        Op::BackfillColumn { .. } | Op::BackfillJson { .. } | Op::RenameJsonKey { .. } | Op::CopyColumns { .. } => {}
        Op::DropColumn { table, column, .. } => {
            let t = table_mut(db, table)?;
            if !t.columns.iter().any(|c| &c.name == column) { return Err(format!("unknown column {table}.{column}")); }
            t.columns.retain(|c| &c.name != column);
        }
        Op::AlterColumnType { table, column, to, .. } => column_mut(db, table, column)?.ty = to.clone(),
        Op::SetNotNull { table, column } => column_mut(db, table, column)?.nullable = false,
        Op::DropNotNull { table, column } => column_mut(db, table, column)?.nullable = true,
        Op::SetDefault { table, column, default } => column_mut(db, table, column)?.default = Some(default.clone()),
        Op::DropDefault { table, column } => column_mut(db, table, column)?.default = None,
        Op::AddIdentity { table, column } => column_mut(db, table, column)?.identity = true,
        Op::DropIdentity { table, column } => column_mut(db, table, column)?.identity = false,
        Op::AddConstraint { table, constraint } => {
            let t = table_mut(db, table)?;
            match constraint {
                Constraint::PrimaryKey(c) => t.primary_key = Some(c.clone()),
                Constraint::Unique(c) => t.uniques.push(c.clone()),
                Constraint::Check(c) => t.checks.push(c.clone()),
                Constraint::Exclusion(c) => t.exclusions.push(c.clone()),
                Constraint::ForeignKey(c) => t.foreign_keys.push(c.clone()),
            }
        }
        Op::DropConstraint { table, name } => {
            let t = table_mut(db, table)?;
            let before = (t.uniques.len(), t.checks.len(), t.exclusions.len(), t.foreign_keys.len(), t.primary_key.is_some());
            if t.primary_key.as_ref().is_some_and(|p| &p.name == name) { t.primary_key = None; }
            t.uniques.retain(|c| &c.name != name);
            t.checks.retain(|c| &c.name != name);
            t.exclusions.retain(|c| &c.name != name);
            t.foreign_keys.retain(|c| &c.name != name);
            if before == (t.uniques.len(), t.checks.len(), t.exclusions.len(), t.foreign_keys.len(), t.primary_key.is_some()) {
                return Err(format!("unknown constraint {name} on {table}"));
            }
        }
        Op::RenameConstraint { table, from, to } => {
            let t = table_mut(db, table)?;
            let mut found = false;
            let mut rename = |n: &mut String| if n == from { *n = to.clone(); found = true; };
            if let Some(p) = &mut t.primary_key { rename(&mut p.name); }
            t.uniques.iter_mut().for_each(|c| rename(&mut c.name));
            t.checks.iter_mut().for_each(|c| rename(&mut c.name));
            t.exclusions.iter_mut().for_each(|c| rename(&mut c.name));
            t.foreign_keys.iter_mut().for_each(|c| rename(&mut c.name));
            if !found { return Err(format!("unknown constraint {from} on {table}")); }
        }
        Op::CreateIndex { table, index } => {
            if let Some(v) = db.views.iter_mut().find(|v| &v.name == table) {
                v.indexes.push(index.clone());
            } else {
                table_mut(db, table)?.indexes.push(index.clone());
            }
        }
        Op::DropIndex { name } => {
            let mut found = false;
            for t in &mut db.tables { let n = t.indexes.len(); t.indexes.retain(|i| &i.name != name); found |= n != t.indexes.len(); }
            for v in &mut db.views { let n = v.indexes.len(); v.indexes.retain(|i| &i.name != name); found |= n != v.indexes.len(); }
            if !found { return Err(format!("unknown index {name}")); }
        }
        Op::RenameIndex { from, to } => {
            let mut found = false;
            let tables = db.tables.iter_mut().flat_map(|t| t.indexes.iter_mut());
            let views = db.views.iter_mut().flat_map(|v| v.indexes.iter_mut());
            for i in tables.chain(views) { if &i.name == from { i.name = to.clone(); found = true; } }
            if !found { return Err(format!("unknown index {from}")); }
        }
        Op::CreateTrigger { table, trigger } => table_mut(db, table)?.triggers.push(trigger.clone()),
        Op::DropTrigger { table, name } => table_mut(db, table)?.triggers.retain(|t| &t.name != name),
        Op::DropRule { table, name } => table_mut(db, table)?.rules.retain(|r| r != name),
        Op::RuleNotRestored { table, name } => {
            let t = table_mut(db, table)?;
            if !t.rules.contains(name) { t.rules.push(name.clone()); }
        }
        Op::SetRowSecurity { table, rls } => table_mut(db, table)?.rls = *rls,
        Op::CreatePolicy { table, policy } => table_mut(db, table)?.policies.push(policy.clone()),
        Op::DropPolicy { table, name } => table_mut(db, table)?.policies.retain(|p| &p.name != name),
        Op::CommentOnTable { table, comment } => table_mut(db, table)?.comment = comment.clone(),
        Op::CommentOnColumn { table, column, comment } => column_mut(db, table, column)?.comment = comment.clone(),
        Op::CreateView(v) => {
            db.views.retain(|x| x.name != v.name);
            // The view's indexes arrive as their own CreateIndex operations.
            db.views.push(SqlView { indexes: vec![], ..v.clone() });
        }
        Op::DropView { name, .. } => db.views.retain(|v| &v.name != name),
    }
    Ok(())
}

/// Applies `op` to `db`: its `state` list for a `separate` operation, no change for SQL and code.
pub fn apply_operation(db: &mut DbSchema, op: &Operation) -> Result<(), String> {
    match &op.kind {
        OpKind::Schema(op) => apply(db, op),
        OpKind::Separate { state, .. } => state.iter().try_for_each(|op| apply_operation(db, op)),
        OpKind::RunSql(_) | OpKind::RunCode(_) | OpKind::Commit => Ok(()),
    }
}

/// The state after `ops`, applied to `db`.
pub fn replay(db: &mut DbSchema, ops: &[Operation]) -> Result<(), String> {
    for (i, op) in ops.iter().enumerate() {
        apply_operation(db, op).map_err(|e| format!("operation {}: {e}", i + 1))?;
    }
    Ok(())
}

/// The lists of a schema in a fixed order, so two states compare equal when only the order of
/// names inside a table differs. The version is a format number and takes no part.
fn normalized(db: &DbSchema) -> serde_json::Value {
    let mut db = db.clone();
    db.version = 0;
    // the identity manifest is not part of the replayed state: no operation carries it
    db.identities = None;
    db.tables.sort_by(|a, b| a.name.cmp(&b.name));
    for t in &mut db.tables {
        // a column added later sits last, whatever its place in the schema
        t.columns.sort_by(|a, b| a.name.cmp(&b.name));
        t.uniques.sort_by(|a, b| a.name.cmp(&b.name));
        t.checks.sort_by(|a, b| a.name.cmp(&b.name));
        t.exclusions.sort_by(|a, b| a.name.cmp(&b.name));
        t.foreign_keys.sort_by(|a, b| a.name.cmp(&b.name));
        t.indexes.sort_by(|a, b| a.name.cmp(&b.name));
        t.triggers.sort_by(|a, b| a.name.cmp(&b.name));
        t.policies.sort_by(|a, b| a.name.cmp(&b.name));
    }
    db.enums.sort_by(|a, b| a.name.cmp(&b.name));
    db.extensions.sort_by(|a, b| a.name.cmp(&b.name));
    db.views.sort_by(|a, b| a.name.cmp(&b.name));
    for v in &mut db.views { v.indexes.sort_by(|a, b| a.name.cmp(&b.name)); }
    db.functions.sort_by_key(|f| f.key());
    serde_json::to_value(&db).unwrap_or_default()
}

/// Whether two states are the same schema.
pub fn same(a: &DbSchema, b: &DbSchema) -> bool {
    normalized(a) == normalized(b)
}

/// The `separate` operation with an empty `database` list that makes `replayed` equal to `target`,
/// or none when they are the same. It carries the state that no schema operation reaches: the
/// models the migrations do not manage and the shape of a `json` column. Each table that differs
/// comes whole, as a `create_table` that replaces the table of the same name.
pub fn state_operation(replayed: &DbSchema, target: &DbSchema) -> Option<Operation> {
    if same(replayed, target) { return None; }
    let json = |t: &Table| serde_json::to_value(t).unwrap_or_default();
    let changed = target.tables.iter().filter(|t| replayed.tables.iter().find(|r| r.name == t.name).is_none_or(|r| json(r) != json(t)));
    let mut state: Vec<Operation> = changed.map(|t| Operation::schema(Op::CreateTable { table: t.clone(), foreign_keys: t.foreign_keys.clone() })).collect();
    state.extend(replayed.tables.iter().filter(|r| !target.tables.iter().any(|t| t.name == r.name)).map(|r| Operation::schema(Op::DropTable { table: r.name.clone() })));
    if state.is_empty() { return None; }
    Some(Operation::new(OpKind::Separate { database: vec![], state }))
}
