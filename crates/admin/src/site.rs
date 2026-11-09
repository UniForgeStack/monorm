//! `Site::build`: the resources of an admin, checked against the prepared schema.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use monorm_core::dialect::{Dialect, Target};
use monorm_core::ir::{ColType, ConstraintIr, FieldIr, ModelIr, Operation, RelKind, ValueType};
use monorm_core::schema::Schema;
use monorm_engine::plan::{Plan, Planner};
use monorm_engine::params::null_of;
use monorm_engine::{Params, Result as EngineResult};
use serde::Deserialize;
use serde_json::Value;

use crate::key;
use crate::meta::{self, BrandConfig};
use crate::permissions::{Op, Strategy};

pub const DEFAULT_PREFIX: &str = "/admin/api";
pub const DEFAULT_PAGE_SIZE: usize = 25;
pub const MAX_PAGE_SIZE: usize = 200;
/// A count stops here: the list shows `10,000+`.
pub const COUNT_CAP: usize = 10_000;
/// A table with more rows allows only the sort columns that lead an index, unless the config names the columns.
pub const LARGE_TABLE_ROWS: usize = 50_000;
pub const DEFAULT_STATEMENT_TIMEOUT_MS: u64 = 15_000;
/// PostgreSQL stores `statement_timeout` in an `int4` of milliseconds and rejects more.
pub const MAX_STATEMENT_TIMEOUT_MS: u64 = i32::MAX as u64;
/// How long a table size stays known.
const LARGE_TTL: Duration = Duration::from_secs(300);
/// The longest search text.
pub const MAX_SEARCH_LEN: usize = 200;

/// A wrong config: the text names the model and the field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError(pub String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

pub(crate) fn err<T>(message: String) -> Result<T, ConfigError> {
    Err(ConfigError(message))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Config {
    /// The path prefix of every route.
    prefix: Option<String>,
    /// `page` (default), `model` or `access`.
    permissions: Option<String>,
    /// The label of the permission names, `admin` by default.
    label: Option<String>,
    /// The name, logo, colors, header links and environment label of the UI.
    brand: Option<BrandConfig>,
    /// The navigation groups in order. A resource with a `group` must name one of them.
    #[serde(default)]
    groups: Vec<String>,
    /// The registered resources. None: every concrete model is a read-only resource.
    #[serde(default)]
    resources: Vec<ResourceConfig>,
    /// The time limit of each statement of a request, in ms (PostgreSQL). 0: none. Default 15000.
    statement_timeout_ms: Option<u64>,
}

/// The rows a resource shows to staff.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QuerysetConfig {
    /// A prepared filter. A `param` with index `i` reads `slots[i]`.
    filter: Option<Value>,
    /// What each parameter holds at a request: `user` (the key of the staff user).
    #[serde(default)]
    slots: Vec<String>,
    /// A host function that returns `{"filter": expr}` at each request, instead of `filter`.
    function: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourceConfig {
    model: String,
    #[serde(default)]
    list: ListConfig,
    /// The operations that the resource offers, for every user: `read`, `create`, `update`, `delete`.
    allow: Option<Vec<String>>,
    /// Page levels: `read`, `manage` and, to split delete from manage, `delete`.
    perms: Option<Vec<String>>,
    /// Overrides the site label in the names of this resource.
    label: Option<String>,
    /// The name in the navigation and the page heading. The model name, spaced, by default.
    title: Option<String>,
    /// The navigation group of the resource.
    group: Option<String>,
    /// Names of field permissions (`see_cost`): `<label>.<name>`.
    #[serde(default)]
    field_permissions: Vec<String>,
    /// Names of actions: `<label>.<action>_<resource>`.
    #[serde(default)]
    actions: Vec<String>,
    /// The rows that staff see and touch.
    queryset: Option<QuerysetConfig>,
    /// Per-row rules, `change` and `delete`: prepared expressions that the list and the detail select as flags.
    #[serde(default)]
    can: BTreeMap<String, Value>,
    /// The text of a form message for a named `@@check`: the name of the constraint, then the text.
    #[serde(default)]
    check_messages: BTreeMap<String, String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ListConfig {
    /// Field names; `author.name` reaches a field through to-one relations. A computed or aggregate column is named here too.
    columns: Option<Vec<String>>,
    /// Field names of the model, `-created_at` for descending.
    order: Option<Vec<String>>,
    page_size: Option<usize>,
    /// Field names of the model that the search box reads.
    search: Option<Vec<String>>,
    /// The name of a host function that replaces the search: it takes the text and returns keys.
    search_with: Option<String>,
    /// Field names that the sort headers may use. Default: the list columns that can sort.
    sortable: Option<Vec<String>>,
    /// The title of a column, by column name.
    #[serde(default)]
    labels: BTreeMap<String, String>,
    /// What an empty value shows in every column of the list.
    empty: Option<String>,
    /// What an empty value shows in one column, by column name.
    #[serde(default)]
    empty_fields: BTreeMap<String, String>,
    /// Fields that the filter bar offers: a field, `author.name` (a field of a to-one relation) or a to-one relation.
    #[serde(default)]
    filters: Vec<String>,
    /// Filters made of prepared expressions: one choice at a time.
    #[serde(default)]
    named_filters: Vec<NamedFilterConfig>,
    /// Columns that a SQL expression or a host function computes.
    #[serde(default)]
    computed: Vec<ComputedConfig>,
    /// Columns that count or sum the rows of a to-many relation.
    #[serde(default)]
    aggregates: Vec<AggregateConfig>,
    /// The to-many relations that the detail page shows as tabs. Default: every one that leads to a resource.
    tabs: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NamedFilterConfig {
    name: String,
    label: Option<String>,
    choices: Vec<NamedChoiceConfig>,
    /// The choice that applies when the URL names none. None: all rows.
    default: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NamedChoiceConfig {
    name: String,
    label: Option<String>,
    /// The condition, as an expression of the engine IR.
    expr: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ComputedConfig {
    name: String,
    label: Option<String>,
    /// The declared type: `int`, `bigint`, `float`, `bool`, `string`, `text`, `datetime`, `date`, `uuid`, `json` or `decimal`.
    #[serde(rename = "type")]
    ty: String,
    /// Whether the value may be null. Default: true.
    nullable: Option<bool>,
    /// A SQL expression, as an expression of the engine IR.
    expr: Option<Value>,
    /// The name of a host function: it takes `{"keys": [...]}` for one page and returns one value per key.
    function: Option<String>,
    /// Whether the column has a sort header. It needs `expr` and `nullable: false`.
    sortable: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AggregateConfig {
    name: String,
    label: Option<String>,
    /// A to-many relation of the model.
    relation: String,
    /// `count`, `sum`, `min`, `max` or `avg`.
    #[serde(rename = "fn")]
    function: String,
    /// A field of the related model: every function but `count` needs one.
    field: Option<String>,
}

/// Where the value of a column comes from.
#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    /// A field of the model or of a model reached through to-one relations.
    Field,
    /// An expression of the engine IR, part of the list statement.
    Expr(Value),
    /// A host function: one call for each page.
    Batch(String),
}

/// One column of a list: a field of the model or of a model reached through to-one relations.
#[derive(Debug, Clone)]
pub struct Column {
    /// `author.name`: the key of the column in a row.
    pub label: String,
    pub path: Vec<String>,
    pub field: String,
    pub ty: ValueType,
    pub nullable: bool,
    pub source: Source,
}

#[derive(Debug, Clone)]
pub struct OrderColumn {
    pub column: Column,
    pub desc: bool,
}

/// How a search field matches the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `=text`
    Exact,
    /// `^text`
    Prefix,
    /// The text alone, on a field that a trigram index serves or that has no better index.
    Contains,
    /// `@text`
    Fulltext,
}

#[derive(Debug, Clone)]
pub struct SearchField {
    pub column: Column,
    /// The mode of a text with no prefix character.
    pub default: Mode,
    /// A text field takes every mode but `Fulltext`; the others take `Exact` only.
    pub text: bool,
    /// The text search configuration of a full-text index on the field. None: no such index.
    pub fulltext: Option<String>,
    /// An index serves the default mode.
    pub indexed: bool,
}

/// A column that a sort header may use.
#[derive(Debug, Clone)]
pub struct Sortable {
    pub column: Column,
    /// The column leads an index.
    pub indexed: bool,
}

#[derive(Debug)]
pub struct Resource {
    pub model: usize,
    pub name: String,
    pub columns: Vec<Column>,
    /// The order of a list, ending with the primary key.
    pub order: Vec<OrderColumn>,
    pub page_size: usize,
    /// Positions of the order columns in `columns`: the cursor of a page reads them back.
    pub order_positions: Vec<usize>,
    /// The lowercase model name in the permission names.
    pub key: String,
    pub label: String,
    pub allow: Vec<Op>,
    /// `delete` is its own page level, and `manage` does not include it.
    pub split_delete: bool,
    pub field_permissions: Vec<String>,
    pub actions: Vec<String>,
    pub title: String,
    pub group: Option<String>,
    /// The key fields as columns, in key order.
    pub key_columns: Vec<Column>,
    /// Positions of the key columns in `columns`: a row names its record with them.
    pub key_positions: Vec<usize>,
    /// The fields of the detail page: every field of the model that the database does not generate.
    pub detail: Vec<Column>,
    /// To-one relations that lead to the key of another resource.
    pub links: Vec<Link>,
    /// The metadata that does not depend on the user, built once by `Site::build`.
    pub meta: serde_json::Value,
    pub search: Vec<SearchField>,
    pub search_with: Option<String>,
    pub sortable: Vec<Sortable>,
    /// The config names the sort columns: no table size rule.
    pub sortable_explicit: bool,
    /// The titles of columns that the config names.
    pub labels: BTreeMap<String, String>,
    /// What an empty value shows in the list.
    pub empty: Option<String>,
    pub empty_fields: BTreeMap<String, String>,
    /// The filters of the filter bar.
    pub filters: Vec<Filter>,
    pub named_filters: Vec<NamedFilter>,
    /// To-many relations that the detail page shows as tabs.
    pub tabs: Vec<Tab>,
    /// The rows that staff see and touch: its filter joins every read of the resource.
    pub queryset: Option<Queryset>,
    /// The per-row rules (`change`, `delete`): each one is a flag in the list and the detail.
    pub can: Vec<(String, Value)>,
    /// The text that a failed named `@@check` shows in the form.
    pub check_messages: BTreeMap<String, String>,
    /// The names of the named read defaults that read no scope value: each one has a bypass permission.
    pub bypass: Vec<String>,
    /// The names of the read defaults that read a `scope.<name>` value: the admin sets no scope value, so every admin read turns them off.
    pub scope_defaults: Vec<String>,
    tabs_config: Option<Vec<String>>,
    /// Whether the table is large, and when it was read.
    large: Mutex<Option<(Instant, bool)>>,
}

/// What a parameter of a `queryset` holds at a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    User,
}

#[derive(Debug, Clone)]
pub struct Queryset {
    /// A prepared filter. A `param` with index `i` reads `slots[i]`.
    pub filter: Option<Value>,
    pub slots: Vec<Slot>,
    /// A host function that returns the filter at each request.
    pub function: Option<String>,
}

/// The per-row rules. Each one is a flag of a row.
pub const CAN_NAMES: [&str; 2] = ["change", "delete"];

/// What a filter reads.
#[derive(Debug, Clone)]
pub enum FilterKind {
    Field(Column),
    /// A to-one relation: the foreign key fields on this model, in the key order of the target, and the target key.
    Relation { target: String, from: Vec<Column>, key: Vec<Column> },
}

#[derive(Debug, Clone)]
pub struct Filter {
    /// `status`, `author.name` or `author`: the name in the URL (`f.<name>`).
    pub name: String,
    pub kind: FilterKind,
}

#[derive(Debug, Clone)]
pub struct NamedChoice {
    pub name: String,
    pub label: String,
    pub expr: Value,
}

/// A filter with a few prepared conditions, one chosen at a time.
#[derive(Debug, Clone)]
pub struct NamedFilter {
    pub name: String,
    pub label: String,
    pub choices: Vec<NamedChoice>,
    pub default: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Tab {
    /// The to-many relation.
    pub name: String,
    /// The name of the child resource.
    pub target: String,
}

/// The rows of a to-many relation of one record: the child rows whose `child_to` equals the `parent_from` of the record.
#[derive(Debug)]
pub struct Related<'a> {
    pub child: &'a Resource,
    pub parent_from: Vec<Column>,
    pub child_to: Vec<Column>,
}

impl Resource {
    /// Whether the table has more than `LARGE_TABLE_ROWS` rows. `measure` runs when the answer is older than `LARGE_TTL`.
    pub async fn large<F, Fut, E>(&self, measure: F) -> Result<bool, E>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<bool, E>>,
    {
        if let Some((at, large)) = *self.large.lock().unwrap_or_else(|e| e.into_inner()) {
            if at.elapsed() < LARGE_TTL {
                return Ok(large);
            }
        }
        let large = measure().await?;
        *self.large.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), large));
        Ok(large)
    }
}

