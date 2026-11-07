//! Ended sessions, kept in the `AdminSessionRevocation` table so that every process of the service sees them.
//! The guard looks a session up here when it renews the cookie, so another process ends the session within `renew_after`.

use monorm_engine::db::Executor;
use sea_query::Value;

use crate::site::Site;
use crate::sql::{mark, Table};

/// Records that session `sid` is ended until `until` (the end of its absolute lifetime), and drops the rows that have lapsed.
pub async fn write(site: &Site, conn: &dyn Executor, sid: &str, until: i64, now: i64) -> Result<(), String> {
    let t = Table::of(site, "AdminSessionRevocation")?;
    let d = conn.dialect();
    let insert = format!("INSERT INTO {} ({}, {}) VALUES ({}, {})", t.quoted(), t.column("sid")?, t.column("until")?, mark(d, 1), mark(d, 2));
    conn.execute(insert, vec![Value::from(sid.to_string()), Value::from(until)]).await.map_err(|e| e.message)?;
    let prune = format!("DELETE FROM {} WHERE {} < {}", t.quoted(), t.column("until")?, mark(d, 1));
    conn.execute(prune, vec![Value::from(now)]).await.map(|_| ()).map_err(|e| e.message)
}

/// True when a row ends session `sid`.
pub async fn is_revoked(site: &Site, conn: &dyn Executor, sid: &str) -> Result<bool, String> {
    if sid.is_empty() || !sid.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return Err("the session id is not a token".to_string());
    }
    let t = Table::of(site, "AdminSessionRevocation")?;
    let sql = format!("SELECT COUNT(*) FROM {} WHERE {} = '{sid}'", t.quoted(), t.column("sid")?);
    let rows = conn.query_text(sql).await.map_err(|e| e.message)?;
    Ok(rows.first().and_then(|r| r.first()).and_then(|v| v.as_deref()).and_then(|v| v.parse::<i64>().ok()).unwrap_or(0) > 0)
}
