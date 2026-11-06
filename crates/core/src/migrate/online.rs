//! The online form of a PostgreSQL migration: the steps that lock a table with rows for a long time
//! move to folders of their own, so no folder holds such a lock longer than one short step.
//!
//! - A new non-unique index on an existing table is built by `CREATE INDEX CONCURRENTLY` in a
//!   no-transaction folder after the main folder.
//! - A dropped index of an existing table goes by `DROP INDEX CONCURRENTLY` in a no-transaction
//!   folder before the main folder.
//! - A new foreign key or check on an existing table is added `NOT VALID` in the main folder and
//!   validated in a following folder.
//!
//! Everything else stays in the main folder: a new table, a unique index, and a partitioned table
//! (PostgreSQL has no `CONCURRENTLY` or `NOT VALID` form for it).

use std::collections::HashSet;

use super::diff::{Constraint, Op};
use super::model::{DbSchema, Index, Table};
use super::{pg, MigrationPlan, Prepared, Step};
use crate::dialect::Dialect;
use crate::schema::{Result, Schema};

/// What the folder of one plan holds, which sets its name and its file header.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum FolderKind {
    /// The steps that run in one transaction.
    #[default]
    Main,
    /// One `DROP INDEX CONCURRENTLY`; the index name as SQL.
    DropIndex(String),
    /// One `CREATE INDEX CONCURRENTLY`; the index name as SQL.
    CreateIndex(String),
    /// The `VALIDATE CONSTRAINT` steps of the constraints the main folder added `NOT VALID`.
    Validate,
}

/// The ops of `up` that leave the main folder.
#[derive(Default)]
struct Split {
    /// Positions in `up` of the moved `CreateIndex` and `DropIndex` ops.
    moved: HashSet<usize>,
    /// The existing indexes to drop, with the table of each in the previous snapshot.
    drops: Vec<(String, Index)>,
    creates: Vec<(String, Index)>,
    /// (table, constraint) of each constraint added `NOT VALID`.
    validates: Vec<(String, String)>,
}

impl Split {
    fn is_empty(&self) -> bool {
        self.drops.is_empty() && self.creates.is_empty() && self.validates.is_empty()
    }
}

fn split(previous: &DbSchema, p: &Prepared, up: &[Op]) -> Split {
    let created: HashSet<&str> = up.iter().filter_map(|op| match op { Op::CreateTable { table, .. } => Some(table.name.as_str()), _ => None }).collect();
    let dropped: HashSet<&str> = up.iter().filter_map(|op| match op { Op::DropTable { table } => Some(table.as_str()), _ => None }).collect();
    // The table of `name` (a name after the change) as it is in the previous snapshot, if it
    // has rows to lock: it exists there, the change does not create it again, and it is not partitioned.
    let existing = |name: &str| -> Option<&Table> {
        if created.contains(name) { return None; }
        let old = p.renames.tables.get(name).map_or(name, String::as_str);
        let table = previous.tables.iter().find(|t| t.name == old)?;
        table.partition_by.is_none().then_some(table)
    };
    let mut s = Split::default();
    for (i, op) in up.iter().enumerate() {
        match op {
            Op::CreateIndex { table, index } if !index.unique => {
                if existing(table).is_some() {
                    s.moved.insert(i);
                    s.creates.push((table.clone(), index.clone()));
                }
            }
            Op::DropIndex { name } => {
                let found = previous.tables.iter().find_map(|t| t.indexes.iter().find(|ix| ix.name == *name).map(|ix| (t, ix)));
                if let Some((t, ix)) = found {
                    if !ix.unique && t.partition_by.is_none() && !dropped.contains(t.name.as_str()) {
                        s.moved.insert(i);
                        s.drops.push((t.name.clone(), ix.clone()));
                    }
                }
            }
            Op::AddConstraint { table, constraint: c @ (Constraint::Check(_) | Constraint::ForeignKey(_)) } if existing(table).is_some() => {
                s.validates.push((table.clone(), c.name().to_owned()));
            }
            _ => {}
        }
    }
    s
}

/// `schema` without the indexes named in `names`.
fn without(schema: &DbSchema, names: &HashSet<&str>) -> DbSchema {
    let mut schema = schema.clone();
    for t in &mut schema.tables {
        t.indexes.retain(|ix| !names.contains(ix.name.as_str()));
    }
    schema
}

const CONCURRENTLY: pg::Online = pg::Online { concurrently: true, not_valid: false };

