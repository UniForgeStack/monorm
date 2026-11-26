//! The schema language (`.prisma` files): Prisma's schema syntax, plus our own
//! attributes and blocks for what Prisma lacks.
//!
//! ```text
//! import "extensions/acme.toml"          // extra extension definitions (see ext.rs)
//!
//! datasource db {
//!   provider   = "postgresql"
//!   extensions = [postgis(version: "3.4")] // pin an extension; used ones are added anyway
//! }
//!
//! model User {
//!   id         BigInt   @id @default(autoincrement())
//!   email      String   @unique @db.Citext   // extension type
//!   created_at DateTime @default(now())
//!   posts      Post[]
//!
//!   @@index([created_at(sort: Desc)], where: raw("email IS NOT NULL"))
//!   @@map("users")
//! }
//! ```
//!
//! `import "app:monorm-auth"` reads the schema of an installed app; the host resolves the name
//! to a file and gives the loader an [`Installed`] set.
//!
//! [`compile`] turns a file into the [`SchemaIr`] every binding consumes. The full
//! reference is `docs/schema/schema.md`; `docs/schema/prisma-syntax.md` has the design.

pub mod expr;
pub mod sql_expr;
pub mod native;
pub(crate) mod expr_arg;
mod lower;
mod render;
mod syntax;

use std::path::{Path, PathBuf};

use crate::ir::SchemaIr;
use crate::schema::Schema;
use crate::settings;

pub(crate) use lower::reads_row;
pub use syntax::Pos;

/// Compiles schema source. `origin` is the file it came from: error messages name it
/// and `import` paths resolve against its directory.
/// It drops the warnings; `compile_project` returns them.
pub fn compile(source: &str, origin: Option<&Path>) -> Result<SchemaIr, String> {
    compile_project(source, origin).map(|project| project.ir)
}

/// What the host found installed: the schema file of each app (`import "app:<name>"`, by distribution name)
/// and the manifest of each extension package (by extension id). The host (Python, Node or `--app`
/// and `--extension`) discovers them; the loader never searches.
#[derive(Clone, Debug, Default)]
pub struct Installed {
    pub apps: std::collections::BTreeMap<String, PathBuf>,
    pub extensions: Vec<(String, PathBuf)>,
    /// The `[database]` table of the settings file: the one typed input that replaces the `datasource` block.
    pub database: Option<crate::settings::DatabaseConfig>,
    /// The `[apps]` table. When it is not empty, a schema file path compiles the apps instead of one file (the CLI sets it; see [`Installed::with_apps`]).
    pub project: Vec<crate::settings::AppEntry>,
}

impl Installed {
    pub fn new() -> Self { Self::default() }

    /// Reads the `[database]` table of the settings file in `dir`, as the command line does for the working directory.
    /// It reads no `[apps]`: a binding that compiles one file path gets a one-app project.
    pub fn with_settings_of(mut self, dir: &Path) -> Result<Self, String> {
        self.database = crate::settings::Settings::load(dir)?.database()?;
        Ok(self)
    }

    /// Sets the apps of the project: the file-based compile functions then build these apps, not one file.
    pub fn with_apps(mut self, apps: Vec<crate::settings::AppEntry>) -> Self {
        self.project = apps;
        self
    }
}

impl From<std::collections::BTreeMap<String, PathBuf>> for Installed {
    fn from(apps: std::collections::BTreeMap<String, PathBuf>) -> Self { Self { apps, extensions: vec![], database: None, project: vec![] } }
}

impl<const N: usize> From<[(String, PathBuf); N]> for Installed {
    fn from(apps: [(String, PathBuf); N]) -> Self { Self::from(std::collections::BTreeMap::from(apps)) }
}

impl FromIterator<(String, PathBuf)> for Installed {
    fn from_iter<I: IntoIterator<Item = (String, PathBuf)>>(iter: I) -> Self { Self::from(iter.into_iter().collect::<std::collections::BTreeMap<_, _>>()) }
}

/// Source ownership for generated modules. Model and enum names remain global.
pub struct SchemaUnit {
    pub path: PathBuf,
    /// The installed app that owns this file, directly or through its own imports.
    pub app: Option<String>,
    pub models: Vec<String>,
    pub enums: Vec<String>,
    pub types: Vec<String>,
}

/// One app of the project: the entry file, the part files that it imports, and its settings after the rules
/// `[apps]` entry, then the app block, then the default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppUnit {
    pub label: String,
    /// The package of an installed app (`app:<package>`); `None` for an app of the project.
    pub package: Option<String>,
    pub entry: PathBuf,
    /// The entry file first, then every part file in import order.
    pub files: Vec<PathBuf>,
    /// The table prefix; J1 reads and stores it, the table names do not use it yet.
    pub prefix: String,
    pub managed: Option<bool>,
    /// The history folder: the `[apps]` key (against the settings folder), else the app block (against the entry file).
    pub migrations: Option<PathBuf>,
}

