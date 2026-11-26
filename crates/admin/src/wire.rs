//! The metadata that the UI reads, as typed output structs.
//! Their JSON Schema (`admin-schema ui`) is the source of `crates/admin/ui/src/generated/api-types.ts`.
//! A key that holds `None` is present as `null`, unless the field says `skip_serializing_if`: then the key is absent.

use std::collections::BTreeMap;

use monorm_core::ir::ColType;
use schemars::{json_schema, JsonSchema, Schema, SchemaGenerator};
use serde::Serialize;

pub const FILTER_KINDS: [&str; 8] = ["enum", "bool", "number", "datetime", "date", "uuid", "text", "relation"];
pub const DATE_KINDS: [&str; 2] = ["date", "datetime"];
pub const INLINE_STYLES: [&str; 2] = ["table", "stacked"];
pub const INLINE_SAVES: [&str; 2] = ["with_parent", "row"];
pub const LEVELS: [&str; 3] = ["info", "warning", "danger"];

fn literals(values: &[&str], null: bool) -> Schema {
    if null {
        let mut all: Vec<serde_json::Value> = values.iter().map(|v| (*v).into()).collect();
        all.push(serde_json::Value::Null);
        json_schema!({"type": ["string", "null"], "enum": all})
    } else {
        json_schema!({"type": "string", "enum": values})
    }
}

fn filter_kind(_: &mut SchemaGenerator) -> Schema {
    literals(&FILTER_KINDS, false)
}
fn date_kind(_: &mut SchemaGenerator) -> Schema {
    literals(&DATE_KINDS, false)
}
fn inline_style(_: &mut SchemaGenerator) -> Schema {
    literals(&INLINE_STYLES, false)
}
fn inline_save(_: &mut SchemaGenerator) -> Schema {
    literals(&INLINE_SAVES, false)
}
fn level(_: &mut SchemaGenerator) -> Schema {
    literals(&LEVELS, true)
}
fn placement(_: &mut SchemaGenerator) -> Schema {
    literals(&crate::site::Placement::NAMES, false)
}
fn variant(_: &mut SchemaGenerator) -> Schema {
    literals(&crate::site::VARIANTS, true)
}
fn input_kind(_: &mut SchemaGenerator) -> Schema {
    literals(&crate::hostwrite::ACTION_INPUT_KINDS, false)
}

/// One column or field: the key in a row, the title, the type and the widget.
#[derive(Serialize, JsonSchema)]
pub struct ColumnMeta {
    pub label: String,
    pub title: String,
    #[serde(rename = "type")]
    #[schemars(with = "String")]
    pub ty: ColType,
    pub nullable: bool,
    pub array: bool,
    pub widget: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub choices: Option<Vec<String>>,
    /// What an empty value shows, when the config sets a text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub empty: Option<String>,
    /// The value is computed: it has no field of its own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub computed: Option<bool>,
    /// A foreign key: the resource that the value names. The page carries the row label in `labels`.
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub ref_: Option<ColumnRef>,
    /// A key that names a record of the resource that the value of another column selects (a union link).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ref_by: Option<ColumnRefBy>,
    /// The value, as text, to a badge colour: the list and the detail page show the value as a badge.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub badge: Option<BTreeMap<String, String>>,
}

#[derive(Serialize, JsonSchema)]
pub struct ColumnRef {
    pub resource: String,
}

#[derive(Serialize, JsonSchema)]
pub struct ColumnRefBy {
    pub column: String,
    pub targets: BTreeMap<String, String>,
}

/// One filter of the filter bar. A field filter has a `type`; a relation filter has a `target`.
#[derive(Serialize, JsonSchema)]
pub struct FilterMeta {
    pub name: String,
    pub title: String,
    #[schemars(schema_with = "filter_kind")]
    pub kind: &'static str,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub ty: Option<ColType>,
    pub nullable: bool,
    pub operators: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub choices: Option<Vec<String>>,
    /// A relation filter: the resource whose key is the value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct NamedChoiceMeta {
    pub name: String,
    pub title: String,
}

