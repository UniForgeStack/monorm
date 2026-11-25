//! One row of the admin log (`AdminLogEntry`) for an event of the admin itself: a login, a failed login, a password change.

use monorm_engine::db::{Cell, Executor};
use monorm_core::schema::Schema;
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
    insert(&site.schema, conn, entry).await.map(|_| ())
}

/// Inserts the row like `record` and returns its id.
pub async fn insert(schema: &Schema, conn: &dyn Executor, entry: Entry<'_>) -> Result<i64, String> {
    let table = Table::in_schema(schema, "AdminLogEntry")?;
    let names = ["user_key", "action", "resource", "record_key", "record_label", "changes"];
    let mut columns = Vec::new();
    for name in names {
        columns.push(table.column(name)?);
    }
    let marks: Vec<String> = (1..=names.len()).map(|i| mark(conn.dialect(), i)).collect();
    let sql = format!("INSERT INTO {} ({}) VALUES ({}) RETURNING {}", table.quoted(), columns.join(", "), marks.join(", "), table.column("id")?);
    let args = vec![
        Value::from(entry.user_key.to_string()),
        Value::from(entry.action.to_string()),
        Value::from(entry.resource.to_string()),
        Value::from(entry.record_key.to_string()),
        Value::from(entry.record_label.to_string()),
        Value::from(entry.changes),
    ];
    let rows = conn.fetch(sql, args).await.map_err(|e| e.message)?;
    if rows.len() != 1 {
        return Err("the insert of the admin log row returned no id".to_string());
    }
    match rows.cell(0, 0).map_err(|e| e.message)? {
        Cell::Int(id) => Ok(i64::from(id)),
        Cell::BigInt(id) => Ok(id),
        _ => Err("the id of the admin log row is not an integer".to_string()),
    }
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

/// Writes the key text of the rows that host code wrote with `ctx.audit()` (`audit_keys` of the answer: `[{id, key}]`, `key` the key values as text).
/// The binding writes no key text: only `key::encode` builds one. A row with `key: null` keeps a blank key text; a bad entry is an error, not a blank.
pub async fn set_keys(site: &Site, conn: &dyn Executor, answer: &Json) -> Result<(), String> {
    let Some(rows) = answer.get("audit_keys").and_then(Json::as_array) else { return Ok(()) };
    let table = Table::of(site, "AdminLogEntry")?;
    let (key_col, id_col) = (table.column("record_key")?, table.column("id")?);
    let d = conn.dialect();
    let sql = format!("UPDATE {} SET {key_col} = {} WHERE {id_col} = {}", table.quoted(), mark(d, 1), mark(d, 2));
    for row in rows {
        let Some(id) = row["id"].as_i64() else { return Err(format!("an audit key entry has no integer id: {row}")) };
        let parts = match &row["key"] {
            Json::Null => continue,
            Json::Array(parts) => parts,
            other => return Err(format!("the audit key of row {id} is {other}: expected an array or null")),
        };
        let key = crate::key::encode(parts);
        conn.execute(sql.clone(), vec![Value::from(key), Value::from(id)]).await.map_err(|e| e.message)?;
    }
    Ok(())
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
