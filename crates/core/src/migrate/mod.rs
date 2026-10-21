//! Migration generator.
//!
//! ```text
//! ORM schema ──build──▶ DbSchema (snapshot) ──diff(previous snapshot)──▶ [Op] ──render──▶ SQL
//! ```
//!
//! The previous snapshot is whatever the last migration recorded, so generating a
//! migration needs no database. The down migration is the diff in the other direction.

pub mod diff;
pub mod files;
pub mod model;
pub mod nested_copy;
pub mod pg;
pub mod pull;
pub mod sqlite;

use serde::Serialize;

pub use diff::Op;
pub use model::DbSchema;

use crate::schema::{Result, Schema};

#[derive(Serialize, Debug, Clone)]
pub struct Step {
    pub summary: String,
    pub sql: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

#[derive(Serialize, Debug)]
pub struct MigrationPlan {
    pub up: Vec<Step>,
    pub down: Vec<Step>,
    /// The schema after `up`: the snapshot to store with the migration.
    pub snapshot: DbSchema,
}

fn steps(ops: &[Op], target: &DbSchema) -> Vec<Step> {
    ops.iter()
        .map(|op| {
            let mut warning = op.warning();
            let parent = match op {
                Op::CreateTable { table, .. } => table.partition_of.as_ref()
                    .filter(|p| p.bound != "DEFAULT").map(|p| p.parent.as_str()),
                Op::AttachPartition { parent, bound, .. } if bound != "DEFAULT" => Some(parent.as_str()),
                _ => None,
            };
            if let Some(parent) = parent {
                if target.tables.iter().any(|t| t.partition_of.as_ref().is_some_and(|p| p.parent == parent && p.bound == "DEFAULT")) {
                    let message = format!("fails if the default partition of {parent} holds rows matching this bound; move those rows first, or detach the default partition, move the rows, and reattach it");
                    warning = Some(match warning { Some(w) => format!("{w}; {message}"), None => message });
                }
            }
            Step { summary: pg::summary(op), sql: pg::render(op, false), warning }
        })
        .collect()
}

pub fn parse_snapshot(json: &str) -> Result<DbSchema> {
    let s: DbSchema = serde_json::from_str(json).map_err(|e| format!("invalid schema snapshot: {e}"))?;
    if s.version > model::SNAPSHOT_VERSION {
        return Err(format!(
            "schema snapshot version {} is newer than this engine supports ({})",
            s.version,
            model::SNAPSHOT_VERSION
        ));
    }
    Ok(s)
}

pub fn snapshot(schema: &Schema) -> Result<DbSchema> {
    let schema = schema.physical();
    model::build(schema).map(|(s, _)| s)
}

/// The migration from `previous` (an earlier snapshot; empty for the first migration)
/// to `schema`.
pub fn plan(schema: &Schema, previous: &DbSchema) -> Result<MigrationPlan> {
    let schema = schema.physical();
    let (current, renames) = model::build(schema)?;
    if let Some(old) = &previous.identities {
        let next = current.identities.as_ref().ok_or("identity manifest removed from schema; retain frozen identities and tombstones")?;
        old.validate_successor(next)?;
    }
    if previous.version != 0 && previous.dialect != current.dialect {
        return Err(format!("migration snapshot targets {}, schema targets {}; use a separate migrations directory", previous.dialect.name(), current.dialect.name()));
    }
    let identity_changed = previous.identities != current.identities;
    let fills = diff::json_backfills(previous, &current, &renames)?;
    diff::check_copies(previous, &current, &renames)?;
    diff::check_copies(&current, previous, &renames.reversed())?;
    if current.dialect == crate::dialect::Dialect::Sqlite {
        let mut up = sqlite::steps_with(previous, &current, &renames, &fills)?;
        let mut down = sqlite::steps(&current, previous, &renames.reversed())?;
        if identity_changed { identity_step(&mut up); identity_step(&mut down); }
        return Ok(MigrationPlan { up, down, snapshot: current });
    }
    let mut up = diff::diff(previous, &current, &renames)?;
    up.extend(fills);
    let down = diff::diff(&current, previous, &renames.reversed())?;
    let mut up = steps(&up, &current);
    let mut down = steps(&down, previous);
    if identity_changed { identity_step(&mut up); identity_step(&mut down); }
    Ok(MigrationPlan { up, down, snapshot: current })
}

fn identity_step(steps: &mut Vec<Step>) {
    steps.push(Step { summary: "Record frozen ContentType identities".into(), sql: "-- Frozen ContentType identity mapping recorded in snapshot.json".into(), warning: None });
}

/// The whole schema as idempotent DDL (`IF NOT EXISTS` / `OR REPLACE`), for
/// `create_tables()` in development and tests.
pub fn create_all(schema: &Schema) -> Result<Vec<String>> {
    let current = snapshot(schema)?;
    if current.dialect == crate::dialect::Dialect::Sqlite { return Ok(sqlite::create_all(&current, true)); }
    let ops = diff::diff(&DbSchema::default(), &current, &Default::default())?;
    Ok(ops.iter().map(|op| pg::render(op, true)).collect())
}

/// The DDL that creates `db` in an empty Postgres schema, without its extensions (they
/// belong to the database, not the schema). Drift detection runs it in a shadow schema.
pub fn create_statements(db: &DbSchema) -> Vec<String> {
    diff::diff(&DbSchema::default(), db, &Default::default())
        .expect("a diff from an empty schema copies no column")
        .iter()
        .filter(|op| !matches!(op, Op::CreateExtension(_)))
        .map(|op| pg::render(op, false))
        .collect()
}

/// Drops every table, enum type and generated function of the schema (`IF EXISTS ... CASCADE`);
/// extensions stay.
pub fn drop_all(schema: &Schema) -> Result<Vec<String>> {
    let current = snapshot(schema)?;
    if current.dialect == crate::dialect::Dialect::Sqlite {
        return Ok(current.tables.iter().rev().map(|t| format!("DROP TABLE IF EXISTS {}", pg::ident(&t.name))).collect());
    }
    let mut out: Vec<String> = current
        .tables
        .iter()
        .rev()
        .map(|t| format!("DROP TABLE IF EXISTS {} CASCADE", pg::ident(&t.name)))
        .collect();
    out.extend(
        current.functions.iter().map(|f| format!("DROP FUNCTION IF EXISTS {}({}) CASCADE", pg::ident(&f.name), f.args)),
    );
    out.extend(current.enums.iter().map(|e| format!("DROP TYPE IF EXISTS {} CASCADE", e.sql())));
    Ok(out)
}

#[cfg(test)]
mod tests;