#[derive(Serialize, JsonSchema)]
pub struct NamedFilterMeta {
    pub name: String,
    pub title: String,
    pub default: Option<String>,
    pub choices: Vec<NamedChoiceMeta>,
}

/// "Add" on a tab: the field of the new record, and the field of the open record that gives its value.
#[derive(Serialize, JsonSchema)]
pub struct PrefillMeta {
    pub parent: String,
    pub child: String,
}

#[derive(Serialize, JsonSchema)]
pub struct TabMeta {
    pub name: String,
    pub title: String,
    pub target: String,
    pub prefill: Vec<PrefillMeta>,
}

/// A relation field of a form: a picker (one record) or chips (many records) over the resource `target`.
#[derive(Serialize, JsonSchema)]
pub struct RelationInfo {
    pub name: String,
    pub target: String,
    /// The column of the target that names a row.
    pub label: String,
    /// The user may create a target record from the form (a modal).
    pub create: bool,
    /// The config limits the choices.
    pub choices: bool,
}

/// What a form needs to know of one writable field.
#[derive(Serialize, JsonSchema)]
pub struct FormFieldMeta {
    pub name: String,
    /// Set when a picker fills the field.
    pub relation: Option<RelationInfo>,
    /// The field holds a file reference: the form uploads and downloads it.
    pub file: bool,
    pub required: bool,
    pub default: bool,
    pub readonly_on_update: bool,
    /// The config makes the field read-only on both forms.
    pub readonly: bool,
    /// The widget after the config: the field, the nested path, the type, then the default of the column.
    pub widget: String,
    pub max_length: Option<u32>,
}

/// A block of fields in a form. `title` is null for the block without a heading.
#[derive(Serialize, JsonSchema)]
pub struct SectionMeta {
    pub title: Option<String>,
    pub fields: Vec<String>,
    pub collapsed: bool,
    /// True for the section that the server adds for the fields that no section names.
    pub other: bool,
}

#[derive(Serialize, JsonSchema)]
pub struct SectionsMeta {
    pub create: Vec<SectionMeta>,
    pub edit: Vec<SectionMeta>,
}

/// A control that only the service of the resource, or an action, reads.
#[derive(Serialize, JsonSchema)]
pub struct InputInfo {
    pub name: String,
    #[serde(rename = "type")]
    #[schemars(schema_with = "input_kind")]
    pub kind: String,
    pub label: String,
    pub required: bool,
    pub help: Option<String>,
    pub choices: Vec<String>,
}

/// The form of a resource or of an inline: whether it is writable, and what each field needs.
#[derive(Serialize, JsonSchema)]
pub struct FormMeta {
    /// False when the admin does not write to the model: `read_only_reason` says why.
    pub writable: bool,
    pub read_only_reason: Option<String>,
    pub form: Vec<FormFieldMeta>,
    /// The many-to-many relations that the form edits as chips.
    pub chips: Vec<RelationInfo>,
    /// The layout of the create form and of the edit form.
    pub sections: SectionsMeta,
    /// A module of the resource replaces the whole form: see `formModuleUrl`.
    pub form_component: bool,
    /// The package module that replaces the form (a path under the API prefix), when a package gives it.
    pub form_module: Option<String>,
    /// The controls that only the service of the resource reads: they go along with a write as `inputs`.
    pub inputs: Vec<InputInfo>,
    /// True when host code does the writes of this resource.
    pub host_write: bool,
}

#[derive(Serialize, JsonSchema)]
pub struct OrderMeta {
    pub column: String,
    pub desc: bool,
}

#[derive(Serialize, JsonSchema)]
pub struct SortableMeta {
    pub column: String,
    pub indexed: bool,
}

#[derive(Serialize, JsonSchema)]
pub struct SearchMeta {
    pub fields: Vec<String>,
    pub custom: bool,
}

