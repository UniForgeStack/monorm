//! Where the classes named by `@@query_set` and `@@model_methods` live.
//! The schema file sits in the package of its code, so the attribute gives the class name only.
use std::path::{Path, PathBuf};

use crate::dsl::SchemaUnit;
use crate::ir::SchemaIr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    Python,
    TypeScript,
}

const PROJECT_FILES: [&str; 3] = ["pyproject.toml", "setup.py", "setup.cfg"];

/// The two kinds of class a model attribute names; each has its own module next to the schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClassKind {
    QuerySet,
    ModelMethods,
}

impl ClassKind {
    pub const ALL: [ClassKind; 2] = [ClassKind::QuerySet, ClassKind::ModelMethods];

    /// The module next to the schema that holds the class.
    pub fn module(self) -> &'static str {
        match self {
            ClassKind::QuerySet => "queries",
            ClassKind::ModelMethods => "methods",
        }
    }

    /// The model attribute that names the class, without `@@`.
    pub fn attribute(self) -> &'static str {
        match self {
            ClassKind::QuerySet => "query_set",
            ClassKind::ModelMethods => "model_methods",
        }
    }

    /// The command-line flag that replaces the schema's class.
    pub fn flag(self) -> &'static str {
        match self {
            ClassKind::QuerySet => "--query-set",
            ClassKind::ModelMethods => "--model-methods",
        }
    }
}

/// The import target of `class` in the module of `kind` next to `schema`:
/// Python `pkg.queries:Class`, TypeScript `pkg/queries.js#Class`.
/// `out_dir` is the directory of the generated models file.
pub fn target(language: Language, schema: &Path, out_dir: &Path, kind: ClassKind, class: &str) -> Result<String, String> {
    let dir = std::path::absolute(schema).map_err(|e| format!("{}: {e}", schema.display()))?.parent().map(Path::to_path_buf).unwrap_or_default();
    let module = kind.module();
    match language {
        Language::Python => python(&dir, module, class),
        Language::TypeScript => {
            let out = std::path::absolute(out_dir).map_err(|e| format!("{}: {e}", out_dir.display()))?;
            typescript(&dir, &out, module, class)
        }
    }
}

/// The directory Python imports the schema's package from: the nearest ancestor with a project file
/// (`src/` under it when the schema is there), or `site-packages` / `dist-packages` for an installed package.
fn import_root(dir: &Path) -> Option<PathBuf> {
    for at in dir.ancestors() {
        if matches!(at.file_name().and_then(|n| n.to_str()), Some("site-packages" | "dist-packages")) {
            return Some(at.to_path_buf());
        }
        if PROJECT_FILES.iter().any(|f| at.join(f).is_file()) {
            let src = at.join("src");
            return Some(if dir.starts_with(&src) { src } else { at.to_path_buf() });
        }
    }
    None
}

fn python(dir: &Path, module: &str, class: &str) -> Result<String, String> {
    let Some(root) = import_root(dir) else {
        return Err(format!(
            "{}: no pyproject.toml, setup.py, setup.cfg or site-packages above, so the module of {module}:{class} is unknown; give the full path with --query-set / --model-methods",
            dir.display()
        ));
    };
    let parts: Vec<String> = dir.strip_prefix(&root).unwrap_or(dir).components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    if let Some(bad) = parts.iter().find(|p| !super::is_python_ident(p)) {
        return Err(format!("{}: the directory {bad} is not a module name; give the full path with --query-set / --model-methods", dir.display()));
    }
    Ok(if parts.is_empty() { format!("{module}:{class}") } else { format!("{}.{module}:{class}", parts.join(".")) })
}

fn nearest_package(dir: &Path) -> Option<&Path> {
    dir.ancestors().find(|d| d.join("package.json").is_file())
}

