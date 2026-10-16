//! Code generation from the schema, one module per language.
#[cfg(feature = "generate-rust")]
pub mod rust;
#[cfg(feature = "generate-python")]
pub mod python;
#[cfg(feature = "generate-typescript")]
pub mod typescript;

/// Generator options that are not part of the schema.
#[derive(Debug, Default, Clone)]
pub struct Options {
    /// The user's query-set class per model: Python `module:Class`, TypeScript
    /// `specifier#Export`.
    pub query_sets: std::collections::BTreeMap<String, String>,
}

impl Options {
    /// The query sets split at `sep` into (module, class), checked against the schema.
    pub fn query_sets(&self, schema: &crate::schema::Schema, sep: char) -> Result<std::collections::BTreeMap<&str, (&str, &str)>, String> {
        let mut out = std::collections::BTreeMap::new();
        for (model, target) in &self.query_sets {
            if !schema.models.iter().any(|m| m.ir.name == *model) {
                return Err(format!("--query-set {model}: the schema has no model {model}"));
            }
            let parts = if sep == '#' { target.rsplit_once(sep) } else { target.split_once(sep) }
                .filter(|(m, c)| !m.is_empty() && if sep == ':' { is_python_ident(c) } else { is_js_ident(c) });
            let Some((module, class)) = parts else {
                return Err(format!("--query-set {model}={target}: expected {}", if sep == ':' { "module:Class" } else { "specifier#Export" }));
            };
            if sep == ':' && !module.split('.').all(is_python_ident) {
                return Err(format!("--query-set {model}={target}: {module} is not an absolute module name"));
            }
            out.insert(model.as_str(), (module, class));
        }
        Ok(out)
    }
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c == '_' || c.is_ascii_alphabetic()) && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

fn is_python_ident(s: &str) -> bool {
    is_ident(s) && !matches!(s,
        "False" | "None" | "True" | "and" | "as" | "assert" | "async" | "await" |
        "break" | "class" | "continue" | "def" | "del" | "elif" | "else" | "except" |
        "finally" | "for" | "from" | "global" | "if" | "import" | "in" | "is" |
        "lambda" | "nonlocal" | "not" | "or" | "pass" | "raise" | "return" | "try" |
        "while" | "with" | "yield")
}

fn is_js_ident(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c == '$' || c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '$' || c == '_' || c.is_ascii_alphanumeric())
}

/// Compact schema payload for self-contained generated modules in every language.
/// Compilation and validation stay in Rust; runtimes receive the same schema IR.
pub fn embedded_schema_json(ir: &crate::ir::SchemaIr) -> Result<String, String> {
    serde_json::to_string(ir).map_err(|e| e.to_string())
}

/// A prepared default filter on `target` can hide it, so a reference to it can be absent.
#[cfg(feature = "reference-loading")]
pub fn reference_target_filtered(ir: &crate::ir::SchemaIr, target: &str) -> bool {
    let behavior = serde_json::to_value(&ir.behavior).expect("serializable schema behavior");
    behavior.get("query_defaults").and_then(serde_json::Value::as_array).is_some_and(|policies| {
        policies.iter().any(|policy| policy.get("model").and_then(serde_json::Value::as_str) == Some(target)
            && policy.get("filter").is_some_and(|filter| !filter.is_null()))
    })
}

#[cfg(test)]
mod reference_tests {
    use super::*;
    fn schema() -> (crate::ir::SchemaIr, crate::schema::Schema) {
        let ir = crate::dsl::compile("model User {\n id Int @id\n posts Post[]\n}\nmodel Post {\n id Int @id\n author_id Int?\n author User? @relation(fields: [author_id], references: [id])\n}", None).unwrap();
        let schema = crate::schema::Schema::from_ir(serde_json::from_value(serde_json::to_value(&ir).unwrap()).unwrap()).unwrap();
        (ir, schema)
    }
    #[test]
    fn loader_generation_matches_artifact_selection() {
        let (ir, schema) = schema();
        let py = python::generate(&ir, &schema, "schema.prisma").unwrap();
        let ts = typescript::generate(&ir, &schema, "schema.prisma", "orm").unwrap();
        if cfg!(feature = "reference-loading") {
            assert!(!py.stub.contains("async def load_author"));
            assert!(!ts.contains("loadAuthor("));
            assert!(py.module.contains("required_capabilities=(\"reference-loading\",)"));
            assert!(ts.contains("requiredCapabilities: [\"reference-loading\"]"));
        } else {
            assert!(!py.stub.contains("load_author"));
            assert!(!ts.contains("loadAuthor"));
            assert!(!py.module.contains("reference-loading"));
            assert!(!ts.contains("reference-loading"));
        }
        assert!(!py.stub.contains("load_posts"));
        assert!(!ts.contains("loadPosts"));
    }
    #[cfg(feature = "reference-loading")]
    #[test]
    fn legacy_loader_names_are_available_to_fields() {
        // Declared in the source, so a composition build also captures its physical column.
        let ir = crate::dsl::compile("model User {\n id Int @id\n posts Post[]\n}\nmodel Post {\n id Int @id\n load_author Int\n author_id Int?\n author User? @relation(fields: [author_id], references: [id])\n}", None).unwrap();
        let schema = crate::schema::Schema::from_ir(serde_json::from_value(serde_json::to_value(&ir).unwrap()).unwrap()).unwrap();
        assert!(python::generate(&ir, &schema, "schema.prisma").unwrap().stub.contains("load_author: f.Integer"));
        assert!(typescript::generate(&ir, &schema, "schema.prisma", "orm").unwrap().contains("readonly loadAuthor: number"));
    }