pub struct CompiledProject {
    pub ir: SchemaIr,
    /// The apps of the project in `[apps]` order (by label); one app for a single schema file.
    pub apps: Vec<AppUnit>,
    pub units: Vec<SchemaUnit>,
    /// Source files and database catalog resources read by compilation.
    pub inputs: Vec<PathBuf>,
    /// Warnings of schema expressions, such as a null test on a column that is never null.
    pub warnings: Vec<expr::Warning>,
    /// The manifests of the artifact and of the selected libraries.
    pub manifests: Vec<crate::behavior::Manifest>,
}

/// What a file is to its app.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    /// The root schema, or the entry file of an app of `[apps]`.
    Root,
    /// The entry file of an installed app that a schema imports (`import "app:..."`, the old form).
    Entry,
    /// A part file.
    Part,
}

/// The keys of an `app settings` block.
#[derive(Default)]
struct AppBlock {
    label: Option<String>,
    prefix: Option<String>,
    managed: Option<bool>,
    migrations: Option<String>,
}

/// The app that the loader reads: its `[apps]` entry, its settings after the precedence rules, and its files.
struct AppState {
    entry: settings::AppEntry,
    options: syntax::ImportOptions,
    migrations: Option<PathBuf>,
    files: Vec<PathBuf>,
}

struct Loader<'a> {
    apps: &'a std::collections::BTreeMap<String, PathBuf>,
    overrides: &'a std::collections::BTreeMap<PathBuf, String>,
    units: Vec<SchemaUnit>,
    seen: std::collections::HashMap<PathBuf, String>,
    active: Vec<PathBuf>,
    locations: Vec<(u32, u32, String)>,
    lines: u32,
    /// The root schema is source text without a file: it has no directory to resolve an app from.
    sourceless: bool,
    /// The app that is being read, when the project has `[apps]`.
    app: Option<AppState>,
    /// The app of each file that `[apps]` loaded, to name both apps when two claim one file.
    owners: std::collections::HashMap<PathBuf, String>,
    /// The extension definitions that an app loaded: a definition that two files import loads once.
    definitions: std::collections::HashSet<PathBuf>,
    /// The label and settings of the root schema's own `app settings` block (a single schema file).
    root_block: Option<AppBlock>,
}

