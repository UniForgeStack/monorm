//! The `[admin]` settings of the project, read through the settings loader.
//!
//! The keys: `app` (`module:attr`, the admin object), `setup` (`module:function`, the hook that connects the database,
//! the extensions and the file storage), `host`, `port`, `timeout` (seconds), `db_settings` (a table of strings),
//! `idle_timeout` and `session_timeout` (seconds, for the login session), `origins` (extra origins a write may come from) and `trusted_proxies` (peers whose X-Forwarded-For the login limits believe).
//! The secret that signs sessions is the environment variable `MONORM_ADMIN_SECRET`, never a setting.
//! The environment variables `MONORM_ADMIN_HOST` and `MONORM_ADMIN_PORT` override `host` and `port`.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use monorm_core::settings::Settings;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::security::{Security, SecurityOptions};
use crate::serve::ServeConfig;

pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DEFAULT_PORT: u16 = 8000;
pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;
pub const DEFAULT_IDLE_SECS: u64 = 30 * 60;
pub const DEFAULT_SESSION_SECS: u64 = 12 * 60 * 60;

/// The settings in the form that the host reads: the host loads `app` and `setup`, and hands the rest back to the server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub app: String,
    pub setup: Option<String>,
    pub host: String,
    pub port: u16,
    pub timeout_ms: u64,
    pub db_settings: BTreeMap<String, String>,
    pub idle_timeout_secs: u64,
    pub session_timeout_secs: u64,
    pub origins: Vec<String>,
    #[serde(default)]
    pub trusted_proxies: Vec<String>,
}

const KEYS: [&str; 10] = ["app", "setup", "host", "port", "timeout", "db_settings", "idle_timeout", "session_timeout", "origins", "trusted_proxies"];

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
        let seconds = |key: &str, default: u64| match table.get(key) {
            None => Ok(default),
            Some(v) => match v.as_u64() {
                Some(n) if n > 0 && i64::try_from(n).is_ok() => Ok(n),
                _ => Err(format!("{place}`admin.{key}` must be a whole number of seconds above 0")),
            },
        };
        let origins = match table.get("origins") {
            None => Vec::new(),
            Some(Value::Array(list)) => list
                .iter()
                .map(|v| match v.as_str() {
                    Some(o) if is_origin(o) => Ok(o.trim_end_matches('/').to_string()),
                    _ => Err(format!("{place}`admin.origins` must hold origins such as \"https://admin.example.com\"")),
                })
                .collect::<Result<_, _>>()?,
            Some(_) => return Err(format!("{place}`admin.origins` must be a list of origins")),
        };
        let trusted_proxies = match table.get("trusted_proxies") {
            None => Vec::new(),
            Some(Value::Array(list)) => list
                .iter()
                .map(|v| match v.as_str().and_then(|a| a.parse::<IpAddr>().ok()) {
                    Some(ip) => Ok(ip.to_string()),
                    None => Err(format!("{place}`admin.trusted_proxies` must hold IP addresses such as \"10.0.0.1\"")),
                })
                .collect::<Result<_, _>>()?,
            Some(_) => return Err(format!("{place}`admin.trusted_proxies` must be a list of IP addresses")),
        };
        let idle_timeout_secs = seconds("idle_timeout", DEFAULT_IDLE_SECS)?;
        let session_timeout_secs = seconds("session_timeout", DEFAULT_SESSION_SECS)?;
        Ok(Config { app, setup: text("setup")?, host, port, timeout_ms, db_settings, idle_timeout_secs, session_timeout_secs, origins, trusted_proxies })
    }

    /// The session signing and limits. `secret` is the value of `MONORM_ADMIN_SECRET`.
    pub fn security(&self, secret: Option<&str>) -> Result<Arc<Security>, String> {
        let secs = |n: u64| i64::try_from(n).unwrap_or(i64::MAX);
        let trusted_proxies = self.trusted_proxies.iter().map(|a| a.parse::<IpAddr>().map_err(|_| format!("`admin.trusted_proxies` holds {a:?}, which is not an IP address"))).collect::<Result<_, _>>()?;
        let options = SecurityOptions { idle_secs: secs(self.idle_timeout_secs), absolute_secs: secs(self.session_timeout_secs), origins: self.origins.clone(), trusted_proxies };
        security_with(secret, options)
    }

    pub fn serve_config(&self) -> ServeConfig {
        ServeConfig {
            host: self.host.clone(),
            port: self.port,
            timeout: Duration::from_millis(self.timeout_ms),
            db_settings: self.db_settings.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            auth: None,
        }
    }
}

/// The signing and limits for `secret` and `options`; the one place that words the missing-secret error.
pub fn security_with(secret: Option<&str>, options: SecurityOptions) -> Result<Arc<Security>, String> {
    let Some(secret) = secret.filter(|s| !s.is_empty()) else {
        return Err("MONORM_ADMIN_SECRET is not set: it signs the login sessions, so set it to a random string of at least 32 characters (for example `openssl rand -hex 32`)".into());
    };
    Security::new(secret.as_bytes(), options)
}

