//! Migration generator.
//!
//! ```text
//! ORM schema ──build──▶ DbSchema (snapshot) ──diff(replayed previous state)──▶ [Op] ──render──▶ SQL
//! ```
//!
//! The previous state is what replaying the migration files gives, so generating a
//! migration needs no database. The down migration is the diff in the other direction.

pub mod diff;
pub mod files;
pub mod json_sql;
pub mod model;
pub mod nested_copy;
pub mod online;
pub mod ops;
pub mod pg;
pub mod pull;
mod reconcile;
pub mod render;
pub mod replay;
pub mod sqlite;

use serde::Serialize;

pub use diff::{Destructive, DestructiveKind, Op};
pub use model::DbSchema;

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
    /// The schema after `up`.
    pub snapshot: DbSchema,
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
    let levels = level_links(schema);
    model::build_with(schema.physical(), false, &levels, &auto_fields(schema)).map(|(s, _)| s)
}

/// The owner links of a composed model, by table and key column of the physical schema.
fn level_links(schema: &Schema) -> Vec<model::LevelLink> {
    let owners = &schema.physical().models;
    schema.owner_links.iter().map(|l| model::LevelLink {
        child: owners[l.child.0].table().to_owned(),
        child_key: owners[l.child.0].fields()[l.child_key].column.clone(),
        parent: owners[l.parent.0].table().to_owned(),
        parent_key: owners[l.parent.0].fields()[l.parent_key].column.clone(),
    }).collect()
}

/// The fields that a write assignment of the logical schema sets on every update.
fn auto_fields(schema: &Schema) -> model::Auto {
    schema.models.iter()
        .map(|m| (m.ir.name.clone(), m.assignments.iter().filter(|a| a.applies_to(crate::writes::WriteKind::Update)).map(|a| m.fields()[a.field].name.clone()).collect()))
        .collect()
}

/// [`snapshot`] of a database that still holds the rules that triggers of the schema replace: the baseline of a database that `pull` read.
pub fn baseline_snapshot(schema: &Schema) -> Result<DbSchema> {
    let levels = level_links(schema);
    model::build_with(schema.physical(), true, &levels, &auto_fields(schema)).map(|(s, _)| s)
}

/// What `plan` and `plan_online` both start from: the schema after the change and its checks.
struct Prepared {
    current: DbSchema,
    renames: model::Renames,
    identity_changed: bool,
    unmanaged_changed: bool,
    /// The shape of a json column changed in a way that has no SQL (see [`shape_changed`]).
    shape_changed: bool,
    /// The data ops on json columns that follow the diff in `up`: the key renames, then the fills of new subfields.
    fills: Vec<Op>,
    /// The key renames that follow the diff in `down`.
    down_fills: Vec<Op>,
}

fn prepare(schema: &Schema, previous: &DbSchema, baseline: bool) -> Result<Prepared> {
    let levels = level_links(schema);
    let auto = auto_fields(schema);
    let schema = schema.physical();
    let (mut current, renames) = model::build_with(schema, baseline, &levels, &auto)?;
    reconcile::reconcile(schema, previous, &mut current);
    if let Some(old) = &previous.identities {
        let next = current.identities.as_ref().ok_or("identity manifest removed from schema; retain frozen identities and tombstones")?;
        old.validate_successor(next)?;
    }
    if previous.version != 0 && previous.dialect != current.dialect {
        return Err(format!("migration snapshot targets {}, schema targets {}; use a separate migrations directory", previous.dialect.name(), current.dialect.name()));
    }
    let identity_changed = previous.identities != current.identities;
    // Compare the stored representation: Table equality ignores nested JSON metadata.
    let unmanaged_state = |db: &DbSchema| serde_json::to_value(db.tables.iter().filter(|t| !t.managed).collect::<Vec<_>>());
    let unmanaged_changed = unmanaged_state(previous).map_err(|e| e.to_string())? != unmanaged_state(&current).map_err(|e| e.to_string())?;
    let shape_changed = shape_changed(previous, &current);
    let (fills, down_fills) = json_ops(previous, &current, &renames)?;
    diff::check_copies(previous, &current, &renames)?;
    diff::check_generated_to_plain(previous, &current, &renames)?;
    diff::check_copies(&current, previous, &renames.reversed())?;
    Ok(Prepared { current, renames, identity_changed, unmanaged_changed, shape_changed, fills, down_fills })
}

