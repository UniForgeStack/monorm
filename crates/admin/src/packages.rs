//! Extension packages: a directory of static files (widgets, form modules and styles) that the admin loads next to its own UI.
//! A package is generic here. The host names it, the admin reads its files once and serves them, and it names no extension.
//!
//! A package widget is named `<package>.<widget>` in the `widgets` config and in the field metadata. `Site::build` reads only the
//! `admin_major` of the config and refuses a package for another major version; the browser checks the `adminMajor` that each
//! module exports, and shows a clear message for a module of another major version.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path as FsPath;

use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Extension;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::server::{authorize, reject, Shared, User};
use crate::error::{code, Fault};
use crate::site::{check_name, err_code};

/// The major version of the widget contract. A package built for another major version does not load.
pub const ADMIN_MAJOR: u32 = 1;

const MAX_FILES: usize = 256;
const MAX_FILE: usize = 1024 * 1024;
const MAX_TOTAL: usize = 8 * 1024 * 1024;

/// One package as the host sends it. Names in `widgets` and `forms` map to a file of `dir`; `styles` are files of `dir`.
#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct PackageConfig {
    pub name: String,
    pub admin_major: u32,
    pub dir: String,
    #[serde(default)]
    pub widgets: BTreeMap<String, String>,
    #[serde(default)]
    pub forms: BTreeMap<String, String>,
    /// Detail tabs: a name to a module that the detail page shows as a tab.
    #[serde(default)]
    pub tabs: BTreeMap<String, TabConfig>,
    #[serde(default)]
    pub styles: Vec<String>,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(untagged)]
pub enum TabConfig {
    Module(String),
    Options(TabOptions),
}

#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct TabOptions {
    module: String,
    #[serde(default)]
    embeddable: bool,
}

/// A loaded package: the declared names and every `.js`, `.mjs` and `.css` file of its directory, in memory.
#[derive(Debug, Clone)]
pub struct Package {
    pub name: String,
    pub admin_major: u32,
    /// Widget name to the path of its module under the prefix: `packages/<package>/<file>`.
    pub widgets: BTreeMap<String, String>,
    pub forms: BTreeMap<String, String>,
    pub tabs: BTreeMap<String, String>,
    pub styles: Vec<String>,
    files: BTreeMap<String, Vec<u8>>,
    embeddable: std::collections::BTreeSet<String>,
}

/// The packages of a site.
#[derive(Debug, Clone, Default)]
pub struct Packages(Vec<Package>);

fn walk(root: &FsPath, dir: &FsPath, out: &mut BTreeMap<String, Vec<u8>>, total: &mut usize, who: &str) -> Result<(), Fault> {
    let mut entries: Vec<_> = fs::read_dir(dir).map_err(|e| Fault::new(code::PACKAGE, format!("{who}: {}: {e}", dir.display())))?.filter_map(Result::ok).map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            walk(root, &p, out, total, who)?;
            continue;
        }
        if !matches!(p.extension().and_then(|e| e.to_str()), Some("js" | "mjs" | "css")) {
            continue;
        }
        // The size is known before the read, so a huge file never loads.
        let size = fs::metadata(&p).map_err(|e| Fault::new(code::PACKAGE, format!("{who}: {}: {e}", p.display())))?.len() as usize;
        *total += size;
        if size > MAX_FILE || *total > MAX_TOTAL || out.len() >= MAX_FILES {
            return err_code(code::PACKAGE, format!("{who}: too large: at most {MAX_FILES} files, {MAX_FILE} bytes each and {MAX_TOTAL} bytes in all"));
        }
        let bytes = fs::read(&p).map_err(|e| Fault::new(code::PACKAGE, format!("{who}: {}: {e}", p.display())))?;
        let rel = p.strip_prefix(root).unwrap_or(&p).to_string_lossy().replace('\\', "/");
        out.insert(rel, bytes);
    }
    Ok(())
}

/// A declared file name: a relative path with no `..`, found among the loaded files.
fn declared(files: &BTreeMap<String, Vec<u8>>, who: &str, what: &str, file: &str) -> Result<String, Fault> {
    let clean = file.trim_start_matches("./");
    if clean.split('/').any(|s| s == ".." || s.is_empty()) || !files.contains_key(clean) {
        return err_code(code::PACKAGE, format!("{who}: {what} names {file:?}, which is not a .js, .mjs or .css file under the package directory"));
    }
    Ok(clean.to_owned())
}