impl Loader<'_> {
    /// The file that holds `line` of the joined source, and the line where that file starts.
    fn file_of(&self, line: u32) -> (&str, u32) {
        let (start, _, label) = self.locations.iter().find(|(start, end, _)| line >= *start && line < *end).unwrap_or(&self.locations[0]);
        (label, *start)
    }

    fn located(&self, e: syntax::Error) -> String {
        let (label, start) = self.file_of(e.pos.line);
        format!("{label}:{}:{}: {}", e.pos.line.saturating_sub(start) + 1, e.pos.col, e.msg)
    }

    /// The file a diagnostic is in, and its span with the line counted from the top of that file.
    fn located_span(&self, span: expr::Span) -> (String, expr::Span) {
        let (label, start) = self.file_of(span.line);
        (label.to_string(), expr::Span { line: span.line.saturating_sub(start) + 1, ..span })
    }

    /// The `app settings` block of one file: its keys and their types, one block, and only in an entry file.
    fn block_of(&self, items: &[syntax::Item], role: Role) -> Result<Option<AppBlock>, String> {
        let mut found: Option<AppBlock> = None;
        for item in items {
            let syntax::Item::AppSettings { pos, props } = item else { continue };
            let fail = |pos: syntax::Pos, msg: String| self.located(syntax::Error { pos, msg });
            if role == Role::Part {
                return Err(fail(*pos, "app settings: the app block belongs in the entry file of the app, not in a part file; move it there".into()));
            }
            if found.is_some() {
                return Err(fail(*pos, "app settings: an entry file has one app block".into()));
            }
            let mut block = AppBlock::default();
            for (key, key_pos, value) in props {
                match (key.as_str(), value) {
                    ("label", syntax::Value::Str(label)) => {
                        settings::check_label(label).map_err(|m| fail(*key_pos, format!("app settings: label {m}")))?;
                        block.label = Some(label.clone());
                    }
                    ("prefix", syntax::Value::Str(prefix)) => block.prefix = Some(prefix.clone()),
                    ("managed", syntax::Value::Bool(managed)) => block.managed = Some(*managed),
                    ("migrations", syntax::Value::Str(dir)) => block.migrations = Some(dir.clone()),
                    ("label" | "prefix" | "migrations", _) => return Err(fail(*key_pos, format!("app settings: {key} is a string, e.g. {key} = \"x\""))),
                    ("managed", _) => return Err(fail(*key_pos, "app settings: managed is true or false".into())),
                    _ => return Err(fail(*key_pos, format!("app settings: {key} is not a key of the app block; the keys are label, prefix, managed and migrations"))),
                }
            }
            found = Some(block);
        }
        Ok(found)
    }

    /// The settings of the app that is being read once its entry file is parsed. `[apps]` wins over the block, the block over the default.
    /// An installed app declares its label in the block, equal to its `[apps]` key, and takes no prefix.
    fn settle(&mut self, origin: &Path, block: Option<AppBlock>) -> Result<(), String> {
        let Some(app) = &mut self.app else { return Ok(()) };
        let entry = &app.entry;
        let installed = matches!(entry.source, settings::AppSource::Installed(_));
        let block = block.unwrap_or_default();
        let name = match &entry.source { settings::AppSource::Installed(package) => format!("app `{}` (`{package}`)", entry.label), _ => format!("app `{}`", entry.label) };
        let file = origin.display();
        match (&block.label, installed) {
            (Some(label), true) if *label != entry.label => return Err(format!(
                "{name}: the package declares the label `{label}` in {file}, so its [apps] key must be `{label}`, not `{}`; the project cannot relabel an installed app", entry.label)),
            (Some(label), false) if *label != entry.label => return Err(format!(
                "{name}: the app block of {file} has the label `{label}`, which differs from the [apps] key `{}`; use one label for both", entry.label)),
            (None, true) => return Err(format!("{name}: {file} has no label; an installed app declares it: `app settings {{ label = \"{}\" }}`", entry.label)),
            _ => {}
        }
        if installed && block.prefix.is_some() {
            return Err(format!("{name}: {} sets `prefix`; the tables of an installed app take the prefix `{}_`, and no prefix can change it", origin.display(), entry.label));
        }
        app.options = syntax::ImportOptions {
            prefix: entry.prefix.clone().or(block.prefix).unwrap_or_default(),
            managed: entry.managed.or(block.managed),
        };
        app.migrations = entry.migrations.clone().or_else(|| block.migrations.map(|dir| origin.parent().unwrap_or(Path::new("")).join(dir)));
        Ok(())
    }

    fn expand(&mut self, source: &str, origin: &Path, via: &str, options: &syntax::ImportOptions, role: Role, owner: Option<&str>) -> Result<Vec<syntax::Item>, String> {
        let canonical = if origin.exists() {
            origin.canonicalize().map_err(|e| e.to_string())?
        } else {
            origin.to_path_buf()
        };
        if self.active.contains(&canonical) {
            return Err(format!("{}: cyclic schema import", origin.display()));
        }
        if let Some(app) = &self.app {
            match self.owners.get(&canonical) {
                Some(first) if *first == app.entry.label => return Ok(vec![]),
                Some(first) => return Err(format!(
                    "{}: a file belongs to one app, and this one belongs to app `{first}` and to app `{}` ({via}); import it from one app only",
                    origin.display(), app.entry.label)),
                None => { self.owners.insert(canonical.clone(), app.entry.label.clone()); }
            }
        } else if let Some(first) = self.seen.get(&canonical) {
            return Err(format!("{}: schema imported more than once, by {first} and by {via}; import a shared app once", origin.display()));
        }
        self.seen.insert(canonical.clone(), via.to_owned());
        self.active.push(canonical);
        let offset = self.lines;
        self.lines += source.lines().count() as u32 + 1;
        self.locations.push((offset + 1, self.lines + 1, origin.display().to_string()));
        let items = if offset == 0 { syntax::parse(source) } else { syntax::parse_at(source, offset + 1) }
            .map_err(|e| self.located(e))?;
        let block = self.block_of(&items, role)?;
        let mut root_options = None;
        if let Some(app) = self.app.as_mut() { app.files.push(origin.to_path_buf()); }
        if self.app.is_some() {
            if role == Role::Root { self.settle(origin, block)?; }
        } else if let Some(block) = block {
            if role == Role::Entry && block.prefix.is_some() {
                return Err(format!("{}: an installed app takes no `prefix`; its tables take `<label>_`", origin.display()));
            }
            if role == Role::Root {
                root_options = Some(syntax::ImportOptions { prefix: block.prefix.clone().unwrap_or_default(), managed: block.managed });
                self.root_block = Some(block);
            }
        }
        let options = match (&self.app, root_options) {
            (Some(app), _) => syntax::ImportOptions { prefix: app.options.prefix.clone(), managed: app.options.managed },
            (None, Some(own)) => own,
            (None, None) => syntax::ImportOptions { prefix: options.prefix.clone(), managed: options.managed },
        };
        let options = &options;
        let index = self.units.len();
        self.units.push(SchemaUnit { path: origin.to_path_buf(), app: owner.map(str::to_owned), models: vec![], enums: vec![], types: vec![] });
        let mut out = vec![];
        for mut item in items {
            match &mut item {
                syntax::Item::Import { pos, path, options: local } => {
                    if self.app.is_some() {
                        let fail = |msg: String| self.located(syntax::Error { pos: *pos, msg });
                        if let Some(name) = path.strip_prefix("app:") {
                            return Err(fail(format!("import {path:?}: an import only adds a part file of this app; list the app in [apps], e.g. {name} = \"{path}\"")));
                        }
                        if !local.prefix.is_empty() || local.managed.is_some() {
                            return Err(fail("import options prefix: and managed: are gone; set `prefix` or `managed` in the [apps] entry or in the app settings block of the entry file".into()));
                        }
                    }
                    let app = path.strip_prefix("app:").map(str::to_owned);
                    let imported = match &app {
                        Some(name) => {
                            let file = self.apps.get(name).ok_or_else(|| self.located(syntax::Error { pos: *pos,
                                msg: if self.sourceless {
                                    format!("import \"app:{name}\": an app import needs a schema file; use `load(path)` or generate the models")
                                } else {
                                    format!("import \"app:{name}\": not installed; install the `{name}` package")
                                } }))?;
                            file.canonicalize().map_err(|e| self.located(syntax::Error { pos: *pos,
                                msg: format!("import \"app:{name}\": {}: {e}", file.display()) }))?
                        }
                        None => origin.parent().unwrap_or(Path::new("")).join(&*path),
                    };
                    if app.is_some() || imported.extension().is_some_and(|e| e == "prisma") {
                        let text = self.overrides.get(&imported.canonicalize().unwrap_or_else(|_| imported.clone())).cloned().map(Ok).unwrap_or_else(|| std::fs::read_to_string(&imported))
                            .map_err(|e| self.located(syntax::Error { pos: *pos, msg: format!("import {path:?}: {e}") }))?;
                        let via = format!("import {path:?} in {}", origin.display());
                        let child = syntax::ImportOptions {
                            prefix: format!("{}{}", options.prefix, local.prefix),
                            managed: options.managed.or(local.managed),
                        };
                        let role = if app.is_some() { Role::Entry } else { Role::Part };
                        out.extend(self.expand(&text, &imported, &via, &child, role, app.as_deref().or(owner))?);
                        continue;
                    }
                    if local.managed.is_some() {
                        return Err(self.located(syntax::Error { pos: *pos, msg: "managed is only allowed on schema imports".into() }));
                    }
                    if self.app.is_none() && (role != Role::Root || !local.prefix.is_empty()) {
                        return Err(self.located(syntax::Error { pos: *pos,
                            msg: "extension imports belong in the main schema and cannot have a prefix".into() }));
                    }
                    let file = imported.canonicalize().map_err(|e| self.located(syntax::Error {
                        pos: *pos, msg: format!("import {path:?}: {e}")
                    }))?;
                    // Two files of an app can import one definition: it loads once.
                    if self.app.is_some() && !self.definitions.insert(file.clone()) { continue; }
                    *path = file.to_string_lossy().into_owned();
                }
                syntax::Item::Datasource { pos, .. } if role != Role::Root => {
                    return Err(self.located(syntax::Error { pos: *pos,
                        msg: "datasource settings are only allowed in the main schema".into() }));
                }
                syntax::Item::Model(m) => {
                    self.units[index].models.push(m.name.clone());
                    m.managed_override = options.managed.filter(|_| !m.is_view);
                    let prefix = &options.prefix;
                    if !prefix.is_empty() {
                        for name in ["map", "renamed_from"] {
                            if let Some(attr) = m.blocks.iter_mut().find(|a| a.name == name) {
                                if let Some((_, syntax::Value::Str(value))) = attr.args.positional.first_mut() {
                                    *value = format!("{prefix}{value}");
                                }
                            } else if name == "map" {
                                m.blocks.push(syntax::Attr { pos: m.pos, name: "map".into(), args: syntax::Args {
                                    pos: m.pos, positional: vec![(m.pos, syntax::Value::Str(format!("{prefix}{}", m.name.to_lowercase())))],
                                    named: vec![], named_values: vec![],
                                } });
                            }
                        }
                    }
                }
                syntax::Item::Enum(e) => self.units[index].enums.push(e.name.clone()),
                syntax::Item::Type(t) => self.units[index].types.push(t.name.clone()),
                _ => {}
            }
            out.push(item);
        }
        self.active.pop();
        Ok(out)
    }
}

