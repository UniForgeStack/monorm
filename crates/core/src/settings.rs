//! The project settings file: one lookup for every `monorm` command and for the apps built on it.
//!
//! The first file that holds settings wins; the files are never merged:
//!
//! 1. `monorm.toml`: the whole file is the settings.
//! 2. `pyproject.toml`: the `[tool.monorm]` table.
//! 3. `package.json`: the `"monorm"` object.
//!
//! A `pyproject.toml` without `[tool.monorm]` and a `package.json` without `"monorm"` hold no settings,
//! so the lookup goes on. The settings are a table of keys. The `monorm` command reads `schema`, `database`, `migrations`,
//! `query_sets` and `model_methods` (`querySets` and `modelMethods` in `package.json`; the other spelling is an error, see [`Settings::reject_spelling`]);
//! every other key (for example `[admin]`) belongs to its owner, which reads it with [`Settings::section`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::dialect::Dialect;

/// The file that the settings came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// `monorm.toml`: the whole file is the settings.
    MonormToml,
    /// `pyproject.toml`: the `[tool.monorm]` table.
    Pyproject,
    /// `package.json`: the `"monorm"` object.
    PackageJson,
}

impl Source {
    /// The file name, relative to the project directory.
    pub fn file(self) -> &'static str {
        match self {
            Source::MonormToml => "monorm.toml",
            Source::Pyproject => "pyproject.toml",
            Source::PackageJson => "package.json",
        }
    }

    /// Where the settings table sits in the file, as the error text names a key.
    fn prefix(self) -> &'static str {
        match self {
            Source::MonormToml => "",
            Source::Pyproject => "tool.monorm.",
            Source::PackageJson => "monorm.",
        }
    }
}

/// The settings of one project.
#[derive(Debug, Clone, Default)]
pub struct Settings {
    found: Option<(Source, serde_json::Map<String, Value>)>,
    /// The folder of the settings file: a relative path in `[apps]` resolves against it.
    dir: std::path::PathBuf,
}

impl Settings {
    /// Reads the settings of the project in `dir`. No settings file is not an error: the result is empty.
    /// A file that cannot be read or parsed, or a settings table of the wrong type, is an error that names the file.
    pub fn load(dir: &Path) -> Result<Settings, String> {
        for source in [Source::MonormToml, Source::Pyproject, Source::PackageJson] {
            let path = dir.join(source.file());
            let text = match std::fs::read_to_string(&path) {
                Ok(text) => text,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(format!("{}: {e}", source.file())),
            };
            if let Some(root) = parse(source, &text)? {
                return Ok(Settings { found: Some((source, root)), dir: dir.to_path_buf() });
            }
        }
        Ok(Settings { dir: dir.to_path_buf(), ..Default::default() })
    }

    /// The file of the settings, or `None` when the project has none.
    pub fn source(&self) -> Option<Source> {
        self.found.as_ref().map(|(source, _)| *source)
    }

    /// The value of the key `key`, as JSON: a section for its owner to read, or a plain value.
    pub fn section(&self, key: &str) -> Option<&Value> {
        self.found.as_ref()?.1.get(key)
    }

    /// `key` as a string, or `None` when it is not set.
    pub fn string(&self, key: &str) -> Result<Option<String>, String> {
        match self.section(key) {
            None => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(self.wrong(key, "must be a string")),
        }
    }

    /// The `migrations` setting: a directory (`migrations = "db"`) or a table (`[migrations]`) with the keys
    /// `dir` (the directory), `online` (a boolean, default `false`: PostgreSQL migrations in the online form) and
    /// `name` (the template of new file names, default `{number}_{slug}`).
    pub fn migrations(&self) -> Result<MigrationsSettings, String> {
        match self.section("migrations") {
            None => Ok(MigrationsSettings::default()),
            Some(Value::String(dir)) => Ok(MigrationsSettings { dir: Some(dir.clone()), ..Default::default() }),
            Some(Value::Object(table)) => {
                let mut out = MigrationsSettings::default();
                for (name, value) in table {
                    match (name.as_str(), value) {
                        ("dir", Value::String(s)) => out.dir = Some(s.clone()),
                        ("dir", _) => return Err(self.wrong("migrations.dir", "must be a string")),
                        ("online", Value::Bool(b)) => out.online = *b,
                        ("online", _) => return Err(self.wrong("migrations.online", "must be true or false")),
                        ("name", Value::String(s)) => out.name = Some(s.clone()),
                        ("name", _) => return Err(self.wrong("migrations.name", "must be a string")),
                        _ => return Err(self.wrong(&format!("migrations.{name}"), "is not a setting; the keys are `dir`, `online` and `name`")),
                    }
                }
                Ok(out)
            }
            Some(_) => Err(self.wrong("migrations", "must be a directory or a table")),
        }
    }