impl Packages {
    pub fn build(configs: &[PackageConfig]) -> Result<Packages, Fault> {
        let mut out: Vec<Package> = Vec::new();
        for c in configs {
            let who = format!("package {:?}", c.name);
            check_name("package name", &c.name)?;
            if out.iter().any(|p| p.name == c.name) {
                return err_code(code::PACKAGE, format!("{who}: registered twice"));
            }
            if c.admin_major != ADMIN_MAJOR {
                return err_code(code::PACKAGE, format!("{who} is built for admin major {}; this admin is major {ADMIN_MAJOR}", c.admin_major));
            }
            let root = FsPath::new(&c.dir);
            let mut files = BTreeMap::new();
            walk(root, root, &mut files, &mut 0, &who)?;
            let url = |file: &str| format!("packages/{}/{file}", c.name);
            let named = |kind: &str, map: &BTreeMap<String, String>| -> Result<BTreeMap<String, String>, Fault> {
                let mut out = BTreeMap::new();
                for (name, file) in map {
                    check_name(&format!("{kind} name"), name)?;
                    out.insert(name.clone(), url(&declared(&files, &who, &format!("{kind} {name:?}"), file)?));
                }
                Ok(out)
            };
            let (widgets, forms) = (named("widget", &c.widgets)?, named("form", &c.forms)?);
            let mut tabs = BTreeMap::new();
            let mut embeddable = std::collections::BTreeSet::new();
            for (name, config) in &c.tabs {
                check_name("tab name", name)?;
                let module = match config {
                    TabConfig::Module(module) => module,
                    TabConfig::Options(options) => {
                        if options.embeddable {
                            embeddable.insert(name.clone());
                        }
                        &options.module
                    }
                };
                tabs.insert(name.clone(), url(&declared(&files, &who, &format!("tab {name:?}"), module)?));
            }
            let mut styles = Vec::new();
            for file in &c.styles {
                let clean = declared(&files, &who, "styles", file)?;
                if !clean.ends_with(".css") {
                    return err_code(code::PACKAGE, format!("{who}: styles names {file:?}, which is not a .css file"));
                }
                styles.push(url(&clean));
            }
            out.push(Package { name: c.name.clone(), admin_major: c.admin_major, widgets, forms, tabs, styles, files, embeddable });
        }
        Ok(Packages(out))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The package that a name `<package>.<item>` leads to, and the item.
    fn split<'a>(&self, full: &'a str) -> Option<(&Package, &'a str)> {
        let (pkg, item) = full.split_once('.')?;
        Some((self.0.iter().find(|p| p.name == pkg)?, item))
    }

    pub fn has_widget(&self, full: &str) -> bool {
        self.split(full).is_some_and(|(p, w)| p.widgets.contains_key(w))
    }

    /// The module path of the form `<package>.<form>`.
    pub fn form(&self, full: &str) -> Option<&str> {
        let (p, f) = self.split(full)?;
        p.forms.get(f).map(String::as_str)
    }

    /// The module path of the detail tab `<package>.<tab>`.
    pub fn tab(&self, full: &str) -> Option<&str> {
        let (p, t) = self.split(full)?;
        p.tabs.get(t).map(String::as_str)
    }

    pub(crate) fn embeddable(&self, full: &str) -> bool {
        self.split(full).is_some_and(|(p, name)| p.embeddable.contains(name))
    }

    pub(crate) fn tab_package(&self, full: &str) -> Value {
        self.split(full).map_or(Value::Null, |(p, name)| {
            json!({"name": p.name, "admin_major": p.admin_major, "tabs": {name: p.tabs.get(name)}, "styles": p.styles})
        })
    }

    /// The `packages` entry of the resources answer.
    pub fn meta(&self) -> Value {
        Value::Array(self.0.iter().map(|p| json!({"name": p.name, "admin_major": p.admin_major, "widgets": p.widgets, "forms": p.forms, "tabs": p.tabs, "styles": p.styles})).collect())
    }
}

/// `GET <prefix>/packages/<package>/<file>`: a loaded file of a package, for a signed-in user. No other path reads the disk.
pub(crate) async fn serve(State(s): Shared, user: Option<Extension<User>>, Path((pkg, file)): Path<(String, String)>) -> Response {
    let (_, scope, _) = match authorize(&s, user).await {
        Ok(t) => t,
        Err(r) => return *r,
    };
    scope.rollback().await;
    let found = s.site.packages.0.iter().find(|p| p.name == pkg).and_then(|p| p.files.get(&file));
    let Some(bytes) = found else { return reject(StatusCode::NOT_FOUND, "NotFound", "no such package file") };
    let kind = if file.ends_with(".css") { "text/css; charset=utf-8" } else { "text/javascript; charset=utf-8" };
    (StatusCode::OK, [(header::CONTENT_TYPE, kind), (header::CACHE_CONTROL, "no-cache")], bytes.clone()).into_response()
}
