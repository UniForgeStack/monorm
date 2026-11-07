//! The `[admin]` settings of the project, read through the settings loader.
//!
//! The keys: `app` (`module:attr`, the admin object), `setup` (`module:function`, the hook that connects the database,
//! the extensions and the file storage), `host`, `port`, `timeout` (seconds) and `db_settings` (a table of strings).
//! The environment variables `MONORM_ADMIN_HOST` and `MONORM_ADMIN_PORT` override `host` and `port`.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use monorm_core::settings::Settings;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::serve::ServeConfig;

pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DEFAULT_PORT: u16 = 8000;
pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;

/// The settings in the form that the host reads: the host loads `app` and `setup`, and hands the rest back to the server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub app: String,
    pub setup: Option<String>,
    pub host: String,
    pub port: u16,
    pub timeout_ms: u64,
    pub db_settings: BTreeMap<String, String>,
}

const KEYS: [&str; 6] = ["app", "setup", "host", "port", "timeout", "db_settings"];

impl Config {
    /// Reads the settings of the project in `dir`. `env` reads one environment variable.
    pub fn load(dir: &Path, env: impl Fn(&str) -> Option<String>) -> Result<Config, String> {
        let settings = Settings::load(dir)?;
        Config::from_settings(&settings, env)
    }

    pub fn from_settings(settings: &Settings, env: impl Fn(&str) -> Option<String>) -> Result<Config, String> {
        let place = match settings.source() {
            Some(source) => format!("{}: ", source.file()),
            None => String::new(),
        };
        let Some(section) = settings.section("admin") else {
            return Err(format!("{place}no admin settings: run `admin init`, or add an `[admin]` section with `app = \"module:attr\"`"));
        };
        let Value::Object(table) = section else {
            return Err(format!("{place}`admin` must be a table"));
        };
        if let Some(key) = table.keys().find(|k| !KEYS.contains(&k.as_str())) {
            return Err(format!("{place}`admin.{key}` is not a setting; the keys are {}", KEYS.join(", ")));
        }
        let text = |key: &str| match table.get(key) {
            None => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(format!("{place}`admin.{key}` must be a string")),
        };
        let app = text("app")?.ok_or_else(|| format!("{place}`admin.app` is not set: it names the admin object, as `module:attr`"))?;
        for (key, value) in [("app", Some(&app)), ("setup", text("setup")?.as_ref())] {
            if let Some(value) = value {
                if !value.contains(':') || value.starts_with(':') || value.ends_with(':') {
                    return Err(format!("{place}`admin.{key}` must be `module:name`, not {value:?}"));
                }
            }
        }
        let host = match env("MONORM_ADMIN_HOST").filter(|v| !v.is_empty()) {
            Some(host) => host,
            None => text("host")?.unwrap_or_else(|| DEFAULT_HOST.into()),
        };
        let port = match env("MONORM_ADMIN_PORT").filter(|v| !v.is_empty()) {
            Some(port) => port.parse().map_err(|_| format!("MONORM_ADMIN_PORT must be a port number, not {port:?}"))?,
            None => match table.get("port") {
                None => DEFAULT_PORT,
                Some(v) => v.as_u64().and_then(|p| u16::try_from(p).ok()).ok_or_else(|| format!("{place}`admin.port` must be a port number"))?,
            },
        };
        let timeout_ms = match table.get("timeout") {
            None => DEFAULT_TIMEOUT_MS,
            Some(v) => match v.as_f64() {
                Some(seconds) if seconds > 0.0 && seconds.is_finite() => (seconds * 1000.0).round() as u64,
                _ => return Err(format!("{place}`admin.timeout` must be a number of seconds above 0")),
            },
        };
        let db_settings = match table.get("db_settings") {
            None => BTreeMap::new(),
            Some(Value::Object(map)) => map
                .iter()
                .map(|(k, v)| match v {
                    Value::String(s) => Ok((k.clone(), s.clone())),
                    _ => Err(format!("{place}`admin.db_settings.{k}` must be a string")),
                })
                .collect::<Result<_, _>>()?,
            Some(_) => return Err(format!("{place}`admin.db_settings` must be a table of strings")),
        };
        Ok(Config { app, setup: text("setup")?, host, port, timeout_ms, db_settings })
    }

