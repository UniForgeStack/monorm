//! Code generation from the schema, one module per language.
pub mod lookup;
#[cfg(feature = "generate-rust")]
pub mod rust;
#[cfg(feature = "generate-python")]
pub mod python;
#[cfg(feature = "generate-typescript")]
pub mod typescript;

#[cfg(any(feature = "generate-python", feature = "generate-typescript"))]
use crate::ir::{FieldIr, NestedStorage, SchemaIr, TypeFieldIr, TypeIr};
#[cfg(any(feature = "generate-python", feature = "generate-typescript"))]
use crate::schema::Model;

#[cfg(any(feature = "generate-python", feature = "generate-typescript"))]
/// A nested field of a model: the `columns` group of its leaf fields, or the one `json` field.
pub(super) struct Group<'a> {
    pub(super) name: &'a str,
    pub(super) ty: &'a str,
    pub(super) nullable: bool,
}

#[cfg(any(feature = "generate-python", feature = "generate-typescript"))]
/// The nested fields of `m`, in field order. Their leaf fields stay out of every public shape.
pub(super) fn groups<'a>(m: &'a Model) -> Vec<Group<'a>> {
    let mut out: Vec<Group<'a>> = vec![];
    for f in m.fields() {
        let Some(n) = &f.nested else { continue };
        if out.iter().any(|g| g.name == n.field) {
            continue;
        }
        let nullable = if n.storage == NestedStorage::Json { f.nullable } else { !n.root_required };
        out.push(Group { name: &n.field, ty: &n.ty, nullable });
    }
    out
}

#[cfg(any(feature = "generate-python", feature = "generate-typescript"))]
/// Whether the query-set class of `model` comes from a type model: the class takes the model (or its spec) as a type parameter.
pub(super) fn declares_generic(ir: &SchemaIr, query_sets: &std::collections::BTreeMap<&str, (&str, &str, bool)>, model: &str) -> bool {
    query_sets.get(model).is_some_and(|q| !q.2)
        && ir.models.iter().any(|m| m.name == model && m.classes_from.as_ref().is_some_and(|c| ir.behavior.type_models.iter().any(|t| &t.name == c)))
}

#[cfg(any(feature = "generate-python", feature = "generate-typescript"))]
pub(super) fn type_of<'a>(ir: &'a SchemaIr, name: &str) -> Option<&'a TypeIr> {
    ir.types.iter().find(|t| t.name == name)
}

#[cfg(any(feature = "generate-python", feature = "generate-typescript"))]
pub(super) fn sub_has_default(sf: &TypeFieldIr) -> bool {
    sf.leaf.as_ref().is_some_and(|l| has_server_value(l) || l.client_default.is_some())
}

#[cfg(any(feature = "generate-python", feature = "generate-typescript"))]
/// Whether a value of `name` needs at least one subfield from the caller.
pub(super) fn type_requires(ir: &SchemaIr, name: &str) -> bool {
    type_of(ir, name).is_some_and(|t| {
        t.fields.iter().any(|sf| {
            !sf.nullable && !sf.list && !sub_has_default(sf) && sf.type_name.as_deref().is_none_or(|n| type_requires(ir, n))
        })
    })
}

#[cfg(any(feature = "generate-python", feature = "generate-typescript"))]
pub(super) fn has_server_value(f: &FieldIr) -> bool {
    f.auto_increment || f.default.is_some() || f.default_now || f.default_sql.is_some() || f.hints.get("composition.key-default").is_some_and(|v| v == "true")
}

/// Generator options that are not part of the schema.
#[derive(Debug, Default, Clone)]
pub struct Options {
    /// The user's query-set class per model: Python `module:Class`, TypeScript
    /// `specifier#Export`.
    pub query_sets: std::collections::BTreeMap<String, String>,
    /// The user's instance-methods class per model, in the same form as `query_sets`.
    pub model_methods: std::collections::BTreeMap<String, String>,
    /// The language code of the artifact's extensions, dependencies first.
    pub extensions: Vec<ExtensionCode>,
    /// Where the generated models import the extension packages from: the dotted module of
    /// the extension directory in Python (`orm_ext`), the relative specifier in TypeScript
    /// (`./orm-ext`).
    pub ext_import: Option<String>,
}