/// The path of a view body, relative to the schema file: `views/<db schema>/<Name>.sql` (`main` on SQLite, `public` on PostgreSQL).
/// The compiler and `pull` both build the path here, so a pulled schema always compiles.
pub fn view_body_path(sqlite: bool, name: &str) -> PathBuf {
    Path::new("views").join(if sqlite { "main" } else { "public" }).join(format!("{name}.sql"))
}

/// The SQL body of each `view` block: `views/<db schema>/<Name>.sql` next to the schema file that declares the view
/// (`public` on PostgreSQL, `main` on SQLite). The text comes from the override map first, then from disk.
/// Returns the files it read.
fn view_bodies(items: &mut [syntax::Item], loader: &Loader<'_>, database: Option<&crate::settings::DatabaseConfig>) -> Result<Vec<PathBuf>, String> {
    let sqlite = match database {
        Some(database) => database.dialect == crate::dialect::Dialect::Sqlite,
        None => items.iter().any(|item| matches!(item, syntax::Item::Datasource { props, .. }
            if props.iter().any(|(key, _, value)| key == "provider" && matches!(value, syntax::Value::Str(p) if p == "sqlite")))),
    };
    let mut read = vec![];
    for item in items {
        let syntax::Item::Model(m) = item else { continue };
        if !m.is_view { continue; }
        let owner = loader.units.iter().find(|unit| unit.models.contains(&m.name)).map(|unit| unit.path.clone()).unwrap_or_default();
        let path = owner.parent().unwrap_or(Path::new("")).join(view_body_path(sqlite, &m.name));
        let text = loader.overrides.get(&path).or_else(|| path.canonicalize().ok().and_then(|c| loader.overrides.get(&c))).cloned()
            .map(Ok).unwrap_or_else(|| std::fs::read_to_string(&path)).map_err(|_| loader.located(syntax::Error { pos: m.pos,
                msg: format!("view {}: the SQL body file {} is missing; write the SELECT of the view there", m.name, path.display()) }))?;
        let body = text.trim().trim_end_matches(';').trim_end().to_owned();
        if body.is_empty() {
            return Err(loader.located(syntax::Error { pos: m.pos, msg: format!("view {}: the SQL body file {} is empty", m.name, path.display()) }));
        }
        m.sql_body = Some(body);
        read.push(path);
    }
    Ok(read)
}

