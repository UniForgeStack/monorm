//! Best-effort provenance collected before any timed workload.
use orm_engine::db::Driver;
use serde_json::{json, Value};
use std::process::Command;

fn command(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (!value.is_empty()).then_some(value)
}

pub async fn collect(db: &dyn Driver) -> Value {
    let cpu = if std::env::consts::OS == "macos" {
        command("sysctl", &["-n", "machdep.cpu.brand_string"])
    } else {
        std::fs::read_to_string("/proc/cpuinfo")
            .ok()
            .and_then(|info| {
                info.lines().find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    let value = value.trim();
                    (matches!(key.trim(), "model name" | "Hardware") && !value.is_empty())
                        .then(|| value.to_owned())
                })
            })
    };
    let postgres_version = db
        .query_text("SELECT version()".into())
        .await
        .ok()
        .and_then(|rows| rows.into_iter().next())
        .and_then(|row| row.into_iter().next().flatten())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "unknown".into());
    json!({
        "commit": command("git", &["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into()),
        "rustc": command("rustc", &["-V"]).unwrap_or_else(|| "unknown".into()),
        "postgres_version": postgres_version,
        "os": format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        "cpu": cpu.unwrap_or_else(|| "unknown".into()),
    })
}