/// A to-one relation that the detail page shows as a link to the page of the target.
#[derive(Debug, Clone)]
pub struct Link {
    pub name: String,
    /// The name of the target resource.
    pub target: String,
    /// Positions in `Resource::detail` of the foreign key fields, in the key order of the target.
    pub from: Vec<usize>,
}

pub struct Site {
    pub schema: Arc<Schema>,
    pub prefix: String,
    pub strategy: Strategy,
    pub resources: Vec<Resource>,
    /// The brand of the UI, checked.
    pub brand: serde_json::Value,
    /// The navigation groups in order.
    pub groups: Vec<String>,
    /// The time limit of each statement of a request. None: no limit.
    pub statement_timeout: Option<Duration>,
    /// What an operator should fix, found when the site was built: a search field with no index.
    pub warnings: Vec<String>,
    meta_builds: AtomicUsize,
}

impl Site {
    pub fn build(schema: Arc<Schema>, config_json: &str) -> Result<Site, ConfigError> {
        let config: Config = if config_json.trim().is_empty() {
            Config::default()
        } else {
            serde_json::from_str(config_json).map_err(|e| ConfigError(format!("invalid admin config: {e}")))?
        };
        let prefix = normalize_prefix(config.prefix.as_deref().unwrap_or(DEFAULT_PREFIX))?;
        let strategy = match config.permissions.as_deref() {
            None => Strategy::Page,
            Some(text) => Strategy::parse(text).ok_or_else(|| ConfigError(format!("permissions {text:?} is not \"page\", \"model\" or \"access\"")))?,
        };
        let label = config.label.as_deref().unwrap_or("admin");
        check_name("label", label)?;
        let mut resources = Vec::new();
        let mut warnings = Vec::new();
        if config.resources.is_empty() {
            for (idx, m) in schema.models.iter().enumerate() {
                if concrete(m) {
                    resources.push(resource(&schema, idx, &ResourceConfig::of(&schema.model(idx).ir.name), strategy, label, &mut warnings)?);
                }
            }
        } else {
            let mut seen = BTreeSet::new();
            for r in &config.resources {
                let idx = schema.model_idx(&r.model).map_err(|_| ConfigError(format!("unknown model {:?}", r.model)))?;
                if !concrete(schema.model(idx)) {
                    return err(format!("{}: not a concrete model, so not a resource", r.model));
                }
                if !seen.insert(idx) {
                    return err(format!("{}: registered twice", r.model));
                }
                resources.push(resource(&schema, idx, r, strategy, label, &mut warnings)?);
            }
        }
        if prefix == key::UI_BASE && !crate::ui::FILES.is_empty() {
            return err(format!("prefix {prefix:?} is the path of the UI: every {prefix}/... path would be an API path and the pages would not load"));
        }
        let mut groups = config.groups.clone();
        for g in &groups {
            if g.trim().is_empty() || groups.iter().filter(|o| *o == g).count() > 1 {
                return err(format!("groups: {g:?} is empty or listed twice"));
            }
        }
        for r in &resources {
            match (&r.group, groups.is_empty()) {
                (Some(g), false) if !groups.contains(g) => return err(format!("{}: group {g:?} is not in groups", r.name)),
                (Some(g), true) if !seen_group(&resources, g, &r.name) => groups.push(g.clone()),
                _ => {}
            }
            if Some(r.name.as_str()) == prefix.strip_prefix(key::UI_BASE).and_then(|p| p.strip_prefix('/')) {
                return err(format!("{}: its page {} is the API prefix", r.name, key::url(&r.name, "").trim_end_matches('/')));
            }
        }
        let links: Vec<Vec<Link>> = (0..resources.len()).map(|i| links_of(&schema, &resources, i)).collect();
        for (r, l) in resources.iter_mut().zip(links) {
            r.links = l;
        }
        let tabs: Vec<Vec<Tab>> = (0..resources.len()).map(|i| tabs_of(&schema, &resources, i)).collect::<Result<_, _>>()?;
        for (r, t) in resources.iter_mut().zip(tabs) {
            r.tabs = t;
        }
        for r in &resources {
            for f in &r.filters {
                if let FilterKind::Relation { target, .. } = &f.kind {
                    if !resources.iter().any(|o| &o.name == target) {
                        return err(format!("{}: filter {:?} leads to {target}, which is not a resource", r.name, f.name));
                    }
                }
            }
        }
        let meta_builds = AtomicUsize::new(0);
        for r in &mut resources {
            r.meta = meta::resource(&schema, r, &meta_builds);
        }
        let brand = meta::brand(config.brand.as_ref())?;
        let statement_timeout = match config.statement_timeout_ms.unwrap_or(DEFAULT_STATEMENT_TIMEOUT_MS) {
            ms if ms > MAX_STATEMENT_TIMEOUT_MS => return err(format!("statement_timeout_ms is {ms}, at most {MAX_STATEMENT_TIMEOUT_MS}")),
            0 => None,
            ms => Some(Duration::from_millis(ms)),
        };
        Ok(Site { schema, prefix, strategy, resources, brand, groups, statement_timeout, warnings, meta_builds })
    }

