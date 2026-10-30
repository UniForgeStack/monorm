//! `monorm init [dir]`: write a small Python or TypeScript project.
//!
//! The command asks for the language, the tool, the database and the built-in extensions when
//! stdin and stderr are terminals. A flag answers its question. Without a terminal the command
//! never blocks: it uses the flags and the defaults.

use std::io::IsTerminal;
use std::path::Path;

use dialoguer::{theme::ColorfulTheme, Input, MultiSelect, Select};

use crate::{Failure, Host, Result};

/// The version of the packages that the project depends on.
const VERSION: &str = env!("CARGO_PKG_VERSION");
const MAIN_PY: &str = include_str!("../templates/main.py");
const INDEX_TS: &str = include_str!("../templates/index.ts");
const SQLITE_URL: &str = "sqlite://dev.db";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Python,
    TypeScript,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Uv,
    Pip,
    Npm,
    Pnpm,
    Yarn,
    Bun,
}

impl Tool {
    fn name(self) -> &'static str {
        match self {
            Tool::Uv => "uv",
            Tool::Pip => "pip",
            Tool::Npm => "npm",
            Tool::Pnpm => "pnpm",
            Tool::Yarn => "yarn",
            Tool::Bun => "bun",
        }
    }

    fn lang(self) -> Lang {
        if matches!(self, Tool::Uv | Tool::Pip) { Lang::Python } else { Lang::TypeScript }
    }
}

const PYTHON_TOOLS: [Tool; 2] = [Tool::Uv, Tool::Pip];
const TYPESCRIPT_TOOLS: [Tool; 4] = [Tool::Npm, Tool::Pnpm, Tool::Yarn, Tool::Bun];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Db {
    Sqlite,
    Postgres,
}

/// A model of the example schema that an extension adds.
#[derive(Debug, Clone, Copy)]
struct Model {
    name: &'static str,
    text: &'static str,
    /// Whether the model has a table, so it has a model identity. An abstract or proxy model has none.
    stored: bool,
}

/// One built-in extension and the part of the example schema that uses it.
#[derive(Debug)]
pub struct Ext {
    /// The short name that `--ext` takes.
    pub name: &'static str,
    label: &'static str,
    user_attrs: &'static str,
    post_attrs: &'static str,
    model: Option<Model>,
    /// Whether the extension needs `schema.identities.json`: it stores the id of a model.
    needs_identities: bool,
}

pub const EXTENSIONS: [Ext; 9] = [
    Ext { name: "timestamps", label: "timestamps: an updated_at column", user_attrs: "", post_attrs: "  @@timestamps.updated_at(\"updated_at\")\n", model: None, needs_identities: false },
    Ext { name: "soft-delete", label: "soft-delete: delete rows with a deleted_at column", user_attrs: "", post_attrs: "  @@soft_delete.deleted_at(\"deleted_at\")\n", model: None, needs_identities: false },
    Ext { name: "locking", label: "locking: a version column for optimistic locking", user_attrs: "", post_attrs: "  @@locking.version\n", model: None, needs_identities: false },
    Ext {
        name: "inherit",
        label: "inherit: abstract base models",
        user_attrs: "  @@inherit.extends(\"Audited\")\n",
        post_attrs: "",
        model: Some(Model { name: "Audited", text: "model Audited {\n  created_at DateTime @default(now())\n  @@inherit.abstract\n}\n", stored: false }),
        needs_identities: false,
    },
    Ext { name: "query-defaults", label: "query-defaults: a named filter on a model", user_attrs: "", post_attrs: "  @@query.filter(\"published_only\", \"published == true\")\n", model: None, needs_identities: false },
    Ext {
        name: "generic",
        label: "generic: a relation to one of several models",
        user_attrs: "",
        post_attrs: "",
        model: Some(Model { name: "Comment", text: "model Comment {\n  id     Int      @id @default(autoincrement())\n  body   String\n  target Generic? @generic.relation(targets: [\"User\", \"Post\"])\n}\n", stored: true }),
        needs_identities: true,
    },
    Ext {
        name: "proxy",
        label: "proxy: a model that reads the table of another model",
        user_attrs: "",
        post_attrs: "",
        model: Some(Model { name: "PostView", text: "model PostView {\n  @@proxy.of(Post)\n}\n", stored: false }),
        needs_identities: false,
    },
    Ext {
        name: "composition",
        label: "composition: a model that adds fields to a parent model",
        user_attrs: "",
        post_attrs: "",
        model: Some(Model { name: "Profile", text: "model Profile {\n  bio String\n  @@composition.model(parent: \"User\", parentRef: \"user\", childRef: \"profile\")\n  @@map(\"profiles\")\n}\n", stored: true }),
        needs_identities: false,
    },
    Ext {
        name: "file-storage",
        label: "file-storage: a file field that lives in a storage",
        user_attrs: "",
        post_attrs: "",
        model: Some(Model { name: "Attachment", text: "model Attachment {\n  id   Int   @id @default(autoincrement())\n  file Json? @storage.file(storage: \"attachments\")\n}\n", stored: true }),
        needs_identities: false,
    },
];