fn typescript(dir: &Path, out: &Path, module: &str, class: &str) -> Result<String, String> {
    // the consumer's own schema shares the package of the models file; a package name does not reach into its `src/`
    let installed = dir.components().any(|c| c.as_os_str() == "node_modules");
    let package = nearest_package(dir).filter(|p| installed || !out.starts_with(p));
    let Some(package) = package else {
        return Ok(format!("{}/{module}.js#{class}", relative(out, dir)));
    };
    let file = package.join("package.json");
    let text = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", file.display()))?;
    let name = json.get("name").and_then(|n| n.as_str()).filter(|n| !n.is_empty())
        .ok_or_else(|| format!("{}: no \"name\"; give the full path with --query-set / --model-methods", file.display()))?;
    Ok(format!("{name}/{module}.js#{class}"))
}

/// `to` as a specifier relative to `from`: `.`, `./a`, `../b`.
pub fn relative(from: &Path, to: &Path) -> String {
    let (from, to): (Vec<_>, Vec<_>) = (from.components().collect(), to.components().collect());
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> = if common == from.len() { vec![".".into()] } else { from[common..].iter().map(|_| "..".to_owned()).collect() };
    parts.extend(to[common..].iter().map(|c| c.as_os_str().to_string_lossy().into_owned()));
    parts.join("/")
}