    /// How many times `build` made the metadata of a resource: one per resource, and no request adds to it.
    pub fn meta_builds(&self) -> usize {
        self.meta_builds.load(Ordering::SeqCst)
    }

    /// The navigation of the resources in `visible` (names): ungrouped first, then each group in order.
    pub fn nav(&self, visible: &[&str]) -> serde_json::Value {
        let pick = |group: Option<&String>| -> Vec<&str> {
            self.resources.iter().filter(|r| r.group.as_ref() == group && visible.contains(&r.name.as_str())).map(|r| r.name.as_str()).collect()
        };
        let mut out = Vec::new();
        let loose = pick(None);
        if !loose.is_empty() {
            out.push(serde_json::json!({"group": null, "resources": loose}));
        }
        for g in &self.groups {
            let names = pick(Some(g));
            if !names.is_empty() {
                out.push(serde_json::json!({"group": g, "resources": names}));
            }
        }
        serde_json::Value::Array(out)
    }

    /// The detail URL of a record (see `key::url_of`). The error text names the resource or the key.
    pub fn url(&self, resource: &str, key: &Value) -> Result<String, String> {
        let resource = self.resource(resource).ok_or_else(|| format!("{resource} is not an admin resource"))?;
        crate::key::url_of(resource, key)
    }

    pub fn resource(&self, name: &str) -> Option<&Resource> {
        self.resources.iter().find(|r| r.name == name)
    }

    /// The rows of the to-many relation `rel` of a record of `parent`: none when `rel` is not a tab of the resource.
    pub fn plan_related<'a>(&'a self, parent: &Resource, rel: &str) -> Option<Related<'a>> {
        let tab = parent.tabs.iter().find(|t| t.name == rel)?;
        let child = self.resource(&tab.target)?;
        let parent_model = self.schema.model(parent.model);
        let relation = parent_model.ir.relations.iter().find(|r| r.name == rel)?;
        let (parent_from, child_to) = (field_columns(&self.schema, parent.model, &relation.from)?, field_columns(&self.schema, child.model, &relation.to)?);
        Some(Related { child, parent_from, child_to })
    }
}

/// The columns of the fields that `names` (field or column names) point to.
fn field_columns(schema: &Schema, model: usize, names: &[String]) -> Option<Vec<Column>> {
    names
        .iter()
        .map(|n| {
            let f = schema.model(model).ir.fields.iter().find(|f| f.name == *n || f.column == *n)?;
            column(schema, model, &f.name).ok()
        })
        .collect()
}

/// The tabs of resource `i`: the to-many relations that lead to a resource, and are not a many-to-many.
fn tabs_of(schema: &Schema, resources: &[Resource], i: usize) -> Result<Vec<Tab>, ConfigError> {
    let r = &resources[i];
    let model = schema.model(r.model);
    let usable = |rel: &monorm_core::ir::RelationIr| -> Option<String> {
        let (_, target) = model.relation(&rel.name).ok()?;
        let child = resources.iter().find(|o| o.model == target)?;
        (rel.kind == RelKind::Many && rel.through.is_none() && field_columns(schema, r.model, &rel.from).is_some() && field_columns(schema, child.model, &rel.to).is_some()).then(|| child.name.clone())
    };
    let mut out = Vec::new();
    match &r.tabs_config {
        None => {
            for rel in &model.ir.relations {
                if let Some(target) = usable(rel) {
                    out.push(Tab { name: rel.name.clone(), target });
                }
            }
        }
        Some(names) => {
            for n in names {
                let Some(rel) = model.ir.relations.iter().find(|rel| rel.name == *n) else {
                    return err(format!("{}: tabs has {n:?}, which is not a relation", r.name));
                };
                let Some(target) = usable(rel) else {
                    return err(format!("{}: tab {n:?} needs a to-many relation (not a many-to-many) to a resource", r.name));
                };
                if out.iter().any(|t: &Tab| t.name == *n) {
                    return err(format!("{}: tab {n:?} is listed twice", r.name));
                }
                out.push(Tab { name: rel.name.clone(), target });
            }
        }
    }
    Ok(out)
}