/// The data ops on json columns of the change (key renames, then fills) and of its reverse (key renames).
fn json_ops(previous: &DbSchema, next: &DbSchema, renames: &model::Renames) -> Result<(Vec<Op>, Vec<Op>)> {
    let mut up = diff::json_key_renames(previous, next, renames)?;
    up.extend(diff::json_backfills(previous, next, renames)?);
    Ok((up, diff::json_key_renames(next, previous, &renames.reversed())?))
}

/// The PostgreSQL ops of the change and of its reverse.
fn pg_ops(previous: &DbSchema, p: &Prepared) -> Result<(Vec<Op>, Vec<Op>)> {
    let mut up = diff::diff(previous, &p.current, &p.renames)?;
    up.extend(p.fills.iter().cloned());
    let mut down = diff::diff(&p.current, previous, &p.renames.reversed())?;
    down.extend(p.down_fills.iter().cloned());
    Ok((up, down))
}

/// The migration from `previous` (an earlier snapshot; empty for the first migration)
/// to `schema`.
pub fn plan(schema: &Schema, previous: &DbSchema) -> Result<MigrationPlan> {
    plan_with(schema, previous, false)
}

/// [`plan`]; with `baseline` the new snapshot is the baseline form (see [`baseline_snapshot`]).
pub fn plan_with(schema: &Schema, previous: &DbSchema, baseline: bool) -> Result<MigrationPlan> {
    let p = prepare(schema, previous, baseline)?;
    if p.current.dialect == crate::dialect::Dialect::Sqlite {
        return sqlite_plan(previous, p);
    }
    let (up, down) = pg_ops(previous, &p)?;
    Ok(pg_plan(previous, p, &up, &down))
}

fn sqlite_plan(previous: &DbSchema, p: Prepared) -> Result<MigrationPlan> {
    let mut up = sqlite::steps_with(previous, &p.current, &p.renames, &p.fills)?;
    let mut down = sqlite::steps_with(&p.current, previous, &p.renames.reversed(), &p.down_fills)?;
    if p.identity_changed { identity_step(&mut up); identity_step(&mut down); }
    snapshot_step(&p, &mut up, &mut down);
    Ok(MigrationPlan { up, down, snapshot: p.current })
}

fn pg_plan(previous: &DbSchema, p: Prepared, up: &[Op], down: &[Op]) -> MigrationPlan {
    let mut up = steps(up, &p.current);
    let mut down = steps(down, previous);
    if p.identity_changed { identity_step(&mut up); identity_step(&mut down); }
    snapshot_step(&p, &mut up, &mut down);
    MigrationPlan { up, down, snapshot: p.current }
}

/// The migration from `previous` to `next`, from the two snapshots alone: no rename hints, and none of the
/// checks of [`plan`]. `next` may be empty (the down plan of a squash). An empty snapshot takes the dialect of the other.
pub fn plan_between(previous: &DbSchema, next: &DbSchema, renames: &model::Renames) -> Result<MigrationPlan> {
    let dialect = if previous.version != 0 { previous.dialect } else { next.dialect };
    let identity_changed = previous.identities != next.identities;
    let (fills, down_fills) = json_ops(previous, next, renames)?;
    let (mut up, mut down) = if dialect == crate::dialect::Dialect::Sqlite {
        (sqlite::steps_with(previous, next, renames, &fills)?, sqlite::steps_with(next, previous, &renames.reversed(), &down_fills)?)
    } else {
        let mut up = diff::diff(previous, next, renames)?;
        up.extend(fills);
        let mut down = diff::diff(next, previous, &renames.reversed())?;
        down.extend(down_fills);
        (steps(&up, next), steps(&down, previous))
    };
    if identity_changed { identity_step(&mut up); identity_step(&mut down); }
    Ok(MigrationPlan { up, down, snapshot: next.clone() })
}

