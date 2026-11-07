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
