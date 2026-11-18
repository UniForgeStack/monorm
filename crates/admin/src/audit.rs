//! One row of the admin log (`AdminLogEntry`) for an event of the admin itself: a login, a failed login, a password change.

use monorm_engine::db::Executor;
use sea_query::Value;
use serde_json::Value as Json;

use crate::site::Site;
use crate::sql::{mark, Table};

pub struct Entry<'a> {
    pub user_key: &'a str,
    pub action: &'a str,
    pub resource: &'a str,
    pub record_key: &'a str,
    pub record_label: &'a str,
    pub changes: Option<Json>,
}

/// Inserts the row on `conn` (the request transaction, so the row commits with the request).
/// The `at` column keeps its database default.
pub async fn record(site: &Site, conn: &dyn Executor, entry: Entry<'_>) -> Result<(), String> {
    let table = Table::of(site, "AdminLogEntry")?;
    let names = ["user_key", "action", "resource", "record_key", "record_label", "changes"];
    let mut columns = Vec::new();
    for name in names {
        columns.push(table.column(name)?);
    }
    let marks: Vec<String> = (1..=names.len()).map(|i| mark(conn.dialect(), i)).collect();
    let sql = format!("INSERT INTO {} ({}) VALUES ({})", table.quoted(), columns.join(", "), marks.join(", "));
    let args = vec![
        Value::from(entry.user_key.to_string()),
        Value::from(entry.action.to_string()),
        Value::from(entry.resource.to_string()),
        Value::from(entry.record_key.to_string()),
        Value::from(entry.record_label.to_string()),
        Value::from(entry.changes),
    ];
    conn.execute(sql, args).await.map(|_| ()).map_err(|e| e.message)
}

/// Sets the label of the rows of one record that a host wrote with no label (`ctx.audit()`). Rows with a label stay.
pub async fn fill_label(site: &Site, conn: &dyn Executor, resource: &str, record_key: &str, label: &str) -> Result<(), String> {
    let table = Table::of(site, "AdminLogEntry")?;
    let (set, by_resource, by_key, blank) = (table.column("record_label")?, table.column("resource")?, table.column("record_key")?, table.column("record_label")?);
    let d = conn.dialect();
    let sql = format!("UPDATE {} SET {set} = {} WHERE {by_resource} = {} AND {by_key} = {} AND {blank} = ''", table.quoted(), mark(d, 1), mark(d, 2), mark(d, 3));
    let args = vec![Value::from(label.to_string()), Value::from(resource.to_string()), Value::from(record_key.to_string())];
    conn.execute(sql, args).await.map(|_| ()).map_err(|e| e.message)
}

/// Completes the row that the default host writer wrote in the write transaction, found by its id: the key text, the label and the full `changes`
/// (the key of a create and the diff are known only after the write and the read of the stored record).
pub async fn complete(site: &Site, conn: &dyn Executor, id: i64, record_key: &str, label: &str, changes: Json) -> Result<(), String> {
    let table = Table::of(site, "AdminLogEntry")?;
    let (key_col, label_col, changes_col, id_col) = (table.column("record_key")?, table.column("record_label")?, table.column("changes")?, table.column("id")?);
    let d = conn.dialect();
    let sql = format!("UPDATE {} SET {key_col} = {}, {label_col} = {}, {changes_col} = {} WHERE {id_col} = {}", table.quoted(), mark(d, 1), mark(d, 2), mark(d, 3), mark(d, 4));
    let args = vec![Value::from(record_key.to_string()), Value::from(label.to_string()), Value::from(Some(changes)), Value::from(id)];
    match conn.execute(sql, args).await.map_err(|e| e.message)? {
        0 => Err(format!("the host wrote no audit row {id}")),
        _ => Ok(()),
    }
}