/// The language code of one extension: what the artifact records and the files it embeds.
#[derive(Debug, Default, Clone)]
pub struct ExtensionCode {
    pub members: orm_contracts::extension::LanguageMembers,
    /// The attributes of the extension; a model that declares one gets the members.
    pub attributes: Vec<String>,
    /// Path relative to the extension's source directory, with the content.
    pub files: Vec<(String, String)>,
}

impl ExtensionCode {
    /// The Python package name of the extension inside the extension directory.
    pub fn python_package(&self) -> String {
        let name: String = self.members.extension.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect();
        if name.starts_with(|c: char| c.is_ascii_digit()) { format!("_{name}") } else { name }
    }

    /// The directory name of the extension inside the TypeScript extension directory.
    pub fn typescript_dir(&self) -> String {
        self.members.extension.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect()
    }

    /// Whether the model gets the members of this extension: it, or one of its fields,
    /// declares an attribute of the extension, or a record the extension prepared names it.
    /// A proxy declares nothing, but the extension prepares the records of its root for it.
    /// An extension that adds no query-set, instance or bound query-set member never applies.
    pub fn applies(&self, ir: &crate::ir::SchemaIr, model: &str) -> bool {
        let m = &self.members;
        let adds_nothing = m.query_set_members.is_empty() && m.instance_members.is_empty() && m.bound_query_set_members.is_empty();
        if adds_nothing {
            return false;
        }
        let id = m.extension.as_str();
        let b = &ir.behavior;
        b.declarations.iter().any(|d| d.model == model && self.attributes.contains(&d.attribute))
            || b.record_sources(model).any(|source| source == id)
    }

    /// The files to write below the extension directory: path, content with the header.
    /// The header is the first three lines; the runtime hashes the content after them.
    pub fn generated_files(&self, language: &str) -> Vec<(String, String)> {
        let (dir, comment) = if language == "python" { (self.python_package(), "#") } else { (self.typescript_dir(), "//") };
        let header = format!(
            "{comment} Generated by `orm generate` from extension {id}. Do not edit.\n{comment} orm-ext {id} {version} {hash}\n\n",
            id = self.members.extension, version = self.members.version, hash = self.members.hash,
        );
        self.files.iter().map(|(name, content)| (format!("{dir}/{name}"), format!("{header}{content}"))).collect()
    }
}

impl Options {
    /// The extensions of `model`, dependencies first.
    pub fn model_extensions(&self, ir: &crate::ir::SchemaIr, model: &str) -> Vec<(usize, &ExtensionCode)> {
        self.extensions.iter().enumerate().filter(|(_, e)| e.applies(ir, model)).collect()
    }
}

impl Options {
    /// The query sets split at `sep` into (module, class, inherited), checked against the schema.
    /// A proxy without a class of its own gets the class of its nearest ancestor that has one, and `inherited` is true.
    pub fn query_sets<'a>(&'a self, ir: &'a crate::ir::SchemaIr, schema: &crate::schema::Schema, sep: char) -> Result<std::collections::BTreeMap<&'a str, (&'a str, &'a str, bool)>, String> {
        let mut out: std::collections::BTreeMap<&str, (&str, &str, bool)> =
            split_targets(lookup::ClassKind::QuerySet.flag(), &self.query_sets, schema, sep)?.into_iter().map(|(m, (module, class))| (m, (module, class, false))).collect();
        for proxy in &ir.behavior.proxy_models {
            if out.contains_key(proxy.model.as_str()) { continue; }
            let ancestors = ir.behavior.ancestors(&proxy.model).unwrap_or_default();
            if let Some(&(module, class, _)) = ancestors.iter().find_map(|a| out.get(a)) {
                out.insert(proxy.model.as_str(), (module, class, true));
            }
        }
        Ok(out)
    }

    /// The instance-methods classes split at `sep` into (module, class), checked against the schema.
    /// A proxy without a class of its own gets the class of its nearest ancestor that has one.
    pub fn model_methods<'a>(&'a self, ir: &'a crate::ir::SchemaIr, schema: &crate::schema::Schema, sep: char) -> Result<std::collections::BTreeMap<&'a str, (&'a str, &'a str)>, String> {
        let mut out = split_targets(lookup::ClassKind::ModelMethods.flag(), &self.model_methods, schema, sep)?;
        for proxy in &ir.behavior.proxy_models {
            if out.contains_key(proxy.model.as_str()) { continue; }
            let ancestors = ir.behavior.ancestors(&proxy.model).unwrap_or_default();
            if let Some(&target) = ancestors.iter().find_map(|a| out.get(a)) {
                out.insert(proxy.model.as_str(), target);
            }
        }
        Ok(out)
    }
}

