//! Declarations are setup data. Native execution consumes resolved host identities.
use std::collections::BTreeMap;
use serde::{Deserialize, Serialize};
use crate::ir::{ColType, SchemaIr, ValueType};
pub use crate::version::ContractVersion;

/// The static Rust API of an extension crate; a library has the C ABI instead.
pub const HOST_CONTRACT_VERSION: ContractVersion = ContractVersion::new(1, 0);
/// The IR records. A library of the same major and a lower or equal minor fits.
pub const SCHEMA_CONTRACT_VERSION: ContractVersion = ContractVersion::new(2, 1);
/// The C ABI that `orm_contracts::extension!` writes.
pub const ABI_VERSION: ContractVersion = ContractVersion::new(0, 2);
/// The stamps of an IR and an artifact keep the major only.
pub const HOST_CONTRACT: u32 = HOST_CONTRACT_VERSION.major as u32;
pub const SCHEMA_CONTRACT: u32 = SCHEMA_CONTRACT_VERSION.major as u32;
/// The minor that an IR carries only when it uses a record that a 2.0 engine lacks (`hooks`, `extension_data`).
pub const SCHEMA_CONTRACT_MINOR: u32 = SCHEMA_CONTRACT_VERSION.minor as u32;
/// The version of `orm.ext` / `orm/ext`, the host module that extension language code imports.
pub const LANGUAGE_CONTRACT: u32 = 1;
/// The languages that an extension can ship code for, as the manifest names them.
pub const LANGUAGES: [&str; 2] = ["python", "typescript"];

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceLocation {
    pub file: String,
    pub line: u32,
    pub column: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Declaration {
    pub attribute: String,
    pub model: String,
    pub field: Option<String>,
    /// The declared type of the field a field attribute is on, as written: `Tag[]`, `Generic?`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field_type: Option<String>,
    pub arguments: BTreeMap<String, serde_json::Value>,
    pub positional: Vec<serde_json::Value>,
    pub location: SourceLocation,
    #[serde(default)]
    pub lowered: bool,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Requirements {
    /// Same-table logical views, prepared by the selected proxy compiler.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub proxy_models: Vec<ProxyModel>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub generic_relations: Vec<crate::generic::GenericRelation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub generic_reverse: Vec<crate::generic::GenericReverse>,
    #[serde(default)]
    pub read_defaults: Vec<ReadDefault>,
    #[serde(default)]
    pub schema_contract: u32,
    /// Zero unless the IR has a 2.1 record. An engine of contract 2.0 reads such an IR only when this is zero.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub schema_contract_minor: u32,
    /// Typed hook bindings that extension passes wrote; the engine calls them through the library.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hooks: Vec<HookBinding>,
    /// The data of each extension that has a library, by extension id. Only its `init` reads it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extension_data: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub declarations: Vec<Declaration>,
    #[serde(default)]
    pub extensions: BTreeMap<String, String>,
    /// The extensions that ran as libraries, by id. `extensions` and `completed_passes` keep only the compiled ones.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub packages: BTreeMap<String, PackageStamp>,
    #[serde(default)]
    pub completed_passes: Vec<String>,
    #[serde(default)]
    pub lowered_models: Vec<String>,
    #[serde(default)]
    pub specializations: Vec<Specialization>,
    #[serde(default)]
    pub result_fields: Vec<ResultDeclaration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<PhysicalSchema>,
    #[serde(default)]
    pub methods: Vec<MethodDeclaration>,
    #[serde(default)]
    pub field_storage: Vec<FieldStorage>,
    #[serde(default)]
    pub owner_links: Vec<OwnerLink>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub field_adapters: Vec<FieldAdapter>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_fields: Vec<FileField>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub write_assignments: Vec<crate::writes::WriteAssignment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub delete_rewrites: Vec<crate::writes::DeleteRewrite>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub instance_guards: Vec<crate::writes::InstanceGuard>,
    /// Models that have no table: the fields of each, for the generated types.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub type_models: Vec<crate::ir::TypeModel>,
    /// A model that took the table of another one: `{"User": "MyUser"}`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub model_overrides: BTreeMap<String, String>,
}
fn is_zero(n: &u32) -> bool { *n == 0 }

/// One hook of an extension library, bound to a model and a field.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HookBinding {
    /// The extension that owns the export.
    pub source: String,
    /// The hook point, 1 to 7.
    pub point: u8,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// The id of the export in the manifest of `source`.
    pub export: String,
    /// The fields of the model that the hook reads (point 5).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reads_context: bool,
}

/// What the IR records about an extension that ran as a library.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PackageStamp {
    pub version: String,
    /// SHA-256 of the raw installed `orm-extension.toml` bytes (lowercase hex).
    /// Empty only for schemas generated before package locks carried a hash.
    #[serde(default)]
    pub manifest_hash: String,
    /// The passes that ran, in order.
    #[serde(default)]
    pub passes: Vec<String>,
    /// The attribute names of the manifest: a lowered declaration of one of them needs no compiled manifest.
    #[serde(default)]
    pub attributes: Vec<String>,
    #[serde(default)]
    pub languages: Vec<String>,
    #[serde(default)]
    pub databases: Vec<String>,
    #[serde(default)]
    pub language_members: BTreeMap<String, LanguageSection>,
    #[serde(default)]
    pub dependencies: BTreeMap<String, String>,
}

impl Requirements {
    /// The proxy ancestors of `model`, nearest first, ending at the root.
    /// `None` when the proxy parents form a cycle.
    pub fn ancestors<'a>(&'a self, model: &'a str) -> Option<Vec<&'a str>> {
        let mut out = vec![];
        let mut current = model;
        while let Some(proxy) = self.proxy_models.iter().find(|p| p.model == current) {
            if proxy.parent == model || out.contains(&proxy.parent.as_str()) { return None; }
            out.push(proxy.parent.as_str());
            current = proxy.parent.as_str();
        }
        Some(out)
    }

    /// The extensions that prepared a read default, delete rewrite, write assignment or instance guard for `model`.
    pub fn record_sources<'a>(&'a self, model: &'a str) -> impl Iterator<Item = &'a str> {
        self.read_defaults.iter().filter(move |r| r.model == model).map(|r| r.source.as_str())
            .chain(self.delete_rewrites.iter().filter(move |r| r.model == model).map(|r| r.source.as_str()))
            .chain(self.write_assignments.iter().filter(move |r| r.model == model).map(|r| r.source.as_str()))
            .chain(self.instance_guards.iter().filter(move |r| r.model == model).map(|r| r.source.as_str()))
    }

    /// Sets the minor of the contract when a 2.1 record is in use, so that an engine of 2.0 reads every other IR.
    pub fn stamp_minor(&mut self) {
        if !self.hooks.is_empty() || !self.extension_data.is_empty() { self.schema_contract_minor = SCHEMA_CONTRACT_MINOR; }
    }

    pub fn is_empty(&self) -> bool {
        self.read_defaults.is_empty() && self.proxy_models.is_empty() && self.generic_relations.is_empty() && self.generic_reverse.is_empty() && self.declarations.is_empty() && self.extensions.is_empty() && self.packages.is_empty() && self.specializations.is_empty()
            && self.lowered_models.is_empty() && self.completed_passes.is_empty() && self.result_fields.is_empty() && self.storage.is_none() && self.field_storage.is_empty() && self.owner_links.is_empty() && self.methods.is_empty() && self.field_adapters.is_empty() && self.file_fields.is_empty()
            && self.write_assignments.is_empty() && self.delete_rewrites.is_empty() && self.instance_guards.is_empty() && self.type_models.is_empty() && self.model_overrides.is_empty() && self.schema_contract == 0
            && self.schema_contract_minor == 0 && self.hooks.is_empty() && self.extension_data.is_empty()
    }
}

/// A build-selected binding codec identity, never a provider or import path.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FieldAdapter {
    pub model: String,
    pub field: String,
    pub adapter: String,
}

/// File-field configuration has durable identity only; clients remain application configuration.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FileField {
    pub model: String,
    pub field: String,
    pub storage: String,
    pub reference_contract: u32,
}

/// The language capability of the async field adapter pipeline: validate, `await prepare`, SQL, decode.
pub const ASYNC_FIELD_ADAPTER_V1: &str = "async-field-adapter.v1";

pub const FILE_REFERENCE_ADAPTER: &str = "file-storage.reference.v1";

/// Names the host runtimes give meaning to on models and instances.
pub const RESERVED_MEMBERS: [&str; 14] =
    ["objects", "qs", "_meta", "DoesNotExist", "MultipleObjectsReturned", "pk", "update", "delete", "refresh", "load", "toJSON", "constructor", "toString", "then"];

