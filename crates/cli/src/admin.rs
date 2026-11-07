//! `monorm admin init`: the admin settings, an admin module that registers every model, and the schema import.
//! Bare `monorm admin` starts the server. The server runs in the host (Python or Node), so the binary only explains that.

use std::path::Path;

use monorm_core::settings::{Settings, Source};

use crate::{failed, Failure, Host, Result};

const STANDALONE: &str = "monorm admin runs through Python or Node, because the admin module and the setup hook are your code: use `python -m monorm admin` or `npx monorm admin`";

/// What `monorm admin <sub>` needs from the command line.
pub struct Request<'a> {
    pub lang: Option<&'a str>,
    pub models: Option<&'a str>,
    pub schema: &'a Path,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Lang {
    Python,
    TypeScript,
}

impl Lang {
    fn parse(request: &Request, host: Host) -> Result<Lang> {
        match (request.lang, host) {
            (Some("python"), _) | (None, Host::Python) => Ok(Lang::Python),
            (Some("typescript"), _) | (None, Host::Node) => Ok(Lang::TypeScript),
            (None, Host::Binary) => Err(Failure::Usage("admin init needs --lang python|typescript".into())),
            (Some(other), _) => Err(Failure::Usage(format!("--lang is python or typescript, not {other}"))),
        }
    }

    fn module(self) -> &'static str {
        match self {
            Lang::Python => "admin.py",
            Lang::TypeScript => "admin.ts",
        }
    }

    /// The `app` setting: where `monorm admin` finds the `Admin` object.
    fn app(self) -> &'static str {
        match self {
            Lang::Python => "admin:admin",
            Lang::TypeScript => "./admin.ts:admin",
        }
    }

    /// The name of the admin app in `import "app:..."`.
    fn app_import(self) -> &'static str {
        match self {
            Lang::Python => "import \"app:monorm-admin\" (prefix: \"admin_\")",
            Lang::TypeScript => "import \"app:admin\" (prefix: \"admin_\")",
        }
    }
}

/// The failure of an admin command other than `init`.
pub fn other(sub: Option<&str>, host: Host) -> Failure {
    match sub {
        None if host == Host::Binary => Failure::Setup(STANDALONE.into()),
        // The host runs these before the CLI sees the command.
        None => Failure::Setup("monorm admin must start from the host entry point".into()),
        Some("sync-permissions") if host == Host::Binary => Failure::Setup(STANDALONE.into()),
        Some("sync-permissions") => Failure::Setup("monorm admin sync-permissions must start from the host entry point".into()),
        Some(other) => Failure::Usage(format!("unknown admin command {other}")),
    }
}

/// `load` compiles the schema; it runs after the checks that need no schema, so a second run fails first on what exists.
pub fn init(request: &Request, host: Host, load: impl FnOnce() -> Result<monorm_core::schema::Schema>) -> Result<i32> {
    let lang = Lang::parse(request, host)?;
    let settings = Settings::load(Path::new(".")).map_err(Failure::Setup)?;
    if settings.section("admin").is_some() {
        return Err(Failure::Setup("the settings already have an admin section".into()));
    }
    let module = Path::new(lang.module());
    if module.exists() {
        return Err(Failure::Setup(format!("{} exists", module.display())));
    }
    let schema = load()?;
    let models: Vec<&str> = schema.models.iter().filter(|m| !m.ir.table.is_empty() && m.view.is_none()).map(|m| m.ir.name.as_str()).collect();
    if models.is_empty() {
        return Err(Failure::Setup("the schema has no model to register".into()));
    }
    let schema_text = std::fs::read_to_string(request.schema).map_err(|e| failed(format!("{}: {e}", request.schema.display())))?;
    let settings_text = settings_file(&settings, lang)?;
    let import = (!schema_text.contains("\"app:monorm-admin\"") && !schema_text.contains("\"app:admin\"")).then(|| lang.app_import());

    crate::write(module, &module_text(lang, request, &models))?;
    crate::write(Path::new(settings_text.0), &settings_text.1)?;
    if let Some(import) = import {
        let sep = if schema_text.ends_with('\n') || schema_text.is_empty() { "" } else { "\n" };
        crate::write(request.schema, &format!("{schema_text}{sep}\n{import}\n"))?;
    }
    println!("wrote {}, the admin settings in {}, and the admin import in {}", module.display(), settings_text.0, request.schema.display());
    let generate = if lang == Lang::Python && request.models.is_none() { "generate, " } else { "" };
    println!("next: {generate}makemigrations, migrate, then `{} admin`", host.prog());
    Ok(0)
}