fn normalize_prefix(prefix: &str) -> Result<String, ConfigError> {
    let trimmed = prefix.trim_end_matches('/');
    if !trimmed.is_empty() && !trimmed.starts_with('/') {
        return err(format!("prefix {prefix:?} must start with /"));
    }
    Ok(trimmed.to_owned())
}

/// A model with its own rows: it has a table, and it is not a derived model.
fn concrete(m: &monorm_core::schema::Model) -> bool {
    !m.ir.table.is_empty() && m.view.is_none()
}

impl ResourceConfig {
    fn of(model: &str) -> ResourceConfig {
        ResourceConfig { model: model.to_owned(), list: ListConfig::default(), allow: None, perms: None, label: None, title: None, group: None, field_permissions: Vec::new(), actions: Vec::new(), queryset: None, can: BTreeMap::new(), check_messages: BTreeMap::new() }
    }
}

/// A part of a permission name: lowercase letters, digits and `_`, so `.` stays the label separator.
fn check_name(what: &str, text: &str) -> Result<(), ConfigError> {
    let ok = !text.is_empty() && text.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if ok { Ok(()) } else { err(format!("{what} {text:?} must be lowercase letters, digits and _")) }
}

/// The permission settings of one resource, checked.
struct PermissionOptions {
    label: String,
    allow: Vec<Op>,
    split_delete: bool,
}

fn permission_options(name: &str, config: &ResourceConfig, strategy: Strategy, site_label: &str) -> Result<PermissionOptions, ConfigError> {
    let label = config.label.clone().unwrap_or_else(|| site_label.to_owned());
    check_name("label", &label)?;
    let mut allow = Vec::new();
    for text in config.allow.clone().unwrap_or_else(|| Op::ALL.iter().map(|o| o.name().to_owned()).collect()) {
        let Some(op) = Op::parse(&text) else {
            return err(format!("{name}: allow has {text:?}, not one of read, create, update, delete"));
        };
        if !allow.contains(&op) {
            allow.push(op);
        }
    }
    let mut split_delete = false;
    if let Some(levels) = &config.perms {
        if strategy != Strategy::Page {
            return err(format!("{name}: perms applies to the page strategy only"));
        }
        for level in levels {
            match level.as_str() {
                "read" | "manage" => {}
                "delete" => split_delete = true,
                other => return err(format!("{name}: perms has {other:?}, not one of read, manage, delete")),
            }
        }
        if !levels.iter().any(|l| l == "read") || !levels.iter().any(|l| l == "manage") {
            return err(format!("{name}: perms must name read and manage"));
        }
    }
    if strategy == Strategy::Access && (!config.field_permissions.is_empty() || !config.actions.is_empty()) {
        return err(format!("{name}: field and action names need the page or model strategy"));
    }
    let mut seen = BTreeSet::new();
    for n in config.field_permissions.iter().chain(&config.actions) {
        check_name(&format!("{name}: permission name"), n)?;
        if !seen.insert(n) {
            return err(format!("{name}: permission name {n:?} is declared twice"));
        }
    }
    Ok(PermissionOptions { label, allow, split_delete })
}

fn resource(schema: &Schema, idx: usize, config: &ResourceConfig, strategy: Strategy, site_label: &str, warnings: &mut Vec<String>) -> Result<Resource, ConfigError> {
    let list = &config.list;
    let model = schema.model(idx);
    let name = model.ir.name.clone();
    let PermissionOptions { label, allow, split_delete } = permission_options(&name, config, strategy, site_label)?;
    let extras = extra_columns(schema, idx, &name, list)?;
    let extra = |n: &str| extras.columns.iter().find(|e| e.label == n).cloned();
    let mut columns = match &list.columns {
        Some(names) if names.is_empty() => return err(format!("{name}: list.columns is empty")),
        Some(names) => names.iter().map(|n| extra(n).map_or_else(|| column(schema, idx, n), Ok)).collect::<Result<Vec<_>, _>>()?,
        None => model.ir.fields.iter().filter(|f| listable(f)).map(|f| column(schema, idx, &f.name)).collect::<Result<Vec<_>, _>>()?,
    };
    for e in &extras.columns {
        if !columns.iter().any(|c| c.label == e.label) {
            columns.push(e.clone());
        }
    }
    let mut seen = BTreeSet::new();
    for c in &columns {
        if !seen.insert(c.label.clone()) {
            return err(format!("{name}: column {:?} is listed twice", c.label));
        }
    }
    let mut order = Vec::new();
    for spec in list.order.clone().unwrap_or_default() {
        let (desc, field) = match spec.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, spec.as_str()),
        };
        order.push(order_column(schema, idx, field, desc, &name)?);
    }
    for key in model.key_fields() {
        if !order.iter().any(|o| o.column.path.is_empty() && o.column.field == key.name) {
            order.push(OrderColumn { column: column(schema, idx, &key.name)?, desc: false });
        }
    }
    // The cursor reads the order columns of the last row, so each one is a column of the row.
    let mut order_positions = Vec::new();
    for o in &order {
        match columns.iter().position(|c| c.label == o.column.label) {
            Some(p) => order_positions.push(p),
            None => {
                columns.push(o.column.clone());
                order_positions.push(columns.len() - 1);
            }
        }
    }
    let page_size = list.page_size.unwrap_or(DEFAULT_PAGE_SIZE);
    if page_size == 0 || page_size > MAX_PAGE_SIZE {
        return err(format!("{name}: list.page_size is {page_size}, between 1 and {MAX_PAGE_SIZE}"));
    }
    let key = name.to_lowercase();
    let mut key_columns = Vec::new();
    let mut key_positions = Vec::new();
    for k in model.key_fields() {
        let c = column(schema, idx, &k.name)?;
        key_positions.push(columns.iter().position(|o| o.label == c.label).ok_or_else(|| ConfigError(format!("{name}: the key field {:?} is not a column", k.name)))?);
        key_columns.push(c);
    }
    let detail = model.ir.fields.iter().filter(|f| listable(f)).map(|f| column(schema, idx, &f.name)).collect::<Result<Vec<_>, _>>()?;
    let title = match &config.title {
        Some(t) if t.trim().is_empty() || t.chars().count() > 80 => return err(format!("{name}: title has 1 to 80 characters")),
        Some(t) => t.clone(),
        None => meta::humanize(&name),
    };
    if config.group.as_ref().is_some_and(|g| g.trim().is_empty()) {
        return err(format!("{name}: group is empty"));
    }
    let search = search_fields(schema, idx, &name, list, warnings)?;
    let mut sortable = sortable_columns(schema, idx, &name, list, &columns)?;
    for c in columns.iter().filter(|c| extras.sortable.contains(&c.label)) {
        sortable.push(Sortable { column: c.clone(), indexed: false });
    }
    for key in list.labels.keys().chain(list.empty_fields.keys()) {
        if !columns.iter().any(|c| c.label == *key) && !detail.iter().any(|c| c.label == *key) {
            return err(format!("{name}: labels and empty_fields name {key:?}, which is not a column"));
        }
    }
    for text in list.labels.values().chain(list.empty_fields.values()).chain(&list.empty) {
        if text.chars().count() > 80 {
            return err(format!("{name}: a label or an empty text has at most 80 characters"));
        }
    }
    let filters = filter_specs(schema, idx, &name, list)?;
    let named_filters = named_filters(schema, idx, &name, list)?;
    let queryset = queryset_of(schema, idx, &name, config.queryset.as_ref())?;
    let can = can_rules(schema, &name, &config.can)?;
    if let Some(k) = config.check_messages.keys().find(|k| !model.ir.constraints.iter().any(|c| matches!(c, ConstraintIr::Check { name: Some(n), .. } if n == *k))) {
        return err(format!("{name}: check_messages names {k:?}, which is not a named @@check of the model"));
    }
    let scope_defaults = scope_default_names(model);
    Ok(Resource {
        model: idx,
        name,
        columns,
        order,
        page_size,
        order_positions,
        key,
        label,
        allow,
        split_delete,
        field_permissions: config.field_permissions.clone(),
        actions: config.actions.clone(),
        title,
        group: config.group.clone(),
        key_columns,
        key_positions,
        detail,
        links: Vec::new(),
        meta: serde_json::Value::Null,
        search,
        search_with: list.search_with.clone(),
        sortable,
        sortable_explicit: list.sortable.is_some(),
        labels: list.computed.iter().map(|c| (&c.name, &c.label)).chain(list.aggregates.iter().map(|a| (&a.name, &a.label))).filter_map(|(n, l)| Some((n.clone(), l.clone()?))).chain(list.labels.clone()).collect(),
        empty: list.empty.clone(),
        empty_fields: list.empty_fields.clone(),
        filters,
        named_filters,
        tabs: Vec::new(),
        queryset,
        can,
        check_messages: config.check_messages.clone(),
        bypass: model.read_defaults.names().into_iter().filter(|n| !scope_defaults.iter().any(|d| d == n)).map(str::to_owned).collect(),
        scope_defaults,
        tabs_config: list.tabs.clone(),
        large: Mutex::new(None),
    })
}