/// What the user asked for with flags. Every field is optional.
#[derive(Default)]
pub struct Request<'a> {
    pub dir: Option<&'a str>,
    pub lang: Option<&'a str>,
    pub tool: Option<&'a str>,
    pub db: Option<&'a str>,
    pub url: Option<&'a str>,
    pub ext: Option<&'a str>,
    pub yes: bool,
}

/// The facts about the caller that choose the defaults.
#[derive(Default)]
pub struct Env {
    pub interactive: bool,
    /// `npm_config_user_agent`: names the tool that runs `npm create`, `bun create` and so on.
    pub user_agent: Option<String>,
    /// `UV` is set in the processes that `uv run` and `uvx` start.
    pub uv: bool,
}

impl Env {
    pub fn current() -> Env {
        Env {
            interactive: std::io::stdin().is_terminal() && std::io::stderr().is_terminal(),
            user_agent: std::env::var("npm_config_user_agent").ok(),
            uv: std::env::var_os("UV").is_some(),
        }
    }
}

/// The answers to all questions.
#[derive(Debug)]
pub struct Plan {
    pub name: String,
    pub lang: Lang,
    pub tool: Tool,
    pub db: Db,
    pub url: String,
    pub ext: Vec<&'static Ext>,
}

fn usage(m: impl Into<String>) -> Failure {
    Failure::Usage(m.into())
}

fn parse_lang(v: &str) -> Result<Lang> {
    match v {
        "python" | "py" => Ok(Lang::Python),
        "typescript" | "ts" => Ok(Lang::TypeScript),
        _ => Err(usage(format!("--lang is python or typescript, not {v}"))),
    }
}

fn parse_tool(v: &str) -> Result<Tool> {
    PYTHON_TOOLS.iter().chain(&TYPESCRIPT_TOOLS).copied().find(|t| t.name() == v)
        .ok_or_else(|| usage(format!("--tool is uv, pip, npm, pnpm, yarn or bun, not {v}")))
}

fn parse_db(v: &str) -> Result<Db> {
    match v {
        "sqlite" => Ok(Db::Sqlite),
        "postgresql" | "postgres" => Ok(Db::Postgres),
        _ => Err(usage(format!("--db is sqlite or postgresql, not {v}"))),
    }
}

fn parse_ext(v: &str) -> Result<Vec<&'static Ext>> {
    let mut picked = [false; EXTENSIONS.len()];
    for word in v.split(',').map(str::trim).filter(|w| !w.is_empty()) {
        match word {
            "none" => {}
            "all" => picked = [true; EXTENSIONS.len()],
            _ => {
                let word = if word == "model-composition" { "composition" } else { word };
                let i = EXTENSIONS.iter().position(|e| e.name == word).ok_or_else(|| {
                    let names: Vec<_> = EXTENSIONS.iter().map(|e| e.name).collect();
                    usage(format!("unknown extension {word}; --ext takes none, all or a list of: {}", names.join(", ")))
                })?;
                picked[i] = true;
            }
        }
    }
    Ok(EXTENSIONS.iter().zip(picked).filter(|(_, p)| *p).map(|(e, _)| e).collect())
}