/// [`compile`] with source text for files that the compiler would read: `overrides` maps the path of a view body
/// (`views/<db schema>/<Name>.sql`, as the compiler builds it) to its text. Pull and drift use it for a schema with no files.
pub fn compile_with_overrides(source: &str, origin: Option<&Path>, overrides: &std::collections::BTreeMap<PathBuf, String>) -> Result<SchemaIr, String> {
    compile_project_overlay(source, origin, None, &Installed::new(), overrides).map(|project| project.ir)
}

/// [`compile_with_overrides`] with the installed extensions and apps, for a schema that uses their attributes.
pub fn compile_installed_with_overrides(source: &str, origin: Option<&Path>, installed: &Installed, overrides: &std::collections::BTreeMap<PathBuf, String>) -> Result<SchemaIr, String> {
    compile_project_overlay(source, origin, None, installed, overrides).map(|project| project.ir)
}

pub fn compile_project(source: &str, origin: Option<&Path>) -> Result<CompiledProject, String> {
    compile_project_mode(source, origin, None, &Installed::new())
}

/// [`compile_project`] with the installed apps that `import "app:<name>"` reads.
pub fn compile_project_with(source: &str, origin: Option<&Path>, installed: &Installed) -> Result<CompiledProject, String> {
    compile_project_mode(source, origin, None, installed)
}

type IdentityUpdate<'a> = (&'a [(String, String)], &'a [String]);

fn compile_project_mode(source: &str, origin: Option<&Path>, update: Option<IdentityUpdate<'_>>, installed: &Installed) -> Result<CompiledProject, String> {
    compile_project_overlay(source, origin, update, installed, &Default::default())
}

/// The label of a single schema file with no app block: the file stem as a Python identifier, and `main` for source text.
fn stem_label(origin: &Path, sourceless: bool) -> String {
    let stem = origin.file_stem().map(|s| s.to_string_lossy().into_owned()).filter(|_| !sourceless).unwrap_or_default();
    let mut label: String = stem.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    if label.is_empty() || label == "project" { return "main".into(); }
    if label.starts_with(|c: char| c.is_ascii_digit()) { label.insert(0, '_'); }
    label
}

