//! The tagged values of the config wire, lowered to the plain config that `site.rs` reads.
//!
//! A host sends a value that holds a host object as a tagged object: `{"$qs": {...}}` is a prepared query set, and
//! `{"$fn": "<local id>"}` is a host function. Rust derives the host name of each function (`queryset.<M>`,
//! `search.<M>`, `column.<M>.<name>`, with the inline tag `@<Parent>.<Child>[.<via>]` after `<M>` for a page that an
//! inline wrapper overrides) and returns the table of local id to host name. The binding registers each function
//! under the name of its id. A value without a tag is the plain config and passes through unchanged.
//!
//! The public structs below the functions are the plain config that `Site::build` parses: one place for every key.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{code, hint, Fault, Hint, PathPart, Spelling};
use monorm_core::schema::Schema;

/// The host name of each local function id.
pub type Handles = BTreeMap<String, String>;

/// The `$qs` flags that a queryset of the admin refuses, with the call that sets each.
fn refused() -> [(&'static str, String); 5] {
    [("load", "load()".into()), ("slice", "a slice".into()), ("join", hint(Hint::FromJoin)), ("lock", "lock()".into()), ("without_defaults", hint(Hint::WithoutDefaults))]
}

/// Lowers the tagged values of `config` in place and returns the function table.
pub(crate) fn lower(config: &mut Value, schema: &Schema) -> Result<Handles, Fault> {
    let mut handles = Handles::new();
    let Some(resources) = config.get_mut("resources").and_then(Value::as_array_mut) else { return Ok(handles) };
    for resource in resources {
        let Some(model) = resource.get("model").and_then(Value::as_str).map(str::to_owned) else { continue };
        lower_page(resource, &model, "", schema, &mut handles).map_err(|f| f.under(PathPart::Model(model.clone())))?;
        let Some(inlines) = resource.get_mut("inlines").and_then(Value::as_array_mut) else { continue };
        for inline in inlines {
            let Some(child) = inline.get("model").and_then(Value::as_str).map(str::to_owned) else { continue };
            let via = inline.get("via").and_then(Value::as_str).map(|v| format!(".{v}")).unwrap_or_default();
            let tag = format!("@{model}.{child}{via}");
            if let Some(overrides) = inline.get_mut("page").filter(|p| p.is_object()) {
                lower_page(overrides, &child, &tag, schema, &mut handles).map_err(|f| f.under(PathPart::Model(child.clone())))?;
            }
        }
    }
    Ok(handles)
}

/// The tag of a value: `Some((tag, body))` for an object whose first key starts with `$`.
fn tagged(value: &Value) -> Option<(&str, &Value)> {
    let map = value.as_object()?;
    let (key, body) = map.iter().next()?;
    key.starts_with('$').then_some((key.as_str(), body))
}

fn lower_page(page: &mut Value, model: &str, tag: &str, schema: &Schema, handles: &mut Handles) -> Result<(), Fault> {
    let Some(map) = page.as_object_mut() else { return Ok(()) };
    if let Some(value) = map.remove("queryset") {
        let (queryset, order) = queryset(model, tag, value, handles)?;
        map.insert("queryset".into(), queryset);
        if let Some(order) = order {
            let list = map.entry("list").or_insert_with(|| json!({}));
            let Some(list) = list.as_object_mut() else { return Ok(()) };
            if list.contains_key("order") {
                return Err(Fault::new(code::INVALID_VALUE, "the order is in the queryset or in the list, not in both"));
            }
            list.insert("order".into(), order);
        }
    }
    if let Some(list) = map.get_mut("list").and_then(Value::as_object_mut) {
        if let Some(value) = list.get("search_with") {
            if let Some(name) = host_function(value, format!("search.{model}{tag}"), handles)? {
                list.insert("search_with".into(), Value::String(name));
            }
        }
        for item in list.get_mut("computed").and_then(Value::as_array_mut).into_iter().flatten() {
            let Some(item) = item.as_object_mut() else { continue };
            let name = item.get("name").and_then(Value::as_str).unwrap_or_default().to_owned();
            if let Some(value) = item.get("function") {
                if let Some(host) = host_function(value, format!("column.{model}{tag}.{name}"), handles)? {
                    item.insert("function".into(), Value::String(host));
                }
            }
        }
    }
    if let Some(relations) = map.get_mut("relations").and_then(Value::as_object_mut) {
        for (name, options) in relations.iter_mut() {
            let Some(choices) = options.get_mut("choices") else { continue };
            let at = || Fault::new(code::INVALID_VALUE, format!("relation {name:?}: choices takes a prepared query set of the target"));
            if tagged(choices).is_none() {
                // An object without a tag is the plain config; any other value is not a query set.
                if choices.is_object() { continue } else { return Err(at()) }
            }
            let Some(("$qs", body)) = tagged(choices) else { return Err(at()) };
            let target = schema.model_idx(model).ok().and_then(|idx| schema.model(idx).relation(name).ok()).map(|(_, t)| schema.model(t).ir.name.clone());
            if let (Some(target), Some(of)) = (&target, body.get("model").and_then(Value::as_str)) {
                if target != of {
                    return Err(Fault::new(code::INVALID_VALUE, format!("relation {name:?}: choices is a query set of {of}, not of {target}")));
                }
            }
            let target = target.unwrap_or_else(|| model.to_owned());
            let (lowered, _) = queryset(&target, "", choices.clone(), handles)?;
            *choices = lowered;
        }
    }
    Ok(())
}

/// The host name of a `{"$fn": id}` value, with the id in the table. `None` for a plain name.
fn host_function(value: &Value, name: String, handles: &mut Handles) -> Result<Option<String>, Fault> {
    match tagged(value) {
        None => Ok(None),
        Some(("$fn", Value::String(id))) => {
            if handles.iter().any(|(other, taken)| *taken == name && other != id) {
                return Err(Fault::new(code::INVALID_VALUE, format!("two host functions get the name {name:?}: one inline model twice under one parent needs a different via")));
            }
            handles.insert(id.clone(), name.clone());
            Ok(Some(name))
        }
        Some((other, _)) => Err(Fault::new(code::INVALID_VALUE, format!("{other:?} is not a host function: a function is {{\"$fn\": id}}"))),
    }
}

/// The plain `queryset` config of a tagged queryset, and the order that a prepared query set sets.
fn queryset(model: &str, tag: &str, value: Value, handles: &mut Handles) -> Result<(Value, Option<Value>), Fault> {
    let wrong = || Fault::new(code::INVALID_VALUE, format!("queryset takes a prepared query set ({}) or an async function", hint(Hint::PreparedQuerySet)));
    let Some((key, body)) = tagged(&value) else {
        // The plain config of a filter or of a function name.
        return if value.is_object() { Ok((value, None)) } else { Err(wrong()) };
    };
    match key {
        "$fn" => {
            let name = host_function(&value, format!("queryset.{model}{tag}"), handles)?.ok_or_else(wrong)?;
            Ok((json!({"function": name}), None))
        }
        "$qs" => {
            let Some(qs) = body.as_object() else { return Err(wrong()) };
            if let Some(of) = qs.get("model").and_then(Value::as_str).filter(|of| *of != model) {
                return Err(Fault::new(code::INVALID_VALUE, format!("queryset is a query set of {of}")));
            }
            for (flag, what) in refused() {
                if qs.get(flag).and_then(Value::as_bool) == Some(true) {
                    return Err(Fault::new(code::UNSUPPORTED, format!("queryset takes a filter and an order; it does not take {what}")));
                }
            }
            let filter = qs.get("filter").filter(|f| !f.is_null()).ok_or_else(|| Fault::new(code::INVALID_VALUE, "queryset needs a filter"))?;
            let slots = qs.get("slots").cloned().unwrap_or_else(|| json!([]));
            let order = qs.get("order").filter(|o| o.as_array().is_some_and(|a| !a.is_empty())).cloned();
            Ok((json!({"filter": filter, "slots": slots}), order))
        }
        _ => Err(wrong()),
    }
}

// ---- The config structs: what `site.rs` and the modules read after `lower`. ----

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// How the host spells an option key in an error text: `snake` (default) or `camel`. `Site::build` reads it first.
    #[allow(dead_code)] // `Site::build` reads it before this parse; the field only lets the key through `deny_unknown_fields`.
    pub spelling: Option<Spelling>,
    /// The table of the active auth user model, supplied by the binding.
    pub user_table: Option<String>,
    /// The path under which the UI and the API live, `/admin` by default.
    pub base_path: Option<String>,
    /// The path prefix of every API route: `<base_path>/api` by default, and always under `base_path`.
    pub prefix: Option<String>,
    /// `page` (default), `model` or `access`.
    pub permissions: Option<String>,
    /// The label of the permission names, `admin` by default.
    pub label: Option<String>,
    /// The name, logo, colors, header links and environment label of the UI.
    pub brand: Option<BrandConfig>,
    /// The title, logo and brand colour of the UI: the short form of `brand`.
    pub theme: Option<ThemeConfig>,
    /// The navigation groups in order. A resource with a `group` must name one of them.
    #[serde(default)]
    pub groups: Vec<String>,
    /// The registered resources. None: every concrete model is a read-only resource.
    #[serde(default)]
    pub resources: Vec<ResourceConfig>,
    /// The time limit of each statement of a request, in ms (PostgreSQL). 0: none. Default 15000.
    pub statement_timeout_ms: Option<u64>,
    /// How long an export waits for the client to take the next chunk, in ms. 0: no limit. Default 30000.
    pub export_idle_timeout_ms: Option<u64>,
    /// Widgets for every resource: `Model.field`, `Type.subfield`, or a type name, to a widget name.
    #[serde(default)]
    pub widgets: BTreeMap<String, String>,
    /// Custom routes under `<prefix>/routes/`.
    #[serde(default)]
    pub routes: Vec<RouteConfig>,
    /// Extension packages: static widgets, form modules and styles under `<prefix>/packages/`.
    #[serde(default)]
    pub packages: Vec<PackageConfig>,
    #[serde(default)]
    pub frame_origins: Vec<String>,
    #[serde(default)]
    pub frame_cards: Vec<FrameConfig>,
    #[serde(default)]
    pub embeds: BTreeMap<String, EmbedConfig>,
}