/// The tool of the `npm_config_user_agent` value, for example `bun/1.2.0 npm/? node/v24.0.0`.
fn tool_of_user_agent(agent: &str) -> Option<Tool> {
    [("bun/", Tool::Bun), ("pnpm/", Tool::Pnpm), ("yarn/", Tool::Yarn), ("npm/", Tool::Npm)]
        .into_iter().find(|(prefix, _)| agent.starts_with(prefix)).map(|(_, t)| t)
}

fn cancelled(e: dialoguer::Error) -> Failure {
    crate::failed(format!("init stopped: {e}"))
}

/// The project name that the directory gives: lower case, with `-` between words.
fn project_name(dir: &Path) -> String {
    let path = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let raw = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    let name: String = raw.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '_' { c } else { '-' }).collect();
    let name = name.trim_matches(|c| c == '-' || c == '.' || c == '_').to_owned();
    if name.is_empty() { "monorm-app".into() } else { name }
}

/// How `resolve` asks: `None` when a flag, a missing terminal or `--yes` answers every question.
type Prompt = Option<ColorfulTheme>;

fn choose(prompt: &Prompt, question: &str, items: &[&str], default: usize) -> Result<Option<usize>> {
    prompt.as_ref().map(|theme| Select::with_theme(theme).with_prompt(question).items(items).default(default).interact().map_err(cancelled)).transpose()
}

fn resolve_lang(req: &Request, host: Host, prompt: &Prompt, tool_flag: Option<Tool>) -> Result<Lang> {
    match (req.lang.map(parse_lang).transpose()?, tool_flag) {
        (Some(lang), _) => Ok(lang),
        (None, Some(tool)) => Ok(tool.lang()),
        (None, None) => {
            let default = if host == Host::Node { Lang::TypeScript } else { Lang::Python };
            let picked = choose(prompt, "Language", &["Python", "TypeScript"], usize::from(default == Lang::TypeScript))?;
            Ok(picked.map_or(default, |i| [Lang::Python, Lang::TypeScript][i]))
        }
    }
}

fn resolve_tool(tool_flag: Option<Tool>, lang: Lang, env: &Env, prompt: &Prompt) -> Result<Tool> {
    let tools: &[Tool] = if lang == Lang::Python { &PYTHON_TOOLS } else { &TYPESCRIPT_TOOLS };
    if let Some(tool) = tool_flag {
        return if tool.lang() == lang { Ok(tool) } else { Err(usage(format!("--tool {} does not fit the language", tool.name()))) };
    }
    let default = match lang {
        Lang::Python if env.uv => Tool::Uv,
        Lang::Python => Tool::Pip,
        Lang::TypeScript => env.user_agent.as_deref().and_then(tool_of_user_agent).unwrap_or(Tool::Npm),
    };
    let names: Vec<_> = tools.iter().map(|t| t.name()).collect();
    let picked = choose(prompt, "Tool", &names, tools.iter().position(|t| *t == default).unwrap_or(0))?;
    Ok(picked.map_or(default, |i| tools[i]))
}

/// The flag, else the database of a PostgreSQL `--url`, else the answer, else SQLite.
fn resolve_db(req: &Request, prompt: &Prompt) -> Result<Db> {
    if let Some(db) = req.db.map(parse_db).transpose()? {
        return Ok(db);
    }
    if req.url.is_some_and(|u| u.starts_with("postgres")) {
        return Ok(Db::Postgres);
    }
    let picked = choose(prompt, "Database", &["SQLite", "PostgreSQL"], 0)?;
    Ok(picked.map_or(Db::Sqlite, |i| [Db::Sqlite, Db::Postgres][i]))
}

fn resolve_url(req: &Request, db: Db, name: &str, prompt: &Prompt) -> Result<String> {
    match db {
        Db::Sqlite if req.url.is_some() => Err(usage("--url needs --db postgresql")),
        Db::Sqlite => Ok(SQLITE_URL.to_owned()),
        Db::Postgres => {
            let default = format!("postgresql://postgres:postgres@localhost:5432/{}", name.replace(['-', '.'], "_"));
            match (req.url, prompt) {
                (Some(url), _) => Ok(url.to_owned()),
                (None, Some(theme)) => Input::<String>::with_theme(theme).with_prompt("PostgreSQL URL").default(default).interact_text().map_err(cancelled),
                (None, None) => Ok(default),
            }
        }
    }
}