/// Reject a file field whose generated Python or TypeScript operations collide with a model member.
pub fn check_file_methods(ir: &SchemaIr, model: &crate::ir::ModelIr, field: &str) -> Result<(), String> {
    for method in [format!("{field}_signed_url"), format!("{field}_open"), format!("{}SignedUrl", camel(field)), format!("{}Open", camel(field))] {
        let taken = |name: &str| name == method || camel(name) == method;
        if RESERVED_MEMBERS.contains(&method.as_str())
            || model.fields.iter().any(|f| taken(&f.name))
            || model.relations.iter().any(|r| taken(&r.name))
            || ir.behavior.methods.iter().any(|m| m.model == model.name && taken(&m.name))
        {
            return Err(format!("{}.{method}: generated file method collides with a model member", model.name));
        }
    }
    Ok(())
}

/// `author_id` -> `authorId`: an underscore after a letter or digit is dropped and the
/// next letter or digit upper-cased (leading and trailing underscores stay).
pub fn camel(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(name.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '_' && i > 0 && chars[i - 1].is_ascii_alphanumeric() {
            let mut j = i;
            while j < chars.len() && chars[j] == '_' {
                j += 1;
            }
            if j < chars.len() && chars[j].is_ascii_alphanumeric() {
                out.extend(chars[j].to_uppercase());
                i = j + 1;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Validate field codec shapes once, before usable models are published.
pub fn validate_field_adapters(ir: &SchemaIr) -> Result<(), String> {
    let mut seen = std::collections::BTreeSet::new();
    for adapter in &ir.behavior.field_adapters {
        let model = ir.models.iter().find(|m| m.name == adapter.model)
            .ok_or_else(|| format!("unknown field adapter model {}", adapter.model))?;
        let field = model.fields.iter().find(|f| f.name == adapter.field)
            .ok_or_else(|| format!("{}.{}: unknown adapter field", adapter.model, adapter.field))?;
        if field.ty != ColType::Json || field.array || field.enum_name.is_some() {
            return Err(format!("{}.{}: field adapters require scalar Json", adapter.model, adapter.field));
        }
        if !seen.insert((&adapter.model, &adapter.field)) || adapter.adapter.is_empty() {
            return Err(format!("{}.{}: duplicate/empty field adapter", adapter.model, adapter.field));
        }
    }
    let mut files = std::collections::BTreeSet::new();
    for file in &ir.behavior.file_fields {
        if !files.insert((&file.model, &file.field)) || file.reference_contract != 1
            || file.storage.is_empty() || file.storage.contains('\0') {
            return Err(format!("{}.{}: invalid file-field contract", file.model, file.field));
        }
        if !ir.behavior.field_adapters.iter().any(|a| a.model == file.model && a.field == file.field && a.adapter == FILE_REFERENCE_ADAPTER) {
            return Err(format!("{}.{}: missing file reference adapter", file.model, file.field));
        }
    }
    for adapter in ir.behavior.field_adapters.iter().filter(|a| a.adapter == FILE_REFERENCE_ADAPTER) {
        if !files.contains(&(&adapter.model, &adapter.field)) {
            return Err(format!("{}.{}: missing file-field configuration", adapter.model, adapter.field));
        }
    }
    Ok(())
}

/// Intended shape only. Physical encoding, enum representation and constraints
/// remain on the storage owner. Only explicit partition ownership implies a predicate.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct ProxyModel {
    pub model: String,
    pub parent: String,
    /// Resolved root physical model identity, filled by lowering. Generic identity
    /// manifests exclude `model` and refer to this concrete storage identity.
    #[serde(default)]
    pub storage_owner: String,
    /// Resolved list-partition ownership; values use the physical enum encoding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition: Option<ProxyPartition>,
    #[serde(default)]
    pub fields: Vec<ProxyField>,
    /// `@@proxy.fields(include:|exclude:)`: which source fields and relations the view keeps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<ProxySelection>,
    /// Source fields and relations that are not part of the view, filled by lowering.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub omitted: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProxyPartition {
    pub table: String,
    pub field: String,
    pub values: Vec<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProxySelection {
    Include(Vec<String>),
    Exclude(Vec<String>),
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProxyField {
    pub field: String,
    #[serde(default)]
    pub non_null: bool,
    /// Parent enum member names, not database labels. None means the whole enum.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subset: Option<Vec<String>>,
}

/// Setup contribution connecting a logical field to one physical owner.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FieldStorage {
    pub model: String,
    pub field: String,
    pub owner: String,
    pub column: String,
}
/// Shared-identity owner edge. Each child has at most one parent.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OwnerLink {
    pub child: String,
    pub parent: String,
    pub child_key: String,
    pub parent_key: String,
}
#[derive(Clone, Copy, Debug)]
pub struct PreparedOwnerLink {
    pub child: OwnerId,
    pub parent: OwnerId,
    pub child_key: usize,
    pub parent_key: usize,
}

/// A generated ordinary model method backed by one compiled native export.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MethodDeclaration {
    pub model: String,
    pub name: String,
    pub native_function: String,
    pub input: ColType,
    pub output: Option<ColType>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResultDeclaration {
    pub model: String,
    pub field: String,
    pub dependencies: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Specialization {
    pub model: String,
    pub fingerprint: String,
    pub exports: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub id: String,
    pub version: String,
    /// The static Rust API. A manifest of a library has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_contract: Option<ContractVersion>,
    pub schema_contract: ContractVersion,
    /// The C ABI of the library. Only the manifest of a library has it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abi: Option<ContractVersion>,
    /// The library file name; the loader adds the platform prefix and suffix when the name has no extension.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library: Option<String>,
    #[serde(default)]
    pub dependencies: BTreeMap<String, String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Language capabilities the extension needs; each language adapter checks them when it loads the artifact.
    #[serde(default)]
    pub requires: Vec<String>,
    pub languages: Vec<String>,
    pub databases: Vec<String>,
    #[serde(default)]
    pub attributes: Vec<Attribute>,
    #[serde(default)]
    pub passes: Vec<Pass>,
    #[serde(default)]
    pub exports: Vec<Export>,
    /// The `orm.ext` version that the language code needs; required when a language section exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language_contract: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python: Option<LanguageSection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub typescript: Option<LanguageSection>,
}
impl Manifest {
    /// Whether the passes of this manifest run from a native library.
    pub fn is_dynamic(&self) -> bool { self.abi.is_some() }

    /// The contract versions against this engine; the error names both versions.
    pub fn check_contracts(&self) -> Result<(), String> {
        self.schema_contract.check("schema contract", SCHEMA_CONTRACT_VERSION, &self.id)?;
        match (self.abi, self.host_contract) {
            (Some(abi), _) => abi.check("ABI", ABI_VERSION, &self.id),
            (None, Some(host)) => host.check("host contract", HOST_CONTRACT_VERSION, &self.id),
            (None, None) => Err(format!("{}: a manifest with no `abi` needs `host_contract`", self.id)),
        }
    }

    /// The language section of `language` (`python` or `typescript`).
    pub fn language_section(&self, language: &str) -> Option<&LanguageSection> {
        match language { "python" => self.python.as_ref(), "typescript" => self.typescript.as_ref(), _ => None }
    }
    /// The languages with a section, in manifest order.
    pub fn language_sections(&self) -> impl Iterator<Item = (&'static str, &LanguageSection)> {
        LANGUAGES.into_iter().filter_map(|l| self.language_section(l).map(|s| (l, s)))
    }
}

/// The code that an extension ships for one language.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LanguageSection {
    /// The source directory, relative to the extension crate.
    pub source: String,
    /// The export that adds query-set members: a `QuerySet` subclass in Python, a mixin function in TypeScript.
    pub query_set: String,
    /// The export that adds instance members.
    pub instance: String,
    /// Optional export for the bound query set (`orm.of(objs)`): a `BoundQuerySet` subclass in Python, a mixin function in TypeScript, whose members keep "at most one row per bound object". Its members are a subset of the `query_set` members; empty means none.
    #[serde(default)]
    pub bound_query_set: String,
    /// The host members that the extension replaces; every other name collision is an error.
    #[serde(default)]
    pub overrides: Vec<String>,
}

/// What the artifact records about the language code of one extension and language.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LanguageMembers {
    pub extension: String,
    pub version: String,
    pub language: String,
    pub query_set: String,
    pub instance: String,
    /// Member names that the query-set class and the instance class add, sorted.
    pub query_set_members: Vec<String>,
    pub instance_members: Vec<String>,
    /// The export of the bound query set, empty when the extension has none, and its member names, sorted.
    #[serde(default)]
    pub bound_query_set: String,
    #[serde(default)]
    pub bound_query_set_members: Vec<String>,
    pub overrides: Vec<String>,
    /// SHA-256 of the embedded source files (name, length and content of each, in name order).
    pub hash: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Attribute {
    pub name: String,
    pub target: AttributeTarget,
    #[serde(default)]
    pub arguments: BTreeMap<String, Argument>,
    #[serde(default)]
    pub positional: Vec<Argument>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Argument {
    pub kind: ArgumentKind,
    #[serde(default)]
    pub required: bool,
    /// The last positional argument only: it takes any number of values of `kind`.
    #[serde(default, skip_serializing_if = "crate::ir::is_false")]
    pub variadic: bool,
}
impl Attribute {
    /// The positional argument that describes value `i`.
    pub fn positional_at(&self, i: usize) -> Option<&Argument> {
        self.positional.get(i).or_else(|| self.positional.last().filter(|a| a.variadic))
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ArgumentKind { String, Integer, Boolean, List, Value }
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttributeTarget { Model, Field }
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Phase { Declaration, Logical, Storage, Behavior, Validation, Generation }
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Pass {
    pub id: String,
    pub phase: Phase,
    /// Compiler entry point: `fn(&mut SchemaIr) -> Result<(), String>`. A pass of a library has none; the loader calls it by id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rust: Option<String>,
    #[serde(default)]
    pub after: Vec<String>,
    /// Exclusive effects, e.g. `model.User.field.name.type`.
    #[serde(default)]
    pub effects: Vec<String>,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExportKind { StringValidator, StringTransform, StringComputed, StringRecordValidator }
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Export {
    pub id: String,
    /// The static form: a Rust path that the build compiles in, with its `kind`. A typed hook has neither.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rust: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ExportKind>,
    /// The hook point, 1 to 7. A typed hook of a library has one; the static form has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub point: Option<u8>,
    /// The type of the value that the hook takes. Points 3, 6 and 7 take none, and then it is absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<ColType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<ColType>,
}

/// The static form of an export, which `orm-extension-build` compiles in.
#[derive(Clone, Copy, Debug)]
pub struct StringExport<'a> {
    pub rust: &'a str,
    pub kind: ExportKind,
    pub input: ColType,
    pub output: Option<ColType>,
}

impl Export {
    pub fn string_form(&self) -> Option<StringExport<'_>> {
        Some(StringExport { rust: self.rust.as_deref()?, kind: self.kind?, input: self.input?, output: self.output })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub host_contract: u32,
    pub schema_contract: u32,
    #[serde(default)]
    pub passes: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    pub extensions: BTreeMap<String, String>,
    /// The `requires` list of each selected extension that has one, by extension id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub requires: BTreeMap<String, Vec<String>>,
    pub exports: Vec<String>,
    pub specializations: Vec<Specialization>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub language_members: Vec<LanguageMembers>,
}
impl Default for Artifact {
    fn default() -> Self {
        Self { host_contract: HOST_CONTRACT, schema_contract: SCHEMA_CONTRACT,
            passes: vec![], capabilities: vec![], extensions: BTreeMap::new(), requires: BTreeMap::new(), exports: vec![], specializations: vec![], language_members: vec![] }
    }
}

/// Stable resolved positions within an immutable schema snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ModelId(pub usize);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FieldId { pub model: ModelId, pub position: usize }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StorageId { pub owner: OwnerId, pub column: usize }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OwnerId(pub usize);
#[derive(Clone, Debug)]
pub struct ResolvedField {
    pub logical: FieldId,
    pub storage: StorageId,
    pub logical_type: ValueType,
    pub physical_type: ValueType,
}
#[derive(Debug)]
pub struct ResultField {
    pub field: FieldId,
    pub physical: Option<usize>,
    pub public: bool,
    pub dependencies: Vec<FieldId>,
}
#[derive(Debug)]
pub struct ResultShape { pub model: ModelId, pub fields: Vec<ResultField> }
#[derive(Debug)]
pub struct QueryContract<'a> {
    pub expression: &'a crate::ir::Expr,
    pub owner: OwnerId,
    pub shape: &'a ResultShape,
}

/// The part of an attribute name before the dot: `timestamps` for `timestamps.updated_at`.
pub fn attribute_namespace(attribute: &str) -> &str {
    attribute.split_once('.').map_or(attribute, |(namespace, _)| namespace)
}

pub fn validate_declarations(ir: &SchemaIr, manifests: &[Manifest], language: Option<&str>) -> Result<(), String> {
    let fail = |d: &Declaration, msg: String| format!("{}:{}:{}: @{}: {msg}",
        d.location.file, d.location.line, d.location.column, d.attribute);
    for d in &ir.behavior.declarations {
        if d.lowered {
            if let Some((id, package)) = ir.behavior.packages.iter().find(|(_, p)| p.attributes.contains(&d.attribute)) {
                validate_package_support(id, package, ir.dialect.name(), language).map_err(|e| fail(d, e))?;
                continue;
            }
        }
        // One name can have a model entry (`@@x`) and a field entry (`@x`).
        let named = || manifests.iter().flat_map(|m| m.attributes.iter().map(move |a| (m, a))).filter(|(_, a)| a.name == d.attribute);
        let found = named().find(|(_, a)| (a.target == AttributeTarget::Field) == d.field.is_some()).or_else(|| named().next());
        let (m, a) = found.ok_or_else(|| {
            let prefix = if d.field.is_some() { "@" } else { "@@" };
            let namespace = attribute_namespace(&d.attribute);
            fail(d, format!("not compiled into this artifact and no installed extension owns it; install its package or rebuild with its extension (namespace {prefix}{namespace}.*, see docs/README.md)"))
        })?;
        if let Err(e) = m.check_contracts() { return Err(fail(d, e)); }
        if !m.databases.iter().any(|s| s == ir.dialect.name()) {
            return Err(fail(d, format!("{} does not support {}", m.id, ir.dialect.name())));
        }
        if language.is_some_and(|l| !m.languages.iter().any(|s| s == l)) {
            return Err(fail(d, format!("{} does not support this language binding", m.id)));
        }
        if (d.field.is_some()) != (a.target == AttributeTarget::Field) {
            let place = if a.target == AttributeTarget::Field { format!("a field attribute; write @{} on a field", d.attribute) } else { format!("a model attribute; write @@{} in the model block", d.attribute) };
            return Err(fail(d, format!("invalid declaration target: it is {place}")));
        }
        // Only `value` arguments take null and objects, also inside a list.
        fn literal(v: &serde_json::Value) -> bool {
            match v { serde_json::Value::Null | serde_json::Value::Object(_) => false, serde_json::Value::Array(items) => items.iter().all(literal), _ => true }
        }
        let matches = |v: &serde_json::Value, arg: &Argument| match arg.kind {
            ArgumentKind::String => v.is_string(), ArgumentKind::Integer => v.is_i64(),
            ArgumentKind::Boolean => v.is_boolean(), ArgumentKind::List => v.is_array() && literal(v),
            ArgumentKind::Value => true,
        };
        for (name, value) in &d.arguments {
            let arg = a.arguments.get(name).ok_or_else(|| fail(d, format!("unknown argument {name}")))?;
            if !matches(value, arg) { return Err(fail(d, format!("wrong type for {name}"))); }
        }
        for (name, arg) in &a.arguments {
            if arg.required && !d.arguments.contains_key(name) { return Err(fail(d, format!("missing argument {name}"))); }
        }
        if a.positional.iter().rev().skip(1).any(|arg| arg.variadic) {
            return Err(fail(d, "only the last positional argument can be variadic".into()));
        }
        if d.positional.len() > a.positional.len() && !a.positional.last().is_some_and(|arg| arg.variadic) {
            return Err(fail(d, "too many positional arguments".into()));
        }
        for (i, arg) in a.positional.iter().enumerate() {
            if d.positional.get(i).is_none() && arg.required { return Err(fail(d, format!("missing positional argument {i}"))); }
        }
        for (i, v) in d.positional.iter().enumerate() {
            if !a.positional_at(i).is_some_and(|arg| matches(v, arg)) { return Err(fail(d, format!("wrong type for positional argument {i}"))); }
        }
    }
    Ok(())
}

/// Contract compatibility is checked once at definition, never at materialization.
pub fn check_requirements(ir: &SchemaIr, artifact: &Artifact) -> Result<(), String> {
    validate_field_adapters(ir)?;
    let r = &ir.behavior;
    if (!r.generic_relations.is_empty() || !r.generic_reverse.is_empty()) && !artifact.capabilities.iter().any(|c| c == "generic-relations") {
        return Err("generic relations are not compiled into this artifact; rebuild with generic-relations".into());
    }
    if !r.proxy_models.is_empty() && !artifact.capabilities.iter().any(|c| c == "proxy-models") {
        return Err("proxy models require the compiled proxy-models capability; rebuild native artifact".into());
    }
    if !r.is_empty() && r.schema_contract != artifact.schema_contract {
        return Err(format!("schema contract {} unavailable (artifact {}); rebuild native artifact", r.schema_contract, artifact.schema_contract));
    }
    if r.schema_contract_minor > SCHEMA_CONTRACT_MINOR {
        return Err(format!("schema contract {}.{} is newer than this engine ({SCHEMA_CONTRACT_VERSION}); update the ORM or regenerate the models", r.schema_contract, r.schema_contract_minor));
    }
    if r.storage.is_some() && !artifact.capabilities.iter().any(|c| c == "physical-schema") {
        return Err(format!("physical schema contributions are not compiled into this artifact (capabilities {:?}); rebuild", artifact.capabilities));
    }
    for pass in &r.completed_passes {
        if !artifact.passes.contains(pass) { return Err(format!("lowering pass {pass} unavailable (artifact has {:?}); rebuild native artifact", artifact.passes)); }
    }
    for (id, version) in &r.extensions {
        if artifact.extensions.get(id) != Some(version) {
            return Err(format!("extension {id}@{version} unavailable; artifact has {:?}; rebuild", artifact.extensions.get(id)));
        }
    }
    for method in &r.methods {
        if !r.specializations.iter().any(|s| s.model == method.model && artifact.specializations.contains(s)) {
            return Err(format!("{}.{}: native method requires a compiled specialization; rebuild", method.model, method.name));
        }
    }
    for result in &r.result_fields {
        if !r.specializations.iter().any(|s| s.model == result.model && artifact.specializations.contains(s)) {
            return Err(format!("{}.{}: native result requires a compiled specialization; rebuild", result.model, result.field));
        }
    }
    for specialization in &r.specializations {
        for export in &specialization.exports {
            if !artifact.exports.contains(export) {
                return Err(format!("{} requires missing export {export} (artifact has {:?}); rebuild native artifact", specialization.model, artifact.exports));
            }
        }
        if !artifact.specializations.contains(specialization) {
            return Err(format!("{}: stale or missing specialization {}; rebuild native artifact", specialization.model, specialization.fingerprint));
        }
    }
    Ok(())
}

fn validate_package_support(id: &str, package: &PackageStamp, database: &str, language: Option<&str>) -> Result<(), String> {
    if !package.databases.iter().any(|d| d == database) {
        return Err(format!("{id}@{} does not support {database}", package.version));
    }
    if language.is_some_and(|l| !package.languages.iter().any(|s| s == l)) {
        return Err(format!("{id}@{} does not support this language binding", package.version));
    }
    Ok(())
}

/// Validate active compiled contributions even when a normalized schema carries no
/// source declarations (e.g. generated code or a matching native specialization).
pub fn validate_requirements(ir: &SchemaIr, manifests: &[Manifest], language: Option<&str>) -> Result<(), String> {
    for (id, package) in &ir.behavior.packages {
        if manifests.iter().any(|m| m.id == *id && !m.is_dynamic()) {
            return Err(format!("extension {id} is in both the generated packages and the native artifact"));
        }
        validate_package_support(id, package, ir.dialect.name(), language)?;
    }
    for (id, version) in &ir.behavior.extensions {
        let manifest = manifests.iter().find(|m| m.id == *id && m.version == *version)
            .ok_or_else(|| format!("missing extension {id}@{version}; rebuild native artifact"))?;
        if !manifest.databases.iter().any(|d| d == ir.dialect.name()) {
            return Err(format!("{id}@{version} does not support {}; rebuild an appropriate profile", ir.dialect.name()));
        }
        if language.is_some_and(|language| !manifest.languages.iter().any(|l| l == language)) {
            return Err(format!("{id}@{version} does not support this binding; rebuild an appropriate profile"));
        }
    }
    for hook in &ir.behavior.hooks { validate_hook(ir, hook, manifests)?; }
    Ok(())
}

fn col_name(ty: ColType) -> String {
    serde_json::to_value(ty).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_else(|| format!("{ty:?}"))
}

/// The typed values of ABI 0.2 that one column type takes: integers, and strings, travel as one value type each.
pub fn same_value_type(a: ColType, b: ColType) -> bool {
    let family = |t: ColType| match t { ColType::Int | ColType::BigInt => ColType::Int, ColType::String | ColType::Text => ColType::String, t => t };
    family(a) == family(b)
}

/// One hook binding. The model, field and dependency checks run always. The export, point and type checks need the
/// manifest of `source`; `Plan::lower` has it, so they run at `orm generate`. A generated model carries only the stamp of the
/// package, so there `source` must be in `behavior.packages`.
fn validate_hook(ir: &SchemaIr, hook: &HookBinding, manifests: &[Manifest]) -> Result<(), String> {
    let at = |field: Option<&str>| match field { Some(f) => format!("{}.{f}", hook.model), None => hook.model.clone() };
    let who = format!("hook {} of {}", hook.export, hook.source);
    if !(1..=7).contains(&hook.point) { return Err(format!("{}: {who} has point {}; use 1 to 7", at(hook.field.as_deref()), hook.point)); }
    let model = ir.models.iter().find(|m| m.name == hook.model)
        .ok_or_else(|| format!("{who} is bound to unknown model {}", hook.model))?;
    for name in &hook.dependencies {
        if !model.fields.iter().any(|f| f.name == *name) { return Err(format!("{}: {who} depends on unknown field {name}", hook.model)); }
    }
    let field = match (hook.point, &hook.field) {
        (3, None) => None,
        (3, Some(f)) => return Err(format!("{}: {who} is a record validator (point 3); bind it to the model, not to a field", at(Some(f)))),
        (point, None) => return Err(format!("{}: {who} at point {point} needs a field", hook.model)),
        (_, Some(name)) => Some(model.fields.iter().find(|f| f.name == *name)
            .ok_or_else(|| format!("{who} is bound to unknown field {}", at(Some(name))))?),
    };
    let Some(manifest) = manifests.iter().find(|m| m.id == hook.source) else {
        if ir.behavior.packages.contains_key(&hook.source) { return Ok(()); }
        return Err(format!("{}: {who} names the source {}, but no manifest and no package of the schema has that id; select the extension or fix the source", at(hook.field.as_deref()), hook.source));
    };
    let export = manifest.exports.iter().find(|e| e.id == hook.export)
        .ok_or_else(|| format!("{}: {who} is not in the manifest of {}", at(hook.field.as_deref()), hook.source))?;
    if export.point != Some(hook.point) {
        return Err(format!("{}: {who} is bound at point {}, and the manifest says point {}", at(hook.field.as_deref()), hook.point, export.point.map_or("none".into(), |p| p.to_string())));
    }
    let Some(field) = field else { return Ok(()) };
    let mismatch = |role: &str, export_type: ColType, wanted: String| format!(
        "{}.{}: {who} {role} {} but the field is {wanted}", model.name, field.name, col_name(export_type));
    if field.array { return Err(format!("{}.{}: {who} cannot bind an array field; ABI 0.2 has no typed array value", model.name, field.name)); }
    let field_type = format!("{}{}", col_name(field.ty), if field.nullable { "?" } else { "" });
    // A point-1 transform and a point-4 or 5 hook give back a value of the field type; points 6 and 7 give back query IR as json.
    if matches!(hook.point, 1 | 2 | 4) {
        if let Some(input) = export.input {
            if !same_value_type(input, field.ty) { return Err(mismatch("takes", input, field_type)); }
        }
    }
    match (hook.point, export.output) {
        (1 | 4 | 5, Some(output)) if !same_value_type(output, field.ty) => return Err(mismatch("returns", output, field_type)),
        (1 | 4 | 5, None) => return Err(format!("{}.{}: {who} has no output type for a value of {field_type}", model.name, field.name)),
        (6 | 7, Some(output)) if output != ColType::Json => return Err(mismatch("returns", output, "bound to query IR, which is json".into())),
        (6 | 7, None) => return Err(format!("{}.{}: {who} must return json (query IR)", model.name, field.name)),
        (2, Some(output)) => return Err(mismatch("returns", output, "validated, so it returns nothing".into())),
        _ => {}
    }
    Ok(())
}

/// Physical schema state is explicit. Removing or renaming a logical field does not
/// alter this snapshot; a compiler pass must contribute physical changes here.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PhysicalSchema { pub models: Vec<crate::ir::ModelIr> }

/// A later batch (for example a second class `prepare()`) adds the owners of its new
/// tables; the snapshot of earlier batches stays unchanged.
/// Test support for pass authors. Captures storage and runs `pass` on a copy of `ir`, as
/// the generated `prepare` does, and returns the IR sections that the pass changed
/// (`models`, `enums`, `behavior.<key>`, ...) to compare with its declared effects.
/// Then marks the batch lowered and fails if a second run changes anything.
pub fn pass_effects(ir: &SchemaIr, pass: impl Fn(&mut SchemaIr) -> Result<(), String>) -> Result<std::collections::BTreeSet<String>, String> {
    fn sections(value: &serde_json::Value, prefix: &str, out: &mut BTreeMap<String, serde_json::Value>) {
        for (key, value) in value.as_object().into_iter().flatten() {
            if prefix.is_empty() && key == "behavior" { sections(value, "behavior.", out); }
            else { out.insert(format!("{prefix}{key}"), value.clone()); }
        }
    }
    let snapshot = |ir: &SchemaIr| -> Result<BTreeMap<String, serde_json::Value>, String> {
        let mut out = BTreeMap::new();
        sections(&serde_json::to_value(ir).map_err(|e| e.to_string())?, "", &mut out);
        Ok(out)
    };
    let changed = |a: &BTreeMap<String, serde_json::Value>, b: &BTreeMap<String, serde_json::Value>| -> std::collections::BTreeSet<String> {
        a.keys().chain(b.keys()).filter(|k| a.get(*k) != b.get(*k)).cloned().collect()
    };
    let mut lowered: SchemaIr = serde_json::from_value(serde_json::to_value(ir).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    capture_storage(&mut lowered)?;
    let before = snapshot(&lowered)?;
    pass(&mut lowered)?;
    let effects = changed(&before, &snapshot(&lowered)?);
    for declaration in &mut lowered.behavior.declarations { declaration.lowered = true; }
    lowered.behavior.lowered_models = lowered.models.iter().map(|m| m.name.clone()).collect();
    capture_storage(&mut lowered)?;
    let once = snapshot(&lowered)?;
    pass(&mut lowered)?;
    let again = changed(&once, &snapshot(&lowered)?);
    if !again.is_empty() { return Err(format!("the pass lowered an already lowered batch again; it changed {again:?}")); }
    Ok(effects)
}

pub fn capture_storage(ir: &mut SchemaIr) -> Result<(), String> {
    let mut models = physical_models(ir)?;
    match &mut ir.behavior.storage {
        None => ir.behavior.storage = Some(PhysicalSchema { models }),
        Some(storage) => {
            models.retain(|m| !ir.behavior.lowered_models.contains(&m.name)
                && !storage.models.iter().any(|s| s.name == m.name || s.table == m.table));
            storage.models.extend(models);
        }
    }
    Ok(())
}

/// The capability, and Cargo feature, of compiled query defaults.
pub const QUERY_DEFAULTS: &str = "query-defaults";

pub const HOST_CAPABILITIES: &[&str] = &[
    "schema-transformations", "physical-schema", "native-string-values",
    "native-string-records", "native-string-results", "file-storage", QUERY_DEFAULTS,
    "model-composition", "proxy-models", "generic-relations",
];

/// Capabilities that a build selects as the host Cargo feature of the same name.
pub const HOST_FEATURE_CAPABILITIES: &[&str] = &[QUERY_DEFAULTS, "model-composition", "proxy-models", "generic-relations"];

/// Combine one new declaration batch with an immutable definition context. Lowered
/// declarations retain their phase state; new declarations are the only pass inputs.
pub fn merge_definition(mut context: SchemaIr, mut incoming: SchemaIr) -> Result<SchemaIr, String> {
    if context.dialect != incoming.dialect { return Err("schemas in one registry must target the same database".into()); }
    match (&context.identities, incoming.identities.take()) {
        (Some(old), Some(new)) if old != &new => return Err("different identity manifests in one registry; define the complete application schema together".into()),
        (None, new) => context.identities = new,
        _ => {}
    }
    for model in &incoming.models {
        if context.models.iter().any(|m| m.name == model.name) {
            return Err(format!("a model named {} is already registered", model.name));
        }
    }
    for e in incoming.enums.drain(..) {
        if let Some(known) = context.enums.iter().find(|k| k.name == e.name) {
            if known != &e { return Err(format!("an enum named {} is already registered", e.name)); }
        } else { context.enums.push(e); }
    }
    for t in incoming.types.drain(..) {
        if let Some(known) = context.types.iter().find(|k| k.name == t.name) {
            let same = serde_json::to_value(known).map_err(|e| e.to_string())? == serde_json::to_value(&t).map_err(|e| e.to_string())?;
            if !same { return Err(format!("a type named {} is already registered", t.name)); }
        } else { context.types.push(t); }
    }
    fn append<T: Serialize>(target: &mut Vec<T>, incoming: Vec<T>) -> Result<(), String> {
        for value in incoming {
            let encoded = serde_json::to_value(&value).map_err(|e| e.to_string())?;
            if !target.iter().any(|v| serde_json::to_value(v).ok().as_ref() == Some(&encoded)) { target.push(value); }
        }
        Ok(())
    }
    append(&mut context.extensions, std::mem::take(&mut incoming.extensions))?;
    append(&mut context.functions, std::mem::take(&mut incoming.functions))?;
    append(&mut context.catalog, std::mem::take(&mut incoming.catalog))?;
    append(&mut context.partitions, std::mem::take(&mut incoming.partitions))?;
    append(&mut context.native_expression_slots, std::mem::take(&mut incoming.native_expression_slots))?;
    let prior_physical = context.behavior.storage.take();
    let new_physical = incoming.behavior.storage.take();
    if prior_physical.is_some() || new_physical.is_some() {
        let prior = match prior_physical {
            Some(p) => p.models,
            None => physical_models(&context)?,
        };
        let new = match new_physical {
            Some(p) => p.models,
            None => physical_models(&incoming)?,
        };
        let mut models = prior;
        for model in new {
            if models.iter().any(|m| m.name == model.name) { return Err(format!("duplicate physical owner {}", model.name)); }
            models.push(model);
        }
        context.behavior.storage = Some(PhysicalSchema { models });
    }
    context.models.extend(incoming.models);
    let c = &mut context.behavior;
    let n = incoming.behavior;
    if c.schema_contract != 0 && n.schema_contract != 0 && c.schema_contract != n.schema_contract {
        return Err("incompatible schema contracts; rebuild dependent schemas together".into());
    }
    c.schema_contract = c.schema_contract.max(n.schema_contract);
    c.schema_contract_minor = c.schema_contract_minor.max(n.schema_contract_minor);
    if !c.completed_passes.is_empty() && !n.completed_passes.is_empty() && c.completed_passes != n.completed_passes {
        return Err("incompatible lowering passes; rebuild dependent schemas together".into());
    }
    if c.completed_passes.is_empty() { c.completed_passes = n.completed_passes; }
    c.lowered_models.extend(n.lowered_models);
    c.lowered_models.sort(); c.lowered_models.dedup();
    c.declarations.extend(n.declarations);
    c.proxy_models.extend(n.proxy_models);
    c.generic_relations.extend(n.generic_relations);
    c.generic_reverse.extend(n.generic_reverse);
    c.read_defaults.extend(n.read_defaults);
    c.specializations.extend(n.specializations);
    c.result_fields.extend(n.result_fields);
    c.methods.extend(n.methods);
    c.field_storage.extend(n.field_storage);
    c.owner_links.extend(n.owner_links);
    c.field_adapters.extend(n.field_adapters);
    c.file_fields.extend(n.file_fields);
    c.write_assignments.extend(n.write_assignments);
    c.delete_rewrites.extend(n.delete_rewrites);
    c.instance_guards.extend(n.instance_guards);
    c.hooks.extend(n.hooks);
    for (id, data) in n.extension_data {
        if c.extension_data.insert(id.clone(), data).is_some() {
            return Err(format!("extension_data of {id} is declared twice; keep one entry per extension"));
        }
    }
    for t in n.type_models {
        match c.type_models.iter().find(|known| known.name == t.name) {
            Some(known) if serde_json::to_value(known).ok() != serde_json::to_value(&t).ok() => return Err(format!("{} is declared as an abstract type twice with different content; rebuild dependent schemas together", t.name)),
            Some(_) => {}
            None => c.type_models.push(t),
        }
    }
    for (base, child) in n.model_overrides {
        if c.model_overrides.get(&base).is_some_and(|known| known != &child) { return Err(format!("{base} is overridden by two models; rebuild dependent schemas together")); }
        c.model_overrides.insert(base, child);
    }
    for (id, version) in n.extensions {
        if c.extensions.get(&id).is_some_and(|v| v != &version) { return Err(format!("incompatible extension {id}; rebuild dependent schemas together")); }
        c.extensions.insert(id, version);
    }
    Ok(context)
}
fn physical_models(ir: &SchemaIr) -> Result<Vec<crate::ir::ModelIr>, String> {
    let mut models: Vec<crate::ir::ModelIr> = serde_json::from_value(serde_json::to_value(&ir.models).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    for model in &mut models {
        model.fields.retain(|f| !ir.behavior.result_fields.iter().any(|r| r.model == model.name && r.field == f.name));
    }
    Ok(models)
}

/// Physical assumptions relevant to one specialization, including inherited owners.
pub fn specialization_storage(model: &crate::ir::ModelIr, requirements: &Requirements, computed: &[String]) -> Result<Vec<crate::ir::ModelIr>, String> {
    if let Some(storage) = &requirements.storage {
        let mut names: std::collections::BTreeSet<String> = requirements.field_storage.iter().filter(|f| f.model == model.name).map(|f| f.owner.clone()).collect();
        if let Some(local) = storage.models.iter().find(|m| m.table == model.table) { names.insert(local.name.clone()); }
        loop {
            let count = names.len();
            for link in &requirements.owner_links { if names.contains(&link.child) { names.insert(link.parent.clone()); } }
            if names.len() == count { break; }
        }
        return serde_json::from_value(serde_json::to_value(storage.models.iter().filter(|m| names.contains(&m.name)).collect::<Vec<_>>()).map_err(|e| e.to_string())?).map_err(|e| e.to_string());
    }
    let mut physical: crate::ir::ModelIr = serde_json::from_value(serde_json::to_value(model).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    physical.fields.retain(|f| !computed.contains(&f.name));
    Ok(vec![physical])
}

/// A named read default contributed by any extension. Expressions are query IR, never SQL.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReadDefault {
    pub source: String,
    pub name: String,
    pub model: String,
    #[serde(default)] pub filter: Option<serde_json::Value>,
    #[serde(default)] pub order: Option<Vec<OrderKey>>,
    #[serde(default)] pub annotate: BTreeMap<String, serde_json::Value>,
    #[serde(default)] pub load: Vec<Vec<String>>,
    #[serde(default)] pub fields: Option<Vec<String>>,
}

/// One column of a default order.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OrderKey {
    pub field: String,
    #[serde(default, skip_serializing_if = "crate::ir::is_false")] pub desc: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub nulls: Option<crate::ir::Nulls>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(languages: &[&str], databases: &[&str]) -> Manifest {
        serde_json::from_value(serde_json::json!({
            "id": "app", "version": "1.0.0", "host_contract": HOST_CONTRACT, "schema_contract": SCHEMA_CONTRACT,
            "languages": languages, "databases": databases,
            "attributes": [{"name": "app.trim", "target": "field"}],
        })).unwrap()
    }
    fn schema(dialect: crate::dialect::Dialect) -> SchemaIr {
        let mut ir = SchemaIr { dialect, ..SchemaIr::default() };
        ir.behavior.declarations.push(Declaration {
            attribute: "app.trim".into(), model: "User".into(), field: Some("name".into()), field_type: None,
            arguments: BTreeMap::new(), positional: vec![],
            location: SourceLocation { file: "schema.prisma".into(), line: 3, column: 7 }, lowered: false,
        });
        ir
    }

    #[test]
    fn packaged_declarations_and_requirements_check_the_stamped_support() {
        let mut ir = schema(crate::dialect::Dialect::Sqlite);
        ir.behavior.declarations[0].lowered = true;
        let mut stamp = PackageStamp { version: "1.0.0".into(), attributes: vec!["app.trim".into()], ..PackageStamp::default() };
        // Deserialize the stamped manifest data, as generated models do.
        let mut value = serde_json::to_value(&stamp).unwrap();
        value["languages"] = serde_json::json!(["python"]);
        value["databases"] = serde_json::json!(["sqlite"]);
        stamp = serde_json::from_value(value).unwrap();
        ir.behavior.packages.insert("app".into(), stamp);
        assert!(validate_declarations(&ir, &[], Some("python")).is_ok());
        assert!(validate_declarations(&ir, &[], Some("typescript")).unwrap_err().contains("does not support this language binding"));
        ir.behavior.declarations.clear();
        assert!(validate_requirements(&ir, &[], Some("typescript")).unwrap_err().contains("does not support this language binding"));
        ir.dialect = crate::dialect::Dialect::Postgres;
        assert!(validate_requirements(&ir, &[], Some("python")).unwrap_err().contains("does not support postgres"));
    }

    #[test]
    fn a_disabled_extension_error_names_the_namespace_and_the_readme() {
        let error = validate_declarations(&schema(crate::dialect::Dialect::Sqlite), &[], None).unwrap_err();
        assert_eq!(error, "schema.prisma:3:7: @app.trim: not compiled into this artifact and no installed extension owns it; install its package or rebuild with its extension (namespace @app.*, see docs/README.md)");
    }

    #[test]
    fn unsupported_database_names_the_declaration() {
        let error = validate_declarations(&schema(crate::dialect::Dialect::Sqlite), &[manifest(&["python"], &["postgres"])], None).unwrap_err();
        assert!(error.starts_with("schema.prisma:3:7: @app.trim:") && error.contains("does not support sqlite"), "{error}");
    }

    #[test]
    fn only_value_arguments_take_null_and_objects() {
        let mut manifest = manifest(&["python"], &["sqlite"]);
        manifest.attributes[0].arguments = serde_json::from_value(serde_json::json!({"tags": {"kind": "list"}, "payload": {"kind": "value"}})).unwrap();
        let check = |name: &str, value: serde_json::Value| {
            let mut ir = schema(crate::dialect::Dialect::Sqlite);
            ir.behavior.declarations[0].arguments.insert(name.into(), value);
            validate_declarations(&ir, std::slice::from_ref(&manifest), None)
        };
        assert!(check("tags", serde_json::json!(["a", [1]])).is_ok());
        assert!(check("tags", serde_json::json!(["a", null])).unwrap_err().contains("wrong type for tags"));
        assert!(check("tags", serde_json::json!([{"k": 1}])).is_err());
        assert!(check("payload", serde_json::json!({"labels": [null]})).is_ok());
    }

    #[test]
    fn a_variadic_last_positional_takes_any_number_of_values() {
        let mut manifest = manifest(&["python"], &["sqlite"]);
        manifest.attributes[0].positional = serde_json::from_value(serde_json::json!([{"kind": "string", "variadic": true}])).unwrap();
        let check = |manifest: &Manifest, values: serde_json::Value| {
            let mut ir = schema(crate::dialect::Dialect::Sqlite);
            ir.behavior.declarations[0].positional = serde_json::from_value(values).unwrap();
            validate_declarations(&ir, std::slice::from_ref(manifest), None)
        };
        assert!(check(&manifest, serde_json::json!([])).is_ok());
        assert!(check(&manifest, serde_json::json!(["-a", "b", "c nulls last"])).is_ok());
        assert!(check(&manifest, serde_json::json!(["a", 1])).unwrap_err().contains("wrong type for positional argument 1"));
        manifest.attributes[0].positional[0].variadic = false;
        assert!(check(&manifest, serde_json::json!(["a", "b"])).unwrap_err().contains("too many positional arguments"));
        manifest.attributes[0].positional = serde_json::from_value(serde_json::json!([{"kind": "string", "variadic": true}, {"kind": "string"}])).unwrap();
        assert!(check(&manifest, serde_json::json!(["a"])).unwrap_err().contains("only the last positional argument"));
    }

    #[test]
    fn one_name_has_a_model_entry_and_a_field_entry() {
        let mut manifest = manifest(&["python"], &["sqlite"]);
        manifest.attributes.push(serde_json::from_value(serde_json::json!({"name": "app.trim", "target": "model", "arguments": {"all": {"kind": "boolean", "required": true}}})).unwrap());
        let mut ir = schema(crate::dialect::Dialect::Sqlite);
        assert!(validate_declarations(&ir, std::slice::from_ref(&manifest), None).is_ok());
        let field = ir.behavior.declarations[0].field.take();
        assert!(validate_declarations(&ir, std::slice::from_ref(&manifest), None).unwrap_err().contains("missing argument all"));
        manifest.attributes.pop();
        let error = validate_declarations(&ir, std::slice::from_ref(&manifest), None).unwrap_err();
        assert!(error.contains("invalid declaration target: it is a field attribute; write @app.trim on a field"), "{error}");
        manifest.attributes[0].target = AttributeTarget::Model;
        ir.behavior.declarations[0].field = field;
        let error = validate_declarations(&ir, std::slice::from_ref(&manifest), None).unwrap_err();
        assert!(error.contains("invalid declaration target: it is a model attribute; write @@app.trim in the model block"), "{error}");
    }

    #[test]
    fn write_records_merge_count_and_reject_unknown_keys() {
        use crate::writes::{AssignValue, UpsertDefault, WriteAssignment, WriteKind};
        let record = WriteAssignment { source: "a".into(), model: "User".into(), field: "n".into(), on: vec![WriteKind::Update], value: AssignValue::Increment, upsert: UpsertDefault::Drop };
        let mut first = schema(crate::dialect::Dialect::Sqlite);
        first.behavior.write_assignments.push(record.clone());
        let mut second = schema(crate::dialect::Dialect::Sqlite);
        second.behavior.declarations.clear();
        second.behavior.delete_rewrites.push(crate::writes::DeleteRewrite { source: "b".into(), model: "User".into(), field: "d".into(), guard: serde_json::json!({"t": "is_null"}) });
        second.behavior.instance_guards.push(crate::writes::InstanceGuard { source: "a".into(), model: "User".into(), field: "n".into() });
        let merged = merge_definition(first, second).unwrap().behavior;
        assert_eq!((merged.write_assignments.len(), merged.delete_rewrites.len(), merged.instance_guards.len()), (1, 1, 1));
        assert!(!merged.is_empty());
        let mut value = serde_json::to_value(&record).unwrap();
        value["role"] = "x".into();
        assert!(serde_json::from_value::<WriteAssignment>(value).unwrap_err().to_string().contains("unknown field"));
        assert_eq!(serde_json::from_value::<WriteAssignment>(serde_json::json!({"source": "a", "model": "U", "field": "f", "on": ["upsert"], "value": "now"})).unwrap().upsert, UpsertDefault::Drop);
    }

    #[test]
    fn unsupported_language_fails() {
        let error = validate_declarations(&schema(crate::dialect::Dialect::Sqlite), &[manifest(&["python"], &["sqlite"])], Some("typescript")).unwrap_err();
        assert!(error.contains("does not support this language binding"), "{error}");
        assert!(validate_declarations(&schema(crate::dialect::Dialect::Sqlite), &[manifest(&["python"], &["sqlite"])], Some("python")).is_ok());
    }

    #[test]
    fn unknown_metadata_and_wrong_export_types_fail() {
        let mut value = serde_json::to_value(manifest(&["python"], &["sqlite"])).unwrap();
        value["unknown"] = serde_json::json!(true);
        assert!(serde_json::from_value::<Manifest>(value).unwrap_err().to_string().contains("unknown field"));
        let mut export = serde_json::json!({"id": "app.x", "rust": "crate::x", "kind": "string_validator", "input": "string"});
        assert!(serde_json::from_value::<Export>(export.clone()).is_ok());
        export["kind"] = "integer_validator".into();
        assert!(serde_json::from_value::<Export>(export).unwrap_err().to_string().contains("unknown variant"));
    }

    #[test]
    fn requirement_errors_name_the_model_requirement_and_artifact_contents() {
        let mut ir = SchemaIr::default();
        ir.behavior.schema_contract = SCHEMA_CONTRACT;
        ir.behavior.completed_passes = vec!["app.lower".into()];
        let artifact = Artifact { passes: vec!["other.lower".into()], ..Artifact::default() };
        let error = check_requirements(&ir, &artifact).unwrap_err();
        assert!(error.contains("app.lower") && error.contains("other.lower") && error.contains("rebuild"), "{error}");
        ir.behavior.completed_passes.clear();
        ir.behavior.specializations = vec![Specialization { model: "User".into(), fingerprint: "f".into(), exports: vec!["app.trim".into()] }];
        let artifact = Artifact { exports: vec!["app.other".into()], ..Artifact::default() };
        let error = check_requirements(&ir, &artifact).unwrap_err();
        assert!(error.starts_with("User requires missing export app.trim") && error.contains("app.other") && error.contains("rebuild"), "{error}");
        let artifact = Artifact { exports: vec!["app.trim".into()], ..Artifact::default() };
        let error = check_requirements(&ir, &artifact).unwrap_err();
        assert!(error.starts_with("User: stale or missing specialization f") && error.contains("rebuild"), "{error}");
    }

    #[test]
    fn the_state_of_another_artifact_fails_the_requirements() {
        let state = |change: &dyn Fn(&mut SchemaIr)| {
            let mut ir = SchemaIr::default();
            ir.behavior.schema_contract = SCHEMA_CONTRACT;
            change(&mut ir);
            ir
        };
        let storage = |ir: &mut SchemaIr| ir.behavior.storage = Some(PhysicalSchema { models: vec![] });
        let artifact = Artifact::default();
        let error = check_requirements(&state(&|ir| { ir.behavior.extensions.insert("acme.lib".into(), "1.0.0".into()); }), &artifact).unwrap_err();
        assert!(error.contains("acme.lib@1.0.0") && error.contains("rebuild"), "{error}");
        let mut other = state(&|_| {});
        other.behavior.schema_contract = SCHEMA_CONTRACT + 1;
        let error = check_requirements(&other, &artifact).unwrap_err();
        assert!(error.contains(&format!("schema contract {}", SCHEMA_CONTRACT + 1)) && error.contains("rebuild"), "{error}");
        let error = check_requirements(&state(&storage), &artifact).unwrap_err();
        assert!(error.contains("physical schema") && error.contains("rebuild"), "{error}");
        let capable = Artifact { capabilities: vec!["physical-schema".into()], ..Artifact::default() };
        assert!(check_requirements(&state(&storage), &capable).is_ok());
    }

    #[test]
    fn a_manifest_of_another_contract_version_names_both_versions_and_the_fix() {
        let check = |change: &dyn Fn(&mut serde_json::Value)| {
            let mut value = serde_json::to_value(manifest(&["python"], &["sqlite"])).unwrap();
            change(&mut value);
            serde_json::from_value::<Manifest>(value).unwrap().check_contracts()
        };
        let next_minor = |v: ContractVersion| format!("{}.{}", v.major, v.minor + 1);
        assert!(check(&|_| {}).is_ok());
        let error = check(&|v| v["schema_contract"] = format!("{}.0", SCHEMA_CONTRACT_VERSION.major + 1).into()).unwrap_err();
        assert!(error.contains("schema contract 3.0") && error.contains("schema contract 2.1") && error.contains("major versions differ"), "{error}");
        let error = check(&|v| v["schema_contract"] = next_minor(SCHEMA_CONTRACT_VERSION).into()).unwrap_err();
        assert!(error.contains("schema contract 2.2 is newer than this engine (schema contract 2.1)") && error.contains("update the ORM"), "{error}");
        let error = check(&|v| v["host_contract"] = next_minor(HOST_CONTRACT_VERSION).into()).unwrap_err();
        assert!(error.contains("host contract 1.1 is newer than this engine (host contract 1.0)"), "{error}");
        let error = check(&|v| { v["abi"] = next_minor(ABI_VERSION).into(); v["library"] = "lib".into(); }).unwrap_err();
        assert!(error.contains("ABI 0.3 is newer than this engine (ABI 0.2)") && error.contains("update the ORM"), "{error}");
        let error = check(&|v| { v.as_object_mut().unwrap().remove("host_contract"); }).unwrap_err();
        assert!(error.contains("app: a manifest with no `abi` needs `host_contract`"), "{error}");
    }

    fn hook_ir(binding: serde_json::Value) -> SchemaIr {
        let mut ir: SchemaIr = serde_json::from_value(serde_json::json!({
            "models": [{"name": "User", "table": "users", "fields": [
                {"name": "id", "column": "id", "type": "int", "primary_key": true},
                {"name": "name", "column": "name", "type": "string"},
                {"name": "age", "column": "age", "type": "int", "nullable": true},
                {"name": "tags", "column": "tags", "type": "string", "array": true},
            ]}],
            "behavior": {"hooks": [binding]},
        })).unwrap();
        ir.behavior.stamp_minor();
        ir
    }
    fn hook_manifest(exports: serde_json::Value) -> Manifest {
        serde_json::from_value(serde_json::json!({
            "id": "acme.lib", "version": "1.0.0", "schema_contract": "2.1", "abi": "0.2", "library": "lib",
            "languages": ["python"], "databases": ["sqlite"], "exports": exports,
        })).unwrap()
    }
    fn binding(point: u8, field: Option<&str>, export: &str) -> serde_json::Value {
        let mut value = serde_json::json!({"source": "acme.lib", "point": point, "model": "User", "export": export});
        if let Some(field) = field { value["field"] = field.into(); }
        value
    }

    #[test]
    fn a_hook_binding_of_the_wrong_type_names_the_model_field_export_and_both_types() {
        let manifest = hook_manifest(serde_json::json!([{"id": "acme.lib.check", "point": 2, "input": "int"}]));
        let error = validate_requirements(&hook_ir(binding(2, Some("name"), "acme.lib.check")), &[manifest.clone()], None).unwrap_err();
        for part in ["User.name", "acme.lib.check", "takes int", "string"] {
            assert!(error.contains(part), "{part} is missing from: {error}");
        }
        assert!(validate_requirements(&hook_ir(binding(2, Some("age"), "acme.lib.check")), &[manifest], None).is_ok());
    }

    #[test]
    fn a_hook_binding_checks_the_output_the_point_and_the_export() {
        let exports = hook_manifest(serde_json::json!([
            {"id": "acme.lib.up", "point": 1, "input": "string", "output": "int"},
            {"id": "acme.lib.up_ok", "point": 1, "input": "string", "output": "text"},
            {"id": "acme.lib.expr", "point": 6, "output": "string"},
            {"id": "acme.lib.rec", "point": 3},
        ]));
        let check = |b: serde_json::Value| validate_requirements(&hook_ir(b), &[exports.clone()], None);
        let error = check(binding(1, Some("name"), "acme.lib.up")).unwrap_err();
        assert!(error.contains("User.name") && error.contains("acme.lib.up") && error.contains("returns int") && error.contains("string"), "{error}");
        assert!(check(binding(1, Some("name"), "acme.lib.up_ok")).is_ok());
        assert!(check(binding(6, Some("name"), "acme.lib.expr")).unwrap_err().contains("returns string"));
        assert!(check(binding(3, None, "acme.lib.rec")).is_ok());
        assert!(check(binding(2, Some("name"), "acme.lib.up")).unwrap_err().contains("is bound at point 2, and the manifest says point 1"));
        assert!(check(binding(2, Some("name"), "acme.lib.missing")).unwrap_err().contains("is not in the manifest of acme.lib"));
        assert!(check(binding(1, Some("tags"), "acme.lib.up_ok")).unwrap_err().contains("array"));
    }

    #[test]
    fn a_hook_binding_on_an_unknown_model_field_or_dependency_fails() {
        let check = |b: serde_json::Value| validate_requirements(&hook_ir(b), &[], None);
        assert!(check(binding(2, Some("nope"), "x")).unwrap_err().contains("unknown field User.nope"));
        let mut other = binding(2, Some("name"), "x");
        other["model"] = "Ghost".into();
        assert!(check(other).unwrap_err().contains("unknown model Ghost"));
        let mut depends = binding(5, Some("name"), "x");
        depends["dependencies"] = serde_json::json!(["missing"]);
        assert!(check(depends).unwrap_err().contains("depends on unknown field missing"));
        assert!(check(binding(2, None, "x")).unwrap_err().contains("needs a field"));
        assert!(check(binding(3, Some("name"), "x")).unwrap_err().contains("bind it to the model"));
        assert!(check(binding(9, Some("name"), "x")).unwrap_err().contains("use 1 to 7"));
    }

    #[test]
    fn a_hook_needs_the_manifest_or_the_package_stamp_of_its_source() {
        let statics = [manifest(&["python"], &["sqlite"])];
        let mut ir = hook_ir(binding(2, Some("name"), "acme.lib.check"));
        let error = validate_requirements(&ir, &statics, None).unwrap_err();
        for part in ["User.name", "acme.lib.check", "names the source acme.lib", "no manifest and no package"] {
            assert!(error.contains(part), "{part} is missing from: {error}");
        }
        // A generated model carries the stamp of the package, not its manifest.
        let stamp = PackageStamp { version: "1.0.0".into(), databases: vec![ir.dialect.name().into()], ..PackageStamp::default() };
        ir.behavior.packages.insert("acme.lib".into(), stamp);
        assert!(validate_requirements(&ir, &statics, None).is_ok());
    }

    #[test]
    fn contract_2_1_records_set_the_minor_and_merge() {
        let ir = hook_ir(binding(2, Some("name"), "acme.lib.check"));
        assert_eq!(ir.behavior.schema_contract_minor, SCHEMA_CONTRACT_MINOR);
        assert!(!ir.behavior.is_empty());
        let json = serde_json::to_string(&ir).unwrap();
        assert!(json.contains("\"schema_contract_minor\":1"), "{json}");
        assert!(!serde_json::to_string(&SchemaIr::default()).unwrap().contains("schema_contract_minor"));
        let mut newer = SchemaIr::default();
        newer.behavior.schema_contract = SCHEMA_CONTRACT;
        newer.behavior.schema_contract_minor = SCHEMA_CONTRACT_MINOR + 1;
        let error = check_requirements(&newer, &Artifact::default()).unwrap_err();
        assert!(error.contains("schema contract 2.2 is newer than this engine (2.1)"), "{error}");
        let data = |id: &str| { let mut ir = SchemaIr::default(); ir.behavior.extension_data.insert(id.into(), serde_json::json!({"prefix": "x"})); ir.behavior.stamp_minor(); ir };
        let merged = merge_definition(data("a"), data("b")).unwrap();
        assert_eq!(merged.behavior.extension_data.len(), 2);
        assert_eq!(merged.behavior.schema_contract_minor, 1);
        let error = merge_definition(data("a"), data("a")).unwrap_err();
        assert!(error.contains("extension_data of a is declared twice"), "{error}");
    }

    #[test]
    fn merge_definition_dedupes_type_models_by_name_and_rejects_a_different_one() {
        let model = |field: &str| -> crate::ir::TypeModel {
            serde_json::from_value(serde_json::json!({"name": "AbstractUser", "fields": [{"name": field, "column": field, "type": "int"}], "extended_by": ["User"]})).unwrap()
        };
        let batch = |m: crate::ir::TypeModel| { let mut ir = SchemaIr::default(); ir.behavior.type_models = vec![m]; ir };
        let merged = merge_definition(batch(model("a")), batch(model("a"))).unwrap();
        assert_eq!(merged.behavior.type_models.len(), 1);
        let error = merge_definition(batch(model("a")), batch(model("b"))).unwrap_err();
        assert!(error.contains("AbstractUser") && error.contains("rebuild"), "{error}");
    }

    #[test]
    fn pass_effects_reports_sections_and_rejects_a_second_lowering() {
        let ir = schema(crate::dialect::Dialect::Sqlite);
        let once = |ir: &mut SchemaIr| -> Result<(), String> {
            if ir.behavior.declarations.iter().any(|d| !d.lowered) { ir.behavior.completed_passes.push("app.lower".into()); }
            Ok(())
        };
        assert_eq!(pass_effects(&ir, once).unwrap().into_iter().collect::<Vec<_>>(), ["behavior.completed_passes"]);
        let always = |ir: &mut SchemaIr| -> Result<(), String> { ir.behavior.lowered_models.push("again".into()); Ok(()) };
        assert!(pass_effects(&ir, always).unwrap_err().contains("behavior.lowered_models"));
    }

    #[test]
    fn a_later_batch_adds_the_owners_of_its_new_tables() {
        let model = |name: &str, table: &str| -> crate::ir::ModelIr {
            serde_json::from_value(serde_json::json!({"name": name, "table": table, "fields": [{"name": "id", "column": "id", "type": "int", "primary_key": true}]})).unwrap()
        };
        let mut ir = SchemaIr { models: vec![model("User", "user")], ..SchemaIr::default() };
        capture_storage(&mut ir).unwrap();
        ir.behavior.lowered_models = vec!["User".into()];
        ir.models[0].table = "renamed".into();
        ir.models.push(model("Other", "other"));
        ir.models.push(model("ActiveUser", "user"));
        capture_storage(&mut ir).unwrap();
        let owners: Vec<_> = ir.behavior.storage.unwrap().models.into_iter().map(|m| (m.name, m.table)).collect();
        assert_eq!(owners, [("User".to_string(), "user".to_string()), ("Other".into(), "other".into())]);
    }
}