/// The apps of `[apps]`: each entry file with the part files that it imports. The `origin` of the compile is the anchor of the project.
fn expand_apps(loader: &mut Loader<'_>, installed: &Installed) -> Result<(Vec<syntax::Item>, Vec<AppUnit>), String> {
    let mut items = vec![];
    let mut apps: Vec<AppUnit> = vec![];
    for entry in &installed.project {
        if let Some(first) = apps.iter().find(|a| a.label == entry.label) {
            return Err(format!("[apps]: two apps have the label `{}` ({}); a label is unique", entry.label, first.entry.display()));
        }
        let (file, package) = match &entry.source {
            settings::AppSource::Path(file) => (file.clone(), None),
            settings::AppSource::Installed(package) => {
                let file = installed.apps.get(package).ok_or_else(|| format!(
                    "[apps]: app `{}` is `app:{package}`, which is not installed; install the `{package}` package", entry.label))?;
                (file.clone(), Some(package.clone()))
            }
        };
        let canonical = file.canonicalize().unwrap_or_else(|_| file.clone());
        let text = loader.overrides.get(&canonical).cloned().map(Ok).unwrap_or_else(|| std::fs::read_to_string(&file))
            .map_err(|e| format!("[apps]: app `{}`: {}: {e}", entry.label, file.display()))?;
        loader.app = Some(AppState { entry: entry.clone(), options: syntax::ImportOptions::default(), migrations: None, files: vec![] });
        let via = format!("the entry file of app `{}`", entry.label);
        items.extend(loader.expand(&text, &file, &via, &Default::default(), Role::Root, package.as_deref())?);
        let state = loader.app.take().expect("the app that was read");
        apps.push(AppUnit { label: entry.label.clone(), package, entry: file, files: state.files, prefix: state.options.prefix, managed: state.options.managed, migrations: state.migrations });
    }
    Ok((items, apps))
}

fn compile_project_overlay(source: &str, origin: Option<&Path>, update: Option<IdentityUpdate<'_>>, installed: &Installed, overrides: &std::collections::BTreeMap<PathBuf, String>) -> Result<CompiledProject, String> {
    compile_project_inner(source, origin, update, installed, overrides, false)
}