fn resolve_ext(req: &Request, prompt: &Prompt) -> Result<Vec<&'static Ext>> {
    match (req.ext, prompt) {
        (Some(v), _) => parse_ext(v),
        (None, Some(theme)) => {
            let labels: Vec<_> = EXTENSIONS.iter().map(|e| e.label).collect();
            let picked = MultiSelect::with_theme(theme).with_prompt("Built-in extensions (space picks, enter accepts)").items(&labels).interact().map_err(cancelled)?;
            Ok(picked.into_iter().map(|i| &EXTENSIONS[i]).collect())
        }
        (None, None) => Ok(vec![]),
    }
}

/// Combines the flags, the questions and the defaults.
pub fn resolve(req: &Request, host: Host, env: &Env, name: String) -> Result<Plan> {
    let prompt = (env.interactive && !req.yes).then(ColorfulTheme::default);
    let tool_flag = req.tool.map(parse_tool).transpose()?;
    let lang = resolve_lang(req, host, &prompt, tool_flag)?;
    let tool = resolve_tool(tool_flag, lang, env, &prompt)?;
    let db = resolve_db(req, &prompt)?;
    let url = resolve_url(req, db, &name, &prompt)?;
    let ext = resolve_ext(req, &prompt)?;
    Ok(Plan { name, lang, tool, db, url, ext })
}

