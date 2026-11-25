//! The online form of a PostgreSQL migration: the operations that lock a table with rows for a long
//! time get `"atomic": false` and run alone, so no transaction holds such a lock longer than one
//! short step.
//!
//! - A new non-unique index on an existing table is built by `CREATE INDEX CONCURRENTLY`, after the
//!   main operations.
//! - A dropped index of an existing table goes by `DROP INDEX CONCURRENTLY`, before the main
//!   operations.
//!
//! Everything else stays in the main group: a new table, a unique index, a constraint, and an index
//! of a partitioned table. PostgreSQL refuses `CREATE INDEX CONCURRENTLY` on a partitioned table.
//! A user who wants a `NOT VALID` constraint writes the `run_sql` pair by hand.

use std::collections::HashSet;

use super::diff::Op;
use super::model::{DbSchema, Renames, Table};
use super::ops::Operation;

/// `ops` (the diff from `previous`) in online order: the drops, the main operations and the builds.
pub fn online_operations(previous: &DbSchema, renames: &Renames, ops: Vec<Op>) -> Vec<Operation> {
    let created: HashSet<String> = ops.iter().filter_map(|op| match op { Op::CreateTable { table, .. } => Some(table.name.clone()), _ => None }).collect();
    let dropped: HashSet<String> = ops.iter().filter_map(|op| match op { Op::DropTable { table } => Some(table.clone()), _ => None }).collect();
    // The table of `name` (a name after the change) as it is before: it exists there and the change
    // does not create it again.
    let existing = |name: &str| -> Option<&Table> {
        if created.contains(name) { return None; }
        let old = renames.tables.get(name).map_or(name, String::as_str);
        previous.tables.iter().find(|t| t.name == old)
    };
    let plain = |name: &str| existing(name).filter(|t| t.partition_by.is_none());
    let (mut drops, mut main, mut builds) = (vec![], vec![], vec![]);
    for op in ops {
        match &op {
            Op::CreateIndex { table, index } if !index.unique && plain(table).is_some() => builds.push(Operation { atomic: false, ..Operation::schema(op) }),
            Op::DropIndex { name } => {
                let found = previous.tables.iter().find_map(|t| t.indexes.iter().find(|ix| ix.name == *name).map(|ix| (t, ix)));
                match found {
                    Some((t, ix)) if !ix.unique && t.partition_by.is_none() && !dropped.contains(&t.name) => drops.push(Operation { atomic: false, ..Operation::schema(op) }),
                    _ => main.push(Operation::schema(op)),
                }
            }
            _ => main.push(Operation::schema(op)),
        }
    }
    let mut out = drops;
    out.extend(main);
    out.extend(builds);
    out
}