/// `apps`: compile the apps of `installed.project` (the entry files) and not `source`. `origin` is then the anchor path of the project.
fn compile_project_inner(source: &str, origin: Option<&Path>, update: Option<IdentityUpdate<'_>>, installed: &Installed, overrides: &std::collections::BTreeMap<PathBuf, String>, apps: bool) -> Result<CompiledProject, String> {
    let mut loader = Loader { overrides, apps: &installed.apps, units: vec![], seen: Default::default(), active: vec![], locations: vec![], lines: 0, sourceless: origin.is_none(),
        app: None, owners: Default::default(), definitions: Default::default(), root_block: None };
    let origin = origin.unwrap_or(Path::new("<schema>"));
    let (mut items, mut app_units) = if apps {
        expand_apps(&mut loader, installed)?
    } else {
        (loader.expand(source, origin, "the root schema", &Default::default(), Role::Root, None)?, vec![])
    };
    if !apps {
        let label = loader.root_block.as_ref().and_then(|b| b.label.clone()).unwrap_or_else(|| stem_label(origin, loader.sourceless));
        let files = loader.units.iter().map(|u| u.path.clone()).collect();
        let block = loader.root_block.as_ref();
        app_units.push(AppUnit { label, package: None, entry: origin.to_path_buf(), files,
            prefix: block.and_then(|b| b.prefix.clone()).unwrap_or_default(), managed: block.and_then(|b| b.managed),
            migrations: block.and_then(|b| b.migrations.as_ref()).map(|dir| origin.parent().unwrap_or(Path::new("")).join(dir)) });
    }
    let sidecars = view_bodies(&mut items, &loader, installed.database.as_ref())?;
    let mut declared = std::collections::HashMap::new();
    for item in &items {
        let (name, pos) = match item {
            syntax::Item::Model(m) => (&m.name, m.pos),
            syntax::Item::Enum(e) => (&e.name, e.pos),
            _ => continue,
        };
        if let Some(first) = declared.insert(name, pos) {
            let first = loader.located(syntax::Error { pos: first, msg: "first declaration".into() });
            return Err(loader.located(syntax::Error { pos, msg: format!("{name} is declared twice; {first}") }));
        }
    }
    let label = origin.display().to_string();
    let named = lower::generator_extensions(&items).map_err(|e| loader.located(e))?;
    let plan = crate::loader::plan(&installed.extensions, &lower::used_namespaces(&items), &named, crate::behavior::manifests())?;
    let manifests = plan.as_ref().map_or(crate::behavior::manifests(), |p| &p.manifests);
    let mut declarations = lower::behavior_declarations(&mut items, &label, manifests).map_err(|e| loader.located(e))?;
    for declaration in &mut declarations {
        let location = &mut declaration.location;
        let (file, base) = match loader.locations.iter().find(|(start, end, _)| location.line >= *start && location.line < *end) {
            Some((start, _, file)) => (file.clone(), *start),
            None => (location.file.clone(), 1),
        };
        location.file = file.clone();
        location.line = location.line + 1 - base;
        expr_arg::stamp(declaration, &file, base);
    }
    // An attribute of a missing extension can take its field out of the model: the namespace error comes before lowering.
    if let Some(d) = declarations.iter().find(|d| d.field.is_some() && lower::declared(manifests, &d.attribute, true).is_none()) {
        return Err(crate::behavior::unowned_declaration_error(d));
    }
    let views = lower::view_models(manifests, &declarations);
    let tableless = lower::tableless_models(manifests, &declarations);
    let path = crate::identity::manifest_path(origin);
    // A handwritten ContentType enum stays ordinary unless generation or an attribute that needs identities asks for them.
    let handwritten = items.iter().any(|i| matches!(i, syntax::Item::Enum(e) if e.name == "ContentType"));
    let needs_identities = update.is_some() || declarations.iter().any(|d| lower::declared(manifests, &d.attribute, d.field.is_some()).is_some_and(|a| a.identities)) || !handwritten && (path.is_file()
        || items.iter().any(|item| matches!(item, syntax::Item::Model(m) if m.members.iter().any(|f| f.ty.name == "ContentType"))));
    let identities = if needs_identities {
        if handwritten {
            return Err("ContentType is generated from the identity manifest; remove the handwritten enum".into());
        }
        let manifest = if let Some((renames, restores)) = update {
            let prior = if path.exists() { crate::identity::read(&path)? } else { Default::default() };
            let names = items.iter().filter_map(|i| if let syntax::Item::Model(m) = i { if views.contains(&m.name) || tableless.contains(&m.name) { None } else { Some(m.name.clone()) } } else { None }).collect::<Vec<_>>();
            crate::identity::reconcile(&prior, &names, renames, restores)?
        } else { crate::identity::read(&path)? };
        let e = crate::identity::content_type(&manifest)?;
        let members = e.values.iter().map(|v| format!("{} @value({})", v.name, v.value)).collect::<Vec<_>>().join("\n");
        let generated = format!("enum ContentType {{\n{members}\n@@storage(int)\n}}");
        items.extend(syntax::parse(&generated).map_err(|e| format!("generated ContentType: {}", e.msg))?);
        // All per-source Python facades expose the same shared enum.
        for unit in &mut loader.units { unit.enums.push("ContentType".into()); }
        Some(manifest)
    } else { None };
    let catalog_inputs = std::cell::RefCell::new(Vec::new());
    let load = |path: &str| {
        catalog_inputs.borrow_mut().push(PathBuf::from(path));
        std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))
    };
    let deferred_identity: std::collections::HashSet<String> = declarations.iter().filter(|d| d.field.is_none()).map(|d| d.model.clone()).collect();
    // Checks after extension lowering report the model or relation member position.
    let mut positions = std::collections::HashMap::new();
    for item in &items {
        if let syntax::Item::Model(m) = item {
            positions.insert((m.name.clone(), None), m.pos);
            for member in &m.members { positions.insert((m.name.clone(), Some(member.name.clone())), member.pos); }
        }
    }
    let at = |model: &str, member: Option<&str>, msg: String| match positions.get(&(model.to_owned(), member.map(str::to_owned))) {
        Some(pos) => loader.located(syntax::Error { pos: *pos, msg }),
        None => msg,
    };
    let owned_types = lower::owned_types(manifests);
    let (mut ir, diagnostics, mut deferred) = lower::Lowering { load: &load, deferred_identity: &deferred_identity, views: &views, owned_types: &owned_types, database: installed.database.as_ref() }.lower_deferred(items).map_err(|e| loader.located(e))?;
    let mut warnings: Vec<expr::Warning> = diagnostics.into_iter().map(|d| {
        let (file, span) = loader.located_span(d.span);
        expr::Warning { file, span, message: d.message }
    }).collect();
    ir.identities = identities;
    if !declarations.is_empty() {
        ir.behavior.schema_contract = crate::behavior::SCHEMA_CONTRACT;
        ir.behavior.declarations = declarations;
    }
    if update.is_none() {
        crate::behavior::prepare_collecting(&mut ir, None, plan.as_ref(), &mut warnings)?;
    }
    // The passes ran, so a native expression may name a field that an extension added.
    // Update mode runs no pass: with declarations it leaves such an expression open, because the identity manifest does not need it.
    let last = update.is_none() || ir.behavior.declarations.is_empty();
    for d in deferred.resolve(&mut ir, last).map_err(|e| loader.located(e))? {
        let (file, span) = loader.located_span(d.span);
        warnings.push(expr::Warning { file, span, message: d.message });
    }
    if last { deferred.finish(&mut ir).map_err(|e| loader.located(e))?; }
    if update.is_none() {
        crate::partition::validate(&ir)?;
        for model in &ir.models {
            if !model.fields.iter().any(|field| field.primary_key) {
                return Err(at(&model.name, None, format!("model {} has no @id field after extension lowering", model.name)));
            }
            for relation in &model.relations {
                if relation.join_label.is_some() {
                    return Err(at(&model.name, Some(&relation.name), format!(
                        "relation {}.{}: {{model}} in a through: name is only expanded in a relation that an extension pass derives; name the join model without it",
                        model.name, relation.name)));
                }
                if relation.label.is_some() || relation.through.is_none() && relation.from.is_empty() && relation.path.is_empty() {
                    let named = relation.label.as_deref().filter(|l| !l.is_empty()).map(|l| format!(" named {l:?}")).unwrap_or_default();
                    return Err(at(&model.name, Some(&relation.name), format!(
                        "relation {}.{}: {} has no relation to {} with fields: / references:{named} after extension lowering",
                        model.name, relation.name, relation.target, model.name)));
                }
                if let Some(target) = ir.models.iter().find(|m| m.name == relation.target) {
                    if let Some(to) = relation.to.iter().find(|to| !target.fields.iter().any(|f| &f.name == *to)) {
                        return Err(at(&model.name, Some(&relation.name), format!("relation {}.{}: {} has no field {to} after extension lowering", model.name, relation.name, relation.target)));
                    }
                }
            }
        }
    }
    if ir.models.iter().any(|m| !m.managed) {
        ir.stamp_schema_contract();
    }
    crate::identity::validate(&ir, manifests)?;
    let mut inputs: Vec<_> = loader.units.iter().map(|unit| unit.path.clone()).collect();
    inputs.extend(catalog_inputs.into_inner());
    inputs.extend(sidecars);
    if ir.identities.is_some() { inputs.push(path); }
    Ok(CompiledProject { ir, apps: app_units, units: loader.units, inputs, warnings, manifests: manifests.to_vec() })
}