    /// The `[database]` table, or `None` when the project has none (the defaults of today: PostgreSQL, no extensions).
    /// The keys and their checks are the keys of the old `datasource` block; `url_env` is new.
    /// An extension is a name or a table `{ name, map, schema, version }`, and `scope_bypass` a table `{ setting, value }`.
    pub fn database(&self) -> Result<Option<DatabaseConfig>, String> {
        let Some(section) = self.section("database") else { return Ok(None) };
        let Value::Object(table) = section else { return Err(self.wrong("database", "must be a table")) };
        const KEYS: [&str; 9] = ["provider", "url_env", "extensions", "bypass_roles", "exempt_roles", "scope_prefix", "scope_bypass", "scope_role_check", "hard_delete_setting"];
        let mut unknown: Vec<&String> = table.keys().filter(|k| !KEYS.contains(&k.as_str())).collect();
        unknown.sort();
        if let Some(name) = unknown.first() {
            return Err(self.wrong(&format!("database.{name}"), &format!("is not a setting; the keys are {}", KEYS.join(", "))));
        }
        let mut out = DatabaseConfig { file: self.source().map(|s| s.file().to_owned()).unwrap_or_default(), ..Default::default() };
        out.dialect = match table.get("provider") {
            Some(Value::String(p)) if p == "postgresql" || p == "postgres" => Dialect::Postgres,
            Some(Value::String(p)) if p == "sqlite" => Dialect::Sqlite,
            Some(_) => return Err(self.wrong("database.provider", r#"must be "postgresql" or "sqlite""#)),
            None => return Err(self.wrong("database.provider", r#"is missing; write provider = "postgresql""#)),
        };
        out.url_env = self.non_empty(table, "url_env", r#"is the name of an environment variable, e.g. "DATABASE_URL""#)?;
        out.bypass_roles = self.roles(table, "bypass_roles", "app_admin")?;
        out.exempt_roles = self.roles(table, "exempt_roles", "reporting")?;
        out.scope_prefix = self.non_empty(table, "scope_prefix", r#"is a non-empty string, e.g. "tenant""#)?;
        out.hard_delete_setting = self.non_empty(table, "hard_delete_setting", r#"is a non-empty string, e.g. "rules.hard_deletion""#)?;
        match table.get("scope_bypass") {
            None => {}
            Some(Value::Object(fields)) => {
                const FORM: &str = r#"is { setting = "tenant.bypass", value = "on" }"#;
                let (mut setting, mut value) = (None, None);
                for (key, v) in fields {
                    let slot = match key.as_str() {
                        "setting" => &mut setting,
                        "value" => &mut value,
                        other => return Err(self.wrong(&format!("database.scope_bypass.{other}"), "is not a key; scope_bypass takes `setting` and `value`")),
                    };
                    match v {
                        Value::String(text) if !text.is_empty() => *slot = Some(text.clone()),
                        _ => return Err(self.wrong(&format!("database.scope_bypass.{key}"), "must be a non-empty string")),
                    }
                }
                out.scope_bypass = match (setting, value) {
                    (Some(setting), Some(value)) => Some((setting, value)),
                    _ => return Err(self.wrong("database.scope_bypass", FORM)),
                };
            }
            Some(_) => return Err(self.wrong("database.scope_bypass", r#"is { setting = "tenant.bypass", value = "on" }"#)),
        }
        match table.get("scope_role_check") {
            None => {}
            Some(Value::Bool(b)) => out.scope_role_check = *b,
            Some(_) => return Err(self.wrong("database.scope_role_check", "must be true or false")),
        }
        match table.get("extensions") {
            None => {}
            Some(Value::Array(list)) => {
                for item in list {
                    let call = self.extension(item)?;
                    if out.extensions.iter().any(|x| x.db_name() == call.db_name()) {
                        return Err(self.wrong("database.extensions", &format!("lists extension {} twice", call.db_name())));
                    }
                    out.extensions.push(call);
                }
            }
            Some(_) => return Err(self.wrong("database.extensions", r#"takes a list, e.g. ["postgis", { name = "citext", schema = "ext" }]"#)),
        }
        Ok(Some(out))
    }

    /// The `[apps]` table: the apps of the project, sorted by label (the key).
    /// An entry is `label = "path/entry.prisma"`, `label = "app:<package>"` or `label = { path, prefix, managed, migrations }`
    /// (`path` takes a file or `app:<package>`). A relative path resolves against the folder of the settings file.
    /// Empty when the project has no `[apps]`. A `schema` key next to `[apps]` is an error: `[apps]` replaces it.
    pub fn apps(&self) -> Result<Vec<AppEntry>, String> {
        let Some(section) = self.section("apps") else { return Ok(vec![]) };
        let Value::Object(table) = section else { return Err(self.wrong("apps", "must be a table of apps, e.g. orders = \"orders.prisma\"")) };
        if self.section("schema").is_some() {
            return Err(self.wrong("schema", "is not a setting when `[apps]` is set; `[apps]` replaces it: list the schema file as an app, e.g. main = \"schema.prisma\""));
        }
        let mut out = vec![];
        for (label, value) in table {
            check_label(label).map_err(|m| self.wrong(&format!("apps.{label}"), &m))?;
            let key = |name: &str| format!("apps.{label}.{name}");
            let mut entry = AppEntry { label: label.clone(), source: AppSource::Path(PathBuf::new()), prefix: None, managed: None, migrations: None };
            let path = match value {
                Value::String(path) => path,
                Value::Object(fields) => {
                    let mut path = None;
                    for (name, v) in fields {
                        match (name.as_str(), v) {
                            ("path", Value::String(p)) => path = Some(p),
                            ("path", _) => return Err(self.wrong(&key("path"), "must be a string")),
                            ("prefix", Value::String(p)) => entry.prefix = Some(p.clone()),
                            ("prefix", _) => return Err(self.wrong(&key("prefix"), "must be a string")),
                            ("managed", Value::Bool(b)) => entry.managed = Some(*b),
                            ("managed", _) => return Err(self.wrong(&key("managed"), "must be true or false")),
                            ("migrations", Value::String(p)) => entry.migrations = Some(self.dir.join(p)),
                            ("migrations", _) => return Err(self.wrong(&key("migrations"), "must be a folder")),
                            _ => return Err(self.wrong(&key(name), "is not a key of an app; the keys are `path`, `prefix`, `managed` and `migrations`")),
                        }
                    }
                    path.ok_or_else(|| self.wrong(&format!("apps.{label}"), "has no `path`"))?
                }
                _ => return Err(self.wrong(&format!("apps.{label}"), "is a path, `app:<package>` or a table with a `path`")),
            };
            if path.is_empty() {
                return Err(self.wrong(&format!("apps.{label}"), "has an empty path"));
            }
            entry.source = match path.strip_prefix("app:") {
                Some("") => return Err(self.wrong(&format!("apps.{label}"), "names no package after `app:`")),
                Some(package) => AppSource::Installed(package.to_owned()),
                None => AppSource::Path(self.dir.join(path)),
            };
            if let (AppSource::Installed(_), Some(_)) = (&entry.source, &entry.prefix) {
                return Err(self.wrong(&key("prefix"), &format!("is not allowed on an installed app; its tables take the prefix `{label}_`")));
            }
            out.push(entry);
        }
        Ok(out)
    }

    /// The name of the environment variable that holds the database URL: `[database] url_env`, else `MONORM_DATABASE_URL`.
    /// The other keys of `[database]` are not checked here, so a command that only needs the URL does not stop on them.
    pub fn url_env(&self) -> Result<String, String> {
        match self.section("database") {
            Some(Value::Object(table)) => match self.non_empty(table, "url_env", r#"is the name of an environment variable, e.g. "DATABASE_URL""#)? {
                Some(name) => Ok(name),
                None => Ok(DEFAULT_URL_ENV.to_owned()),
            },
            _ => Ok(DEFAULT_URL_ENV.to_owned()),
        }
    }

    fn non_empty(&self, table: &serde_json::Map<String, Value>, key: &str, what: &str) -> Result<Option<String>, String> {
        match table.get(key) {
            None => Ok(None),
            Some(Value::String(s)) if !s.is_empty() => Ok(Some(s.clone())),
            Some(_) => Err(self.wrong(&format!("database.{key}"), what)),
        }
    }

    fn roles(&self, table: &serde_json::Map<String, Value>, key: &str, example: &str) -> Result<Vec<String>, String> {
        let name = format!("database.{key}");
        match table.get(key) {
            None => Ok(vec![]),
            Some(Value::Array(list)) => {
                let mut roles: Vec<String> = vec![];
                for role in list {
                    match role {
                        Value::String(role) if !role.is_empty() => if !roles.contains(role) { roles.push(role.clone()) },
                        _ => return Err(self.wrong(&name, &format!("holds a non-empty string for each role, e.g. \"{example}\""))),
                    }
                }
                Ok(roles)
            }
            Some(_) => Err(self.wrong(&name, &format!("takes a list of role names, e.g. [\"{example}\"]"))),
        }
    }

    fn extension(&self, item: &Value) -> Result<ExtensionCall, String> {
        const FORM: &str = r#"is a name or a table, e.g. "postgis" or { name = "postgis", schema = "ext", version = "3.4" }"#;
        match item {
            Value::String(name) if !name.is_empty() => Ok(ExtensionCall { name: name.clone(), ..Default::default() }),
            Value::Object(fields) => {
                let mut call = ExtensionCall::default();
                for (key, v) in fields {
                    let slot = match key.as_str() {
                        "name" => &mut call.name,
                        "map" | "schema" | "version" => {
                            let Value::String(text) = v else { return Err(self.wrong(&format!("database.extensions.{key}"), "must be a string")) };
                            match key.as_str() {
                                "map" => call.map = Some(text.clone()),
                                "schema" => call.schema = Some(text.clone()),
                                _ => call.version = Some(text.clone()),
                            }
                            continue;
                        }
                        other => return Err(self.wrong(&format!("database.extensions.{other}"), "is not a key; an extension takes `name`, `map`, `schema` and `version`")),
                    };
                    match v {
                        Value::String(text) if !text.is_empty() => *slot = text.clone(),
                        _ => return Err(self.wrong("database.extensions.name", "must be a non-empty string")),
                    }
                }
                if call.name.is_empty() {
                    return Err(self.wrong("database.extensions", "holds a table without a `name`"));
                }
                Ok(call)
            }
            _ => Err(self.wrong("database.extensions", FORM)),
        }
    }

    /// `key` as a table of strings (for example `query_sets`: model to target), empty when it is not set.
    pub fn string_table(&self, key: &str) -> Result<BTreeMap<String, String>, String> {
        match self.section(key) {
            None => Ok(BTreeMap::new()),
            Some(Value::Object(table)) => table
                .iter()
                .map(|(name, value)| match value {
                    Value::String(s) => Ok((name.clone(), s.clone())),
                    _ => Err(self.wrong(&format!("{key}.{name}"), "must be a string")),
                })
                .collect(),
            Some(_) => Err(self.wrong(key, "must be a table of strings")),
        }
    }

    /// Fails when `other` is set: it is the spelling of another file format, and this file spells the key `right`.
    pub fn reject_spelling(&self, other: &str, right: &str) -> Result<(), String> {
        match self.section(other) {
            None => Ok(()),
            Some(_) => Err(self.wrong(other, &format!("is not a setting; use `{right}`"))),
        }
    }

    /// The error text for a wrong setting: the file, the key as the file spells it, and what is wrong.
    pub fn wrong(&self, key: &str, what: &str) -> String {
        match &self.found {
            Some((source, _)) => format!("{}: `{}{key}` {what}", source.file(), source.prefix()),
            None => format!("`{key}` {what}"),
        }
    }
}

/// An app label: a Python identifier, and not `project` (the name of the project history).
pub fn check_label(label: &str) -> Result<(), String> {
    let mut chars = label.chars();
    let first = chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    if !first || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(format!("{label:?} is not a label; a label is a Python identifier, such as `orders`"));
    }
    if label == "project" {
        return Err("`project` is reserved for the project history; choose another label".into());
    }
    Ok(())
}

/// Where the schema of an app comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppSource {
    /// A `.prisma` entry file of the project.
    Path(PathBuf),
    /// An installed package (`app:<package>`); the host finds its entry file.
    Installed(String),
}

/// One entry of `[apps]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppEntry {
    /// The key: the unique label of the app.
    pub label: String,
    pub source: AppSource,
    pub prefix: Option<String>,
    pub managed: Option<bool>,
    /// A history folder, resolved against the folder of the settings file.
    pub migrations: Option<PathBuf>,
}

/// The file that stands for the project where one schema path is needed (the identity file, the output folder of `generate`):
/// the entry file of the first project app by label.
pub fn anchor(apps: &[AppEntry]) -> Option<&Path> {
    apps.iter().find_map(|a| match &a.source { AppSource::Path(p) => Some(p.as_path()), AppSource::Installed(_) => None })
}

/// The environment variable that holds the database URL when `[database]` does not name one.
pub const DEFAULT_URL_ENV: &str = "MONORM_DATABASE_URL";

/// The `[database]` table of the settings file, as one typed input for the compiler. It carries the keys of the old
/// `datasource` block (the compiler fills the same IR fields from it) and `url_env`, which only the commands read.
#[derive(Debug, Clone, Default)]
pub struct DatabaseConfig {
    /// The settings file that the table came from, for error texts.
    pub file: String,
    pub dialect: Dialect,
    pub url_env: Option<String>,
    pub extensions: Vec<ExtensionCall>,
    pub bypass_roles: Vec<String>,
    pub exempt_roles: Vec<String>,
    pub scope_prefix: Option<String>,
    /// The setting and the value that mark a scope bypass.
    pub scope_bypass: Option<(String, String)>,
    pub scope_role_check: bool,
    pub hard_delete_setting: Option<String>,
}

impl DatabaseConfig {
    /// The `[database]` table for the keys that `pull` finds in a live database: provider, bypass roles and extensions.
    pub fn to_toml(&self) -> String {
        let quote = |s: &str| serde_json::to_string(s).expect("a string prints");
        let mut out = format!("[database]\nprovider = {}\n", quote(if self.dialect == Dialect::Sqlite { "sqlite" } else { "postgresql" }));
        if !self.bypass_roles.is_empty() {
            out.push_str(&format!("bypass_roles = [{}]\n", self.bypass_roles.iter().map(|r| quote(r)).collect::<Vec<_>>().join(", ")));
        }
        if !self.extensions.is_empty() {
            let list: Vec<String> = self.extensions.iter().map(|e| {
                let mut keys = vec![format!("name = {}", quote(&e.name))];
                for (key, value) in [("map", &e.map), ("schema", &e.schema), ("version", &e.version)] {
                    if let Some(v) = value { keys.push(format!("{key} = {}", quote(v))); }
                }
                if keys.len() == 1 { quote(&e.name) } else { format!("{{ {} }}", keys.join(", ")) }
            }).collect();
            out.push_str(&format!("extensions = [{}]\n", list.join(", ")));
        }
        out
    }
}

/// One database extension of `[database] extensions`: the keys of the old call form `postgis(map: "x", schema: "ext", version: "3.4")`.
#[derive(Debug, Clone, Default)]
pub struct ExtensionCall {
    /// The name of the extension definition (for the types that it provides).
    pub name: String,
    /// The name in the database, when it differs from `name`.
    pub map: Option<String>,
    pub schema: Option<String>,
    pub version: Option<String>,
}

impl ExtensionCall {
    /// The name that the database knows.
    pub fn db_name(&self) -> &str {
        self.map.as_deref().unwrap_or(&self.name)
    }
}

/// The `migrations` setting, see [`Settings::migrations`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct MigrationsSettings {
    pub dir: Option<String>,
    pub online: bool,
    pub name: Option<String>,
}

/// The settings table of `text`, or `None` when the file holds none (only `pyproject.toml` and `package.json` can).
fn parse(source: Source, text: &str) -> Result<Option<serde_json::Map<String, Value>>, String> {
    let file = source.file();
    match source {
        Source::MonormToml => {
            let table: toml::Table = text.parse().map_err(|e| format!("{file}: {e}"))?;
            Ok(Some(to_json(file, table)?))
        }
        Source::Pyproject => {
            let doc: toml::Table = text.parse().map_err(|e| format!("{file}: {e}"))?;
            match doc.get("tool").and_then(|t| t.get("monorm")) {
                None => Ok(None),
                Some(toml::Value::Table(table)) => Ok(Some(to_json(file, table.clone())?)),
                Some(_) => Err(format!("{file}: `tool.monorm` must be a table")),
            }
        }
        Source::PackageJson => {
            let doc: Value = serde_json::from_str(text).map_err(|e| format!("{file}: {e}"))?;
            match doc.get("monorm") {
                None => Ok(None),
                Some(Value::Object(object)) => Ok(Some(object.clone())),
                Some(_) => Err(format!("{file}: `monorm` must be an object")),
            }
        }
    }
}

fn to_json(file: &str, table: toml::Table) -> Result<serde_json::Map<String, Value>, String> {
    match serde_json::to_value(table).map_err(|e| format!("{file}: {e}"))? {
        Value::Object(map) => Ok(map),
        _ => unreachable!("a TOML table is a JSON object"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(files: &[(&str, &str)]) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!("monorm-settings-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, text) in files {
            std::fs::write(dir.join(name), text).unwrap();
        }
        dir
    }

    fn load(files: &[(&str, &str)]) -> Result<Settings, String> {
        let dir = project(files);
        let settings = Settings::load(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        settings
    }

    const PYPROJECT: &str = "[tool.monorm]\nschema = \"py.prisma\"\n";
    const PACKAGE: &str = "{\"monorm\": {\"schema\": \"js.prisma\"}}";

    #[test]
    fn no_file_gives_empty_settings() {
        let settings = load(&[]).unwrap();
        assert_eq!(settings.source(), None);
        assert_eq!(settings.string("schema").unwrap(), None);
        assert!(settings.string_table("query_sets").unwrap().is_empty());
    }

    #[test]
    fn the_first_file_wins_in_the_documented_order_and_files_do_not_merge() {
        let both = [("package.json", PACKAGE), ("pyproject.toml", PYPROJECT)];
        let settings = load(&both).unwrap();
        assert_eq!((settings.source(), settings.string("schema").unwrap().as_deref()), (Some(Source::Pyproject), Some("py.prisma")));

        let all = [("package.json", PACKAGE), ("pyproject.toml", PYPROJECT), ("monorm.toml", "migrations = \"db\"\n")];
        let settings = load(&all).unwrap();
        assert_eq!(settings.source(), Some(Source::MonormToml));
        assert_eq!(settings.string("migrations").unwrap().as_deref(), Some("db"));
        assert_eq!(settings.string("schema").unwrap(), None, "the schema of pyproject.toml must not leak in");

        assert_eq!(load(&[("package.json", PACKAGE)]).unwrap().string("schema").unwrap().as_deref(), Some("js.prisma"));
    }

    #[test]
    fn migrations_is_a_directory_or_a_table() {
        let migrations = |file: &str, text: &str| load(&[(file, text)]).and_then(|s| s.migrations());
        assert_eq!(load(&[]).unwrap().migrations().unwrap(), MigrationsSettings::default());
        assert_eq!(migrations("monorm.toml", "migrations = \"db\"\n").unwrap(), MigrationsSettings { dir: Some("db".into()), ..Default::default() });
        assert_eq!(migrations("monorm.toml", "[migrations]\nonline = true\n").unwrap(), MigrationsSettings { online: true, ..Default::default() });
        assert_eq!(migrations("pyproject.toml", "[tool.monorm.migrations]\ndir = \"db\"\nonline = true\n").unwrap(), MigrationsSettings { dir: Some("db".into()), online: true, name: None });
        assert_eq!(migrations("monorm.toml", "[migrations]\nname = \"{timestamp}_{slug}\"\n").unwrap().name.as_deref(), Some("{timestamp}_{slug}"));
        assert_eq!(migrations("package.json", "{\"monorm\": {\"migrations\": {\"online\": true}}}").unwrap().online, true);
        assert_eq!(migrations("pyproject.toml", "[tool.monorm.migrations]\nname = \"{timestamp}-{slug}\"\n").unwrap().name.as_deref(), Some("{timestamp}-{slug}"));
        assert_eq!(migrations("package.json", "{\"monorm\": {\"migrations\": {\"name\": \"{number}_{slug}\"}}}").unwrap().name.as_deref(), Some("{number}_{slug}"));
        assert!(migrations("package.json", "{\"monorm\": {\"migrations\": {\"name\": 3}}}").unwrap_err().contains("migrations.name"));
        assert!(migrations("monorm.toml", "[migrations]\nonline = \"yes\"\n").unwrap_err().contains("migrations.online"));
        assert!(migrations("monorm.toml", "[migrations]\nonlien = true\n").unwrap_err().contains("migrations.onlien"));
        assert!(migrations("monorm.toml", "migrations = 3\n").unwrap_err().contains("directory or a table"));
    }

    #[test]
    fn a_file_without_monorm_settings_is_skipped() {
        let files = [("pyproject.toml", "[project]\nname = \"x\"\n"), ("package.json", PACKAGE)];
        assert_eq!(load(&files).unwrap().source(), Some(Source::PackageJson));
        let files = [("package.json", "{\"name\": \"x\"}")];
        assert_eq!(load(&files).unwrap().source(), None);
    }

    #[test]
    fn an_empty_monorm_toml_still_wins() {
        let files = [("monorm.toml", ""), ("pyproject.toml", PYPROJECT)];
        let settings = load(&files).unwrap();
        assert_eq!((settings.source(), settings.string("schema").unwrap()), (Some(Source::MonormToml), None));
    }

    #[test]
    fn other_sections_go_to_their_owner_in_every_file() {
        let toml = load(&[("monorm.toml", "[admin]\napp = \"shop.admin:admin\"\n")]).unwrap();
        assert_eq!(toml.section("admin").unwrap()["app"], "shop.admin:admin");
        let py = load(&[("pyproject.toml", "[tool.monorm.admin]\napp = \"shop.admin:admin\"\n")]).unwrap();
        assert_eq!(py.section("admin").unwrap()["app"], "shop.admin:admin");
        let js = load(&[("package.json", "{\"monorm\": {\"admin\": {\"app\": \"shop.admin:admin\"}}}")]).unwrap();
        assert_eq!(js.section("admin").unwrap()["app"], "shop.admin:admin");
    }

    #[test]
    fn query_set_tables_read_in_every_file() {
        let toml = load(&[("monorm.toml", "[query_sets]\nPost = \"blog.queries:PostQueries\"\n")]).unwrap();
        assert_eq!(toml.string_table("query_sets").unwrap()["Post"], "blog.queries:PostQueries");
        let js = load(&[("package.json", "{\"monorm\": {\"querySets\": {\"Post\": \"./q.js#Post\"}}}")]).unwrap();
        assert_eq!(js.string_table("querySets").unwrap()["Post"], "./q.js#Post");
    }

    #[test]
    fn an_error_names_the_file_and_the_key() {
        let err = load(&[("monorm.toml", "schema = 3\n")]).unwrap().string("schema").unwrap_err();
        assert_eq!(err, "monorm.toml: `schema` must be a string");
        let err = load(&[("pyproject.toml", "[tool.monorm]\nschema = 3\n")]).unwrap().string("schema").unwrap_err();
        assert_eq!(err, "pyproject.toml: `tool.monorm.schema` must be a string");
        let err = load(&[("package.json", "{\"monorm\": {\"schema\": []}}")]).unwrap().string("schema").unwrap_err();
        assert_eq!(err, "package.json: `monorm.schema` must be a string");
        let err = load(&[("monorm.toml", "[query_sets]\nPost = 1\n")]).unwrap().string_table("query_sets").unwrap_err();
        assert_eq!(err, "monorm.toml: `query_sets.Post` must be a string");
        let err = load(&[("monorm.toml", "query_sets = \"x\"\n")]).unwrap().string_table("query_sets").unwrap_err();
        assert_eq!(err, "monorm.toml: `query_sets` must be a table of strings");
    }

    #[test]
    fn the_spelling_of_another_file_format_is_an_error_that_names_the_file_and_the_key() {
        let toml = load(&[("monorm.toml", "[querySets]\nBook = \"a:B\"\n")]).unwrap();
        assert_eq!(
            toml.reject_spelling("querySets", "query_sets").unwrap_err(),
            "monorm.toml: `querySets` is not a setting; use `query_sets`"
        );
        let py = load(&[(
            "pyproject.toml",
            "[tool.monorm.modelMethods]\nBook = \"a:B\"\n",
        )])
        .unwrap();
        assert_eq!(
            py.reject_spelling("modelMethods", "model_methods")
                .unwrap_err(),
            "pyproject.toml: `tool.monorm.modelMethods` is not a setting; use `model_methods`"
        );
        let js = load(&[("package.json", "{\"monorm\": {\"query_sets\": {}}}")]).unwrap();
        assert_eq!(
            js.reject_spelling("query_sets", "querySets").unwrap_err(),
            "package.json: `monorm.query_sets` is not a setting; use `querySets`"
        );

        assert!(
            js.reject_spelling("model_methods", "modelMethods").is_ok(),
            "an unset key is fine"
        );
        assert!(
            Settings::default()
                .reject_spelling("querySets", "query_sets")
                .is_ok(),
            "no settings file is fine"
        );
    }

    #[test]
    fn a_broken_file_is_an_error_that_names_it() {
        assert!(load(&[("monorm.toml", "schema = \n")]).unwrap_err().starts_with("monorm.toml: "));
        assert!(load(&[("pyproject.toml", "[tool\n")]).unwrap_err().starts_with("pyproject.toml: "));
        assert!(load(&[("package.json", "{")]).unwrap_err().starts_with("package.json: "));
        assert_eq!(load(&[("pyproject.toml", "[tool]\nmonorm = 1\n")]).unwrap_err(), "pyproject.toml: `tool.monorm` must be a table");
        assert_eq!(load(&[("package.json", "{\"monorm\": 1}")]).unwrap_err(), "package.json: `monorm` must be an object");
    }

    #[test]
    fn a_broken_file_after_the_winner_is_not_read() {
        assert!(load(&[("monorm.toml", ""), ("package.json", "{")]).is_ok());
    }

    const DATABASE: &str = r#"
[database]
provider = "postgresql"
url_env = "DATABASE_URL"
extensions = ["citext", { name = "postgis", schema = "ext", version = "3.4" }]
bypass_roles = ["app_admin"]
exempt_roles = ["reporting"]
scope_prefix = "tenant"
scope_bypass = { setting = "tenant.bypass", value = "on" }
scope_role_check = true
hard_delete_setting = "rules.hard_deletion"
"#;

    fn database(text: &str) -> Result<Option<DatabaseConfig>, String> {
        load(&[("monorm.toml", text)])?.database()
    }

    #[test]
    fn the_database_table_reads_every_key() {
        let db = database(DATABASE).unwrap().unwrap();
        assert_eq!((db.dialect, db.url_env.as_deref(), db.file.as_str()), (Dialect::Postgres, Some("DATABASE_URL"), "monorm.toml"));
        assert_eq!(db.extensions.iter().map(|e| (e.name.as_str(), e.schema.as_deref(), e.version.as_deref())).collect::<Vec<_>>(), [("citext", None, None), ("postgis", Some("ext"), Some("3.4"))]);
        assert_eq!((db.bypass_roles, db.exempt_roles), (vec!["app_admin".to_owned()], vec!["reporting".to_owned()]));
        assert_eq!((db.scope_prefix.as_deref(), db.scope_role_check, db.hard_delete_setting.as_deref()), (Some("tenant"), true, Some("rules.hard_deletion")));
        assert_eq!(db.scope_bypass, Some(("tenant.bypass".to_owned(), "on".to_owned())));
    }

    #[test]
    fn the_database_table_has_one_spelling_in_each_file() {
        let py = load(&[("pyproject.toml", "[tool.monorm.database]\nprovider = \"sqlite\"\n")]).unwrap().database().unwrap().unwrap();
        assert_eq!((py.dialect, py.file.as_str()), (Dialect::Sqlite, "pyproject.toml"));
        let js = load(&[("package.json", r#"{"monorm": {"database": {"provider": "postgres", "url_env": "DB"}}}"#)]).unwrap();
        assert_eq!((js.database().unwrap().unwrap().dialect, js.url_env().unwrap().as_str()), (Dialect::Postgres, "DB"));
    }

    #[test]
    fn no_database_table_means_the_defaults_and_the_default_url_variable() {
        let settings = load(&[("monorm.toml", "migrations = \"db\"\n")]).unwrap();
        assert!(settings.database().unwrap().is_none());
        assert_eq!(settings.url_env().unwrap(), "MONORM_DATABASE_URL");
        assert_eq!(load(&[]).unwrap().url_env().unwrap(), "MONORM_DATABASE_URL");
        let no_url = load(&[("monorm.toml", "[database]\nprovider = \"sqlite\"\n")]).unwrap();
        assert_eq!(no_url.url_env().unwrap(), "MONORM_DATABASE_URL");
    }

    #[test]
    fn url_env_does_not_check_the_other_keys() {
        let settings = load(&[("monorm.toml", "[database]\nurl_env = \"DB\"\nbogus = 1\n")]).unwrap();
        assert_eq!(settings.url_env().unwrap(), "DB");
        assert!(settings.database().is_err());
    }

    #[test]
    fn a_wrong_database_table_names_the_file_and_the_key() {
        let cases = [
            ("database = 3\n", "monorm.toml: `database` must be a table"),
            ("[database]\n", "`database.provider` is missing"),
            ("[database]\nprovider = \"mysql\"\n", "`database.provider` must be"),
            ("[database]\nprovider = \"sqlite\"\nurl = \"x\"\n", "`database.url` is not a setting"),
            ("[database]\nprovider = \"sqlite\"\nurl_env = \"\"\n", "`database.url_env`"),
            ("[database]\nprovider = \"sqlite\"\nbypass_roles = \"a\"\n", "`database.bypass_roles` takes a list"),
            ("[database]\nprovider = \"sqlite\"\nexempt_roles = [1]\n", "`database.exempt_roles` holds"),
            ("[database]\nprovider = \"sqlite\"\nscope_bypass = { setting = \"a\" }\n", "`database.scope_bypass` is { setting"),
            ("[database]\nprovider = \"sqlite\"\nscope_bypass = { setting = \"a\", value = \"b\", more = \"c\" }\n", "`database.scope_bypass.more` is not a key"),
            ("[database]\nprovider = \"sqlite\"\nscope_role_check = \"yes\"\n", "`database.scope_role_check` must be true or false"),
            ("[database]\nprovider = \"sqlite\"\nhard_delete_setting = 1\n", "`database.hard_delete_setting`"),
            ("[database]\nprovider = \"sqlite\"\nextensions = \"citext\"\n", "`database.extensions` takes a list"),
            ("[database]\nprovider = \"sqlite\"\nextensions = [{ schema = \"x\" }]\n", "holds a table without a `name`"),
            ("[database]\nprovider = \"sqlite\"\nextensions = [{ name = \"a\", color = \"x\" }]\n", "`database.extensions.color` is not a key"),
            ("[database]\nprovider = \"postgresql\"\nextensions = [\"a\", { name = \"b\", map = \"a\" }]\n", "lists extension a twice"),
        ];
        for (text, expect) in cases {
            let error = database(text).unwrap_err();
            assert!(error.starts_with("monorm.toml: ") && error.contains(expect), "{text}: {error}");
        }
    }

    fn apps(text: &str) -> Result<Vec<AppEntry>, String> {
        load(&[("monorm.toml", text)]).unwrap().apps()
    }

    #[test]
    fn the_apps_table_reads_the_three_entry_forms() {
        let entries = apps("[apps]\nmain = \"schema.prisma\"\nauth = \"app:monorm-auth\"\norders = { path = \"orders/orders.prisma\", prefix = \"ord_\", managed = false, migrations = \"history/shop\" }\n").unwrap();
        let labels: Vec<_> = entries.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(labels, ["auth", "main", "orders"]);
        assert_eq!(entries[0].source, AppSource::Installed("monorm-auth".to_owned()));
        assert!(matches!(&entries[1].source, AppSource::Path(p) if p.ends_with("schema.prisma") && p.is_absolute()));
        assert_eq!((entries[2].prefix.as_deref(), entries[2].managed), (Some("ord_"), Some(false)));
        assert!(entries[2].migrations.as_ref().unwrap().ends_with("history/shop"));
        assert!(anchor(&entries).unwrap().ends_with("schema.prisma"));
        assert!(apps("migrations = \"db\"\n").unwrap().is_empty());
    }

    #[test]
    fn a_wrong_apps_table_names_the_file_and_the_key() {
        let cases = [
            ("apps = 3\n", "monorm.toml: `apps` must be a table"),
            ("schema = \"a.prisma\"\n[apps]\nmain = \"a.prisma\"\n", "`schema` is not a setting when `[apps]` is set"),
            ("[apps]\n\"my-app\" = \"a.prisma\"\n", "`apps.my-app`"),
            ("[apps]\nproject = \"a.prisma\"\n", "`apps.project`"),
            ("[apps]\n\"1a\" = \"a.prisma\"\n", "`apps.1a`"),
            ("[apps]\nmain = 3\n", "`apps.main`"),
            ("[apps]\nmain = { prefix = \"a_\" }\n", "`apps.main` has no `path`"),
            ("[apps]\nmain = { path = \"a.prisma\", color = 1 }\n", "`apps.main.color` is not a key"),
            ("[apps]\nauth = { path = \"app:monorm-auth\", prefix = \"x_\" }\n", "is not allowed on an installed app"),
            ("[apps]\nmain = { path = \"a.prisma\", managed = \"no\" }\n", "`apps.main.managed`"),
        ];
        for (text, expect) in cases {
            let error = apps(text).unwrap_err();
            assert!(error.starts_with("monorm.toml: ") && error.contains(expect), "{text}: {error}");
        }
    }
}