    #[test]
    fn query_sets_reach_both_generators() {
        let (ir, schema) = schema();
        let mut options = Options::default();
        options.query_sets.insert("Post".into(), "app.queries:PostQueries".into());
        let py = python::generate_with(&ir, &schema, "schema.prisma", &options).unwrap();
        assert!(py.module.contains("use_query_set(Post, \"app.queries:PostQueries\")"));
        assert!(py.stub.contains("from app.queries import PostQueries as _PostObjects") && py.stub.contains("objects: ClassVar[_PostObjects]"));
        options.query_sets.insert("Post".into(), "./queries.js#PostQueries".into());
        let ts = typescript::generate_with(&ir, &schema, "schema.prisma", "orm", &options).unwrap();
        assert!(ts.contains("import * as _q0 from \"./queries.js\";") && ts.contains("useQuerySet(Post, () => _q0.PostQueries);"));
        assert!(ts.contains("readonly queries: _q0.PostQueries;") && ts.contains("RelatedSet<PostSpec, \"authorId\" | \"author\"> & QueriesOf<PostSpec>"));
        assert!(python::generate_with(&ir, &schema, "s", &options).err().unwrap().contains("module:Class"));
        options.query_sets = [("Nope".to_string(), "a:B".to_string())].into();
        assert!(python::generate_with(&ir, &schema, "s", &options).err().unwrap().contains("no model Nope"));
    }

    #[test]
    fn python_query_sets_reject_hard_keywords() {
        let (ir, schema) = schema();
        for target in ["None:Q", "app.class:Q", "app:class", "app:False", "async:Q"] {
            let options = Options { query_sets: [("Post".into(), target.into())].into() };
            assert!(python::generate_with(&ir, &schema, "s", &options).is_err(), "{target}");
        }
    }

    #[test]
    fn python_query_sets_accept_soft_keywords() {
        let (ir, schema) = schema();
        for name in ["match", "case", "type", "_"] {
            let options = Options { query_sets: [("Post".into(), format!("app.{name}:{name}"))].into() };
            let py = python::generate_with(&ir, &schema, "s", &options).unwrap();
            assert!(py.stub.contains(&format!("from app.{name} import {name} as _PostObjects")));
        }
    }

    #[test]
    fn typescript_query_sets_accept_subpaths_and_dollar_exports() {
        let (ir, schema) = schema();
        for (module, export) in [("#app/queries", "Q"), ("./queries.js", "$Q$"), ("#app/queries", "default")] {
            let options = Options { query_sets: [("Post".into(), format!("{module}#{export}"))].into() };
            let ts = typescript::generate_with(&ir, &schema, "s", "orm", &options).unwrap();
            assert!(ts.contains(&format!("import * as _q0 from \"{module}\";")));
            assert!(ts.contains(&format!("useQuerySet(Post, () => _q0.{export});")));
            assert!(ts.contains(&format!("readonly queries: _q0.{export};")));
        }
        for target in ["#Q", "app#", "app#1Q", "app#bad-name"] {
            let options = Options { query_sets: [("Post".into(), target.into())].into() };
            assert!(typescript::generate_with(&ir, &schema, "s", "orm", &options).is_err());
        }
    }

    #[test]
    fn client_defaults_make_insert_fields_optional() {
        let ir = crate::dsl::compile("model A {\n id String @id @client_default(uuid())\n name String\n}", None).unwrap();
        let schema = crate::schema::Schema::from_ir(serde_json::from_value(serde_json::to_value(&ir).unwrap()).unwrap()).unwrap();
        let py = python::generate(&ir, &schema, "schema.prisma").unwrap().stub;
        assert!(py.contains("    id: NotRequired[str]\n    name: str\n"), "{py}");
        let ts = typescript::generate(&ir, &schema, "schema.prisma", "orm").unwrap();
        assert!(ts.contains("  id?: In<string>;\n  name: In<string>;\n"), "{ts}");
    }
}