/// A to-many relation that the detail page shows read-only (a fixed condition or union members): the page lists its rows as links.
#[derive(Serialize, JsonSchema)]
pub struct LinkedMeta {
    pub name: String,
    pub title: String,
    pub many: bool,
}

#[derive(Serialize, JsonSchema)]
pub struct LinkMeta {
    pub name: String,
    pub title: String,
    pub target: String,
}

/// The date field of the drill-down bar. `counts: false` skips the counts of the periods.
#[derive(Serialize, JsonSchema)]
pub struct DateHierarchyMeta {
    pub field: String,
    pub counts: bool,
    #[schemars(schema_with = "date_kind")]
    pub kind: &'static str,
}

#[derive(Serialize, JsonSchema)]
pub struct FieldsetMeta {
    pub name: String,
    pub title: String,
    pub fields: Vec<String>,
}

/// A package detail tab: the name `<package>.<tab>`, the title and the module path.
#[derive(Serialize, JsonSchema)]
pub struct PackageTabMeta {
    pub name: String,
    pub title: String,
    pub module: String,
}

/// An inline: the rows of a to-many relation on the page of the parent. `form`, `sections` and `inputs` are those of the inline page.
#[derive(Serialize, JsonSchema)]
pub struct InlineInfo {
    pub name: String,
    pub label: String,
    #[schemars(schema_with = "inline_style")]
    pub style: &'static str,
    pub max: usize,
    pub collapsed: bool,
    pub delete: bool,
    /// `with_parent`: the rows go along with the parent save. `row`: each row saves alone.
    #[schemars(schema_with = "inline_save")]
    pub save: &'static str,
    /// The resource of the row page, when the user may read it.
    pub link: Option<String>,
    pub model: String,
    pub fields: Vec<ColumnMeta>,
    /// The child fields that the parent fixes: the form does not show them.
    pub pins: Vec<String>,
    pub key: Vec<String>,
    /// The rules that a row can allow: `change`, `delete`.
    pub can: Vec<String>,
    #[serde(flatten)]
    pub form: FormMeta,
    /// Set for each user.
    #[serde(flatten)]
    pub user: Option<InlineUserKeys>,
}

/// The key of an inline that depends on the user.
#[derive(Serialize, JsonSchema)]
pub struct InlineUserKeys {
    /// The writes that this user may do. Set for each user.
    pub ops: Vec<&'static str>,
}

/// One action of a resource. `applies`: a row carries the flag `applies:<name>` that says whether the action runs on it.
#[derive(Serialize, JsonSchema)]
pub struct ActionInfo {
    pub name: String,
    pub label: String,
    #[schemars(schema_with = "placement")]
    pub placement: &'static str,
    pub confirm: Option<String>,
    /// How the button looks: `default`, `primary`, `success`, `warning` or `danger`.
    #[schemars(schema_with = "variant")]
    pub variant: Option<String>,
    /// A lucide name in kebab case.
    pub icon: Option<String>,
    /// A `detail` action with a `menu` joins the dropdown of that name in the header.
    pub menu: Option<String>,
    pub inputs: Vec<InputInfo>,
    pub applies: bool,
}