impl super::Options {
    /// Adds the classes the schema names with `@@query_set` and `@@model_methods`.
    /// A flag or a configured entry for the same model replaces the schema's.
    /// The classes are in the package of the schema file that declared them (`classes_from`, else the model itself).
    pub fn with_schema_classes(&mut self, ir: &SchemaIr, units: &[SchemaUnit], language: Language, out_dir: &Path) -> Result<(), String> {
        for model in &ir.models {
            for kind in ClassKind::ALL {
                let Some(class) = (match kind {
                    ClassKind::QuerySet => &model.query_set,
                    ClassKind::ModelMethods => &model.model_methods,
                }) else { continue };
                let valid = if language == Language::Python { super::is_python_ident(class) } else { super::is_js_ident(class) };
                if !valid {
                    return Err(format!("{}: @@{}(\"{class}\") is not a class name", model.name, kind.attribute()));
                }
                let map = match kind {
                    ClassKind::QuerySet => &mut self.query_sets,
                    ClassKind::ModelMethods => &mut self.model_methods,
                };
                if map.contains_key(&model.name) {
                    continue;
                }
                let owner = model.classes_from.as_deref().unwrap_or(&model.name);
                let unit = units.iter().find(|u| u.models.iter().any(|m| m == owner))
                    .ok_or_else(|| format!("{}: @@{}(\"{class}\") comes from {owner}, which no schema file declares", model.name, kind.attribute()))?;
                map.insert(model.name.clone(), target(language, &unit.path, out_dir, kind, class)?);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("monorm-lookup-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn touch(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    const Q: ClassKind = ClassKind::QuerySet;
    const M: ClassKind = ClassKind::ModelMethods;

    #[test]
    fn python_package_path_starts_at_the_project_file_directory() {
        let root = dir("py-package");
        touch(&root.join("pyproject.toml"), "");
        touch(&root.join("monorm_auth/__init__.py"), "");
        touch(&root.join("monorm_auth/db/__init__.py"), "");
        let schema = root.join("monorm_auth/db/schema.prisma");
        assert_eq!(target(Language::Python, &schema, &root, Q, "UserQueries").unwrap(), "monorm_auth.db.queries:UserQueries");
        assert_eq!(target(Language::Python, &schema, &root, M, "UserMethods").unwrap(), "monorm_auth.db.methods:UserMethods");
    }

    #[test]
    fn python_schema_next_to_the_project_file_gives_the_top_level_module() {
        let root = dir("py-plain");
        touch(&root.join("pyproject.toml"), "");
        assert_eq!(target(Language::Python, &root.join("schema.prisma"), &root, Q, "UserQueries").unwrap(), "queries:UserQueries");
    }

    #[test]
    fn python_namespace_package_is_the_path_from_src() {
        let root = dir("py-namespace");
        touch(&root.join("pyproject.toml"), "");
        touch(&root.join("src/acme/auth/__init__.py"), "");
        let schema = root.join("src/acme/auth/schema.prisma");
        assert_eq!(target(Language::Python, &schema, &root, Q, "UserQueries").unwrap(), "acme.auth.queries:UserQueries");
        touch(&root.join("lib/acme/schema.prisma"), "");
        assert_eq!(target(Language::Python, &root.join("lib/acme/schema.prisma"), &root, M, "UserMethods").unwrap(), "lib.acme.methods:UserMethods");
    }

    #[test]
    fn python_schema_in_a_plain_subdirectory_is_the_path_from_the_project_root() {
        let root = dir("py-subdir");
        touch(&root.join("pyproject.toml"), "");
        assert_eq!(target(Language::Python, &root.join("db/schema.prisma"), &root, Q, "UserQueries").unwrap(), "db.queries:UserQueries");
    }

    #[test]
    fn python_installed_package_is_the_path_from_site_packages() {
        let root = dir("py-installed");
        touch(&root.join("pyproject.toml"), "");
        let schema = root.join(".venv/lib/python3.14/site-packages/orm_auth/schema.prisma");
        assert_eq!(target(Language::Python, &schema, &root, Q, "UserQueries").unwrap(), "monorm_auth.queries:UserQueries");
    }

    #[test]
    fn python_without_an_import_root_asks_for_the_full_path() {
        let root = dir("py-no-root");
        let error = target(Language::Python, &root.join("db/schema.prisma"), &root, Q, "UserQueries").unwrap_err();
        assert!(error.contains("queries:UserQueries") && error.contains("--query-set / --model-methods"), "{error}");
    }

    #[test]
    fn python_directory_that_is_not_a_module_name_is_an_error() {
        let root = dir("py-bad-name");
        touch(&root.join("pyproject.toml"), "");
        let error = target(Language::Python, &root.join("my-app/schema.prisma"), &root, Q, "UserQueries").unwrap_err();
        assert!(error.contains("my-app is not a module name"), "{error}");
    }

    #[test]
    fn typescript_reads_the_package_name() {
        let root = dir("ts-package");
        touch(&root.join("app/package.json"), r#"{"name":"app"}"#);
        touch(&root.join("app/node_modules/orm-auth/package.json"), r#"{"name":"monorm-auth"}"#);
        let schema = root.join("app/node_modules/orm-auth/schema.prisma");
        assert_eq!(target(Language::TypeScript, &schema, &root.join("app/src"), Q, "UserQueries").unwrap(), "monorm-auth/queries.js#UserQueries");
        assert_eq!(target(Language::TypeScript, &schema, &root.join("app/src"), M, "UserMethods").unwrap(), "monorm-auth/methods.js#UserMethods");
    }

    #[test]
    fn typescript_schema_outside_a_package_is_relative_to_the_models_file() {
        let root = dir("ts-loose");
        let schema = root.join("db/schema.prisma");
        assert_eq!(target(Language::TypeScript, &schema, &root.join("db"), Q, "UserQueries").unwrap(), "./queries.js#UserQueries");
        assert_eq!(target(Language::TypeScript, &schema, &root.join("src/gen"), Q, "UserQueries").unwrap(), "../../db/queries.js#UserQueries");
        assert_eq!(target(Language::TypeScript, &schema, &root, M, "UserMethods").unwrap(), "./db/methods.js#UserMethods");
    }

    #[test]
    fn typescript_own_schema_in_a_named_package_is_relative_to_the_models_file() {
        let root = dir("ts-own");
        touch(&root.join("app/package.json"), r#"{"name":"myapp"}"#);
        let schema = root.join("app/src/db/schema.prisma");
        assert_eq!(target(Language::TypeScript, &schema, &root.join("app/src/db"), Q, "UserQueries").unwrap(), "./queries.js#UserQueries");
        assert_eq!(target(Language::TypeScript, &schema, &root.join("app/src/gen"), M, "UserMethods").unwrap(), "../db/methods.js#UserMethods");
    }

    #[test]
    fn typescript_package_without_a_name_asks_for_the_full_path() {
        let root = dir("ts-nameless");
        touch(&root.join("app/package.json"), r#"{"name":"app"}"#);
        touch(&root.join("lib/package.json"), "{}");
        let error = target(Language::TypeScript, &root.join("lib/schema.prisma"), &root.join("app"), Q, "UserQueries").unwrap_err();
        assert!(error.contains("no \"name\"") && error.contains("--query-set"), "{error}");
    }

    #[test]
    fn the_classes_come_from_the_unit_that_declared_them() {
        let root = dir("from-unit");
        touch(&root.join("pyproject.toml"), "");
        let unit = |path: &str, model: &str| SchemaUnit { app: None, path: root.join(path), models: vec![model.into()], enums: vec![], types: vec![] };
        let units = [unit("monorm_auth/schema.prisma", "AbstractUser"), unit("app/schema.prisma", "User")];
        let mut ir = crate::dsl::compile("model AbstractUser {\n id Int @id\n @@query_set(\"UserQueries\")\n}\nmodel User {\n id Int @id\n}", None).unwrap();
        ir.models[1].query_set = Some("UserQueries".into());
        ir.models[1].classes_from = Some("AbstractUser".into());
        let mut options = crate::codegen::Options::default();
        options.with_schema_classes(&ir, &units, Language::Python, &root).unwrap();
        assert_eq!(options.query_sets["User"], "monorm_auth.queries:UserQueries");
        assert_eq!(options.query_sets["AbstractUser"], "monorm_auth.queries:UserQueries");
        ir.models[1].classes_from = Some("Gone".into());
        let error = crate::codegen::Options::default().with_schema_classes(&ir, &units, Language::Python, &root).unwrap_err();
        assert!(error.contains("User: @@query_set") && error.contains("Gone"), "{error}");
        let error = crate::codegen::Options::default().with_schema_classes(&ir, &[], Language::Python, &root).unwrap_err();
        assert!(error.contains("no schema file declares"), "{error}");
    }

    #[test]
    fn mixed_own_and_inherited_classes_resolve_in_the_child_package() {
        let root = dir("mixed-classes");
        touch(&root.join("pyproject.toml"), "");
        touch(&root.join("base/package.json"), r#"{"name":"base"}"#);
        touch(&root.join("child/package.json"), r#"{"name":"child"}"#);
        let unit = |name: &str, model: &str| SchemaUnit { app: None, path: root.join(format!("{name}/schema.prisma")), models: vec![model.into()], enums: vec![], types: vec![] };
        let units = [unit("base", "Base"), unit("child", "Child")];
        // This is the IR produced by inherit when either class is declared on Child:
        // it copies the other name, leaving classes_from empty.
        for own_query_set in [true, false] {
            let mut ir = crate::dsl::compile("model Child {\n id Int @id\n}\n", None).unwrap();
            let child = &mut ir.models[0];
            child.query_set = Some(if own_query_set { "OwnQueries" } else { "BaseQueries" }.into());
            child.model_methods = Some(if own_query_set { "BaseMethods" } else { "OwnMethods" }.into());
            assert!(child.classes_from.is_none());
            for language in [Language::Python, Language::TypeScript] {
                let mut options = crate::codegen::Options::default();
                options.with_schema_classes(&ir, &units, language, &root.join("child")).unwrap();
                let (queries, methods) = if own_query_set { ("OwnQueries", "BaseMethods") } else { ("BaseQueries", "OwnMethods") };
                match language {
                    Language::Python => {
                        assert_eq!(options.query_sets["Child"], format!("child.queries:{queries}"));
                        assert_eq!(options.model_methods["Child"], format!("child.methods:{methods}"));
                    }
                    Language::TypeScript => {
                        assert_eq!(options.query_sets["Child"], format!("./queries.js#{queries}"));
                        assert_eq!(options.model_methods["Child"], format!("./methods.js#{methods}"));
                    }
                }
            }
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