fn seen_group(resources: &[Resource], group: &str, until: &str) -> bool {
    resources.iter().take_while(|r| r.name != until).any(|r| r.group.as_deref() == Some(group))
}

/// The to-one relations of resource `i` whose `to` columns are exactly the key of another resource.
fn links_of(schema: &Schema, resources: &[Resource], i: usize) -> Vec<Link> {
    let model = schema.model(resources[i].model);
    let mut out = Vec::new();
    for rel in &model.ir.relations {
        let Ok((_, target_idx)) = model.relation(&rel.name) else { continue };
        let Some(target) = resources.iter().find(|r| r.model == target_idx) else { continue };
        if rel.kind != RelKind::One {
            continue;
        }
        let target_model = schema.model(target_idx);
        let keys = target_model.key_fields();
        if rel.to.len() != keys.len() {
            continue;
        }
        let mut from = Vec::new();
        for k in &keys {
            let Some(n) = rel.to.iter().position(|t| *t == k.column || *t == k.name) else { break };
            let Some(p) = resources[i].detail.iter().position(|d| model.ir.fields.iter().any(|f| f.name == d.field && (f.column == rel.from[n] || f.name == rel.from[n]))) else { break };
            from.push(p);
        }
        if from.len() == keys.len() {
            out.push(Link { name: rel.name.clone(), target: target.name.clone(), from });
        }
    }
    out
}

fn listable(f: &monorm_core::ir::FieldIr) -> bool {
    f.generated.is_none()
}

/// A column from `author.name`: each name before the last is a to-one relation.
pub(crate) fn column(schema: &Schema, root: usize, spec: &str) -> Result<Column, ConfigError> {
    let model_name = &schema.model(root).ir.name;
    let mut parts: Vec<&str> = spec.split('.').collect();
    let field_name = parts.pop().unwrap_or_default();
    let mut cur = root;
    let mut path = Vec::new();
    for hop in parts {
        let m = schema.model(cur);
        let Ok((rel, target)) = m.relation(hop) else {
            return err(format!("{model_name}: unknown field {spec:?}: {} has no relation {hop:?}", m.ir.name));
        };
        if rel.kind == RelKind::Many {
            return err(format!("{model_name}: list column {spec:?} goes through the to-many relation {hop:?}; a column reads one value per row"));
        }
        path.push(hop.to_owned());
        cur = target;
    }
    let m = schema.model(cur);
    let Some(field) = m.ir.fields.iter().find(|f| f.name == field_name) else {
        let hint = if m.relation(field_name).is_ok() { format!(": {field_name:?} is a relation, name one of its fields") } else { String::new() };
        return err(format!("{model_name}: unknown field {spec:?}{hint}"));
    };
    let mut ty = ValueType::scalar(field.ty);
    ty.array = field.array;
    ty.enum_idx = field.enum_idx;
    Ok(Column { label: spec.to_owned(), path, field: field.name.clone(), ty, nullable: field.nullable, source: Source::Field })
}

fn order_column(schema: &Schema, root: usize, field: &str, desc: bool, name: &str) -> Result<OrderColumn, ConfigError> {
    if field.contains('.') {
        return err(format!("{name}: order {field:?}: order takes a field of {name}, not of a related model"));
    }
    let column = column(schema, root, field)?;
    let f = schema.model(root).field(field).map_err(|e| ConfigError(e.to_string()))?;
    if column.nullable {
        return err(format!("{name}: order {field:?}: the field is nullable, and a keyset page cannot compare NULLs"));
    }
    if column.ty.array || column.ty.enum_idx.is_some() || matches!(column.ty.ty, ColType::Json | ColType::Decimal) || f.write_sql.is_some() {
        return err(format!("{name}: order {field:?}: a field of this type cannot order a list"));
    }
    Ok(OrderColumn { column, desc })
}

/// The columns that a sort header may use: own fields in the list that a keyset page can compare.
fn sortable_columns(schema: &Schema, idx: usize, name: &str, list: &ListConfig, columns: &[Column]) -> Result<Vec<Sortable>, ConfigError> {
    let model = schema.model(idx);
    let named: Vec<String> = match &list.sortable {
        Some(names) => names.clone(),
        None => columns.iter().filter(|c| c.path.is_empty() && c.source == Source::Field).map(|c| c.field.clone()).collect(),
    };
    let mut out = Vec::new();
    for field in named {
        let explicit = list.sortable.is_some();
        let ok = order_column(schema, idx, &field, false, name);
        match ok {
            Ok(o) => {
                if !columns.iter().any(|c| c.label == o.column.label) {
                    return err(format!("{name}: sortable {field:?} is not a list column"));
                }
                if out.iter().any(|s: &Sortable| s.column.label == o.column.label) {
                    return err(format!("{name}: sortable {field:?} is listed twice"));
                }
                let f = model.field(&field).map_err(|e| ConfigError(e.to_string()))?;
                out.push(Sortable { column: o.column, indexed: leads_btree(&model.ir, f, false) });
            }
            Err(e) if explicit => return Err(e),
            Err(_) => {}
        }
    }
    Ok(out)
}

fn names(f: &FieldIr, text: &str) -> bool {
    f.name == text || f.column == text
}

/// The field is the first key of a primary key, a unique constraint or a btree index.
/// A `*_pattern_ops` index serves `LIKE 'x%'` but not `ORDER BY` under a non-C collation: `pattern_ops` counts only for a prefix search.
fn leads_btree(m: &ModelIr, f: &FieldIr, pattern_ops: bool) -> bool {
    if f.primary_key || f.unique || f.index || m.primary_key.first().is_some_and(|k| names(f, k)) {
        return true;
    }
    let unique = m.constraints.iter().any(|c| matches!(c, ConstraintIr::Unique { fields, .. } if fields.first().is_some_and(|k| names(f, k))));
    unique
        || m.indexes.iter().any(|i| {
            let btree = i.method.as_deref().is_none_or(|m| m.eq_ignore_ascii_case("btree"));
            let first = i.columns.first();
            btree && i.where_.is_none() && first.is_some_and(|c| c.field.as_deref().is_some_and(|k| names(f, k)) && c.expr.is_none() && c.opclass.as_deref().is_none_or(|o| pattern_ops && o.ends_with("_pattern_ops")))
        })
}