/// The squash of the migrations that end in `snapshot`: `up` builds it from an empty database (with its
/// extensions), `down` removes it.
pub fn plan_squash(snapshot: &DbSchema) -> Result<MigrationPlan> {
    let empty = DbSchema { dialect: snapshot.dialect, version: snapshot.version, ..Default::default() };
    let mut plan = plan_between(&empty, snapshot, &Default::default())?;
    if snapshot.dialect == crate::dialect::Dialect::Sqlite {
        let sql = sqlite::create_all(snapshot, false).join(";\n");
        plan.up = if sql.is_empty() { vec![] } else { vec![Step::new("Create the schema".into(), sql, None, vec![])] };
        if snapshot.identities.is_some() { identity_step(&mut plan.up); }
    }
    Ok(plan)
}

/// Shadow databases can add parentheses to equivalent native expressions.
/// Compare their checked structure so pull and drift do not report those as changes.
pub fn reconcile_introspection(previous: &DbSchema, current: &mut DbSchema) {
    let pulled = pull::pull(current, &[]);
    if let Ok(schema) = crate::dsl::compile_with_overrides(&pulled.schema, None, &pulled.views).and_then(Schema::from_ir) {
        reconcile::reconcile(&schema, previous, current);
    }
}

/// Whether a json column kept by both snapshots has another shape in `current`, for example a subfield
/// that became optional. The database is the same, but the snapshot must keep the new shape for the next
/// fill. A column whose old snapshot has no shape is not compared: that snapshot is older than the shape.
fn shape_changed(previous: &DbSchema, current: &DbSchema) -> bool {
    current.tables.iter().any(|t| {
        let Some(old) = previous.tables.iter().find(|o| o.name == t.name) else { return false };
        t.columns.iter().any(|c| {
            let Some(oc) = old.columns.iter().find(|o| o.name == c.name) else { return false };
            matches!((&oc.nested.0, &c.nested.0), (Some(a), Some(b)) if serde_json::to_value(a).ok() != serde_json::to_value(b).ok())
        })
    })
}

/// A comment step makes `make` write changes to state when there is no SQL.
fn snapshot_step(p: &Prepared, up: &mut Vec<Step>, down: &mut Vec<Step>) {
    if p.unmanaged_changed && up.is_empty() {
        for steps in [&mut *up, &mut *down] {
            steps.push(Step::new("Record unmanaged models".into(), "-- Unmanaged models are recorded in the migration state".into(), None, vec![]));
        }
    }
    if p.shape_changed && up.is_empty() {
        for steps in [up, down] {
            steps.push(Step::new("Record the shape of json columns".into(), "-- The shape of a json column is recorded in the migration state".into(), None, vec![]));
        }
    }
}

fn identity_step(steps: &mut Vec<Step>) {
    steps.push(Step::new("Record frozen ContentType identities".into(), "-- Frozen ContentType identity mapping recorded in the migration state".into(), None, vec![]));
}

/// The numbered operations of the migration `name` of `dir` with their SQL, for `sqlmigrate`.
pub fn render_migration(dir: &std::path::Path, name: &str, dialect: crate::dialect::Dialect, direction: render::Direction) -> std::result::Result<String, String> {
    let entry = files::find(dir, name)?;
    let file = files::read(&entry)?;
    let start = files::state_before(dir, &entry.name, dialect)?;
    let rendered = render::render(&entry.name, dir, &file, &start, direction, None)?;
    let title = format!("{} ({})", entry.name, if direction == render::Direction::Up { "up" } else { "down" });
    Ok(rendered.text(&title))
}

/// The operations of the migration from `previous` to `schema`. `online` (PostgreSQL) gives the
/// operations that lock a table for long in a form with `"atomic": false`. A last `separate`
/// operation with no `database` list carries the part of the new state that no schema operation reaches.
pub fn generate(schema: &Schema, previous: &DbSchema, baseline: bool, online: bool) -> Result<Vec<ops::Operation>> {
    let p = prepare(schema, previous, baseline)?;
    let mut list = diff::diff(previous, &p.current, &p.renames)?;
    list.extend(p.fills.iter().cloned());
    let mut operations: Vec<ops::Operation> = if online && p.current.dialect != crate::dialect::Dialect::Sqlite {
        online::online_operations(previous, &p.renames, list)
    } else {
        list.into_iter().map(ops::Operation::schema).collect()
    };
    let mut replayed = previous.clone();
    replayed.dialect = p.current.dialect;
    replay::replay(&mut replayed, &operations)?;
    operations.extend(replay::state_operation(&replayed, &p.current));
    Ok(operations)
}