fn split_targets<'a>(flag: &str, targets: &'a std::collections::BTreeMap<String, String>, schema: &crate::schema::Schema, sep: char) -> Result<std::collections::BTreeMap<&'a str, (&'a str, &'a str)>, String> {
    let mut out = std::collections::BTreeMap::new();
    for (model, target) in targets {
        if !schema.models.iter().any(|m| m.ir.name == *model) {
            return Err(format!("{flag} {model}: the schema has no model {model}"));
        }
        let parts = if sep == '#' { target.rsplit_once(sep) } else { target.split_once(sep) }
            .filter(|(m, c)| !m.is_empty() && if sep == ':' { is_python_ident(c) } else { is_js_ident(c) });
        let Some((module, class)) = parts else {
            return Err(format!("{flag} {model}={target}: expected {}", if sep == ':' { "module:Class" } else { "specifier#Export" }));
        };
        if sep == ':' && !module.split('.').all(is_python_ident) {
            return Err(format!("{flag} {model}={target}: {module} is not an absolute module name"));
        }
        out.insert(model.as_str(), (module, class));
    }
    Ok(out)
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

/// A default filter on `target` can hide it, so a reference to it can be absent.
pub fn reference_target_filtered(schema: &crate::schema::Schema, target: &str) -> bool {
    schema.models.iter().any(|m| m.ir.name == target && m.read_flags.filtered)
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
    fn model_methods_reach_both_generators() {
        let (ir, schema) = schema();
        let mut options = Options::default();
        options.model_methods.insert("Post".into(), "app.methods:PostMethods".into());
        let py = python::generate_with(&ir, &schema, "schema.prisma", &options).unwrap();
        assert!(py.module.contains("from app.methods import PostMethods as _PostMethods") && py.module.contains("model_methods={\"Post\": _PostMethods}"));
        assert!(py.stub.contains("class Post(_PostMethods, Model):"));
        options.model_methods.insert("Post".into(), "./methods.js#PostMethods".into());
        let ts = typescript::generate_with(&ir, &schema, "schema.prisma", "orm", &options).unwrap();
        assert!(ts.contains("import * as _m0 from \"./methods.js\";") && ts.contains("useModelMethods(Post, () => _m0.PostMethods);"));
        assert!(python::generate_with(&ir, &schema, "s", &options).err().unwrap().contains("--model-methods") );
        options.model_methods = [("Nope".to_string(), "a:B".to_string())].into();
        assert!(python::generate_with(&ir, &schema, "s", &options).err().unwrap().contains("no model Nope"));
    }

    #[test]
    fn a_schema_class_name_fills_in_only_where_the_consumer_gave_none() {
        use super::lookup::Language;
        let mut ir = crate::dsl::compile("model Post {\n id Int @id\n @@query_set(\"PostQueries\")\n @@model_methods(\"PostMethods\")\n}\nmodel Tag {\n id Int @id\n @@query_set(\"TagQueries\")\n}", None).unwrap();
        let dir = std::env::temp_dir().join(format!("orm-schema-classes-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("pkg")).unwrap();
        std::fs::write(dir.join("pyproject.toml"), "").unwrap();
        let unit = || crate::dsl::SchemaUnit { app: None, path: dir.join("pkg/schema.prisma"), models: vec!["Post".into(), "Tag".into()], enums: vec![], types: vec![] };
        let mut options = Options::default();
        options.query_sets.insert("Post".into(), "mine.queries:PostQueries".into());
        options.with_schema_classes(&ir, &[unit()], Language::Python, &dir).unwrap();
        assert_eq!(options.query_sets["Post"], "mine.queries:PostQueries");
        assert_eq!(options.query_sets["Tag"], "pkg.queries:TagQueries");
        assert_eq!(options.model_methods["Post"], "pkg.methods:PostMethods");
        ir.models[0].query_set = Some("not a class".into());
        let error = Options::default().with_schema_classes(&ir, &[unit()], Language::Python, &dir).unwrap_err();
        assert!(error.contains("is not a class name"), "{error}");
        std::fs::remove_dir_all(&dir).ok();
    }

    fn tagged(ir: &mut crate::ir::SchemaIr, model: &str) -> ExtensionCode {
        ir.behavior.declarations.push(crate::behavior::Declaration {
            attribute: "example.tagged".into(), model: model.into(), field: None, field_type: None,
            arguments: Default::default(), positional: vec![],
            location: crate::behavior::SourceLocation { file: "schema.prisma".into(), line: 1, column: 1 }, lowered: false,
        });
        ExtensionCode {
            members: orm_contracts::extension::LanguageMembers {
                extension: "example.language".into(), version: "1.0.0".into(), language: "python".into(),
                query_set: "TaggedQuerySet".into(), instance: "TaggedInstance".into(), hash: "abc".into(), query_set_members: vec!["tagged".into()], ..Default::default()
            },
            attributes: vec!["example.tagged".into()],
            files: vec![("__init__.py".into(), "x = 1\n".into())],
        }
    }

    #[test]
    fn extension_members_reach_the_models_that_declare_the_attribute() {
        let (mut ir, schema) = schema();
        let ext = tagged(&mut ir, "Post");
        let mut options = Options { extensions: vec![ext.clone()], ..Options::default() };
        let py = python::generate_with(&ir, &schema, "schema.prisma", &options).unwrap();
        assert!(py.module.contains("from orm_ext import example_language as _ext0"));
        assert!(py.module.contains("extensions={\"example.language\": _ext0}, members={\"Post\": (\"example.language\",)})"));
        assert!(py.stub.contains("from orm_ext.example_language import TaggedQuerySet as _ext0QuerySet, TaggedInstance as _ext0Instance"));
        assert!(py.stub.contains("class Post(_ext0Instance, Model):") && py.stub.contains("class User(Model):"));
        assert!(py.stub.contains("class _PostObjects(_ext0QuerySet[Post], PostQuerySet): ...") && py.stub.contains("objects: ClassVar[_PostObjects]"));
        assert!(py.stub.contains("objects: ClassVar[UserQuerySet]"));
        options.query_sets.insert("Post".into(), "app.queries:PostQueries".into());
        options.ext_import = Some("app.orm_ext".into());
        let py = python::generate_with(&ir, &schema, "schema.prisma", &options).unwrap();
        assert!(py.stub.contains("from app.queries import PostQueries as _PostUserObjects"));
        assert!(py.stub.contains("class _PostObjects(_PostUserObjects, _ext0QuerySet[Post], PostQuerySet): ...") && py.module.contains("from app.orm_ext import example_language"));
        let mut bound_ext = ext.clone();
        bound_ext.members.bound_query_set = "TaggedBoundQuerySet".into();
        bound_ext.members.bound_query_set_members = vec!["tagged".into()];
        let options = Options { extensions: vec![bound_ext], ..Options::default() };
        let py = python::generate_with(&ir, &schema, "schema.prisma", &options).unwrap();
        assert!(py.stub.contains("TaggedInstance as _ext0Instance, TaggedBoundQuerySet as _ext0BoundQuerySet"));
        assert!(py.stub.contains("class _PostBoundObjects(_ext0BoundQuerySet[Post], BoundQuerySet[Post]): ..."));
        assert!(py.stub.contains("def of(self, objs: Iterable[Post] | Post) -> _PostBoundObjects: ..."));
        assert!(py.stub.contains("def qs(self) -> _PostBoundObjects: ...") && !py.stub.contains("_UserBoundObjects"));
        let mut ts_ext = ext;
        ts_ext.members.language = "typescript".into();
        ts_ext.members.query_set = "taggedQuerySet".into();
        ts_ext.members.bound_query_set = "taggedBoundQuerySet".into();
        let options = Options { extensions: vec![ts_ext], ..Options::default() };
        let ts = typescript::generate_with(&ir, &schema, "schema.prisma", "orm", &options).unwrap();
        assert!(ts.contains("import * as _e0 from \"./orm-ext/example-language/index.js\";"));
        assert!(ts.contains("export interface Post extends PostData, Instance<PostSpec>, _e0.TaggedInstance {"));
        assert!(ts.contains("export interface PostBaseSpec {") && ts.contains("export interface PostSpec extends PostBaseSpec {"));
        assert!(ts.contains("readonly queries: InstanceType<ReturnType<typeof _e0.taggedQuerySet<new (...args: any[]) => QuerySet<PostBaseSpec>>>>;"));
        assert!(ts.contains("export interface UserSpec {") && ts.contains("QueriesOf<PostSpec>"));
        assert!(ts.contains("readonly boundQueries: InstanceType<ReturnType<typeof _e0.taggedBoundQuerySet<new (...args: any[]) => BoundQuerySet<PostBaseSpec>>>>;"));
        assert!(ts.lines().find(|l| l.starts_with("import { define")).is_some_and(|l| l.contains("type BoundQuerySet")));
        assert!(ts.contains("extensions: { \"example.language\": { module: _e0, hash: \"abc\", dir: new URL(\"./orm-ext/example-language/\", import.meta.url) } }, members: { \"Post\": [\"example.language\"] }"));
    }

    /// `Post`, its proxy `Draft` and a proxy of that, `Final`, and an unrelated `Note`.
    fn proxied() -> (crate::ir::SchemaIr, crate::schema::Schema) {
        let mut ir = crate::dsl::compile("model Post {\n id Int @id\n}\nmodel Draft {\n id Int @id\n}\nmodel Final {\n id Int @id\n}\nmodel Note {\n id Int @id\n}", None).unwrap();
        // The schema is built first: only an artifact with the proxy-models capability accepts a proxy there.
        let schema = crate::schema::Schema::from_ir(serde_json::from_value(serde_json::to_value(&ir).unwrap()).unwrap()).unwrap();
        for (model, parent) in [("Draft", "Post"), ("Final", "Draft")] {
            ir.behavior.proxy_models.push(orm_contracts::extension::ProxyModel { model: model.into(), parent: parent.into(), storage_owner: "Post".into(), ..Default::default() });
        }
        (ir, schema)
    }

    fn delete_rewrite(model: &str) -> orm_contracts::writes::DeleteRewrite {
        orm_contracts::writes::DeleteRewrite { source: "example.language".into(), model: model.into(), field: "deleted".into(), guard: serde_json::Value::Null }
    }

    #[test]
    fn a_proxy_gets_the_extension_members_of_its_root_through_the_prepared_records() {
        let (mut ir, schema) = proxied();
        let ext = tagged(&mut ir, "Post");
        let options = Options { extensions: vec![ext.clone()], ..Options::default() };
        // The attribute is on the root only and no record names the proxy: only the root has the members.
        let py = python::generate_with(&ir, &schema, "schema.prisma", &options).unwrap();
        assert!(py.module.contains("members={\"Post\": (\"example.language\",)})"), "{}", py.module);
        // A prepared record of the extension names a proxy, and a proxy of a proxy, the same way.
        ir.behavior.delete_rewrites.extend([delete_rewrite("Draft"), delete_rewrite("Final")]);
        let py = python::generate_with(&ir, &schema, "schema.prisma", &options).unwrap();
        assert!(py.module.contains("members={\"Post\": (\"example.language\",), \"Draft\": (\"example.language\",), \"Final\": (\"example.language\",)})"), "{}", py.module);
        assert!(py.stub.contains("class Draft(_ext0Instance, Model):") && py.stub.contains("class Final(_ext0Instance, Model):"));
        assert!(py.stub.contains("class _DraftObjects(_ext0QuerySet[Draft], DraftQuerySet): ...") && py.stub.contains("class Note(Model):"));
        let mut ts_ext = ext;
        ts_ext.members.language = "typescript".into();
        ts_ext.members.query_set = "taggedQuerySet".into();
        let options = Options { extensions: vec![ts_ext], ..Options::default() };
        let ts = typescript::generate_with(&ir, &schema, "schema.prisma", "orm", &options).unwrap();
        assert!(ts.contains("export interface Draft extends DraftData, Instance<DraftSpec>, _e0.TaggedInstance {"), "{ts}");
        assert!(ts.contains("\"Draft\": [\"example.language\"]") && ts.contains("\"Final\": [\"example.language\"]") && !ts.contains("\"Note\": ["));
    }

    #[test]
    fn a_proxy_takes_the_query_set_class_of_its_root_unless_it_names_its_own() {
        let (ir, schema) = proxied();
        let mut options = Options::default();
        options.query_sets.insert("Post".into(), "app.queries:PostQueries".into());
        let py = python::generate_with(&ir, &schema, "schema.prisma", &options).unwrap();
        for model in ["Post", "Draft", "Final"] {
            assert!(py.module.contains(&format!("use_query_set({model}, \"app.queries:PostQueries\")")), "{}", py.module);
        }
        assert!(!py.module.contains("use_query_set(Note"));
        // The root's class has the root as its model, so the stub of a proxy types the user's methods as `Any`.
        assert!(py.stub.contains("from app.queries import PostQueries as _PostObjects") && !py.stub.contains("as _DraftObjects"));
        assert!(py.stub.contains("class _DraftObjects(DraftQuerySet):\n    def __getattr__(self, name: str) -> Any: ..."), "{}", py.stub);
        assert!(py.stub.contains("objects: ClassVar[_DraftObjects]") && py.stub.contains("objects: ClassVar[NoteQuerySet]"));
        // The class of the nearest proxy ancestor that has one wins, and a proxy's own class wins over the root's.
        options.query_sets.insert("Draft".into(), "app.queries:DraftQueries".into());
        let py = python::generate_with(&ir, &schema, "schema.prisma", &options).unwrap();
        assert!(py.module.contains("use_query_set(Draft, \"app.queries:DraftQueries\")") && py.module.contains("use_query_set(Final, \"app.queries:DraftQueries\")"));
        assert!(py.stub.contains("from app.queries import DraftQueries as _DraftObjects") && py.stub.contains("class _FinalObjects(FinalQuerySet):"));
        let mut options = Options::default();
        options.query_sets.insert("Post".into(), "./queries.js#PostQueries".into());
        let ts = typescript::generate_with(&ir, &schema, "schema.prisma", "orm", &options).unwrap();
        assert!(ts.contains("useQuerySet(Post, () => _q0.PostQueries);"), "{ts}");
        for model in ["Draft", "Final"] {
            assert!(ts.contains(&format!("useQuerySet({model}, (() => _q0.PostQueries) as never);")), "{ts}");
        }
        assert!(!ts.contains("useQuerySet(Note") && ts.matches("readonly queries: _q0.PostQueries;").count() == 3);
    }

    #[test]
    fn a_proxy_takes_the_model_methods_of_its_root_unless_it_names_its_own() {
        let (ir, schema) = proxied();
        let mut options = Options::default();
        options.model_methods.insert("Post".into(), "app.methods:PostMethods".into());
        let py = python::generate_with(&ir, &schema, "schema.prisma", &options).unwrap();
        assert!(py.module.contains("model_methods={\"Draft\": _DraftMethods, \"Final\": _FinalMethods, \"Post\": _PostMethods}"), "{}", py.module);
        assert!(py.module.contains("from app.methods import PostMethods as _DraftMethods") && !py.module.contains("_NoteMethods"), "{}", py.module);
        assert!(py.stub.contains("class Draft(_DraftMethods, Model):") && py.stub.contains("class Note(Model):"), "{}", py.stub);
        options.model_methods.insert("Draft".into(), "app.methods:DraftMethods".into());
        let py = python::generate_with(&ir, &schema, "schema.prisma", &options).unwrap();
        assert!(py.module.contains("from app.methods import DraftMethods as _FinalMethods"), "{}", py.module);
        let mut options = Options::default();
        options.model_methods.insert("Post".into(), "./methods.js#PostMethods".into());
        let ts = typescript::generate_with(&ir, &schema, "schema.prisma", "orm", &options).unwrap();
        for model in ["Post", "Draft", "Final"] {
            assert!(ts.contains(&format!("useModelMethods({model}, () => _m0.PostMethods);")), "{ts}");
        }
        assert!(!ts.contains("useModelMethods(Note"), "{ts}");
    }

    #[test]
    fn extension_files_carry_a_header_and_no_extension_means_no_change() {
        let (mut ir, schema) = schema();
        let ext = tagged(&mut ir, "Post");
        let files = ext.generated_files("python");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0, "example_language/__init__.py");
        assert_eq!(files[0].1, "# Generated by `orm generate` from extension example.language. Do not edit.\n# orm-ext example.language 1.0.0 abc\n\nx = 1\n");
        assert_eq!(ext.generated_files("typescript")[0].0, "example-language/__init__.py");
        let plain = Options::default();
        let with_unused = Options { extensions: vec![ExtensionCode { attributes: vec!["other".into()], ..ext }], ..Options::default() };
        assert_eq!(python::generate_with(&ir, &schema, "s", &plain).unwrap().module, python::generate_with(&ir, &schema, "s", &with_unused).unwrap().module);
        assert_eq!(typescript::generate_with(&ir, &schema, "s", "orm", &plain).unwrap(), typescript::generate_with(&ir, &schema, "s", "orm", &with_unused).unwrap());
    }

    #[test]
    fn python_query_sets_reject_hard_keywords() {
        let (ir, schema) = schema();
        for target in ["None:Q", "app.class:Q", "app:class", "app:False", "async:Q"] {
            let options = Options { query_sets: [("Post".into(), target.into())].into(), ..Options::default() };
            assert!(python::generate_with(&ir, &schema, "s", &options).is_err(), "{target}");
        }
    }

    #[test]
    fn python_query_sets_accept_soft_keywords() {
        let (ir, schema) = schema();
        for name in ["match", "case", "type", "_"] {
            let options = Options { query_sets: [("Post".into(), format!("app.{name}:{name}"))].into(), ..Options::default() };
            let py = python::generate_with(&ir, &schema, "s", &options).unwrap();
            assert!(py.stub.contains(&format!("from app.{name} import {name} as _PostObjects")));
        }
    }

    /// `User` extends the abstract `AbstractUser`, which declares the query-set class `UserQueries`.
    fn abstract_user() -> (crate::ir::SchemaIr, crate::schema::Schema) {
        let mut ir = crate::dsl::compile("model User {\n id Int @id\n email String\n}", None).unwrap();
        ir.behavior.type_models = serde_json::from_value(serde_json::json!([
            {"name": "AbstractUser", "fields": [
                {"name": "id", "column": "id", "type": "int", "primary_key": true},
                {"name": "email", "column": "email", "type": "string"}], "extended_by": ["User"]},
            {"name": "Timestamped", "fields": [{"name": "created_at", "column": "created_at", "type": "date_time"}]}
        ]))
        .unwrap();
        ir.models[0].classes_from = Some("AbstractUser".into());
        ir.behavior.schema_contract = orm_contracts::extension::SCHEMA_CONTRACT;
        let schema = crate::schema::Schema::from_ir(serde_json::from_value(serde_json::to_value(&ir).unwrap()).unwrap()).unwrap();
        (ir, schema)
    }

    #[test]
    fn python_abstract_models_become_stub_classes_without_a_runtime_model() {
        let (ir, schema) = abstract_user();
        let options = Options { query_sets: [("User".into(), "app.queries:UserQueries".into())].into(), ..Options::default() };
        let py = python::generate_with(&ir, &schema, "schema.prisma", &options).unwrap();
        assert!(py.stub.contains("class AbstractUserFields(Model):") && py.stub.contains("class TimestampedFields(Model):"), "{}", py.stub);
        assert!(py.stub.contains("class AbstractUserFields(Model):\n") && py.stub.contains("    email: f.String[str]\n"), "{}", py.stub);
        assert!(py.stub.contains("class User(AbstractUserFields, Model):"), "{}", py.stub);
        assert!(py.stub.contains("from app.queries import UserQueries as _UserUserObjects") && py.stub.contains("class _UserObjects(_UserUserObjects[User], UserQuerySet)"), "{}", py.stub);
        assert!(py.module.contains("AbstractUserFields = TimestampedFields = Model"), "{}", py.module);
        assert!(!py.module.contains("class AbstractUserFields"));
    }

    #[test]
    fn typescript_abstract_models_become_a_data_type_and_a_spec_bound() {
        let (ir, schema) = abstract_user();
        let options = Options { query_sets: [("User".into(), "./queries.js#UserQueries".into())].into(), ..Options::default() };
        let ts = typescript::generate_with(&ir, &schema, "schema.prisma", "orm", &options).unwrap();
        assert!(ts.contains("export interface AbstractUserData {") && ts.contains("export interface AbstractUserSpec extends ModelSpec {"), "{ts}");
        assert!(ts.contains("export interface TimestampedSpec extends ModelSpec {") && ts.contains("readonly createdAt: Date;"), "{ts}");
        assert!(ts.contains("readonly queries: _q0.UserQueries<UserSpec>;") && ts.contains("useQuerySet(User, () => _q0.UserQueries<UserSpec>);"), "{ts}");
        assert!(!ts.contains("export const AbstractUser ") && !ts.contains("export const Timestamped "));
    }

    #[test]
    fn a_class_of_a_plain_model_is_not_generic() {
        let (mut ir, schema) = abstract_user();
        ir.models[0].classes_from = None;
        let options = Options { query_sets: [("User".into(), "./queries.js#UserQueries".into())].into(), ..Options::default() };
        let ts = typescript::generate_with(&ir, &schema, "schema.prisma", "orm", &options).unwrap();
        assert!(ts.contains("readonly queries: _q0.UserQueries;") && ts.contains("useQuerySet(User, () => _q0.UserQueries);"), "{ts}");
    }

    #[test]
    fn an_extension_without_members_does_not_apply() {
        let (mut ir, _) = schema();
        let mut ext = tagged(&mut ir, "Post");
        assert!(ext.applies(&ir, "Post"));
        ext.members.query_set_members.clear();
        assert!(!ext.applies(&ir, "Post"));
    }

    #[test]
    fn typescript_query_sets_accept_subpaths_and_dollar_exports() {
        let (ir, schema) = schema();
        for (module, export) in [("#app/queries", "Q"), ("./queries.js", "$Q$"), ("#app/queries", "default")] {
            let options = Options { query_sets: [("Post".into(), format!("{module}#{export}"))].into(), ..Options::default() };
            let ts = typescript::generate_with(&ir, &schema, "s", "orm", &options).unwrap();
            assert!(ts.contains(&format!("import * as _q0 from \"{module}\";")));
            assert!(ts.contains(&format!("useQuerySet(Post, () => _q0.{export});")));
            assert!(ts.contains(&format!("readonly queries: _q0.{export};")));
        }
        for target in ["#Q", "app#", "app#1Q", "app#bad-name"] {
            let options = Options { query_sets: [("Post".into(), target.into())].into(), ..Options::default() };
            assert!(typescript::generate_with(&ir, &schema, "s", "orm", &options).is_err());
        }
    }

    #[test]
    fn non_null_group_with_a_nullable_middle_level_is_not_nullable() {
        let src = "type Geo {\n  lat Float\n}\n\ntype Wrap {\n  inner Geo?\n}\n\ntype Tagged {\n  tag String\n  inner Geo?\n}\n\nmodel S {\n  id Int @id\n  w Wrap\n  t Tagged?\n}\n";
        let ir = crate::dsl::compile(src, None).unwrap();
        let schema = crate::schema::Schema::from_ir(serde_json::from_value(serde_json::to_value(&ir).unwrap()).unwrap()).unwrap();
        let py = python::generate(&ir, &schema, "schema.prisma").unwrap().stub;
        assert!(py.contains("    w: f.Nested[Wrap, _WrapRef]\n"), "{py}");
        assert!(py.contains("    t: f.Nested[Tagged | None, _TaggedRef]\n"), "{py}");
        let ts = typescript::generate(&ir, &schema, "schema.prisma", "orm").unwrap();
        assert!(ts.contains("  readonly w: Wrap;\n"), "{ts}");
        assert!(ts.contains("  readonly t: Tagged | null;\n"), "{ts}");
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

    #[test]
    fn pyi_imports_only_the_names_it_uses() {
        let ir = crate::dsl::compile("model A {\n id Int @id\n}", None).unwrap();
        let schema = crate::schema::Schema::from_ir(serde_json::from_value(serde_json::to_value(&ir).unwrap()).unwrap()).unwrap();
        let py = python::generate(&ir, &schema, "schema.prisma").unwrap().stub;
        for name in ["Sequence", "NestedRef", "NestedValue"] {
            assert!(!py.contains(name), "{name} in {py}");
        }
        let src = "type Geo {\n  lat Float\n  tags String[]\n}\n\nmodel S {\n  id Int @id\n  g Geo\n}\n";
        let ir = crate::dsl::compile(src, None).unwrap();
        let schema = crate::schema::Schema::from_ir(serde_json::from_value(serde_json::to_value(&ir).unwrap()).unwrap()).unwrap();
        let py = python::generate(&ir, &schema, "schema.prisma").unwrap().stub;
        assert!(py.contains("from collections.abc import Iterable, Sequence\n"), "{py}");
        assert!(py.contains("Model, NestedRef, NestedValue, QuerySet"), "{py}");
    }

    #[test]
    fn value_comment_names_the_type_without_a_wrong_article() {
        let src = "type Address {\n  city String\n}\n\nmodel S {\n  id Int @id\n  a Address\n}\n";
        let ir = crate::dsl::compile(src, None).unwrap();
        let schema = crate::schema::Schema::from_ir(serde_json::from_value(serde_json::to_value(&ir).unwrap()).unwrap()).unwrap();
        let ts = typescript::generate(&ir, &schema, "schema.prisma", "orm").unwrap();
        assert!(ts.contains("/** Immutable `Address` value, compared with `equals()`. */"), "{ts}");
        assert!(!ts.contains("A `Address`"), "{ts}");
    }
}