    pub fn serve_config(&self) -> ServeConfig {
        ServeConfig {
            host: self.host.clone(),
            port: self.port,
            timeout: Duration::from_millis(self.timeout_ms),
            db_settings: self.db_settings.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str, file: &str, text: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("monorm-admin-config-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(file), text).unwrap();
        dir
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn defaults_fill_what_the_file_leaves_out() {
        let d = dir("defaults", "monorm.toml", "[admin]\napp = \"admin:admin\"\n");
        let c = Config::load(&d, no_env).unwrap();
        assert_eq!((c.app.as_str(), c.setup, c.host.as_str(), c.port, c.timeout_ms), ("admin:admin", None, "127.0.0.1", 8000, 30_000));
        assert!(c.db_settings.is_empty());
    }

    #[test]
    fn every_key_is_read_and_the_environment_wins_for_host_and_port() {
        let text = "[admin]\napp = \"a:b\"\nsetup = \"s:go\"\nhost = \"0.0.0.0\"\nport = 9000\ntimeout = 2.5\n[admin.db_settings]\n\"app.tenant\" = \"t1\"\n";
        let d = dir("all", "monorm.toml", text);
        let c = Config::load(&d, no_env).unwrap();
        assert_eq!((c.setup.as_deref(), c.host.as_str(), c.port, c.timeout_ms), (Some("s:go"), "0.0.0.0", 9000, 2500));
        assert_eq!(c.db_settings["app.tenant"], "t1");
        let env = |k: &str| match k {
            "MONORM_ADMIN_HOST" => Some("10.0.0.1".to_string()),
            "MONORM_ADMIN_PORT" => Some("0".to_string()),
            _ => None,
        };
        let c = Config::load(&d, env).unwrap();
        assert_eq!((c.host.as_str(), c.port), ("10.0.0.1", 0));
    }

    #[test]
    fn pyproject_and_package_json_hold_the_same_section() {
        let d = dir("pyproject", "pyproject.toml", "[tool.monorm.admin]\napp = \"a:b\"\n");
        assert_eq!(Config::load(&d, no_env).unwrap().app, "a:b");
        let d = dir("package", "package.json", "{\"monorm\": {\"admin\": {\"app\": \"./admin.ts:admin\", \"port\": 1}}}");
        let c = Config::load(&d, no_env).unwrap();
        assert_eq!((c.app.as_str(), c.port), ("./admin.ts:admin", 1));
    }

    #[test]
    fn a_wrong_setting_names_the_file_and_the_key() {
        for (text, needle) in [
            ("[admin]\nport = 70000\napp = \"a:b\"\n", "`admin.port` must be a port number"),
            ("[admin]\napp = \"a:b\"\ntimeout = 0\n", "`admin.timeout` must be a number of seconds above 0"),
            ("[admin]\napp = \"a:b\"\nthread = 2\n", "`admin.thread` is not a setting"),
            ("[admin]\napp = \"nocolon\"\n", "`admin.app` must be `module:name`"),
            ("[admin]\nhost = \"x\"\n", "`admin.app` is not set"),
            ("[admin]\napp = \"a:b\"\n[admin.db_settings]\nk = 1\n", "`admin.db_settings.k` must be a string"),
        ] {
            let d = dir("wrong", "monorm.toml", text);
            let e = Config::load(&d, no_env).unwrap_err();
            assert!(e.starts_with("monorm.toml: ") && e.contains(needle), "{e}");
        }
    }

    #[test]
    fn no_admin_section_says_how_to_add_one() {
        let d = dir("none", "monorm.toml", "schema = \"schema.prisma\"\n");
        assert!(Config::load(&d, no_env).unwrap_err().contains("run `admin init`"));
        let e = Config::load(&d, |k| (k == "MONORM_ADMIN_PORT").then(|| "x".to_string())).unwrap_err();
        assert!(e.contains("no admin settings"), "{e}");
    }

    #[test]
    fn a_bad_port_variable_is_an_error() {
        let d = dir("badenv", "monorm.toml", "[admin]\napp = \"a:b\"\n");
        let e = Config::load(&d, |k| (k == "MONORM_ADMIN_PORT").then(|| "x".to_string())).unwrap_err();
        assert!(e.contains("MONORM_ADMIN_PORT"), "{e}");
    }
}