impl Plan {
    fn concrete_models(&self) -> Vec<&'static str> {
        let mut names = vec!["Post", "User"];
        names.extend(self.ext.iter().filter_map(|e| e.model).filter(|m| m.stored).map(|m| m.name));
        names.sort_unstable();
        names
    }

    /// The example schema: `User` and `Post`, and one small use of each picked extension.
    pub fn schema(&self) -> String {
        let provider = if self.db == Db::Sqlite { "sqlite" } else { "postgresql" };
        let user_attrs: String = self.ext.iter().map(|e| e.user_attrs).collect();
        let post_attrs: String = self.ext.iter().map(|e| e.post_attrs).collect();
        let mut out = format!(
            "datasource db {{\n  provider = \"{provider}\"\n}}\n\n\
             model User {{\n  id    Int    @id @default(autoincrement())\n  name  String\n  posts Post[]\n{user_attrs}}}\n\n\
             model Post {{\n  id        Int     @id @default(autoincrement())\n  title     String\n  published Boolean\n  author_id Int\n  author    User    @relation(fields: [author_id], references: [id])\n{post_attrs}}}\n"
        );
        for model in self.ext.iter().filter_map(|e| e.model) {
            out.push('\n');
            out.push_str(model.text);
        }
        out
    }

    /// The frozen model ids that an extension needs. A model without a table has no id.
    fn identities(&self) -> Option<String> {
        if !self.ext.iter().any(|e| e.needs_identities) { return None; }
        let entries: Vec<_> = self.concrete_models().iter().enumerate()
            .map(|(i, m)| serde_json::json!({"id": i + 1, "model": m, "retired": false})).collect();
        let mut text = serde_json::to_string_pretty(&serde_json::json!({"version": 1, "entries": entries})).expect("a JSON value prints");
        text.push('\n');
        Some(text)
    }

    fn script(&self) -> String {
        let template = if self.lang == Lang::Python { MAIN_PY } else { INDEX_TS };
        template.replace("{{URL}}", &self.url)
    }

    fn package_file(&self) -> (&'static str, String) {
        match self.lang {
            Lang::Python => ("pyproject.toml", format!(
                "[project]\nname = \"{}\"\nversion = \"0.1.0\"\nrequires-python = \">=3.11\"\ndependencies = [\"monorm>={VERSION}\"]\n", self.name)),
            Lang::TypeScript => {
                let start = if self.tool == Tool::Bun { "bun index.ts" } else { "node index.ts" };
                let json = serde_json::json!({
                    "name": self.name,
                    "private": true,
                    "type": "module",
                    "scripts": {"start": start},
                    "dependencies": {"monorm": format!("npm:@ufstack/monorm@^{VERSION}")},
                });
                ("package.json", serde_json::to_string_pretty(&json).expect("a JSON value prints") + "\n")
            }
        }
    }

    fn gitignore(&self) -> &'static str {
        match self.lang {
            Lang::Python => ".venv/\n__pycache__/\n*.db\n",
            Lang::TypeScript => "node_modules/\n*.db\n",
        }
    }

    /// The commands that run the project, one for each line.
    pub fn next_commands(&self) -> Vec<String> {
        let tool = self.tool.name();
        match self.tool {
            Tool::Uv => vec!["uv run main.py".into()],
            Tool::Pip => vec![
                "python -m venv .venv".into(),
                if cfg!(windows) { ".venv\\Scripts\\activate".into() } else { ". .venv/bin/activate".into() },
                "pip install monorm".into(),
                "python main.py".into(),
            ],
            Tool::Npm | Tool::Pnpm | Tool::Yarn | Tool::Bun => vec![format!("{tool} install"), format!("{tool} start")],
        }
    }

    fn readme(&self) -> String {
        let script = if self.lang == Lang::Python { "main.py" } else { "index.ts" };
        let mut out = format!(
            "# {}\n\nA MonOrm project.\n`schema.prisma` holds the models.\n`{script}` creates the tables, writes two posts and reads them back.\n\n## Run\n\n```bash\n{}\n```\n",
            self.name, self.next_commands().join("\n"));
        if self.lang == Lang::TypeScript {
            out.push_str("\nThe `start` script needs Node 22.18 or later, or Bun.\n");
        }
        match self.db {
            Db::Sqlite => out.push_str("\nThe database is the file `dev.db`.\nSet `MONORM_DATABASE_URL` to use another database.\n"),
            Db::Postgres => out.push_str(&format!("\nThe database must exist before the first run.\nThe script uses `{}`.\nSet `MONORM_DATABASE_URL` to change it.\n", self.url)),
        }
        if !self.ext.is_empty() {
            let names: Vec<_> = self.ext.iter().map(|e| e.name).collect();
            out.push_str(&format!("\nThe schema uses these built-in extensions: {}.\n", names.join(", ")));
        }
        if self.identities().is_some() {
            out.push_str("\n`schema.identities.json` holds the frozen model ids that `generic` needs.\nAfter you add or rename a model, run `monorm identities`.\n");
        }
        out
    }

    /// Every file of the project: (path, text).
    pub fn files(&self) -> Vec<(&'static str, String)> {
        let script = if self.lang == Lang::Python { "main.py" } else { "index.ts" };
        let mut files = vec![
            ("schema.prisma", self.schema()),
            (script, self.script()),
            self.package_file(),
            (".gitignore", self.gitignore().to_owned()),
            ("README.md", self.readme()),
        ];
        if let Some(text) = self.identities() {
            files.push(("schema.identities.json", text));
        }
        files
    }
}

/// Refuses a directory with files in it.
fn ensure_empty(target: &str, dir: &Path) -> Result<()> {
    if dir.exists() {
        let mut entries = std::fs::read_dir(dir).map_err(|_| crate::failed(format!("{target} is not a directory.")))?;
        if entries.next().is_some() {
            return Err(crate::failed(format!("{target} is not empty.")));
        }
    }
    Ok(())
}

pub fn run(req: &Request, host: Host, env: &Env) -> Result<i32> {
    let target = req.dir.unwrap_or(".");
    let dir = Path::new(target);
    ensure_empty(target, dir)?;
    let plan = resolve(req, host, env, project_name(dir))?;
    for (path, text) in plan.files() {
        crate::write(&dir.join(path), &text)?;
    }
    let mut next = plan.next_commands();
    if target != "." {
        next.insert(0, format!("cd {target}"));
    }
    println!("Created {}.\n\n  {}", if target == "." { "the project" } else { target }, next.join("\n  "));
    Ok(0)
}

#[cfg(test)]
mod tests;
