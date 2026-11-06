//! The project settings file: one lookup for every `monorm` command and for the apps built on it.
//!
//! The first file that holds settings wins; the files are never merged:
//!
//! 1. `monorm.toml`: the whole file is the settings.
//! 2. `pyproject.toml`: the `[tool.monorm]` table.
//! 3. `package.json`: the `"monorm"` object.
//!
//! A `pyproject.toml` without `[tool.monorm]` and a `package.json` without `"monorm"` hold no settings,
//! so the lookup goes on. The settings are a table of keys. The `monorm` command reads `schema`, `migrations`,
//! `query_sets` and `model_methods` (`querySets` and `modelMethods` in `package.json`; the other spelling is an error, see [`Settings::reject_spelling`]);
//! every other key (for example `[admin]`) belongs to its owner, which reads it with [`Settings::section`].

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

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
                return Ok(Settings { found: Some((source, root)) });
            }
        }
        Ok(Settings::default())
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
    /// `dir` (the directory) and `online` (a boolean, default `false`: PostgreSQL migrations in the online form).
    pub fn migrations(&self) -> Result<MigrationsSettings, String> {
        match self.section("migrations") {
            None => Ok(MigrationsSettings::default()),
            Some(Value::String(dir)) => Ok(MigrationsSettings { dir: Some(dir.clone()), online: false }),
            Some(Value::Object(table)) => {
                let mut out = MigrationsSettings::default();
                for (name, value) in table {
                    match (name.as_str(), value) {
                        ("dir", Value::String(s)) => out.dir = Some(s.clone()),
                        ("dir", _) => return Err(self.wrong("migrations.dir", "must be a string")),
                        ("online", Value::Bool(b)) => out.online = *b,
                        ("online", _) => return Err(self.wrong("migrations.online", "must be true or false")),
                        _ => return Err(self.wrong(&format!("migrations.{name}"), "is not a setting; the keys are `dir` and `online`")),
                    }
                }
                Ok(out)
            }
            Some(_) => Err(self.wrong("migrations", "must be a directory or a table")),
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

    fn wrong(&self, key: &str, what: &str) -> String {
        match &self.found {
            Some((source, _)) => format!("{}: `{}{key}` {what}", source.file(), source.prefix()),
            None => format!("`{key}` {what}"),
        }
    }
}

/// The `migrations` setting, see [`Settings::migrations`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct MigrationsSettings {
    pub dir: Option<String>,
    pub online: bool,
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
        assert_eq!(migrations("monorm.toml", "migrations = \"db\"\n").unwrap(), MigrationsSettings { dir: Some("db".into()), online: false });
        assert_eq!(migrations("monorm.toml", "[migrations]\nonline = true\n").unwrap(), MigrationsSettings { dir: None, online: true });
        assert_eq!(migrations("pyproject.toml", "[tool.monorm.migrations]\ndir = \"db\"\nonline = true\n").unwrap(), MigrationsSettings { dir: Some("db".into()), online: true });
        assert_eq!(migrations("package.json", "{\"monorm\": {\"migrations\": {\"online\": true}}}").unwrap().online, true);
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
}