/// The rows a resource shows to staff.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuerysetConfig {
    /// A prepared filter. A `param` with index `i` reads `slots[i]`.
    pub filter: Option<Value>,
    /// What each parameter holds at a request: `user` (the key of the staff user).
    #[serde(default)]
    pub slots: Vec<String>,
    /// A host function that returns `{"filter": expr}` at each request, instead of `filter`.
    pub function: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceConfig {
    pub model: String,
    #[serde(default)]
    pub list: ListConfig,
    /// The operations that the resource offers, for every user: `read`, `create`, `update`, `delete`.
    pub allow: Option<Vec<String>>,
    /// Page levels: `read`, `manage` and, to split delete from manage, `delete`.
    pub perms: Option<Vec<String>>,
    /// Overrides the site label in the names of this resource.
    pub label: Option<String>,
    /// The name in the navigation and the page heading. The model name, spaced, by default.
    pub title: Option<String>,
    /// The navigation group of the resource.
    pub group: Option<String>,
    /// Names of field permissions (`see_cost`): `<label>.<name>`.
    #[serde(default)]
    pub field_permissions: Vec<String>,
    /// Field to permission name: only a user with that name sees the field. A bound name is declared as a field permission.
    #[serde(default)]
    pub see: BTreeMap<String, String>,
    /// Field to permission name: only a user with that name changes the field, the others see it read-only.
    #[serde(default)]
    pub change: BTreeMap<String, String>,
    /// Actions: a permission name (`<label>.<action>_<resource>`), a placement, a confirmation and inputs.
    #[serde(default)]
    pub actions: Vec<ActionConfig>,
    /// Bulk delete is on by default: `false` removes it from the list.
    #[serde(default = "yes")]
    pub bulk_delete: bool,
    #[serde(default)]
    pub set_password: bool,
    /// The rows that staff see and touch.
    pub queryset: Option<QuerysetConfig>,
    /// Per-row rules, `change` and `delete`: prepared expressions that the list and the detail select as flags.
    #[serde(default)]
    pub can: BTreeMap<String, Value>,
    /// The text of a form message for a named `@@check`: the name of the constraint, then the text.
    #[serde(default)]
    pub check_messages: BTreeMap<String, String>,
    /// Host functions that run the writes: `create`, `update`, `delete`, each with `sync` for a plain Python function.
    #[serde(default)]
    pub service: ServiceConfig,
    /// Controls that a form shows next to the fields. Only a service reads them.
    #[serde(default)]
    pub inputs: Vec<InputConfig>,
    /// The resource opts in to writes of a protected model.
    #[serde(default)]
    pub allow_protected_writes: bool,
    /// The model has host behavior that the host knows (a field adapter): its writes run through the host.
    #[serde(default)]
    pub host_behavior: bool,
    /// Sections: a list, or `{create, edit}` with a list for each form. A field that no section names goes last.
    pub form: Option<Value>,
    /// Fields that the form shows and a request cannot write.
    #[serde(default)]
    pub readonly: Vec<String>,
    /// Bytes fields that the host shows as another value: the field to `json` (the host converts both ways) or `text` (read-only).
    #[serde(default)]
    pub views: BTreeMap<String, String>,
    /// Widgets of this resource: a field, `Type.subfield`, or a type name, to a widget name.
    #[serde(default)]
    pub widgets: BTreeMap<String, String>,
    /// The path of a JavaScript module that replaces the form.
    pub form_component: Option<String>,
    /// Package detail tabs (`<package>.<tab>`) that the detail page shows after the relation tabs.
    #[serde(default)]
    pub detail_tabs: Vec<String>,
    #[serde(default)]
    pub frames: Vec<FrameConfig>,
    #[serde(default)]
    pub fieldsets: Vec<Fieldset>,
    /// Relation fields with a picker (to-one) or chips (many-to-many), by relation name.
    #[serde(default)]
    pub relations: BTreeMap<String, RelationConfig>,
    /// Json fields that hold an uploaded file: the form uploads and downloads them.
    #[serde(default)]
    pub files: Vec<String>,
    /// To-many relations whose rows the detail page edits in place. `Site::build` merges the page of the child into each one.
    #[serde(default)]
    pub inlines: Vec<InlineConfig>,
}