pub fn compile_project_file(path: &Path) -> Result<CompiledProject, String> {
    compile_project_file_with(path, &Installed::new())
}

pub fn compile_project_file_with(path: &Path, installed: &Installed) -> Result<CompiledProject, String> {
    if !installed.project.is_empty() {
        return compile_project_inner("", Some(path), None, installed, &Default::default(), true);
    }
    let source = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    compile_project_with(&source, Some(path), installed)
}

/// Compiles and validates a schema file.
/// It drops the warnings; `compile_project_file` returns them.
pub fn compile_file(path: &Path) -> Result<SchemaIr, String> {
    compile_file_with(path, &Installed::new())
}

/// `compile_file` with installed apps and extensions. It drops the warnings; `compile_project_file_with` returns them.
pub fn compile_file_with(path: &Path, installed: &Installed) -> Result<SchemaIr, String> {
    compile_project_file_with(path, installed).map(|project| project.ir)
}

/// Compiles source and checks it the way the engine will (relations, keys, schema
/// objects), so a bad schema fails at compile time rather than at connect.
pub fn check(ir: SchemaIr) -> Result<(SchemaIr, Schema), String> {
    let json = serde_json::to_string(&ir).map_err(|e| e.to_string())?;
    let schema = Schema::from_ir_unbound(serde_json::from_str(&json).map_err(|e| e.to_string())?)?;
    crate::migrate::snapshot(&schema)?;
    for warning in crate::partition::warnings(&ir) { eprintln!("warning: {warning}"); }
    Ok((ir, schema))
}

#[cfg(test)]
mod tests;

/// Explicit allocation is separate from compilation and usable without a compiled
/// extension that needs identities. A schema without extension declarations gets the full
/// engine check before atomic publication; extension lowering waits for compilation.
pub fn generate_identities(path: &Path, renames: &[(String, String)], restores: &[String]) -> Result<monorm::identity::IdentityManifest, String> {
    generate_identities_with(path, renames, restores, &Installed::new())
}

pub fn generate_identities_with(path: &Path, renames: &[(String, String)], restores: &[String], installed: &Installed) -> Result<monorm::identity::IdentityManifest, String> {
    let project = if installed.project.is_empty() {
        let source = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        compile_project_mode(&source, Some(path), Some((renames, restores)), installed)?
    } else {
        compile_project_inner("", Some(path), Some((renames, restores)), installed, &Default::default(), true)?
    };
    let manifest = project.ir.identities.clone().ok_or("missing generated identities")?;
    if project.ir.behavior.declarations.is_empty() { check(project.ir)?; }
    crate::identity::write(&crate::identity::manifest_path(path), &manifest)?;
    Ok(manifest)
}