/// A trigram index (gin or gist, `gin_trgm_ops`) on the field.
fn has_trigram(m: &ModelIr, f: &FieldIr) -> bool {
    m.indexes.iter().any(|i| {
        let method = i.method.as_deref().unwrap_or("btree").to_ascii_lowercase();
        (method == "gin" || method == "gist") && i.where_.is_none() && i.columns.iter().any(|c| c.field.as_deref().is_some_and(|k| names(f, k)) && c.opclass.as_deref().is_some_and(|o| o.contains("trgm")))
    })
}

/// The text search configuration of a gin or gist index on `to_tsvector('cfg', field)`; none when the index names none.
fn fulltext_config(m: &ModelIr, f: &FieldIr) -> Option<String> {
    for i in &m.indexes {
        let method = i.method.as_deref().unwrap_or("btree").to_ascii_lowercase();
        if (method != "gin" && method != "gist") || i.where_.is_some() {
            continue;
        }
        let Some(expr) = i.columns.first().and_then(|c| c.expr.as_deref()) else { continue };
        let lower = expr.to_ascii_lowercase();
        let Some(at) = lower.find("to_tsvector(") else { continue };
        if !lower.contains(&f.column.to_ascii_lowercase()) && !lower.contains(&f.name.to_ascii_lowercase()) {
            continue;
        }
        let rest = expr[at + "to_tsvector(".len()..].trim_start();
        let config = rest.strip_prefix('\'').and_then(|r| r.split('\'').next()).filter(|c| !c.is_empty() && c.chars().all(|ch| ch.is_alphanumeric() || ch == '_' || ch == '.')).unwrap_or("");
        if config.is_empty() {
            return None;
        }
        return Some(config.to_owned());
    }
    None
}

/// The search fields of a resource, each with the mode that its indexes serve. A field that no index serves is a warning;
/// a field of a type that no mode fits is an error.
fn search_fields(schema: &Schema, idx: usize, name: &str, list: &ListConfig, warnings: &mut Vec<String>) -> Result<Vec<SearchField>, ConfigError> {
    let model = schema.model(idx);
    let mut out: Vec<SearchField> = Vec::new();
    for field in list.search.clone().unwrap_or_default() {
        if field.contains('.') {
            return err(format!("{name}: search {field:?}: search takes a field of {name}, not of a related model"));
        }
        if out.iter().any(|s| s.column.field == field) {
            return err(format!("{name}: search {field:?} is listed twice"));
        }
        let column = column(schema, idx, &field)?;
        let f = model.field(&field).map_err(|e| ConfigError(e.to_string()))?;
        let ty = column.ty;
        let text = matches!(ty.ty, ColType::String | ColType::Text) && !ty.array && ty.enum_idx.is_none();
        let exact = matches!(ty.ty, ColType::Int | ColType::BigInt | ColType::Uuid) && !ty.array && ty.enum_idx.is_none();
        if f.write_sql.is_some() && !text || !(text || exact) {
            return err(format!("{name}: search {field:?}: no search strategy fits a {} field; search takes text, integer and uuid fields", type_name(&column)));
        }
        let postgres = schema.dialect == Dialect::Postgres;
        let btree = leads_btree(&model.ir, f, true);
        let trigram = postgres && text && has_trigram(&model.ir, f);
        let fulltext = if postgres && text { fulltext_config(&model.ir, f) } else { None };
        let (default, indexed) = if !text {
            (Mode::Exact, btree)
        } else if trigram {
            (Mode::Contains, true)
        } else if btree {
            (Mode::Prefix, true)
        } else if fulltext.is_some() {
            (Mode::Fulltext, true)
        } else {
            (Mode::Contains, false)
        };
        if !indexed {
            warnings.push(format!("{name}: search field {field:?} has no index that serves a search; add an index on it (a btree for exact and prefix, a trigram index for contains, a full-text index for @)"));
        }
        out.push(SearchField { column, default, text, fulltext, indexed });
    }
    if list.search_with.is_some() && !out.is_empty() {
        return err(format!("{name}: search and search_with are two ways to search; set one"));
    }
    if list.search_with.as_ref().is_some_and(|n| n.trim().is_empty()) {
        return err(format!("{name}: search_with is empty"));
    }
    Ok(out)
}

fn type_name(c: &Column) -> String {
    if c.ty.array {
        "array".into()
    } else if c.ty.enum_idx.is_some() {
        "enum".into()
    } else {
        format!("{:?}", c.ty.ty).to_lowercase()
    }
}

/// Computed and aggregate columns of a resource, and which of them have a sort header.
struct Extras {
    columns: Vec<Column>,
    sortable: Vec<String>,
}