/// The wrapper of an inline: what a page cannot say. `page` holds the page keys that the wrapper overrides, merged over the registered page of the child model.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InlineConfig {
    pub model: String,
    /// The to-many relation of the parent that leads to the child model. Needed when more than one does.
    pub via: Option<String>,
    /// `with_parent` (default): the rows save with the parent in one change set. `row`: each row saves alone.
    pub save: Option<String>,
    /// The name of a permission (`<label>.<name>`) that replaces the write permission of the parent.
    pub permission: Option<String>,
    /// `table` (default) or `stacked`.
    pub style: Option<String>,
    /// The most rows an inline shows and holds. Default 20, at most 200.
    pub max: Option<usize>,
    pub label: Option<String>,
    #[serde(default)]
    pub collapsed: bool,
    /// Whether a row can be deleted. Default true.
    pub delete: Option<bool>,
    pub page: Option<Box<ResourceConfig>>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ListConfig {
    /// Field names; `author.name` reaches a field through to-one relations. A computed or aggregate column is named here too.
    pub columns: Option<Vec<String>>,
    /// Field names of the model, `-created_at` for descending.
    pub order: Option<Vec<String>>,
    pub page_size: Option<usize>,
    /// Field names of the model that the search box reads.
    pub search: Option<Vec<String>>,
    /// The name of a host function that replaces the search: it takes the text and returns keys.
    pub search_with: Option<String>,
    /// Field names whose cells the list edits in place: list columns that are scalar fields of the model.
    #[serde(default)]
    pub editable: Vec<String>,
    /// Field names that the sort headers may use. Default: the list columns that can sort.
    pub sortable: Option<Vec<String>>,
    /// The title of a column, by column name.
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    /// What an empty value shows in every column of the list.
    pub empty: Option<String>,
    /// What an empty value shows in one column, by column name.
    #[serde(default)]
    pub empty_fields: BTreeMap<String, String>,
    /// Fields that the filter bar offers: a field, `author.name` (a field of a to-one relation) or a to-one relation.
    #[serde(default)]
    pub filters: Vec<String>,
    /// Filters made of prepared expressions: one choice at a time.
    #[serde(default)]
    pub named_filters: Vec<NamedFilterConfig>,
    /// Columns that a SQL expression or a host function computes.
    #[serde(default)]
    pub computed: Vec<ComputedConfig>,
    /// Columns that count or sum the rows of a to-many relation.
    #[serde(default)]
    pub aggregates: Vec<AggregateConfig>,
    /// The to-many relations that the detail page shows as tabs. Default: every one that leads to a resource.
    pub tabs: Option<Vec<String>>,
    /// A column name to a map of value (as text) to a colour of the palette: the list and the detail page show the value as a badge.
    #[serde(default)]
    pub badges: BTreeMap<String, BTreeMap<String, String>>,
    /// A date or datetime field: the list shows a bar of the years, months and days with their row counts.
    pub date_hierarchy: Option<DateHierarchyConfig>,
}