/// The session options of a JSON object with the keys `idle_timeout`, `session_timeout` (seconds), `origins` and `trusted_proxies`;
/// a missing key takes its default. This is what a test client gives in place of the `[admin]` settings.
pub fn security_options(json: &str) -> Result<SecurityOptions, String> {
    let value: Value = serde_json::from_str(json).map_err(|e| format!("session options are not JSON: {e}"))?;
    let Value::Object(map) = value else {
        return Err("session options must be an object".into());
    };
    if let Some(key) = map.keys().find(|k| !["idle_timeout", "session_timeout", "origins", "trusted_proxies"].contains(&k.as_str())) {
        return Err(format!("`{key}` is not a session option; the keys are idle_timeout, session_timeout, origins, trusted_proxies"));
    }
    let seconds = |key: &str, default: u64| match map.get(key) {
        None => Ok(default as i64),
        Some(v) => match v.as_u64() {
            Some(n) if n > 0 => i64::try_from(n).map_err(|_| format!("`{key}` is too large")),
            _ => Err(format!("`{key}` must be a whole number of seconds above 0")),
        },
    };
    let origins = match map.get("origins") {
        None => Vec::new(),
        Some(Value::Array(list)) => list
            .iter()
            .map(|v| match v.as_str() {
                Some(o) if is_origin(o) => Ok(o.trim_end_matches('/').to_string()),
                _ => Err("`origins` must hold origins such as \"https://admin.example.com\"".to_string()),
            })
            .collect::<Result<_, _>>()?,
        Some(_) => return Err("`origins` must be a list of origins".into()),
    };
    let trusted_proxies = match map.get("trusted_proxies") {
        None => Vec::new(),
        Some(Value::Array(list)) => list
            .iter()
            .map(|v| v.as_str().and_then(|a| a.parse::<IpAddr>().ok()).ok_or_else(|| "`trusted_proxies` must hold IP addresses such as \"10.0.0.1\"".to_string()))
            .collect::<Result<_, _>>()?,
        Some(_) => return Err("`trusted_proxies` must be a list of IP addresses".into()),
    };
    Ok(SecurityOptions { idle_secs: seconds("idle_timeout", DEFAULT_IDLE_SECS)?, absolute_secs: seconds("session_timeout", DEFAULT_SESSION_SECS)?, origins, trusted_proxies })
}

fn is_origin(text: &str) -> bool {
    let rest = text.strip_prefix("https://").or_else(|| text.strip_prefix("http://"));
    rest.is_some_and(|r| !r.trim_end_matches('/').is_empty() && !r.trim_end_matches('/').contains(['/', '?', '#']))
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

    #[test]
    fn session_keys_default_and_are_read() {
        let d = dir("session-default", "monorm.toml", "[admin]\napp = \"a:b\"\n");
        let c = Config::load(&d, no_env).unwrap();
        assert_eq!((c.idle_timeout_secs, c.session_timeout_secs), (DEFAULT_IDLE_SECS, DEFAULT_SESSION_SECS));
        assert!(c.origins.is_empty());
        let text = "[admin]\napp = \"a:b\"\nidle_timeout = 60\nsession_timeout = 3600\norigins = [\"https://admin.example.com/\"]\n";
        let c = Config::load(&dir("session-all", "monorm.toml", text), no_env).unwrap();
        assert_eq!((c.idle_timeout_secs, c.session_timeout_secs), (60, 3600));
        assert_eq!(c.origins, ["https://admin.example.com"]);
    }

    #[test]
    fn a_wrong_session_key_names_the_key() {
        for (text, needle) in [
            ("[admin]\napp = \"a:b\"\nidle_timeout = 0\n", "`admin.idle_timeout`"),
            ("[admin]\napp = \"a:b\"\nsession_timeout = \"x\"\n", "`admin.session_timeout`"),
            ("[admin]\napp = \"a:b\"\norigins = [\"admin.example.com\"]\n", "`admin.origins`"),
            ("[admin]\napp = \"a:b\"\norigins = \"https://a.b\"\n", "`admin.origins`"),
        ] {
            let e = Config::load(&dir("session-wrong", "monorm.toml", text), no_env).unwrap_err();
            assert!(e.contains(needle), "{e}");
        }
    }

    #[test]
    fn the_secret_must_have_32_bytes() {
        let d = dir("secret", "monorm.toml", "[admin]\napp = \"a:b\"\n");
        let c = Config::load(&d, no_env).unwrap();
        assert!(c.security(None).unwrap_err().contains("MONORM_ADMIN_SECRET"));
        assert!(c.security(Some("short")).unwrap_err().contains("at least 32 bytes"));
        assert!(c.security(Some(&"x".repeat(32))).is_ok());
    }

    #[test]
    fn session_options_take_defaults_and_name_a_wrong_key_or_value() {
        let o = security_options("{}").unwrap();
        assert_eq!((o.idle_secs, o.absolute_secs, o.origins.len()), (DEFAULT_IDLE_SECS as i64, DEFAULT_SESSION_SECS as i64, 0));
        let o = security_options(r#"{"idle_timeout": 5, "session_timeout": 9, "origins": ["https://a.example/"]}"#).unwrap();
        assert_eq!((o.idle_secs, o.absolute_secs, o.origins), (5, 9, vec!["https://a.example".to_string()]));
        for (text, needle) in [
            ("[]", "must be an object"),
            (r#"{"idle": 1}"#, "`idle` is not a session option"),
            (r#"{"idle_timeout": 0}"#, "`idle_timeout` must be a whole number"),
            (r#"{"session_timeout": 1.5}"#, "`session_timeout` must be a whole number"),
            (r#"{"origins": "https://a.example"}"#, "must be a list of origins"),
            (r#"{"origins": ["a.example"]}"#, "must hold origins"),
            (r#"{"origins": ["https://a.example/path"]}"#, "must hold origins"),
        ] {
            let e = security_options(text).unwrap_err();
            assert!(e.contains(needle), "{text}: {e}");
        }
    }

    #[test]
    fn a_missing_or_short_secret_names_the_variable() {
        for secret in [None, Some(""), Some("short")] {
            let e = security_with(secret, SecurityOptions::default()).err().unwrap();
            assert!(e.contains("MONORM_ADMIN_SECRET") || e.contains("32 bytes"), "{e}");
        }
        assert!(security_with(Some(&"s".repeat(32)), SecurityOptions::default()).is_ok());
    }
}