fn folder(kind: FolderKind, up: Vec<Step>, down: Vec<Step>, snapshot: DbSchema) -> MigrationPlan {
    MigrationPlan { up, down, snapshot, kind }
}

/// The migration from `previous` to `schema` as an ordered list of folders: the no-transaction
/// folders that drop indexes, the main folder, the no-transaction folders that build indexes, and
/// the folder that validates the constraints. The last folder carries the snapshot of `schema`.
/// Other than on PostgreSQL, or when nothing moves, this is the one plan of [`super::plan`].
pub fn plan_online(schema: &Schema, previous: &DbSchema) -> Result<Vec<MigrationPlan>> {
    let p = super::prepare(schema, previous)?;
    if p.current.dialect == Dialect::Sqlite {
        return Ok(vec![super::sqlite_plan(previous, p)?]);
    }
    let (up, down) = super::pg_ops(previous, &p)?;
    let s = split(previous, &p, &up);
    if s.is_empty() {
        return Ok(vec![super::pg_plan(previous, p, &up, &down)]);
    }
    let drop_names: HashSet<&str> = s.drops.iter().map(|(_, ix)| ix.name.as_str()).collect();
    let create_names: HashSet<&str> = s.creates.iter().map(|(_, ix)| ix.name.as_str()).collect();
    let validate: HashSet<(&str, &str)> = s.validates.iter().map(|(t, n)| (t.as_str(), n.as_str())).collect();

    let mut plans = Vec::new();
    // drop folders: the index is gone after each of them
    let mut gone: HashSet<&str> = HashSet::new();
    for (table, index) in &s.drops {
        gone.insert(&index.name);
        let up = super::steps_with(&[Op::DropIndex { name: index.name.clone() }], previous, |_| CONCURRENTLY);
        let down = super::steps_with(&[Op::CreateIndex { table: table.clone(), index: index.clone() }], previous, |_| CONCURRENTLY);
        plans.push(folder(FolderKind::DropIndex(pg::ident(&index.name)), up, down, without(previous, &gone)));
    }

    // the main folder: what stays, and the down of what moved is left to the folders of the moves
    let main_up: Vec<Op> = up.iter().enumerate().filter(|(i, _)| !s.moved.contains(i)).map(|(_, op)| op.clone()).collect();
    let main_down: Vec<Op> = down
        .iter()
        .filter(|op| match op {
            Op::DropIndex { name } => !create_names.contains(name.as_str()),
            Op::CreateIndex { index, .. } => !drop_names.contains(index.name.as_str()),
            _ => true,
        })
        .cloned()
        .collect();
    let mut main_up = super::steps_with(&main_up, &p.current, |op| match op {
        Op::AddConstraint { table, constraint } if validate.contains(&(table.as_str(), constraint.name())) => pg::Online { concurrently: false, not_valid: true },
        _ => pg::Online::default(),
    });
    let mut main_down = super::steps(&main_down, previous);
    if p.identity_changed {
        super::identity_step(&mut main_up);
        super::identity_step(&mut main_down);
    }
    if !main_up.is_empty() || !main_down.is_empty() {
        plans.push(folder(FolderKind::Main, main_up, main_down, without(&p.current, &create_names)));
    }

    // create folders: the index is there after each of them
    let mut later = create_names.clone();
    for (_, index) in &s.creates {
        later.remove(index.name.as_str());
        let table = s.creates.iter().find(|(_, ix)| ix.name == index.name).map(|(t, _)| t.clone()).unwrap_or_default();
        let up = super::steps_with(&[Op::CreateIndex { table, index: index.clone() }], &p.current, |_| CONCURRENTLY);
        let down = super::steps_with(&[Op::DropIndex { name: index.name.clone() }], previous, |_| CONCURRENTLY);
        plans.push(folder(FolderKind::CreateIndex(pg::ident(&index.name)), up, down, without(&p.current, &later)));
    }

    if !s.validates.is_empty() {
        let up = s
            .validates
            .iter()
            .map(|(table, name)| {
                Step::new(
                    format!("Validate constraint {name} on {table}"),
                    format!("ALTER TABLE {} VALIDATE CONSTRAINT {}", pg::ident(table), pg::ident(name)),
                    None,
                    vec![],
                )
            })
            .collect();
        plans.push(folder(FolderKind::Validate, up, vec![], p.current));
    }
    Ok(plans)
}