/// The settings file with the `admin` section added, as (file, text). No settings file: a new `monorm.toml`.
fn settings_file(settings: &Settings, lang: Lang) -> Result<(&'static str, String)> {
    let app = lang.app();
    let Some(source) = settings.source() else {
        return Ok(("monorm.toml", format!("[admin]\napp = \"{app}\"\n")));
    };
    let file = source.file();
    let text = std::fs::read_to_string(file).map_err(|e| failed(format!("{file}: {e}")))?;
    let sep = if text.ends_with('\n') { "" } else { "\n" };
    let edited = match source {
        Source::MonormToml => format!("{text}{sep}\n[admin]\napp = \"{app}\"\n"),
        Source::Pyproject => format!("{text}{sep}\n[tool.monorm.admin]\napp = \"{app}\"\n"),
        Source::PackageJson => insert_json(&text, app).ok_or_else(|| Failure::Setup(format!("package.json: cannot add \"admin\" to \"monorm\"; add {{\"app\": \"{app}\"}} by hand")))?,
    };
    Ok((file, edited))
}

/// Puts `"admin": {"app": ...}` first in the `"monorm"` object. The text edit keeps the other keys in their order.
fn insert_json(text: &str, app: &str) -> Option<String> {
    let mut from = 0;
    while let Some(at) = text[from..].find("\"monorm\"") {
        let key_end = from + at + "\"monorm\"".len();
        let rest = text[key_end..].trim_start();
        if let Some(after_colon) = rest.strip_prefix(':') {
            let body = after_colon.trim_start();
            if body.starts_with('{') {
                let open = text.len() - body.len();
                let inner = &text[open + 1..];
                let indent: String = inner.trim_start_matches(['\r', '\n']).chars().take_while(|c| *c == ' ' || *c == '\t').collect();
                let closes = inner.trim_start().starts_with('}');
                let entry = format!("\"admin\": {{\"app\": \"{app}\"}}");
                let edited = if closes { format!("{{ {entry} }}") } else { format!("{{\n{indent}{entry},") };
                let skip = if closes { inner.find('}').map(|i| i + 1)? } else { 0 };
                let result = format!("{}{}{}", &text[..open], edited, &text[open + 1 + skip..]);
                return serde_json::from_str::<serde_json::Value>(&result).ok().filter(|v| v["monorm"]["admin"]["app"] == app).map(|_| result);
            }
        }
        from = key_end;
    }
    None
}

fn module_text(lang: Lang, request: &Request, models: &[&str]) -> String {
    let schema = request.schema.to_string_lossy().replace('\\', "/");
    let spec = if request.schema.is_absolute() { schema.clone() } else { format!("./{schema}") };
    match (lang, request.models) {
        (Lang::Python, module) => {
            let module = module.unwrap_or("models");
            let mut out = format!("\"\"\"The admin of this project: `monorm admin` imports `admin` from here.\"\"\"\n\nfrom monorm.admin import Admin\nfrom {module} import {}\n\nadmin = Admin()\n", models.join(", "));
            for model in models {
                out.push_str(&format!("admin.register({model})\n"));
            }
            out
        }
        (Lang::TypeScript, None) => {
            let mut out = format!("import {{ fileURLToPath }} from \"node:url\";\nimport {{ load }} from \"monorm\";\nimport {{ Admin }} from \"monorm/admin\";\n\nconst models = load(fileURLToPath(new URL(\"{spec}\", import.meta.url)));\nexport const admin = new Admin();\n");
            for model in models {
                out.push_str(&format!("admin.register(models.{model});\n"));
            }
            out
        }
        (Lang::TypeScript, Some(module)) => {
            let mut out = format!("import {{ Admin }} from \"monorm/admin\";\nimport {{ {} }} from \"{module}\";\n\nexport const admin = new Admin();\n", models.join(", "));
            for model in models {
                out.push_str(&format!("admin.register({model});\n"));
            }
            out
        }
    }
}