fn ident_ok(text: &str) -> bool {
    !text.is_empty() && text.chars().count() <= 40 && text.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn parse_type(text: &str) -> Option<ColType> {
    Some(match text.to_ascii_lowercase().replace('_', "").as_str() {
        "int" | "integer" => ColType::Int,
        "bigint" => ColType::BigInt,
        "float" => ColType::Float,
        "bool" | "boolean" => ColType::Bool,
        "string" => ColType::String,
        "text" => ColType::Text,
        "datetime" => ColType::DateTime,
        "date" => ColType::Date,
        "uuid" => ColType::Uuid,
        "json" => ColType::Json,
        "decimal" => ColType::Decimal,
        _ => return None,
    })
}

/// A prepared expression is constant: a parameter or a scope value would have no value at the request.
fn has_binding(expr: &Value) -> bool {
    match expr {
        Value::Object(map) => matches!(map.get("t").and_then(Value::as_str), Some("param" | "in_param" | "scope")) || map.values().any(has_binding),
        Value::Array(items) => items.iter().any(has_binding),
        _ => false,
    }
}

/// The type of an expression, as the planner reads it. The planner runs with no connection and no data.
fn probe_type(schema: &Schema, model: &str, expr: &Value) -> Result<ValueType, String> {
    let plan = probe(schema, json_select(model, Some(expr), None))?;
    plan.types.first().copied().ok_or_else(|| "the expression has no type".to_owned())
}

fn json_select(model: &str, column: Option<&Value>, filter: Option<&Value>) -> Value {
    let mut op = serde_json::json!({"op": "select", "model": model, "limit": 1});
    op["columns"] = match column {
        Some(e) => serde_json::json!([{"t": "expr", "expr": e}]),
        None => serde_json::json!([{"t": "model"}]),
    };
    if let Some(f) = filter {
        op["filters"] = serde_json::json!([f]);
    }
    op
}

fn probe(schema: &Schema, op: Value) -> Result<monorm_engine::plan::SelectPlan, String> {
    probe_with(schema, op, &ProbeParams(0))
}

/// Parameters that are null: the planner checks an expression with no request.
struct ProbeParams(usize);

impl Params for ProbeParams {
    fn len(&self) -> usize {
        self.0
    }
    fn value(&self, _: usize, ty: Option<ValueType>) -> EngineResult<sea_query::Value> {
        Ok(null_of(ty))
    }
    fn text(&self, _: usize) -> EngineResult<String> {
        Ok(String::new())
    }
    fn count(&self, _: usize) -> EngineResult<u64> {
        Ok(0)
    }
}

fn probe_with(schema: &Schema, mut op: Value, params: &ProbeParams) -> Result<monorm_engine::plan::SelectPlan, String> {
    // A request turns these defaults off too: a probe has no scope value either.
    if let Some(model) = op["model"].as_str().and_then(|m| schema.model_idx(m).ok()) {
        let names = scope_default_names(schema.model(model));
        if !names.is_empty() {
            op["without_defaults"] = serde_json::json!(names);
        }
    }
    let op: Operation = serde_json::from_value(op).map_err(|e| e.to_string())?;
    match Planner::plan(schema, Target::new(schema.dialect), &op, params).map_err(|e| e.to_string())? {
        Plan::Select(p) => Ok(p),
        _ => Err("the expression is not a select".to_owned()),
    }
}

fn same_type(declared: ColType, found: ColType) -> bool {
    declared == found || matches!((declared, found), (ColType::String, ColType::Text) | (ColType::Text, ColType::String))
}

fn extra_columns(schema: &Schema, idx: usize, name: &str, list: &ListConfig) -> Result<Extras, ConfigError> {
    let model = schema.model(idx);
    let mut columns: Vec<Column> = Vec::new();
    let mut sortable = Vec::new();
    let taken = |columns: &[Column], n: &str| -> Result<(), ConfigError> {
        if !ident_ok(n) {
            return err(format!("{name}: column name {n:?} has 1 to 40 letters, digits and _"));
        }
        if model.ir.fields.iter().any(|f| f.name == n) || model.ir.relations.iter().any(|r| r.name == n) || columns.iter().any(|c| c.label == n) {
            return err(format!("{name}: computed column {n:?} has the name of a field, a relation or another computed column"));
        }
        Ok(())
    };
    for c in &list.computed {
        taken(&columns, &c.name)?;
        let Some(declared) = parse_type(&c.ty) else {
            return err(format!("{name}: computed {:?}: type {:?} is not int, bigint, float, bool, string, text, datetime, date, uuid, json or decimal", c.name, c.ty));
        };
        let nullable = c.nullable.unwrap_or(true);
        let source = match (&c.expr, &c.function) {
            (Some(expr), None) => {
                if has_binding(expr) {
                    return err(format!("{name}: computed {:?}: an expression has no parameters or scope values", c.name));
                }
                let found = probe_type(schema, &model.ir.name, expr).map_err(|e| ConfigError(format!("{name}: computed {:?}: {e}", c.name)))?;
                if found.array || found.enum_idx.is_some() || !same_type(declared, found.ty) {
                    return err(format!("{name}: computed {:?}: declared {declared:?}, but the expression is {:?}{}", c.name, found.ty, if found.array { " array" } else { "" }));
                }
                Source::Expr(expr.clone())
            }
            (None, Some(f)) if !f.trim().is_empty() => Source::Batch(f.clone()),
            _ => return err(format!("{name}: computed {:?} sets expr or function, one of them", c.name)),
        };
        if c.sortable == Some(true) {
            if matches!(source, Source::Batch(_)) {
                return err(format!("{name}: computed {:?}: a column of a host function cannot sort; the database does not know its values", c.name));
            }
            if nullable || matches!(declared, ColType::Json | ColType::Decimal) {
                return err(format!("{name}: computed {:?}: a sort column is not null (set nullable to false) and not json or decimal", c.name));
            }
            sortable.push(c.name.clone());
        }
        columns.push(Column { label: c.name.clone(), path: Vec::new(), field: c.name.clone(), ty: ValueType::scalar(declared), nullable, source });
    }
    for a in &list.aggregates {
        taken(&columns, &a.name)?;
        let Ok((rel, target)) = model.relation(&a.relation) else {
            return err(format!("{name}: aggregate {:?}: {} has no relation {:?}", a.name, model.ir.name, a.relation));
        };
        if rel.kind != RelKind::Many {
            return err(format!("{name}: aggregate {:?}: {:?} is a to-one relation; an aggregate reads a to-many relation", a.name, a.relation));
        }
        let expr = match (a.function.as_str(), &a.field) {
            ("count", None) => serde_json::json!({"t": "func", "name": "count", "rel": [a.relation]}),
            ("count", Some(_)) => return err(format!("{name}: aggregate {:?}: count takes no field", a.name)),
            ("sum" | "min" | "max" | "avg", Some(field)) => {
                if schema.model(target).field(field).is_err() {
                    return err(format!("{name}: aggregate {:?}: {} has no field {field:?}", a.name, schema.model(target).ir.name));
                }
                serde_json::json!({"t": "func", "name": a.function, "args": [{"t": "col", "path": [a.relation], "name": field}]})
            }
            ("sum" | "min" | "max" | "avg", None) => return err(format!("{name}: aggregate {:?}: {} needs a field", a.name, a.function)),
            (other, _) => return err(format!("{name}: aggregate {:?}: function {other:?} is not count, sum, min, max or avg", a.name)),
        };
        let found = probe_type(schema, &model.ir.name, &expr).map_err(|e| ConfigError(format!("{name}: aggregate {:?}: {e}", a.name)))?;
        columns.push(Column { label: a.name.clone(), path: Vec::new(), field: a.name.clone(), ty: found, nullable: a.function != "count", source: Source::Expr(expr) });
    }
    for (c, label) in list.computed.iter().map(|c| (&c.name, &c.label)).chain(list.aggregates.iter().map(|a| (&a.name, &a.label))) {
        if label.as_ref().is_some_and(|l| l.trim().is_empty() || l.chars().count() > 80) {
            return err(format!("{name}: the label of {c:?} has 1 to 80 characters"));
        }
    }
    Ok(Extras { columns, sortable })
}

/// A field that a filter can compare: not an array, a JSON value or a decimal.
fn filterable(c: &Column) -> bool {
    !c.ty.array && !matches!(c.ty.ty, ColType::Json | ColType::Decimal)
}

fn filter_specs(schema: &Schema, idx: usize, name: &str, list: &ListConfig) -> Result<Vec<Filter>, ConfigError> {
    let model = schema.model(idx);
    let mut out: Vec<Filter> = Vec::new();
    for spec in &list.filters {
        if out.iter().any(|f| f.name == *spec) {
            return err(format!("{name}: filter {spec:?} is listed twice"));
        }
        if !spec.contains('.') && model.relation(spec).is_ok() {
            out.push(Filter { name: spec.clone(), kind: relation_filter(schema, idx, name, spec)? });
            continue;
        }
        let c = column(schema, idx, spec).map_err(|e| ConfigError(e.0.replace("list column", "filter")))?;
        if !filterable(&c) {
            return err(format!("{name}: filter {spec:?}: a filter does not take an array, json or decimal field"));
        }
        out.push(Filter { name: spec.clone(), kind: FilterKind::Field(c) });
    }
    for n in list.named_filters.iter().map(|f| &f.name) {
        if out.iter().any(|f| f.name == *n) {
            return err(format!("{name}: named filter {n:?} has the name of a filter"));
        }
    }
    Ok(out)
}

/// A to-one relation as a filter: the foreign key fields on the model compare with a key of the target.
fn relation_filter(schema: &Schema, idx: usize, name: &str, spec: &str) -> Result<FilterKind, ConfigError> {
    let model = schema.model(idx);
    let (rel, target_idx) = model.relation(spec).map_err(|e| ConfigError(e.to_string()))?;
    if rel.kind != RelKind::One {
        return err(format!("{name}: filter {spec:?}: a filter takes a to-one relation, and {spec:?} is to-many"));
    }
    let target = schema.model(target_idx);
    let keys = target.key_fields();
    if rel.to.len() != keys.len() {
        return err(format!("{name}: filter {spec:?}: the relation does not lead to the key of {}", target.ir.name));
    }
    let (mut from, mut key) = (Vec::new(), Vec::new());
    for k in &keys {
        let Some(n) = rel.to.iter().position(|t| *t == k.column || *t == k.name) else {
            return err(format!("{name}: filter {spec:?}: the relation does not lead to the key of {}", target.ir.name));
        };
        let Some(f) = model.ir.fields.iter().find(|f| f.column == rel.from[n] || f.name == rel.from[n]) else {
            return err(format!("{name}: filter {spec:?}: the foreign key field {:?} is not a field", rel.from[n]));
        };
        from.push(column(schema, idx, &f.name)?);
        key.push(column(schema, target_idx, &k.name)?);
    }
    Ok(FilterKind::Relation { target: target.ir.name.clone(), from, key })
}

fn named_filters(schema: &Schema, idx: usize, name: &str, list: &ListConfig) -> Result<Vec<NamedFilter>, ConfigError> {
    let model = schema.model(idx);
    let mut out: Vec<NamedFilter> = Vec::new();
    for f in &list.named_filters {
        if !ident_ok(&f.name) || out.iter().any(|o| o.name == f.name) {
            return err(format!("{name}: named filter {:?} has 1 to 40 letters, digits and _, and a name of its own", f.name));
        }
        if f.choices.is_empty() {
            return err(format!("{name}: named filter {:?} has no choices", f.name));
        }
        let mut choices: Vec<NamedChoice> = Vec::new();
        for c in &f.choices {
            if !ident_ok(&c.name) || c.name == "all" || choices.iter().any(|o| o.name == c.name) {
                return err(format!("{name}: named filter {:?}: choice {:?} has 1 to 40 letters, digits and _, is not \"all\" (that is every row) and has a name of its own", f.name, c.name));
            }
            if has_binding(&c.expr) {
                return err(format!("{name}: named filter {:?}: choice {:?}: an expression has no parameters or scope values", f.name, c.name));
            }
            let key = model.key_fields().first().map(|k| serde_json::json!({"t": "col", "path": [], "name": k.name}));
            probe(schema, json_select(&model.ir.name, key.as_ref(), Some(&c.expr))).map_err(|e| ConfigError(format!("{name}: named filter {:?}: choice {:?}: {e}", f.name, c.name)))?;
            choices.push(NamedChoice { name: c.name.clone(), label: c.label.clone().unwrap_or_else(|| meta::humanize(&c.name)), expr: c.expr.clone() });
        }
        if let Some(d) = &f.default {
            if !choices.iter().any(|c| c.name == *d) {
                return err(format!("{name}: named filter {:?}: default {d:?} is not a choice", f.name));
            }
        }
        out.push(NamedFilter { name: f.name.clone(), label: f.label.clone().unwrap_or_else(|| meta::humanize(&f.name)), choices, default: f.default.clone() });
    }
    Ok(out)
}

/// The names of the read defaults of the model that read a `scope.<name>` value.
fn scope_default_names(model: &monorm_core::schema::Model) -> Vec<String> {
    model.read_defaults.parts.iter().filter(|p| !p.scopes.is_empty()).map(|p| p.identity.name.clone()).collect()
}

/// The name of a host function: letters, digits, `_`, `.`, `:` and `-`, at most 80 characters.
fn check_function(what: &str, name: &str) -> Result<(), ConfigError> {
    let ok = !name.is_empty() && name.chars().count() <= 80 && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-'));
    if ok { Ok(()) } else { err(format!("{what} {name:?} is not a function name")) }
}

fn queryset_of(schema: &Schema, idx: usize, name: &str, config: Option<&QuerysetConfig>) -> Result<Option<Queryset>, ConfigError> {
    let Some(q) = config else { return Ok(None) };
    let mut slots = Vec::new();
    for s in &q.slots {
        slots.push(match s.as_str() {
            "user" => Slot::User,
            other => return err(format!("{name}: queryset slot {other:?} is not \"user\"")),
        });
    }
    match (&q.filter, &q.function) {
        (Some(_), Some(_)) => return err(format!("{name}: queryset has a filter or a function, not both")),
        (None, Some(f)) => {
            check_function(&format!("{name}: queryset.function"), f)?;
            if !slots.is_empty() {
                return err(format!("{name}: a queryset function takes no slots: it gets the user"));
            }
        }
        (Some(f), None) => {
            let model = &schema.model(idx).ir.name;
            let key = schema.model(idx).key_fields().first().map(|k| serde_json::json!({"t": "col", "path": [], "name": k.name}));
            probe_with(schema, json_select(model, key.as_ref(), Some(f)), &ProbeParams(slots.len())).map_err(|e| ConfigError(format!("{name}: queryset filter: {e}")))?;
            if has_scope(f) {
                return err(format!("{name}: queryset filter: a scope value has no value in the admin"));
            }
            if let Some(i) = max_param(f).filter(|&i| i >= slots.len()) {
                return err(format!("{name}: queryset filter: parameter {i} has no slot"));
            }
        }
        (None, None) => return err(format!("{name}: queryset has a filter or a function")),
    }
    Ok(Some(Queryset { filter: q.filter.clone(), slots, function: q.function.clone() }))
}

pub(crate) fn has_scope(expr: &Value) -> bool {
    match expr {
        Value::Object(map) => map.get("t").and_then(Value::as_str) == Some("scope") || map.values().any(has_scope),
        Value::Array(items) => items.iter().any(has_scope),
        _ => false,
    }
}

/// The highest parameter index of an expression.
pub(crate) fn max_param(expr: &Value) -> Option<usize> {
    match expr {
        Value::Object(map) => {
            let here = matches!(map.get("t").and_then(Value::as_str), Some("param" | "in_param")).then(|| map.get("i").and_then(Value::as_u64).map(|i| i as usize)).flatten();
            map.values().filter_map(max_param).chain(here).max()
        }
        Value::Array(items) => items.iter().filter_map(max_param).max(),
        _ => None,
    }
}

fn can_rules(schema: &Schema, name: &str, config: &BTreeMap<String, Value>) -> Result<Vec<(String, Value)>, ConfigError> {
    let mut out = Vec::new();
    for rule in CAN_NAMES {
        let Some(expr) = config.get(rule) else { continue };
        if has_binding(expr) {
            return err(format!("{name}: can {rule:?}: an expression has no parameters or scope values"));
        }
        let ty = probe_type(schema, name, expr).map_err(|e| ConfigError(format!("{name}: can {rule:?}: {e}")))?;
        if ty.ty != ColType::Bool || ty.array {
            return err(format!("{name}: can {rule:?}: the expression is not a boolean"));
        }
        out.push((rule.to_owned(), expr.clone()));
    }
    if let Some(other) = config.keys().find(|k| !CAN_NAMES.contains(&k.as_str())) {
        return err(format!("{name}: can has {other:?}, not one of {}", CAN_NAMES.join(", ")));
    }
    Ok(out)
}