/// The metadata of one resource. The keys of the last group depend on the user.
#[derive(Serialize, JsonSchema)]
pub struct ResourceMeta {
    pub name: String,
    pub title: String,
    pub group: Option<String>,
    /// A lucide icon name.
    pub icon: Option<String>,
    pub columns: Vec<ColumnMeta>,
    pub order: Vec<OrderMeta>,
    pub page_size: usize,
    pub max_page_size: usize,
    pub sortable: Vec<SortableMeta>,
    pub sortable_explicit: bool,
    /// The labels of the columns that the list edits in its cells (`list.editable`). Empty: the list is read-only.
    pub editable: Vec<String>,
    pub search: SearchMeta,
    pub permissions: Vec<&'static str>,
    pub table: String,
    pub key: Vec<String>,
    /// The label of the column that names a record, or none when the resource has no text column for it.
    pub name_column: Option<String>,
    pub fields: Vec<ColumnMeta>,
    pub linked: Vec<LinkedMeta>,
    pub links: Vec<LinkMeta>,
    pub filters: Vec<FilterMeta>,
    pub named_filters: Vec<NamedFilterMeta>,
    /// The field of the date drill-down bar over the list, or null.
    pub date_hierarchy: Option<DateHierarchyMeta>,
    pub tabs: Vec<TabMeta>,
    pub fieldsets: Vec<FieldsetMeta>,
    pub package_tabs: Vec<PackageTabMeta>,
    pub inlines: Vec<InlineInfo>,
    pub empty: Option<String>,
    /// The rules that a row can allow: `change`, `delete`. Each row and detail carries one flag for each.
    pub can: Vec<String>,
    #[serde(flatten)]
    pub form: FormMeta,
    /// The keys of the last group: set for each user.
    #[serde(flatten)]
    pub user: Option<UserKeys>,
}

/// The keys of a resource that depend on the user.
#[derive(Serialize, JsonSchema)]
pub struct UserKeys {
    /// The actions that this user may run, by placement. Set for each user.
    pub actions: Vec<ActionInfo>,
    /// The writes that this user may do: `create`, `update`, `delete`. Set for each user.
    pub ops: Vec<&'static str>,
    /// True when this user may delete the selected rows of a list in one request. Set for each user.
    pub bulk_delete: bool,
    /// The read defaults that this user may switch off, with `?without=<name>`. Set for each user.
    pub bypass: Vec<String>,
    /// The field permissions that this user holds. Set for each user.
    pub field_permissions: Vec<String>,
}

/// The answer of `/meta` for one user: the resources that the user may read, and the permission names that the user holds.
/// A resource is the JSON of `ResourceMeta` with its `UserKeys` set.
#[derive(Serialize)]
pub struct UserMetadata {
    pub resources: Vec<serde_json::Value>,
    pub permissions: Vec<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct BrandLink {
    pub label: String,
    pub url: String,
}

/// The brand colours: `#rgb` or `#rrggbb`.
#[derive(Serialize, JsonSchema, Default)]
pub struct BrandColors {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accent: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct SwitcherService {
    pub key: String,
    pub title: String,
    /// Null for the current service: its row has no link.
    pub url: Option<String>,
    pub icon: Option<String>,
    pub color: Option<String>,
}

/// The build values that the switcher shows.
#[derive(Serialize, JsonSchema)]
pub struct SwitcherBuildMeta {
    pub commit: Option<String>,
    pub built_at: Option<String>,
    pub deployed_at: Option<String>,
    pub tree: Option<String>,
}

/// The service switcher as the server checked it. `rows` are the build rows, `subheader` the line under the title.
#[derive(Serialize, JsonSchema)]
pub struct Switcher {
    pub current: Option<String>,
    pub services: Vec<SwitcherService>,
    pub subheader: String,
    pub rows: Vec<String>,
    pub build: SwitcherBuildMeta,
}

#[derive(Serialize, JsonSchema)]
pub struct Brand {
    pub name: String,
    pub logo: Option<String>,
    pub colors: BrandColors,
    pub links: Vec<BrandLink>,
    pub environment: Option<String>,
    #[schemars(schema_with = "level")]
    pub environment_level: Option<String>,
    pub switcher: Option<Switcher>,
}

/// The root of the schema: it names the types that the UI reads. Every type that they hold is in `$defs`.
#[derive(JsonSchema)]
#[allow(dead_code)]
struct Root {
    resource: ResourceMeta,
    brand: Brand,
}

/// The JSON Schema of the metadata types, as the server writes them.
pub fn schema() -> Schema {
    schemars::generate::SchemaSettings::draft2020_12().for_serialize().into_generator().into_root_schema_for::<Root>()
}
