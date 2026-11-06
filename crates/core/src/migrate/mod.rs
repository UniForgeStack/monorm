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
pub mod online;
pub mod pg;
pub mod pull;
mod reconcile;
pub mod sqlite;

use serde::Serialize;

pub use diff::{Destructive, DestructiveKind, Op};
pub use model::DbSchema;
pub use online::{plan_online, FolderKind};

use crate::schema::{Result, Schema};

#[derive(Serialize, Debug, Clone)]
pub struct Step {
    pub summary: String,
    pub sql: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    /// The data the step can delete; serialized as a flag for the bindings.
    #[serde(serialize_with = "any")]
    pub destructive: Vec<Destructive>,
    /// `false` for a step that PostgreSQL refuses inside a transaction (`CREATE INDEX CONCURRENTLY`).
    pub transaction: bool,
}

impl Step {
    pub fn new(summary: String, sql: String, warning: Option<String>, destructive: Vec<Destructive>) -> Step {
        Step { summary, sql, warning, destructive, transaction: true }
    }
}

fn any<S: serde::Serializer>(list: &[Destructive], s: S) -> std::result::Result<S::Ok, S::Error> {
    s.serialize_bool(!list.is_empty())
}

#[derive(Serialize, Debug)]
pub struct MigrationPlan {
    pub up: Vec<Step>,
    pub down: Vec<Step>,
    /// The schema after `up`: the snapshot to store with the migration.
    pub snapshot: DbSchema,
    /// What the folder of this plan is. Not serialized: the bindings see the merged plan.
    #[serde(skip)]
    pub kind: FolderKind,
}

fn steps(ops: &[Op], target: &DbSchema) -> Vec<Step> {
    steps_with(ops, target, |_| pg::Online::default())
}

/// The steps of `ops`; `online` chooses the form of each operation.
fn steps_with(ops: &[Op], target: &DbSchema, online: impl Fn(&Op) -> pg::Online) -> Vec<Step> {
    let destructive = diff::destructive_ops(ops);
    ops.iter()
        .zip(destructive)
        .map(|(op, destructive)| {
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
            let online = online(op);
            let mut step = Step::new(pg::summary(op), pg::render_with(op, false, online), warning, destructive.into_iter().collect());
            step.transaction = !online.concurrently;
            step
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

/// What `plan` and `plan_online` both start from: the schema after the change and its checks.
struct Prepared {
    current: DbSchema,
    renames: model::Renames,
    identity_changed: bool,
    /// The ops that fill new nested subfields; they follow the diff in `up`.
    fills: Vec<Op>,
}

fn prepare(schema: &Schema, previous: &DbSchema) -> Result<Prepared> {
    let schema = schema.physical();
    let (mut current, renames) = model::build(schema)?;
    reconcile::reconcile(schema, previous, &mut current);
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
    diff::check_generated_to_plain(previous, &current, &renames)?;
    diff::check_copies(&current, previous, &renames.reversed())?;
    Ok(Prepared { current, renames, identity_changed, fills })
}

/// The PostgreSQL ops of the change and of its reverse.
fn pg_ops(previous: &DbSchema, p: &Prepared) -> Result<(Vec<Op>, Vec<Op>)> {
    let mut up = diff::diff(previous, &p.current, &p.renames)?;
    up.extend(p.fills.iter().cloned());
    let down = diff::diff(&p.current, previous, &p.renames.reversed())?;
    Ok((up, down))
}

/// The migration from `previous` (an earlier snapshot; empty for the first migration)
/// to `schema`.
pub fn plan(schema: &Schema, previous: &DbSchema) -> Result<MigrationPlan> {
    let p = prepare(schema, previous)?;
    if p.current.dialect == crate::dialect::Dialect::Sqlite {
        return sqlite_plan(previous, p);
    }
    let (up, down) = pg_ops(previous, &p)?;
    Ok(pg_plan(previous, p, &up, &down))
}

fn sqlite_plan(previous: &DbSchema, p: Prepared) -> Result<MigrationPlan> {
    let mut up = sqlite::steps_with(previous, &p.current, &p.renames, &p.fills)?;
    let mut down = sqlite::steps(&p.current, previous, &p.renames.reversed())?;
    if p.identity_changed { identity_step(&mut up); identity_step(&mut down); }
    Ok(MigrationPlan { up, down, snapshot: p.current, kind: FolderKind::Main })
}

fn pg_plan(previous: &DbSchema, p: Prepared, up: &[Op], down: &[Op]) -> MigrationPlan {
    let mut up = steps(up, &p.current);
    let mut down = steps(down, previous);
    if p.identity_changed { identity_step(&mut up); identity_step(&mut down); }
    MigrationPlan { up, down, snapshot: p.current, kind: FolderKind::Main }
}

/// Shadow databases can add parentheses to equivalent native expressions.
/// Compare their checked structure so pull and drift do not report those as changes.
pub fn reconcile_introspection(previous: &DbSchema, current: &mut DbSchema) {
    let pulled = pull::pull(current, &[]);
    if let Ok(schema) = crate::dsl::compile(&pulled.schema, None).and_then(Schema::from_ir) {
        reconcile::reconcile(&schema, previous, current);
    }
}

fn identity_step(steps: &mut Vec<Step>) {
    steps.push(Step::new("Record frozen ContentType identities".into(), "-- Frozen ContentType identity mapping recorded in snapshot.json".into(), None, vec![]));
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