/// `date_hierarchy: "created"` or `date_hierarchy: {field: "created", counts: false}`.
#[derive(Deserialize)]
#[serde(untagged)]
pub enum DateHierarchyConfig {
    Field(String),
    Full(DateHierarchyFull),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DateHierarchyFull {
    pub field: String,
    /// False: the bar shows the periods without counts and the route runs no counting statement.
    pub counts: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamedFilterConfig {
    pub name: String,
    pub label: Option<String>,
    pub choices: Vec<NamedChoiceConfig>,
    /// The choice that applies when the URL names none. None: all rows.
    pub default: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamedChoiceConfig {
    pub name: String,
    pub label: Option<String>,
    /// The condition, as an expression of the engine IR.
    pub expr: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputedConfig {
    pub name: String,
    pub label: Option<String>,
    /// The declared type: `int`, `bigint`, `float`, `bool`, `string`, `text`, `datetime`, `date`, `uuid`, `json` or `decimal`.
    #[serde(rename = "type")]
    pub ty: String,
    /// Whether the value may be null. Default: true.
    pub nullable: Option<bool>,
    /// A SQL expression, as an expression of the engine IR.
    pub expr: Option<Value>,
    /// The name of a host function: it takes `{"keys": [...]}` for one page and returns one value per key.
    pub function: Option<String>,
    /// Whether the column has a sort header. It needs `order_by`, or `expr` and `nullable: false`.
    pub sortable: Option<bool>,
    /// What the sort header orders by: a field path (a string) or an expression of the engine IR. It makes the column sortable.
    pub order_by: Option<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AggregateConfig {
    pub name: String,
    pub label: Option<String>,
    /// A to-many relation of the model.
    pub relation: String,
    /// `count`, `sum`, `min`, `max` or `avg`.
    #[serde(rename = "fn")]
    pub function: String,
    /// A field of the related model: every function but `count` needs one.
    pub field: Option<String>,
}

/// The config of one action as a host sends it. An entry with no placement only declares a permission.
#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct ActionConfig {
    pub name: String,
    pub label: Option<String>,
    pub placement: Option<String>,
    /// The permission name `<label>.<permission>_<resource>`. None: the action needs `manage`.
    pub permission: Option<String>,
    /// The text of the confirmation. None: no confirmation.
    pub confirm: Option<String>,
    #[serde(default)]
    pub inputs: Vec<InputConfig>,
    /// A prepared filter: the rows that the action applies to.
    pub applies_to: Option<Value>,
    /// A row permission condition with optional user slot 0.
    pub can: Option<Value>,
    #[serde(default)]
    pub replace_change: bool,
    #[serde(default)]
    pub sync: bool,
    #[serde(default)]
    pub threaded: bool,
}

#[derive(Deserialize, Default, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct ServiceConfig {
    pub create: Option<ServiceOp>,
    pub update: Option<ServiceOp>,
    pub delete: Option<ServiceOp>,
}

/// A control that the form shows next to the fields. Only a service reads its value.
#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct InputConfig {
    pub name: String,
    #[serde(rename = "type", default = "text")]
    pub kind: String,
    pub label: Option<String>,
    #[serde(default)]
    pub required: bool,
    pub help: Option<String>,
    #[serde(default)]
    pub choices: Vec<String>,
    /// The audit row stores a mask, not the value.
    #[serde(default)]
    pub secret: bool,
}

/// The config of one relation field of a resource.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RelationConfig {
    /// The rows of the target that the field accepts, on top of the `queryset` of the target.
    pub choices: Option<QuerysetConfig>,
    /// The form offers a modal that creates a record of the target.
    #[serde(default)]
    pub create: bool,
    /// The text column of the target that names a row in the picker. By default the first text column.
    pub label: Option<String>,
}

/// One route as the host sends it: `permission` is a full name, `<label>.<name>`.
#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct RouteConfig {
    pub method: String,
    pub path: String,
    pub permission: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct BrandConfig {
    /// The name in the header and the page title. `Admin` by default.
    pub name: Option<String>,
    /// The URL of the logo image.
    pub logo: Option<String>,
    /// `primary` and `accent`, as `#rgb` or `#rrggbb`.
    #[serde(default)]
    pub colors: BTreeMap<String, String>,
    /// Links in the header.
    #[serde(default)]
    pub links: Vec<LinkConfig>,
    /// A short label such as `staging`, shown in the header.
    pub environment: Option<String>,
}

/// The short form of the look of a site: the title, the logo and the brand colour. It sets the same values as `brand`
/// (`title` is `brand.name`, `color` is `brand.colors.primary`); a value in both is a config error.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ThemeConfig {
    pub title: Option<String>,
    pub logo: Option<String>,
    pub color: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkConfig {
    pub label: String,
    pub url: String,
}

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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrameConfig {
    pub name: String,
    pub title: String,
    pub url: String,
    #[serde(default)]
    pub sandbox: Vec<String>,
    pub scripts: Option<bool>,
    pub same_origin: Option<bool>,
    pub height: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fieldset {
    pub name: String,
    pub title: Option<String>,
    pub fields: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbedConfig {
    pub resource: String,
    pub tab: String,
    pub lookup: String,
    pub ancestors: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SectionConfig {
    pub title: Option<String>,
    pub fields: Vec<String>,
    #[serde(default)]
    pub collapsed: bool,
}

/// One service function of a resource. `sync` is a plain Python function; it runs in a thread.
#[derive(Deserialize, Default, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct ServiceOp {
    #[serde(default)]
    pub sync: bool,
    /// A sync function that the author marked as threaded: it runs in a thread, and its async database calls go through `ctx.run`.
    #[serde(default)]
    pub threaded: bool,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct TabOptions {
    pub module: String,
    #[serde(default)]
    pub embeddable: bool,
}

fn text() -> String {
    "text".into()
}

fn yes() -> bool {
    true
}