/// The managed schema as idempotent DDL (`IF NOT EXISTS` / `OR REPLACE`), for
/// `create_tables()` in development and tests.
pub fn create_all(schema: &Schema) -> Result<Vec<String>> {
    let current = snapshot(schema)?;
    if current.dialect == crate::dialect::Dialect::Sqlite { return Ok(sqlite::create_all(&current, true)); }
    let ops = diff::diff(&DbSchema::default(), &current, &Default::default())?;
    Ok(ops.iter().map(|op| pg::render(op, true)).collect())
}

/// `REFRESH MATERIALIZED VIEW [CONCURRENTLY] <view>` for the materialized view model `model`.
/// `CONCURRENTLY` needs a unique index on the view; PostgreSQL checks that.
pub fn refresh_statement(schema: &Schema, model: &str, concurrently: bool) -> Result<String> {
    let m = schema.model(schema.model_idx(model)?);
    let view = m.ir.sql_view.as_ref().ok_or_else(|| format!("{model} is not a view; only a materialized view can be refreshed"))?;
    if schema.dialect == crate::dialect::Dialect::Sqlite {
        return Err(format!("{model}: SQLite has no materialized views to refresh"));
    }
    if !view.materialized {
        return Err(format!("{model} is a plain view; only a materialized view can be refreshed"));
    }
    Ok(format!("REFRESH MATERIALIZED VIEW {}{}", if concurrently { "CONCURRENTLY " } else { "" }, pg::ident(m.table())))
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

/// Drops the managed tables, enum types and generated functions of the schema (`IF EXISTS ... CASCADE`);
/// extensions stay.
pub fn drop_all(schema: &Schema) -> Result<Vec<String>> {
    let current = snapshot(schema)?.managed_schema();
    if current.dialect == crate::dialect::Dialect::Sqlite {
        let mut out: Vec<String> = current.views.iter().rev().map(|v| format!("DROP VIEW IF EXISTS {}", pg::ident(&v.name))).collect();
        out.extend(current.tables.iter().rev().map(|t| format!("DROP TABLE IF EXISTS {}", pg::ident(&t.name))));
        return Ok(out);
    }
    let mut out: Vec<String> = current.views.iter().rev()
        .map(|v| format!("DROP {}VIEW IF EXISTS {} CASCADE", if v.materialized { "MATERIALIZED " } else { "" }, pg::ident(&v.name)))
        .collect();
    out.extend(current
        .tables
        .iter()
        .rev()
        .map(|t| format!("DROP TABLE IF EXISTS {} CASCADE", pg::ident(&t.name))));
    out.extend(
        current.functions.iter().map(|f| format!("DROP FUNCTION IF EXISTS {}({}) CASCADE", pg::ident(&f.name), f.args)),
    );
    out.extend(current.enums.iter().map(|e| format!("DROP TYPE IF EXISTS {} CASCADE", e.sql())));
    Ok(out)
}

/// Empties every table of the schema, for test isolation: one `TRUNCATE ... RESTART IDENTITY CASCADE`
/// on PostgreSQL; `DELETE` and the `AUTOINCREMENT` reset on SQLite. Views stay.
pub fn truncate_all(schema: &Schema) -> Result<Vec<String>> {
    let current = snapshot(schema)?;
    if current.tables.is_empty() {
        return Ok(Vec::new());
    }
    if current.dialect == crate::dialect::Dialect::Sqlite {
        let mut out: Vec<String> = current.tables.iter().rev().map(|t| format!("DELETE FROM {}", pg::ident(&t.name))).collect();
        if current.tables.iter().any(|t| t.columns.iter().any(|c| c.identity)) {
            out.push("DELETE FROM sqlite_sequence".into());
        }
        return Ok(out);
    }
    let names: Vec<String> = current.tables.iter().map(|t| pg::ident(&t.name)).collect();
    Ok(vec![format!("TRUNCATE {} RESTART IDENTITY CASCADE", names.join(", "))])
}

#[cfg(test)]
mod tests;
