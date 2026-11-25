//! `Site::build`: the resources of an admin, checked against the prepared schema.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use monorm_core::dialect::{Dialect, Target};
use monorm_core::ir::{ColType, ConstraintIr, FieldIr, ModelIr, Operation, RelKind, ValueType};
use monorm_core::schema::Schema;
use monorm_engine::plan::{Plan, Planner};
use monorm_engine::params::null_of;
use monorm_engine::{Params, Result as EngineResult};
use serde_json::{json, Map, Value};

mod builtin;

use crate::error::{code, opt, AdminError, Fault, PathPart, Spelling};
use crate::form::FormConfig;
use crate::key;
use crate::meta;
use crate::permissions::{Op, Strategy};
use crate::site_config::{self, Handles};
use crate::site_config::{Config, SwitcherConfig, QuerysetConfig, ResourceConfig, ListConfig, DateHierarchyConfig, ActionConfig};

pub const DEFAULT_PAGE_SIZE: usize = 25;
pub const MAX_PAGE_SIZE: usize = 200;
/// A count stops here: the list shows `10,000+`.
pub const COUNT_CAP: usize = 10_000;
/// A table with more rows allows only the sort columns that lead an index, unless the config names the columns.
pub const LARGE_TABLE_ROWS: usize = 50_000;
pub const DEFAULT_STATEMENT_TIMEOUT_MS: u64 = 15_000;
pub const DEFAULT_EXPORT_IDLE_TIMEOUT_MS: u64 = 30_000;
/// The rows of one keyset batch of an export.
pub const EXPORT_BATCH: usize = 1_000;
/// PostgreSQL stores `statement_timeout` in an `int4` of milliseconds and rejects more.
pub const MAX_STATEMENT_TIMEOUT_MS: u64 = i32::MAX as u64;
/// How long a table size stays known.
const LARGE_TTL: Duration = Duration::from_secs(300);
/// The longest search text.
pub const MAX_SEARCH_LEN: usize = 200;

/// The user views of the resources: the key is the index of the resource and which bound names the user holds.
pub(crate) type Views = std::collections::HashMap<(usize, Vec<bool>), Arc<Resource>>;

/// A wrong value: `config.invalid_value`. A rule with its own code uses `err_code`.
pub(crate) fn invalid_fault(message: String) -> Fault {
    Fault::new(code::INVALID_VALUE, message)
}

/// A wrong config of the kind `code`, such as `config.route`.
pub(crate) fn err_code<T>(code: &'static str, message: String) -> Result<T, Fault> {
    Err(Fault::new(code, message))
}

/// A wrong value: `config.invalid_value`.
pub(crate) fn invalid<T>(message: String) -> Result<T, Fault> {
    Err(invalid_fault(message))
}

/// A wrong value of the model `model`: the model leads the path and the text.
pub(crate) fn invalid_in<T>(model: &str, message: String) -> Result<T, Fault> {
    Err(invalid_fault(message).under(PathPart::Model(model.to_owned())))
}

/// A wrong config of the model `model` and the kind `code`: the model leads the path and the text.
pub(crate) fn err_code_in<T>(model: &str, code: &'static str, message: String) -> Result<T, Fault> {
    Err(Fault::new(code, message).under(PathPart::Model(model.to_owned())))
}

/// The key that serde names in the text of an unknown-field error. The one place that reads serde wording for this.
pub(crate) fn unknown_field_key(text: &str) -> Option<&str> {
    text.strip_prefix("unknown field `").and_then(|t| t.split('`').next())
}

/// One step of the place where a config fails to parse.
enum Step {
    Key(String),
    Index(usize),
}

/// The marked text of a key that the user wrote: an option key is spelled for the host.
fn marked_key(name: &str) -> String {
    if name.chars().all(|c| c.is_ascii_lowercase() || c == '_') { opt(name) } else { name.to_owned() }
}

/// The value at `steps`.
fn at<'a>(value: &'a Value, steps: &[Step]) -> Option<&'a Value> {
    steps.iter().try_fold(value, |v, s| match s {
        Step::Key(k) => v.get(k),
        Step::Index(i) => v.get(*i),
    })
}

/// The model of the resource that `steps` lead into.
fn resource_model(value: &Value, steps: &[Step]) -> Option<String> {
    match steps {
        [Step::Key(r), Step::Index(i), ..] if r == "resources" => value.get("resources")?.get(*i)?.get("model")?.as_str().map(str::to_owned),
        _ => None,
    }
}

/// The text of an unknown option, by the place of the error: the same words that the bindings used. None: no friendly text for this place.
fn unknown_option_text(value: &Value, steps: &[Step], key: &str) -> Option<String> {
    let shown = format!("'{}'", marked_key(key));
    let model_of = |index: usize| value.get("resources")?.get(index)?.get("model")?.as_str().map(str::to_owned);
    match steps {
        [Step::Key(k)] if k == "brand" || k == "theme" || k == "switcher" => Some(format!("unknown {k} option {shown}")),
        [Step::Key(r), Step::Index(i)] if r == "resources" => Some(format!("{}: unknown option {shown}", model_of(*i)?)),
        [Step::Key(r), Step::Index(i), Step::Key(l)] if r == "resources" && l == "list" => Some(format!("{}: unknown list option {shown}", model_of(*i)?)),
        [Step::Key(r), Step::Index(i), Step::Key(l), Step::Key(d)] if r == "resources" && l == "list" && d == "date_hierarchy" => Some(format!("{}: unknown date_hierarchy option {shown}", model_of(*i)?)),
        [Step::Key(r), Step::Index(i), Step::Key(l), Step::Key(rel)] if r == "resources" && l == "relations" => Some(format!("{}: relations['{rel}'] has an unknown option {shown}", model_of(*i)?)),
        _ => None,
    }
}

/// What a resource option takes, for the error of a value of the wrong shape. None: the serde text stands.
fn shape_rule(key: &str) -> Option<&'static str> {
    Some(match key {
        "widgets" => "maps a field, a path or a type to a widget name",
        "see" | "change" => "maps a field name to a permission name",
        "check_messages" => "maps a check name to a message",
        "readonly" => "is a list of field names",
        "form_component" => "is the path of a JavaScript file",
        "detail_tabs" => "is a list of package tab names",
        "bulk_delete" => "is true or false",
        _ => return None,
    })
}

/// The fault of a config that does not parse. Each name that serde quotes is an option key of the wire.
pub(crate) fn parse_fault(value: &Value, e: serde_path_to_error::Error<serde_json::Error>) -> Fault {
    use serde_path_to_error::Segment;
    // The path ends with the key that failed, so for an unknown key it names the key itself.
    let mut steps: Vec<Step> = e.path().iter().filter_map(|s| match s {
        Segment::Map { key } => Some(Step::Key(key.clone())),
        Segment::Seq { index } => Some(Step::Index(*index)),
        _ => None,
    }).collect();
    let text = e.inner().to_string();
    if let Some(key) = unknown_field_key(&text) {
        if matches!(steps.last(), Some(Step::Key(k)) if k == key) {
            steps.pop();
        }
        if let Some(friendly) = unknown_option_text(value, &steps, key) {
            return Fault::new(code::UNKNOWN_OPTION, friendly);
        }
    }
    // An untagged enum hides the cause: read the object itself.
    if text.contains("untagged enum DateHierarchyConfig") {
        if let (Some(model), Some(Value::Object(o))) = (resource_model(value, &steps), at(value, &steps)) {
            if let Some(key) = o.keys().find(|k| *k != "field" && *k != "counts") {
                return Fault::new(code::UNKNOWN_OPTION, format!("{model}: unknown date_hierarchy option '{}'", marked_key(key)));
            }
            if !o.contains_key("field") {
                return Fault::new(code::INVALID_VALUE, format!("{model}: date_hierarchy needs a field"));
            }
        }
    }
    let code = if unknown_field_key(&text).is_some() { code::UNKNOWN_OPTION } else { code::INVALID_VALUE };
    let mut marked = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(open) = rest.find('`') {
        let Some(len) = rest[open + 1..].find('`') else { break };
        let name = &rest[open + 1..open + 1 + len];
        marked.push_str(&rest[..open + 1]);
        marked.push_str(&marked_key(name));
        marked.push('`');
        rest = &rest[open + 2 + len..];
    }
    marked.push_str(rest);
    // A wrong value of a resource option: the model and the option come first.
    if code == code::INVALID_VALUE {
        if let [Step::Key(k), ..] = steps.as_slice() {
            if k == "widgets" {
                return Fault::new(code, format!("{} {}", opt("widgets"), shape_rule("widgets").unwrap_or_default()));
            }
        }
        if let Some(model) = resource_model(value, &steps) {
            let keys: Vec<&str> = steps[2..].iter().filter_map(|s| if let Step::Key(k) = s { Some(k.as_str()) } else { None }).collect();
            // The page of an inline takes the same options as a registered page.
            let page = if keys.first() == Some(&"inlines") { keys.iter().position(|k| *k == "page").map_or(&keys[..], |p| &keys[p + 1..]) } else { &keys[..] };
            if let Some(rule) = page.first().and_then(|k| shape_rule(k)) {
                return Fault::new(code, format!("{model}: {} {rule}", opt(page[0])));
            }
            if !keys.is_empty() {
                return Fault::new(code, format!("{model}: {}: {marked}", opt(&keys.join("."))));
            }
        }
    }
    Fault::new(code, format!("invalid admin config: {marked}"))
}

pub const DEFAULT_INLINE_MAX: usize = 20;
pub const MAX_INLINE_MAX: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineSave {
    WithParent,
    Row,
}

/// A to-many relation of a page, shown and edited in place on its detail page: the page of the child with the wrapper settings.
#[derive(Debug, Clone)]
pub struct Inline {
    /// The relation name: the name of the inline in the URL and in a change set.
    pub name: String,
    pub label: String,
    pub style: String,
    pub max: usize,
    pub collapsed: bool,
    pub delete: bool,
    pub save: InlineSave,
    /// The full name of the own write permission: `<label>.<name>`.
    pub permission: Option<String>,
    /// The page of the child model, with the overrides of the wrapper.
    pub resource: Resource,
    pub parent_from: Vec<Column>,
    pub child_to: Vec<Column>,
    /// The name of the registered page of the child model, when there is one: a row links to it.
    pub link: Option<String>,
}

impl DateHierarchyConfig {
    fn field(&self) -> &str {
        match self {
            DateHierarchyConfig::Field(f) => f,
            DateHierarchyConfig::Full(f) => &f.field,
        }
    }
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

/// How the host shows the stored bytes of a column: the host converts each value, and `Column::ty` is the type of the shown value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// JSON, read and written through the host.
    Json,
    /// A text that the host makes of the bytes; the field is read-only.
    Text,
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
    pub view: Option<View>,
    /// The column that orders a list sorted by this one, when it is not this column itself.
    pub sort: Option<Box<Column>>,
}

impl Column {
    /// The column that a sort by this column orders and pages by.
    pub fn order_by(&self) -> &Column {
        self.sort.as_deref().unwrap_or(self)
    }

    /// Two cells of this column hold the same value. A JSON view compares numbers by value, so `1` and `1.0` match.
    pub fn same(&self, a: &Value, b: &Value) -> bool {
        match self.view {
            Some(View::Json) => crate::json::same_json(a, b),
            _ => crate::json::same(self.ty.ty, a, b),
        }
    }
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

/// A detail tab that a package gives: `name` is `<package>.<tab>`, `module` the path of its file under the prefix.
#[derive(Debug, Clone)]
pub struct PackageTab {
    pub name: String,
    pub title: String,
    pub module: String,
}

#[derive(Debug, Clone)]
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
    /// Field (column label) to the field permission that shows it.
    pub see: BTreeMap<String, String>,
    /// Field (column label) to the field permission that lets a user change it.
    pub change: BTreeMap<String, String>,
    /// The full names (`<label>.<name>`) of the field permissions of this page and of its inlines that bind a field, sorted: the key of a user view.
    pub bound: Vec<String>,
    /// The labels of the fields that the user of this view may not see: empty on the page itself.
    pub hidden: BTreeSet<String>,
    /// On the view of the audit log: for each audited resource, the fields that the user may not see. The log drops them from `changes`.
    pub log_hidden: BTreeMap<String, BTreeSet<String>>,
    /// The labels of the fields that the user of this view may see and not change.
    pub locked: BTreeSet<String>,
    pub actions: Vec<Action>,
    /// Bulk delete of the list: on by default.
    pub bulk_delete: bool,
    pub set_password: bool,
    pub title: String,
    pub group: Option<String>,
    /// A lucide icon name for the navigation item.
    pub icon: Option<String>,
    /// The key fields as columns, in key order.
    pub key_columns: Vec<Column>,
    /// Positions of the key columns in `columns`: a row names its record with them.
    pub key_positions: Vec<usize>,
    /// The fields of the detail page: every field of the model that the database does not generate.
    pub detail: Vec<Column>,
    /// To-one relations that lead to the key of another resource.
    pub links: Vec<Link>,
    /// Foreign-key columns of the list that show the label of the related record, read in the same statement.
    pub refs: Vec<RefLabel>,
    /// The sections, read-only fields, widgets and module of the forms.
    pub form: crate::form::FormSpec,
    /// The metadata that does not depend on the user, built once by `Site::build`.
    pub meta: serde_json::Value,
    pub search: Vec<SearchField>,
    pub search_with: Option<String>,
    /// The labels of the list columns that a user edits in the list: every user view cuts it to the fields that user may change.
    pub editable: Vec<String>,
    pub sortable: Vec<Sortable>,
    /// The config names the sort columns: no table size rule.
    pub sortable_explicit: bool,
    /// The titles of columns that the config names.
    pub labels: BTreeMap<String, String>,
    /// What an empty value shows in the list.
    pub empty: Option<String>,
    pub empty_fields: BTreeMap<String, String>,
    /// Column label to value (as text) to a colour of the palette.
    pub badges: BTreeMap<String, BTreeMap<String, String>>,
    /// The filters of the filter bar.
    pub filters: Vec<Filter>,
    pub named_filters: Vec<NamedFilter>,
    /// The date field of the drill-down bar. It is also a filter, named `field`.
    pub date_hierarchy: Option<DateHierarchy>,
    /// To-many relations that the detail page shows as tabs.
    pub tabs: Vec<Tab>,
    /// The rows that staff see and touch: its filter joins every read of the resource.
    pub queryset: Option<Queryset>,
    /// The per-row rules (`change`, `delete`): each one is a flag in the list and the detail.
    pub can: Vec<(String, Value)>,
    /// The text that a failed named `@@check` shows in the form.
    pub check_messages: BTreeMap<String, String>,
    /// The host functions that run the writes.
    pub service: crate::site_config::ServiceConfig,
    /// The controls of the form that only a service reads.
    pub inputs: Vec<crate::site_config::InputConfig>,
    /// The resource writes a protected model, by its own opt-in.
    pub allow_protected: bool,
    /// The host reports host behavior that the schema does not show.
    pub host_behavior: bool,
    /// The names of the named read defaults that read no scope value: each one has a bypass permission.
    pub bypass: Vec<String>,
    /// The names of the read defaults that read a `scope.<name>` value: the admin sets no scope value, so every admin read turns them off.
    pub scope_defaults: Vec<String>,
    /// The relation fields with a picker or chips.
    pub relations: Vec<crate::relations::Relation>,
    /// The Json fields that hold an uploaded file.
    pub files: Vec<String>,
    /// The to-many relations that the detail page edits in place.
    pub inlines: Vec<Inline>,
    /// The package tabs of the detail page.
    pub package_tabs: Vec<PackageTab>,
    pub(crate) frames: Vec<crate::frames::Frame>,
    pub(crate) fieldsets: Vec<crate::site_config::Fieldset>,
    tabs_config: Option<Vec<String>>,
    /// Whether the table is large, and when it was read.
    large: Arc<Mutex<Option<(Instant, bool)>>>,
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

/// Where an action shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// The list, on the selected rows or on all rows that match.
    Bulk,
    /// One row of a list or of a tab.
    Row,
    /// The detail page of one record.
    Detail,
    /// The list, with no rows.
    List,
    /// The form, next to the save button: runs on the saved record.
    Submit,
}

impl Placement {
    pub const NAMES: [&'static str; 5] = ["bulk", "row", "detail", "list", "submit"];

    pub fn parse(text: &str) -> Option<Placement> {
        Some(match text {
            "bulk" => Placement::Bulk,
            "row" => Placement::Row,
            "detail" => Placement::Detail,
            "list" => Placement::List,
            "submit" => Placement::Submit,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        Self::NAMES[self as usize]
    }

    pub(crate) fn single(self) -> bool {
        matches!(self, Self::Row | Self::Detail | Self::Submit)
    }
}

/// The colour of an action button.
pub const VARIANTS: [&str; 5] = ["default", "primary", "success", "warning", "danger"];

/// One action of a resource, checked.
#[derive(Debug, Clone)]
pub struct Action {
    pub name: String,
    pub label: String,
    pub placement: Option<Placement>,
    /// One of `VARIANTS`: the colour of the button. None: `default`.
    pub variant: Option<String>,
    /// A lucide icon name.
    pub icon: Option<String>,
    /// The label of a dropdown menu in the detail header. Actions with the same label share one menu.
    pub menu: Option<String>,
    pub permission: Option<String>,
    pub confirm: Option<String>,
    pub inputs: Vec<crate::site_config::InputConfig>,
    pub applies: Option<Value>,
    /// The action's full permission rule, including change unless replaced.
    pub can: Option<Value>,
    pub sync: bool,
    pub threaded: bool,
}

impl Action {
    /// The name of the flag of a row that tells whether `applies_to` holds.
    pub fn flag(&self) -> String {
        format!("applies:{}", self.name)
    }

    fn can_flag(&self) -> String {
        format!("can:{}", self.name)
    }
}

/// The per-row rules. Each one is a flag of a row.
/// The name of the built-in action that deletes the selected rows.
pub const BULK_DELETE: &str = "bulk_delete";
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

/// The date field of the drill-down bar.
#[derive(Debug, Clone)]
pub struct DateHierarchy {
    /// The name of the field and of its filter: `created` or `author.joined`.
    pub field: String,
    pub column: Column,
    pub counts: bool,
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
    pub child: crate::sight::Sight<'a>,
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

/// A foreign-key column of a list whose related record has a label: one more expression of the list statement, a join on the relation.
#[derive(Debug, Clone)]
pub struct RefLabel {
    /// The label of the foreign-key column in the list.
    pub column: String,
    /// The name of the target resource.
    pub target: String,
    pub expr: Value,
}

pub struct Site {
    pub schema: Arc<Schema>,
    /// The path of the UI, without a trailing `/`. The cookies are scoped to `<base_path>/`.
    pub base_path: String,
    /// The path of the API, under `base_path`.
    pub prefix: String,
    pub strategy: Strategy,
    pub resources: Vec<Resource>,
    /// The brand of the UI, checked.
    pub brand: serde_json::Value,
    /// The navigation groups in order.
    pub groups: Vec<String>,
    /// A lucide icon name for each group, checked against `groups`.
    pub group_icons: BTreeMap<String, String>,
    /// The time limit of each statement of a request. None: no limit.
    pub statement_timeout: Option<Duration>,
    /// How long an export waits for the client to take the next chunk before it stops. None: no limit.
    pub export_idle_timeout: Option<Duration>,
    /// What an operator should fix, found when the site was built: a search field with no index.
    pub warnings: Vec<String>,
    /// The custom routes, checked.
    pub routes: Vec<crate::routes::Route>,
    /// The loaded extension packages.
    pub packages: crate::packages::Packages,
    pub(crate) ui_csp: axum::http::HeaderValue,
    pub(crate) frame_cards: Vec<Value>,
    pub(crate) embeds: BTreeMap<String, crate::frames::Embed>,
    pub(crate) meta_builds: AtomicUsize,
    /// The user views of the pages that bind fields: one for each page and set of granted names.
    pub(crate) views: Mutex<Views>,
}

impl Site {
    /// Plans the resources. The text of a wrong config spells the option keys as the `spelling` key of the config says, `snake` by default.
    pub fn build(schema: Arc<Schema>, config_json: &str) -> Result<Site, AdminError> {
        Site::build_with_handles(schema, config_json).map(|(site, _)| site)
    }

    /// Like [`Site::build`], and returns the host name of each local function id that the config tagged with `$fn`.
    pub fn build_with_handles(schema: Arc<Schema>, config_json: &str) -> Result<(Site, Handles), AdminError> {
        let spelling = serde_json::from_str::<Value>(config_json).ok().and_then(|v| v.get("spelling").cloned()).and_then(|v| serde_json::from_value::<Spelling>(v).ok()).unwrap_or_default();
        Site::plan(schema, config_json).map_err(|fault| fault.render(spelling))
    }

    fn plan(schema: Arc<Schema>, config_json: &str) -> Result<(Site, Handles), Fault> {
        let mut handles = Handles::new();
        let mut config: Config = if config_json.trim().is_empty() {
            Config::default()
        } else {
            let mut value: Value = serde_json::from_str(config_json).map_err(|e| Fault::new(code::INVALID_JSON, format!("invalid admin config: {e}")))?;
            handles = site_config::lower(&mut value, &schema)?;
            merge_inline_pages(&mut value)?;
            serde_path_to_error::deserialize(&value).map_err(|e| parse_fault(&value, e))?
        };
        let base_path = normalize_base(config.base_path.as_deref().unwrap_or(key::DEFAULT_BASE))?;
        let prefix = match config.prefix.as_deref() {
            None => format!("{base_path}/api"),
            Some(text) => normalize_prefix(text, &base_path)?,
        };
        // `normalize_prefix` put the prefix under the base path: its first segment below the base must be free of pages and files.
        let first = prefix.strip_prefix(base_path.as_str()).and_then(|p| p.strip_prefix('/')).and_then(|p| p.split('/').next()).unwrap_or_default();
        if first == "-" || first == "auth" || crate::ui::is_file_segment(first) {
            return err_code(code::PATH, format!("prefix {prefix:?} starts with {:?} below the base path: the UI uses that path for its pages and files", format!("{base_path}/{first}")));
        }
        let strategy = match config.permissions.as_deref() {
            None => Strategy::Page,
            Some(text) => Strategy::parse(text).ok_or_else(|| invalid_fault(format!("permissions {text:?} is not \"page\", \"model\" or \"access\"")))?,
        };
        let label = config.label.as_deref().unwrap_or("admin");
        check_name("label", label)?;
        let packages = crate::packages::Packages::build(&config.packages)?;
        let origins = crate::frames::origins(&config.frame_origins)?;
        let ui_csp = crate::ui::policy(&base_path, !packages.is_empty(), &origins)?;
        let frame_cards = crate::frames::build(&config.frame_cards, &origins, &base_path, None)?
            .iter().map(|f| f.payload(&base_path, |_| None)).collect();
        let global = crate::form::global(&schema, packages.clone(), &config.widgets)?;
        let explicit = !config.resources.is_empty();
        let mut configs: Vec<ResourceConfig> = if explicit {
            std::mem::take(&mut config.resources)
        } else {
            schema.models.iter().filter(|m| concrete(m)).map(|m| ResourceConfig::of(&m.ir.name)).collect()
        };
        if explicit {
            let mut seen = BTreeSet::new();
            for r in &configs {
                let idx = schema.model_idx(&r.model).map_err(|_| Fault::new(code::UNKNOWN_MODEL, format!("unknown model {:?}", r.model)))?;
                if !concrete(schema.model(idx)) {
                    return invalid(format!("{}: not a concrete model, so not a resource", r.model));
                }
                if !seen.insert(idx) {
                    return err_code(code::DUPLICATE, format!("{}: registered twice", r.model));
                }
            }
        }
        let added = builtin::apply(&schema, &mut configs);
        let mut resources = Vec::new();
        let mut warnings = Vec::new();
        for c in &configs {
            let idx = schema.model_idx(&c.model).expect("checked above, or a model of the schema");
            if !c.files.is_empty() && !config.storage {
                return err_code_in(&c.model, code::INVALID_VALUE, "files needs a file storage registry: pass `storage` to Admin".to_string());
            }
            if c.set_password && config.user_table.as_deref() != Some(schema.model(idx).ir.table.as_str()) {
                return invalid(format!("{}: \u{1}set_password\u{2} needs the auth user model", c.model));
            }
            let mut built = resource(&schema, idx, c, strategy, label, &global, &mut warnings)?;
            built.frames = crate::frames::build(&c.frames, &origins, &base_path, Some((&schema, idx)))?;
            crate::frames::check_fieldsets(&built)?;
            resources.push(built);
        }
        let mut groups = config.groups.clone();
        for g in &groups {
            if g.trim().is_empty() || groups.iter().filter(|o| *o == g).count() > 1 {
                return err_code(code::DUPLICATE, format!("groups: {g:?} is empty or listed twice"));
            }
        }
        // The built-in pages bring their group: a site that lists its groups need not list this one.
        if !groups.is_empty() && !groups.iter().any(|g| g == builtin::GROUP) && resources.iter().any(|r| r.group.as_deref() == Some(builtin::GROUP)) {
            groups.push(builtin::GROUP.to_owned());
        }
        for r in &resources {
            match (&r.group, groups.is_empty()) {
                (Some(g), false) if !groups.contains(g) => return invalid_in(&r.name, format!("group {g:?} is not in groups")),
                (Some(g), true) if !seen_group(&resources, g, &r.name) => groups.push(g.clone()),
                _ => {}
            }
            if r.name == "auth" {
                return invalid(format!("{}: its page {} is the path of the single sign-on routes", r.name, key::url(&base_path, &r.name, "").trim_end_matches('/')));
            }
            if r.name == first {
                return invalid(format!("{}: its page {} is, or holds, the API prefix", r.name, key::url(&base_path, &r.name, "").trim_end_matches('/')));
            }
        }
        let links: Vec<Vec<Link>> = resources.iter().map(|r| links_of(&schema, &resources, r)).collect();
        for (r, l) in resources.iter_mut().zip(links) {
            r.links = l;
        }
        let refs: Vec<Vec<RefLabel>> = resources.iter().map(|r| refs_of(&schema, &resources, r)).collect();
        for (r, f) in resources.iter_mut().zip(refs) {
            r.refs = f;
        }
        let tabs: Vec<Vec<Tab>> = (0..resources.len()).map(|i| tabs_of(&schema, &resources, i)).collect::<Result<_, _>>()?;
        for (r, t) in resources.iter_mut().zip(tabs) {
            let mut names: BTreeSet<&str> = t.iter().map(|t| t.name.as_str()).chain(r.package_tabs.iter().map(|t| t.name.as_str())).collect();
            for name in r.fieldsets.iter().map(|f| f.name.as_str()).chain(r.frames.iter().map(|f| f.name())) {
                if !names.insert(name) {
                    return err_code_in(&r.name, code::DUPLICATE, format!("duplicate detail tab {name:?}"));
                }
            }
            r.tabs = t;
        }
        for (r, c) in configs.iter().enumerate() {
            let built = crate::relations::build(&schema, &resources, &resources[r], &c.relations)?;
            resources[r].relations = built;
        }
        for (i, c) in configs.iter().enumerate() {
            let built = inlines_of(&schema, &resources, i, c, strategy, label, &global, &mut warnings)?;
            resources[i].inlines = built;
        }
        for r in &resources {
            for f in &r.filters {
                if let FilterKind::Relation { target, .. } = &f.kind {
                    if !resources.iter().any(|o| &o.name == target) {
                        return invalid(format!("{}: filter {:?} leads to {target}, which is not a resource", r.name, f.name));
                    }
                }
            }
        }
        let meta_builds = AtomicUsize::new(0);
        for r in &mut resources {
            r.bound = crate::sight::bound_names(r);
        }
        crate::sight::bind_log(&mut resources);
        for r in &mut resources {
            r.meta = meta::resource(&schema, r, &meta_builds);
        }
        for (g, icon) in &config.group_icons {
            if !groups.contains(g) {
                return invalid(format!("\u{1}group_icons\u{2}: {g:?} is not in groups"));
            }
            if !meta::icon_ok(icon) {
                return invalid(format!("\u{1}group_icons\u{2}: icon {icon:?} of {g:?} must be a lucide icon name such as \"book-open\""));
            }
        }
        let switcher = SwitcherConfig::merge(config.switcher.as_ref(), config.switcher_file.as_ref()).or_else(invalid)?;
        let brand = meta::brand(config.brand.as_ref(), config.theme.as_ref(), switcher.as_ref())?;
        let statement_timeout = match config.statement_timeout {
            None => Some(Duration::from_millis(DEFAULT_STATEMENT_TIMEOUT_MS)),
            Some(secs) if !secs.is_finite() || secs < 0.0 => return err_code(code::INVALID_VALUE, format!("{} is a number of seconds, 0 or more (0 sets no limit)", opt("statement_timeout"))),
            Some(secs) => match (secs * 1000.0).round() as u64 {
                ms if ms > MAX_STATEMENT_TIMEOUT_MS => return err_code(code::INVALID_VALUE, format!("{} is {secs}, at most {} seconds", opt("statement_timeout"), MAX_STATEMENT_TIMEOUT_MS as f64 / 1000.0)),
                0 => None,
                ms => Some(Duration::from_millis(ms)),
            },
        };
        let export_idle_timeout = match config.export_idle_timeout_ms.unwrap_or(DEFAULT_EXPORT_IDLE_TIMEOUT_MS) {
            0 => None,
            ms => Some(Duration::from_millis(ms)),
        };
        let mut site = Site { schema, base_path, prefix, strategy, resources, brand, groups, group_icons: config.group_icons.clone(), statement_timeout, export_idle_timeout, warnings, routes: Vec::new(), packages, ui_csp, frame_cards, embeds: BTreeMap::new(), meta_builds, views: Mutex::new(Default::default()) };
        site.embeds = crate::frames::build_embeds(&config.embeds, &site)?;
        site.routes = crate::routes::build(&config.routes, &site)?;
        if let Some(role) = added.role {
            // The picker lists the names of the site, the names of the routes included, so it waits for the routes.
            let names = site.permission_names();
            if let Some(relation) = site.resources[role].relations.iter_mut().find(|r| r.name == "permissions") {
                relation.choices = Some(builtin::names_filter(&names));
            }
        }
        Ok((site, handles))
    }

    /// How many times the metadata of a resource was made: one per resource at `build`, and one per set of granted names of a page that binds fields.
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
                let mut entry = serde_json::json!({"group": g, "resources": names});
                if let Some(icon) = self.group_icons.get(g) {
                    entry["icon"] = serde_json::json!(icon);
                }
                out.push(entry);
            }
        }
        serde_json::Value::Array(out)
    }

    /// The detail URL of a record (see `key::url_of`). The error text names the resource or the key.
    pub fn url(&self, resource: &str, key: &Value) -> Result<String, String> {
        let resource = self.resource(resource).ok_or_else(|| format!("{resource} is not an admin resource"))?;
        crate::key::url_of(&self.base_path, resource, key)
    }

    /// Whether a request path (with the site prefix) leads to a route that takes or gives bytes: a file, an export or a custom route.
    /// The test clients ask this, so the list of such routes lives here only.
    pub fn is_raw(&self, path: &str) -> bool {
        let path = path.split('?').next().unwrap_or(path);
        let rest = path.strip_prefix(self.prefix.as_str()).unwrap_or(path);
        ["/files/", "/export/", "/routes/"].iter().any(|p| rest.starts_with(p))
    }

    /// The page as built, with no user view: for names, permissions and the build. A handler that reads columns uses `sight`.
    pub fn resource(&self, name: &str) -> Option<&Resource> {
        self.resources.iter().find(|r| r.name == name)
    }

    /// The rows of an inline of a record: the child rows whose `child_to` equals the `parent_from` of the record.
    pub fn inline_related<'a>(&self, inline: &'a Inline) -> Related<'a> {
        Related { child: crate::sight::Sight::Page(&inline.resource), parent_from: inline.parent_from.clone(), child_to: inline.child_to.clone() }
    }

    /// The rows of the to-many relation `rel` of a record of `parent`: none when `rel` is not a tab of the resource.
    pub fn plan_related<'a>(&'a self, parent: &Resource, rel: &str, grants: &crate::permissions::Grants) -> Option<Related<'a>> {
        let tab = parent.tabs.iter().find(|t| t.name == rel)?;
        let child = self.sight(&tab.target, grants)?;
        let parent_model = self.schema.model(parent.model);
        let relation = parent_model.ir.relations.iter().find(|r| r.name == rel)?;
        let (parent_from, child_to) = (field_columns(&self.schema, parent.model, &relation.from)?, field_columns(&self.schema, child.model, &relation.to)?);
        Some(Related { child, parent_from, child_to })
    }
}

/// The columns of the fields that `names` (field or column names) point to.
pub(crate) fn field_columns(schema: &Schema, model: usize, names: &[String]) -> Option<Vec<Column>> {
    names
        .iter()
        .map(|n| {
            let f = schema.model(model).ir.fields.iter().find(|f| f.name == *n || f.column == *n)?;
            column(schema, model, &f.name).ok()
        })
        .collect()
}

const INLINE_SAVE_REASON: &str = "a service writes one record: an inline row is written by the page of the parent (with_parent) or by the page of the child (row)";
const INLINE_FIELD_PERMISSIONS_REASON: &str = "field permissions belong to the registered page of the child";

/// A page key that an inline wrapper cannot override, with the reason.
const INLINE_PAGE_REFUSED: &[(&str, &str)] = &[
    ("create", INLINE_SAVE_REASON),
    ("update", INLINE_SAVE_REASON),
    ("delete", INLINE_SAVE_REASON),
    ("inlines", "an inline is never nested"),
    ("files", "a file field needs a multipart body, which a change set does not carry"),
    ("title", "an inline is not in the navigation"),
    ("group", "an inline is not in the navigation"),
    ("icon", "an inline is not in the navigation"),
    ("perms", "the permission of an inline follows its parent: use \u{1}permission\u{2} of the inline"),
    ("label", "the permission of an inline follows its parent: use \u{1}permission\u{2} of the inline"),
    ("field_permissions", INLINE_FIELD_PERMISSIONS_REASON),
    ("see", INLINE_FIELD_PERMISSIONS_REASON),
    ("change", INLINE_FIELD_PERMISSIONS_REASON),
    ("actions", "an inline has no actions"),
    ("bulk_delete", "an inline has no bulk delete"),
    ("set_password", "an inline has no set password action"),
    ("form_component", "a replaced form cannot be a row of a table"),
    ("detail_tabs", "a detail tab belongs to the registered page of the child"),
    ("frames", "a frame tab belongs to the registered page of the child"),
    ("fieldsets", "a fieldset belongs to the registered page of the child"),
    ("allow_protected_writes", "the opt-in belongs to the registered page of the child"),
];

/// Keys that a host adds to a lowered page. A user does not write them.
const LOWERED_PAGE_KEYS: &[&str] = &["model", "service", "host_behavior", "views"];

/// The fault of the first page key that an inline cannot take: a refused key, or a key that no page has.
fn refused_page_key(page: &Map<String, Value>, at: &str) -> Option<Fault> {
    page.keys().find_map(|key| {
        if let Some((_, reason)) = INLINE_PAGE_REFUSED.iter().find(|(k, _)| k == key) {
            return Some(Fault::new(code::UNSUPPORTED, format!("{at}page does not take '{}': {reason}", opt(key))));
        }
        let known = crate::builder::RESOURCE_KEYS.contains(&key.as_str()) || LOWERED_PAGE_KEYS.contains(&key.as_str());
        (!known).then(|| Fault::new(code::UNKNOWN_OPTION, format!("{at}unknown page option '{}'", opt(key))))
    })
}

/// Gives each inline the page of its child model: the registered page without its own inlines, with the keys of the wrapper's `page` laid over it, top level key by key.
fn merge_inline_pages(config: &mut Value) -> Result<(), Fault> {
    let Some(resources) = config.get("resources").and_then(Value::as_array).cloned() else { return Ok(()) };
    let mut merged = resources.clone();
    for (r, parent) in resources.iter().enumerate() {
        let Some(inlines) = parent.get("inlines").and_then(Value::as_array) else { continue };
        let at = parent.get("model").and_then(Value::as_str).unwrap_or("?");
        let mut built = Vec::new();
        for inline in inlines {
            let Some(model) = inline.get("model").and_then(Value::as_str) else {
                built.push(inline.clone());
                continue;
            };
            let mut page = resources.iter().find(|p| p.get("model").and_then(Value::as_str) == Some(model)).cloned().unwrap_or_else(|| json!({"model": model}));
            if let Some(page) = page.as_object_mut() {
                // A wrapped page does not bring its inlines: an inline is never nested.
                page.remove("inlines");
            }
            let overrides = match inline.get("page") {
                None | Some(Value::Null) => Map::new(),
                Some(Value::Object(m)) => m.clone(),
                Some(_) => return invalid(format!("{at}: inline {model}: page is an object")),
            };
            if let Some(fault) = refused_page_key(&overrides, &format!("{at}: inline {model}: ")) {
                return Err(fault);
            }
            if overrides.get("model").is_some_and(|m| m.as_str() != Some(model)) {
                return invalid(format!("{at}: inline {model}: page.model is the model of the inline"));
            }
            if let Some(page) = page.as_object_mut() {
                page.extend(overrides);
            }
            let mut inline = inline.clone();
            inline["page"] = page;
            built.push(inline);
        }
        merged[r]["inlines"] = Value::Array(built);
    }
    config["resources"] = Value::Array(merged);
    Ok(())
}

/// The to-many relation of `parent` that an inline named: `via`, or the only one that leads to the child model.
fn inline_relation<'a>(schema: &'a Schema, parent: &Resource, child: usize, via: Option<&str>, at: &str) -> Result<&'a monorm_core::ir::RelationIr, Fault> {
    let pm = schema.model(parent.model);
    let child_name = &schema.model(child).ir.name;
    let leads = |rel: &monorm_core::ir::RelationIr| rel.kind == RelKind::Many && rel.through.is_none() && rel.path.is_empty() && !rel.is_conditioned() && matches!(pm.relation(&rel.name), Ok((_, t)) if t == child);
    match via {
        Some(v) => {
            let Some(rel) = pm.ir.relations.iter().find(|r| r.name == v) else { return err_code(code::RELATION_UNKNOWN, format!("{at}: via {v:?} is not a relation of {}", parent.name)) };
            if rel.is_conditioned() {
                return err_code(code::UNSUPPORTED, format!("{at}: via {v:?} has fixed conditions or union members, which the admin does not support"));
            }
            if !leads(rel) {
                return invalid(format!("{at}: via {v:?} is not a to-many relation (not a many-to-many) to {child_name}"));
            }
            Ok(rel)
        }
        None => {
            let found: Vec<_> = pm.ir.relations.iter().filter(|r| leads(r)).collect();
            match found.as_slice() {
                [] => err_code(code::RELATION_UNKNOWN, format!("{at}: {} has no to-many relation to {child_name}", parent.name)),
                [one] => Ok(one),
                many => invalid(format!("{at}: ambiguous: {} has {} relations to {child_name} ({}): name one with via", parent.name, many.len(), many.iter().map(|r| r.name.as_str()).collect::<Vec<_>>().join(", "))),
            }
        }
    }
}

/// The inlines of resource `i`: the page of each child with its wrapper, checked.
#[allow(clippy::too_many_arguments)]
fn inlines_of(schema: &Schema, resources: &[Resource], i: usize, config: &ResourceConfig, strategy: Strategy, site_label: &str, global: &crate::form::Global, warnings: &mut Vec<String>) -> Result<Vec<Inline>, Fault> {
    let parent = &resources[i];
    let mut out: Vec<Inline> = Vec::new();
    for c in &config.inlines {
        let at = format!("{}: inline {}", parent.name, c.model);
        let child = schema.model_idx(&c.model).map_err(|_| Fault::new(code::UNKNOWN_MODEL, format!("{at}: unknown model")))?;
        if !concrete(schema.model(child)) {
            return invalid(format!("{at}: not a concrete model"));
        }
        let rel = inline_relation(schema, parent, child, c.via.as_deref(), &at)?;
        if out.iter().any(|o| o.name == rel.name) {
            return invalid(format!("{at}: the relation {:?} is already an inline of {}", rel.name, parent.name));
        }
        let default;
        let page = match &c.page {
            Some(p) => p,
            None => {
                default = Box::new(ResourceConfig::of(&c.model));
                &default
            }
        };
        let mut inline_resource = resource(schema, child, page, strategy, site_label, global, warnings)?;
        if !inline_resource.files.is_empty() {
            return invalid(format!("{at}: file fields are not edited in an inline: leave files out of the page of {}", c.model));
        }
        let (Some(parent_from), Some(child_to)) = (field_columns(schema, parent.model, &rel.from), field_columns(schema, child, &rel.to)) else {
            return invalid(format!("{at}: the relation {:?} needs fields on both sides", rel.name));
        };
        let save = match c.save.as_deref() {
            None | Some("with_parent") => InlineSave::WithParent,
            Some("row") => InlineSave::Row,
            Some(other) => return invalid(format!("{at}: save is {other:?}, not with_parent or row")),
        };
        let style = match c.style.as_deref() {
            None | Some("table") => "table",
            Some("stacked") => "stacked",
            Some(other) => return invalid(format!("{at}: style is {other:?}, not table or stacked")),
        };
        let max = c.max.unwrap_or(DEFAULT_INLINE_MAX);
        if max == 0 || max > MAX_INLINE_MAX {
            return invalid(format!("{at}: max is {max}, between 1 and {MAX_INLINE_MAX}"));
        }
        let permission = match &c.permission {
            None => None,
            Some(_) if strategy == Strategy::Access => return invalid(format!("{at}: permission needs the page or model strategy")),
            Some(name) => {
                check_name(&format!("{at}: permission"), name)?;
                Some(format!("{}.{name}", parent.label))
            }
        };
        if c.label.as_ref().is_some_and(|l| l.trim().is_empty() || l.chars().count() > 80) {
            return invalid(format!("{at}: label has 1 to 80 characters"));
        }
        if save == InlineSave::WithParent {
            let child_host = [Op::Create, Op::Update, Op::Delete].iter().any(|&op| crate::hostwrite::route(schema, &inline_resource, op).is_some());
            for op in [Op::Create, Op::Update] {
                match crate::hostwrite::route(schema, parent, op) {
                    None if child_host => return invalid(format!("{at}: the page of {} has a service or host behavior, so its rows cannot save in one change set with a parent that writes the plain way: use save \"row\", or a service on {}", c.model, parent.name)),
                    Some(f) if !f.starts_with("service.") => return invalid(format!("{at}: {} has host behavior and no service for {}: use save \"row\", or a service", parent.name, crate::hostwrite::op_name(op))),
                    _ => {}
                }
            }
        }
        inline_resource.links = links_of(schema, resources, &inline_resource);
        inline_resource.relations = crate::relations::build(schema, resources, &inline_resource, &page.relations)?;
        out.push(Inline {
            name: rel.name.clone(),
            label: c.label.clone().unwrap_or_else(|| meta::humanize(&rel.name)),
            style: style.to_owned(),
            max,
            collapsed: c.collapsed,
            delete: c.delete.unwrap_or(true),
            save,
            permission,
            resource: inline_resource,
            parent_from,
            child_to,
            link: resources.iter().find(|r| r.model == child).map(|r| r.name.clone()),
        });
    }
    Ok(out)
}

/// The tabs of resource `i`: the to-many relations that lead to a resource, and are not a many-to-many.
/// A relation with fixed conditions or union members has no tab: the tab and its "Add" prefill read only `from` and `to`.
fn tabs_of(schema: &Schema, resources: &[Resource], i: usize) -> Result<Vec<Tab>, Fault> {
    let r = &resources[i];
    let model = schema.model(r.model);
    let usable = |rel: &monorm_core::ir::RelationIr| -> Option<String> {
        let (_, target) = model.relation(&rel.name).ok()?;
        let child = resources.iter().find(|o| o.model == target)?;
        (rel.kind == RelKind::Many && rel.through.is_none() && rel.path.is_empty() && !rel.is_conditioned() && field_columns(schema, r.model, &rel.from).is_some() && field_columns(schema, child.model, &rel.to).is_some()).then(|| child.name.clone())
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
                    return err_code_in(&r.name, code::RELATION_UNKNOWN, format!("tabs has {n:?}, which is not a relation"));
                };
                let Some(target) = usable(rel) else {
                    return invalid_in(&r.name, format!("tab {n:?} needs a to-many relation (not a many-to-many, no fixed conditions or union members) to a resource"));
                };
                if out.iter().any(|t: &Tab| t.name == *n) {
                    return err_code_in(&r.name, code::DUPLICATE, format!("tab {n:?} is listed twice"));
                }
                out.push(Tab { name: rel.name.clone(), target });
            }
        }
    }
    Ok(out)
}

fn normalize_prefix(prefix: &str, base: &str) -> Result<String, Fault> {
    let trimmed = prefix.trim_end_matches('/');
    if !trimmed.is_empty() && !trimmed.starts_with('/') {
        return err_code(code::PATH, format!("prefix {prefix:?} must start with /"));
    }
    if trimmed == base {
        return err_code(code::PATH, format!("prefix {prefix:?} is the path of the UI: every {base}/... path would be an API path and the pages would not load"));
    }
    // The session cookie has `Path=<base>/`: a browser sends it to no route outside the base path.
    if !trimmed.strip_prefix(base).is_some_and(|rest| rest.starts_with('/')) {
        return err_code(code::PATH, format!("prefix {prefix:?} must sit under the base path {base:?}: the session cookie goes only to paths under it"));
    }
    Ok(trimmed.to_owned())
}

/// The base path: one leading `/`, no trailing `/`, plain segments. The text goes into a `Set-Cookie` header, a JSON import map and HTML.
pub fn normalize_base(base: &str) -> Result<String, Fault> {
    let trimmed = base.trim_end_matches('/');
    if trimmed.is_empty() {
        return err_code(code::PATH, format!("\u{1}base_path\u{2} {base:?} is empty: name a path such as \"/admin\" (the health routes keep the root)"));
    }
    if !trimmed.starts_with('/') {
        return err_code(code::PATH, format!("\u{1}base_path\u{2} {base:?} must start with /"));
    }
    if let Some(bad) = trimmed.chars().find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '.' | '_' | '~'))) {
        return err_code(code::PATH, format!("\u{1}base_path\u{2} {base:?} holds {bad:?}: use letters, digits and - . _ ~ between slashes"));
    }
    if trimmed[1..].split('/').any(|s| s.is_empty() || s == "." || s == "..") {
        return err_code(code::PATH, format!("\u{1}base_path\u{2} {base:?} has an empty or dot segment"));
    }
    Ok(trimmed.to_owned())
}

/// A model with its own rows: it has a table, and it is not a derived model.
fn concrete(m: &monorm_core::schema::Model) -> bool {
    !m.ir.table.is_empty() && m.view.is_none()
}

impl ResourceConfig {
    fn of(model: &str) -> ResourceConfig {
        ResourceConfig { model: model.to_owned(), list: ListConfig::default(), allow: None, perms: None, label: None, title: None, group: None, icon: None, field_permissions: Vec::new(), see: BTreeMap::new(), change: BTreeMap::new(), actions: Vec::new(), bulk_delete: true, set_password: false, queryset: None, can: BTreeMap::new(), check_messages: BTreeMap::new(), service: Default::default(), inputs: Vec::new(), allow_protected_writes: false, host_behavior: false, form: None, readonly: Vec::new(), views: BTreeMap::new(), widgets: BTreeMap::new(), form_component: None, detail_tabs: Vec::new(), frames: Vec::new(), fieldsets: Vec::new(), relations: BTreeMap::new(), files: Vec::new(), inlines: Vec::new() }
    }
}

/// A part of a permission name: lowercase letters, digits and `_`, so `.` stays the label separator.
pub(crate) fn check_name(what: &str, text: &str) -> Result<(), Fault> {
    let ok = !text.is_empty() && text.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if ok { Ok(()) } else { invalid(format!("{what} {text:?} must be lowercase letters, digits and _")) }
}

/// The permission settings of one resource, checked.
struct PermissionOptions {
    label: String,
    allow: Vec<Op>,
    split_delete: bool,
}

fn permission_options(name: &str, config: &ResourceConfig, field_names: &[String], strategy: Strategy, site_label: &str) -> Result<PermissionOptions, Fault> {
    let label = config.label.clone().unwrap_or_else(|| site_label.to_owned());
    check_name("label", &label)?;
    let mut allow = Vec::new();
    for text in config.allow.clone().unwrap_or_else(|| Op::ALL.iter().map(|o| o.name().to_owned()).collect()) {
        let Some(op) = Op::parse(&text) else {
            return invalid_in(name, format!("allow has {text:?}, not one of read, create, update, delete"));
        };
        if !allow.contains(&op) {
            allow.push(op);
        }
    }
    let mut split_delete = false;
    if let Some(levels) = &config.perms {
        if strategy != Strategy::Page {
            return invalid_in(name, "perms applies to the page strategy only".to_string());
        }
        for level in levels {
            match level.as_str() {
                "read" | "manage" => {}
                "delete" => split_delete = true,
                other => return invalid_in(name, format!("perms has {other:?}, not one of read, manage, delete")),
            }
        }
        if !levels.iter().any(|l| l == "read") || !levels.iter().any(|l| l == "manage") {
            return invalid_in(name, "perms must name read and manage".to_string());
        }
    }
    if strategy == Strategy::Access && (!field_names.is_empty() || config.actions.iter().any(|a| a.permission.is_some())) {
        return invalid_in(name, "field and action names need the page or model strategy".to_string());
    }
    // An unnamed action needs `update`, and the access strategy has no such grant: every staff user would run it.
    if strategy == Strategy::Access {
        if let Some(a) = config.actions.iter().find(|a| a.permission.is_none()) {
            return invalid_in(name, format!("action {:?} has no permission, and the access strategy has no grant to check: use the page or model strategy", a.name));
        }
    }
    let mut seen = BTreeSet::new();
    for n in field_names {
        check_name(&format!("{name}: permission name"), n)?;
        if !seen.insert(n) {
            return err_code_in(name, code::DUPLICATE, format!("permission name {n:?} is declared twice"));
        }
    }
    let mut actions = BTreeSet::new();
    for a in &config.actions {
        if let Some(p) = &a.permission {
            check_name(&format!("{name}: permission name"), p)?;
            if field_names.contains(p) {
                return err_code_in(name, code::DUPLICATE, format!("permission name {p:?} is declared twice"));
            }
        }
        check_name(&format!("{name}: action"), &a.name)?;
        if !actions.insert(&a.name) {
            return err_code_in(name, code::DUPLICATE, format!("action {:?} is declared twice", a.name));
        }
    }
    Ok(PermissionOptions { label, allow, split_delete })
}

fn resource(schema: &Schema, idx: usize, config: &ResourceConfig, strategy: Strategy, site_label: &str, global: &crate::form::Global, warnings: &mut Vec<String>) -> Result<Resource, Fault> {
    let list = &config.list;
    let model = schema.model(idx);
    let name = model.ir.name.clone();
    let mut field_names = config.field_permissions.clone();
    for n in config.see.values().chain(config.change.values()) {
        if !field_names.contains(n) {
            field_names.push(n.clone());
        }
    }
    let PermissionOptions { label, allow, split_delete } = permission_options(&name, config, &field_names, strategy, site_label)?;
    let extras = extra_columns(schema, idx, &name, list)?;
    let extra = |n: &str| extras.columns.iter().find(|e| e.label == n).cloned();
    let mut columns = match &list.columns {
        Some(names) if names.is_empty() => return invalid_in(&name, "\u{1}list.columns\u{2} is empty".to_string()),
        Some(names) => names.iter().map(|n| extra(n).map_or_else(|| column(schema, idx, n), Ok)).collect::<Result<Vec<_>, _>>()?,
        None => model.ir.fields.iter().filter(|f| listable(f)).map(|f| column(schema, idx, &f.name)).collect::<Result<Vec<_>, _>>()?,
    };
    for e in &extras.columns {
        if !columns.iter().any(|c| c.label == e.label) {
            columns.push(e.clone());
        }
    }
    let listed: Vec<String> = columns.iter().map(|c| c.label.clone()).collect();
    let mut seen = BTreeSet::new();
    for c in &columns {
        if !seen.insert(c.label.clone()) {
            return err_code_in(&name, code::DUPLICATE, format!("column {:?} is listed twice", c.label));
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
        return invalid_in(&name, format!("\u{1}list.page_size\u{2} is {page_size}, between 1 and {MAX_PAGE_SIZE}"));
    }
    let key = name.to_lowercase();
    let mut key_columns = Vec::new();
    let mut key_positions = Vec::new();
    for k in model.key_fields() {
        let c = column(schema, idx, &k.name)?;
        key_positions.push(columns.iter().position(|o| o.label == c.label).ok_or_else(|| Fault::new(code::UNKNOWN_FIELD, format!("{name}: the key field {:?} is not a column", k.name)))?);
        key_columns.push(c);
    }
    let mut detail = model.ir.fields.iter().filter(|f| listable(f)).map(|f| column(schema, idx, &f.name)).collect::<Result<Vec<_>, _>>()?;
    if !config.views.is_empty() && !config.host_behavior {
        return invalid_in(&name, "views needs \u{1}host_behavior\u{2}: the host converts a view field and writes it".to_string());
    }
    let text_views = apply_views(&name, model, &config.views, &mut [&mut columns, &mut detail])?;
    let title = match &config.title {
        Some(t) if t.trim().is_empty() || t.chars().count() > 80 => return invalid_in(&name, "title has 1 to 80 characters".to_string()),
        Some(t) => t.clone(),
        None => meta::humanize(&name),
    };
    if config.group.as_ref().is_some_and(|g| g.trim().is_empty()) {
        return invalid_in(&name, "group is empty".to_string());
    }
    if let Some(icon) = config.icon.as_ref().filter(|i| !meta::icon_ok(i)) {
        return invalid_in(&name, format!("icon {icon:?} must be a lucide icon name such as \"book-open\""));
    }
    check_badges(schema, &name, &list.badges, &listed, &columns)?;
    let search = search_fields(schema, idx, &name, list, warnings)?;
    let mut sortable = sortable_columns(schema, idx, &name, list, &columns)?;
    for c in columns.iter().filter(|c| extras.sortable.contains(&c.label)) {
        let by = c.order_by();
        let indexed = by.path.is_empty() && by.source == Source::Field && model.field(&by.field).is_ok_and(|f| leads_btree(&model.ir, f, false));
        sortable.push(Sortable { column: c.clone(), indexed });
    }
    for key in list.labels.keys().chain(list.empty_fields.keys()) {
        if !columns.iter().any(|c| c.label == *key) && !detail.iter().any(|c| c.label == *key) {
            return err_code_in(&name, code::UNKNOWN_FIELD, format!("labels and \u{1}empty_fields\u{2} name {key:?}, which is not a column"));
        }
    }
    for text in list.labels.values().chain(list.empty_fields.values()).chain(&list.empty) {
        if text.chars().count() > 80 {
            return invalid_in(&name, "a label or an empty text has at most 80 characters".to_string());
        }
    }
    let date_hierarchy = date_hierarchy_of(schema, idx, &name, list, warnings)?;
    let filters = filter_specs(schema, idx, &name, list, date_hierarchy.as_ref())?;
    let named_filters = named_filters(schema, idx, &name, list)?;
    let queryset = queryset_of(schema, idx, &name, config.queryset.as_ref())?;
    let mut can = can_rules(schema, &name, &config.can)?;
    let actions = actions_of(schema, &name, &config.actions, &mut can, config.set_password)?;
    if let Some(k) = config.check_messages.keys().find(|k| !model.ir.constraints.iter().any(|c| matches!(c, ConstraintIr::Check { name: Some(n), .. } if n == *k))) {
        return invalid_in(&name, format!("\u{1}check_messages\u{2} names {k:?}, which is not a named @@check of the model"));
    }
    crate::hostwrite::check_config(&name, &detail, &config.service, &config.inputs)?;
    check_bindings(&name, config, &columns, &detail, &key_columns)?;
    let form = crate::form::build(schema, idx, &name, &detail, &allow, &FormConfig { form: config.form.clone(), readonly: config.readonly.iter().cloned().chain(text_views).collect(), widgets: config.widgets.clone(), component: config.form_component.clone() }, global)?;
    let editable = editable_of(&name, list, &columns, &key_columns, &detail, &form, &allow, config, model)?;
    let mut package_tabs = Vec::new();
    for full in &config.detail_tabs {
        let Some(module) = global.packages.tab(full) else { return invalid_in(&name, format!("\u{1}detail_tabs\u{2} names {full:?}, which is not a tab of a package")) };
        package_tabs.push(PackageTab { name: full.clone(), title: crate::meta::humanize(full.split_once('.').map_or(full, |(_, t)| t)), module: module.to_owned() });
    }
    let scope_defaults = scope_default_names(model);
    let files = files_of(schema, idx, &name, &config.files)?;
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
        field_permissions: field_names,
        see: config.see.clone(),
        change: config.change.clone(),
        bound: Vec::new(),
        hidden: BTreeSet::new(),
        log_hidden: BTreeMap::new(),
        locked: BTreeSet::new(),
        actions,
        bulk_delete: config.bulk_delete,
        set_password: config.set_password,
        title,
        group: config.group.clone(),
        icon: config.icon.clone(),
        key_columns,
        key_positions,
        detail,
        links: Vec::new(),
        refs: Vec::new(),
        form,
        meta: serde_json::Value::Null,
        search,
        search_with: list.search_with.clone(),
        editable,
        sortable,
        sortable_explicit: list.sortable.is_some(),
        labels: list.computed.iter().map(|c| (&c.name, &c.label)).chain(list.aggregates.iter().map(|a| (&a.name, &a.label))).filter_map(|(n, l)| Some((n.clone(), l.clone()?))).chain(list.labels.clone()).collect(),
        empty: list.empty.clone(),
        empty_fields: list.empty_fields.clone(),
        badges: list.badges.clone(),
        filters,
        named_filters,
        date_hierarchy,
        tabs: Vec::new(),
        queryset,
        can,
        check_messages: config.check_messages.clone(),
        service: config.service.clone(),
        inputs: config.inputs.clone(),
        allow_protected: config.allow_protected_writes,
        host_behavior: config.host_behavior,
        bypass: model.read_defaults.names().into_iter().filter(|n| !scope_defaults.iter().any(|d| d == n)).map(str::to_owned).collect(),
        scope_defaults,
        relations: Vec::new(),
        files,
        inlines: Vec::new(),
        package_tabs,
        frames: Vec::new(),
        fieldsets: config.fieldsets.clone(),
        tabs_config: list.tabs.clone(),
        large: Arc::new(Mutex::new(None)),
    })
}

/// `list.editable` names list columns that are scalar fields the update form writes, on a page that allows `update` and asks no required input.
#[allow(clippy::too_many_arguments)]
fn editable_of(name: &str, list: &ListConfig, columns: &[Column], key_columns: &[Column], detail: &[Column], form: &crate::form::FormSpec, allow: &[Op], config: &ResourceConfig, model: &monorm_core::schema::Model) -> Result<Vec<String>, Fault> {
    let mut out: Vec<String> = Vec::new();
    for f in &list.editable {
        let Some(column) = columns.iter().find(|c| c.label == *f) else { return err_code_in(name, code::UNKNOWN_FIELD, format!("\u{1}list.editable\u{2} names {f:?}, which is not a column of the list")) };
        if out.contains(f) {
            return err_code_in(name, code::DUPLICATE, format!("\u{1}list.editable\u{2} lists {f:?} twice"));
        }
        if key_columns.iter().any(|k| k.label == *f) {
            return invalid_in(name, format!("\u{1}list.editable\u{2} names {f:?}, a key field: a row names its record with the key"));
        }
        if config.files.contains(f) {
            return invalid_in(name, format!("\u{1}list.editable\u{2} names {f:?}, a file field: upload it on the detail page"));
        }
        if column.view.is_some() {
            return invalid_in(name, format!("\u{1}list.editable\u{2} names {f:?}, a field that the host shows as another value: edit it on the detail page"));
        }
        if !crate::write::writable(column) {
            return invalid_in(name, format!("\u{1}list.editable\u{2} names {f:?}, which is not a scalar field of the model (a relation, computed or aggregate column cannot be edited in the list)"));
        }
        if form.readonly.contains(f) {
            return invalid_in(name, format!("\u{1}list.editable\u{2} names {f:?}, a read-only field of the update form"));
        }
        if !detail.iter().any(|d| d.label == *f) {
            return invalid_in(name, format!("\u{1}list.editable\u{2} names {f:?}, a field that the update form does not write"));
        }
        out.push(f.clone());
    }
    if out.is_empty() {
        return Ok(out);
    }
    if !allow.contains(&Op::Update) {
        return invalid_in(name, "\u{1}list.editable\u{2} needs the page to allow update".to_string());
    }
    if config.service.update.is_some() && config.inputs.iter().any(|i| i.required) {
        return invalid_in(name, "\u{1}list.editable\u{2} cannot save through an update service that asks a required input: the list has no input to give".to_string());
    }
    // The page carries the list columns only, and a save sends them as `loaded`: the guard fields must be among them.
    if let Some(g) = crate::write::guard_labels(model, detail).into_iter().find(|g| !columns.iter().any(|c| c.label == *g)) {
        return invalid_in(name, format!("\u{1}list.editable\u{2} needs the guard field {g:?} among the list columns: a save sends the loaded value of each column"));
    }
    Ok(out)
}

/// `see` and `change` name a column or a field of the page, never a key field: a row names its record with the key.
fn check_bindings(name: &str, config: &ResourceConfig, columns: &[Column], detail: &[Column], key_columns: &[Column]) -> Result<(), Fault> {
    for (what, map) in [("see", &config.see), ("change", &config.change)] {
        for (field, permission) in map {
            let known = if what == "see" { columns.iter().chain(detail).any(|c| c.label == *field) } else { detail.iter().any(|c| c.label == *field) };
            if !known {
                return invalid_in(name, format!("{what} names {field:?}, which is not {}", if what == "see" { "a column or a field" } else { "a field of the form" }));
            }
            if key_columns.iter().any(|k| k.label == *field) {
                return invalid_in(name, format!("{what} names the key field {field:?}: a row names its record with the key"));
            }
            check_name(&format!("{name}: {what} permission name"), permission)?;
        }
    }
    Ok(())
}

/// The Json fields of a resource that hold an uploaded file.
fn files_of(schema: &Schema, idx: usize, name: &str, files: &[String]) -> Result<Vec<String>, Fault> {
    let model = schema.model(idx);
    let mut out: Vec<String> = Vec::new();
    for f in files {
        let Some(field) = model.ir.fields.iter().find(|x| x.name == *f) else { return err_code_in(name, code::UNKNOWN_FIELD, format!("files has {f:?}, which is not a field")) };
        if field.ty != ColType::Json || field.array || field.enum_idx.is_some() || !field.nullable || field.generated.is_some() {
            return invalid_in(name, format!("files {f:?} must be a nullable Json field that the database does not generate"));
        }
        if out.contains(f) {
            return err_code_in(name, code::DUPLICATE, format!("files lists {f:?} twice"));
        }
        out.push(f.clone());
    }
    if !out.is_empty() && model.key_fields().len() != 1 {
        return invalid_in(name, "files needs a resource with a single-column key".to_string());
    }
    Ok(out)
}

fn seen_group(resources: &[Resource], group: &str, until: &str) -> bool {
    resources.iter().take_while(|r| r.name != until).any(|r| r.group.as_deref() == Some(group))
}

/// The to-one relations of resource `i` whose `to` columns are exactly the key of another resource.
fn links_of(schema: &Schema, resources: &[Resource], r: &Resource) -> Vec<Link> {
    let model = schema.model(r.model);
    let mut out = Vec::new();
    for rel in &model.ir.relations {
        let Ok((_, target_idx)) = model.relation(&rel.name) else { continue };
        let Some(target) = resources.iter().find(|r| r.model == target_idx) else { continue };
        if rel.kind != RelKind::One || rel.is_conditioned() {
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
            let Some(p) = r.detail.iter().position(|d| model.ir.fields.iter().any(|f| f.name == d.field && (f.column == rel.from[n] || f.name == rel.from[n]))) else { break };
            from.push(p);
        }
        if from.len() == keys.len() {
            out.push(Link { name: rel.name.clone(), target: target.name.clone(), from });
        }
    }
    out
}

/// The column that names a record: the first plain text column of the list that is not part of the key and has no `see` permission.
/// The list label of a foreign key and the filter chips of the UI use this one rule.
pub(crate) fn name_of(r: &Resource) -> Option<&Column> {
    r.columns.iter().find(|c| c.path.is_empty() && c.source == Source::Field && crate::list::is_text(c.ty) && !c.ty.array && !r.key_columns.iter().any(|k| k.label == c.label) && !r.see.contains_key(&c.label))
}

/// The foreign-key columns of the list that can show the name of the related record.
fn refs_of(schema: &Schema, resources: &[Resource], r: &Resource) -> Vec<RefLabel> {
    let model = schema.model(r.model);
    let mut out = Vec::new();
    for link in &r.links {
        let [at] = link.from[..] else { continue };
        let Some(fk) = r.detail.get(at).and_then(|d| r.columns.iter().find(|c| c.label == d.label && c.path.is_empty() && c.source == Source::Field)) else { continue };
        let Some(target) = resources.iter().find(|t| t.name == link.target) else { continue };
        if target.queryset.is_some() || !target.scope_defaults.is_empty() {
            continue;
        }
        let Some(name) = name_of(target) else { continue };
        if model.relation(&link.name).is_err() {
            continue;
        }
        out.push(RefLabel { column: fk.label.clone(), target: target.name.clone(), expr: serde_json::json!({"t": "col", "path": [link.name], "name": name.field}) });
    }
    out
}

fn listable(f: &monorm_core::ir::FieldIr) -> bool {
    f.generated.is_none()
}

/// Marks the columns of the view fields and gives them the type of the shown value. Answers the labels of the read-only ones.
fn apply_views(name: &str, model: &monorm_core::schema::Model, views: &BTreeMap<String, String>, sets: &mut [&mut Vec<Column>]) -> Result<Vec<String>, Fault> {
    let mut read_only = Vec::new();
    for (field, mode) in views {
        let Some(f) = model.ir.fields.iter().find(|f| &f.name == field) else { return err_code_in(name, code::UNKNOWN_FIELD, format!("views names {field:?}, which is not a field")) };
        if f.ty != ColType::Bytes || f.array {
            return invalid_in(name, format!("views names {field:?}, which is not a Bytes field"));
        }
        let (view, shown) = match mode.as_str() {
            "json" => (View::Json, ColType::Json),
            "text" => (View::Text, ColType::String),
            other => return invalid_in(name, format!("views gives {field:?} the mode {other:?}, not json or text")),
        };
        if view == View::Text {
            read_only.push(field.clone());
        }
        for c in sets.iter_mut().flat_map(|s| s.iter_mut()).filter(|c| c.path.is_empty() && &c.field == field && c.source == Source::Field) {
            c.view = Some(view);
            c.ty = ValueType::scalar(shown);
        }
    }
    Ok(read_only)
}

/// A column from `author.name`: each name before the last is a to-one relation.
pub(crate) fn column(schema: &Schema, root: usize, spec: &str) -> Result<Column, Fault> {
    let model_name = &schema.model(root).ir.name;
    let mut parts: Vec<&str> = spec.split('.').collect();
    let field_name = parts.pop().unwrap_or_default();
    let mut cur = root;
    let mut path = Vec::new();
    for hop in parts {
        let m = schema.model(cur);
        let Ok((rel, target)) = m.relation(hop) else {
            return err_code_in(model_name, code::UNKNOWN_FIELD, format!("unknown field {spec:?}: {} has no relation {hop:?}", m.ir.name));
        };
        if rel.kind == RelKind::Many {
            return invalid_in(model_name, format!("list column {spec:?} goes through the to-many relation {hop:?}; a column reads one value per row"));
        }
        path.push(hop.to_owned());
        cur = target;
    }
    let m = schema.model(cur);
    let Some(field) = m.ir.fields.iter().find(|f| f.name == field_name) else {
        let hint = if m.relation(field_name).is_ok() { format!(": {field_name:?} is a relation, name one of its fields") } else { String::new() };
        return err_code_in(model_name, code::UNKNOWN_FIELD, format!("unknown field {spec:?}{hint}"));
    };
    let mut ty = ValueType::scalar(field.ty);
    ty.array = field.array;
    ty.enum_idx = field.enum_idx;
    Ok(Column { label: spec.to_owned(), path, field: field.name.clone(), ty, nullable: field.nullable, source: Source::Field, view: None, sort: None })
}

fn order_column(schema: &Schema, root: usize, field: &str, desc: bool, name: &str) -> Result<OrderColumn, Fault> {
    if field.contains('.') {
        return invalid_in(name, format!("order {field:?}: order takes a field of {name}, not of a related model"));
    }
    let column = column(schema, root, field)?;
    let f = schema.model(root).field(field).map_err(|e| invalid_fault(e.to_string()))?;
    if column.nullable {
        return invalid_in(name, format!("order {field:?}: the field is nullable, and a keyset page cannot compare NULLs"));
    }
    if column.ty.array || column.ty.enum_idx.is_some() || matches!(column.ty.ty, ColType::Json | ColType::Decimal) || f.write_sql.is_some() {
        return invalid_in(name, format!("order {field:?}: a field of this type cannot order a list"));
    }
    Ok(OrderColumn { column, desc })
}

/// The colours of a badge.
pub const BADGE_COLOURS: [&str; 6] = ["grey", "blue", "green", "yellow", "red", "purple"];

/// `list.badges`: each key is a list column of a type that shows as a badge, each value of its map fits that type, each colour is in the palette.
fn check_badges(schema: &Schema, name: &str, badges: &BTreeMap<String, BTreeMap<String, String>>, listed: &[String], columns: &[Column]) -> Result<(), Fault> {
    for (label, map) in badges {
        let Some(c) = columns.iter().find(|c| c.label == *label).filter(|c| listed.contains(&c.label)) else {
            return err_code_in(name, code::UNKNOWN_FIELD, format!("badges names {label:?}, which is not a column of the list"));
        };
        let enum_values = c.ty.enum_idx.and_then(|i| schema.enums.get(i as usize)).map(|e| e.values.iter().map(|v| v.name.as_str()).collect::<Vec<_>>());
        let shown = !c.ty.array && c.view.is_none() && (enum_values.is_some() || matches!(c.ty.ty, ColType::String | ColType::Text | ColType::Int | ColType::BigInt | ColType::Bool));
        if !shown {
            return invalid_in(name, format!("badges {label:?}: a {} column has no badge; a badge takes a string, text, enum, int, bigint or bool column", type_name(c)));
        }
        for (value, colour) in map {
            let fits = match (&enum_values, c.ty.ty) {
                (Some(members), _) => members.contains(&value.as_str()),
                (None, ColType::Int | ColType::BigInt) => value.parse::<i64>().is_ok_and(|n| n.to_string() == *value),
                (None, ColType::Bool) => value == "true" || value == "false",
                _ => true,
            };
            if !fits {
                return invalid_in(name, format!("badges {label:?}: {value:?} is not a value of the column"));
            }
            if !BADGE_COLOURS.contains(&colour.as_str()) {
                return invalid_in(name, format!("badges {label:?}: colour {colour:?} is not one of {}", BADGE_COLOURS.join(", ")));
            }
        }
    }
    Ok(())
}

/// The columns that a sort header may use: own fields in the list that a keyset page can compare.
fn sortable_columns(schema: &Schema, idx: usize, name: &str, list: &ListConfig, columns: &[Column]) -> Result<Vec<Sortable>, Fault> {
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
                    return invalid_in(name, format!("sortable {field:?} is not a list column"));
                }
                if out.iter().any(|s: &Sortable| s.column.label == o.column.label) {
                    return err_code_in(name, code::DUPLICATE, format!("sortable {field:?} is listed twice"));
                }
                let f = model.field(&field).map_err(|e| invalid_fault(e.to_string()))?;
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
fn search_fields(schema: &Schema, idx: usize, name: &str, list: &ListConfig, warnings: &mut Vec<String>) -> Result<Vec<SearchField>, Fault> {
    let model = schema.model(idx);
    let mut out: Vec<SearchField> = Vec::new();
    for field in list.search.clone().unwrap_or_default() {
        if field.contains('.') {
            return invalid_in(name, format!("search {field:?}: search takes a field of {name}, not of a related model"));
        }
        if out.iter().any(|s| s.column.field == field) {
            return err_code_in(name, code::DUPLICATE, format!("search {field:?} is listed twice"));
        }
        let column = column(schema, idx, &field)?;
        let f = model.field(&field).map_err(|e| invalid_fault(e.to_string()))?;
        let ty = column.ty;
        let text = matches!(ty.ty, ColType::String | ColType::Text) && !ty.array && ty.enum_idx.is_none();
        let exact = matches!(ty.ty, ColType::Int | ColType::BigInt | ColType::Uuid) && !ty.array && ty.enum_idx.is_none();
        if f.write_sql.is_some() && !text || !(text || exact) {
            return invalid_in(name, format!("search {field:?}: no search strategy fits a {} field; search takes text, integer and uuid fields", type_name(&column)));
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
        return invalid_in(name, "search and \u{1}search_with\u{2} are two ways to search; set one".to_string());
    }
    if list.search_with.as_ref().is_some_and(|n| n.trim().is_empty()) {
        return invalid_in(name, "\u{1}search_with\u{2} is empty".to_string());
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
    forbidden_binding(expr, false)
}

fn forbidden_binding(expr: &Value, allow_user: bool) -> bool {
    match expr {
        Value::Object(map) => {
            let forbidden = match map.get("t").and_then(Value::as_str) {
                Some("param") => !allow_user || map.get("i").and_then(Value::as_u64) != Some(0),
                Some("in_param" | "scope") => true,
                _ => false,
            };
            forbidden || map.values().any(|v| forbidden_binding(v, allow_user))
        }
        Value::Array(items) => items.iter().any(|v| forbidden_binding(v, allow_user)),
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

fn extra_columns(schema: &Schema, idx: usize, name: &str, list: &ListConfig) -> Result<Extras, Fault> {
    let model = schema.model(idx);
    let mut columns: Vec<Column> = Vec::new();
    let mut sortable = Vec::new();
    let taken = |columns: &[Column], n: &str| -> Result<(), Fault> {
        if !ident_ok(n) {
            return invalid_in(name, format!("column name {n:?} has 1 to 40 letters, digits and _"));
        }
        if model.ir.fields.iter().any(|f| f.name == n) || model.ir.relations.iter().any(|r| r.name == n) || columns.iter().any(|c| c.label == n) {
            return invalid_in(name, format!("computed column {n:?} has the name of a field, a relation or another computed column"));
        }
        Ok(())
    };
    for c in &list.computed {
        taken(&columns, &c.name)?;
        let Some(declared) = parse_type(&c.ty) else {
            return invalid_in(name, format!("computed {:?}: type {:?} is not int, bigint, float, bool, string, text, datetime, date, uuid, json or decimal", c.name, c.ty));
        };
        let nullable = c.nullable.unwrap_or(true);
        let source = match (&c.expr, &c.function) {
            (Some(expr), None) => {
                if has_binding(expr) {
                    return invalid_in(name, format!("computed {:?}: an expression has no parameters or scope values", c.name));
                }
                let found = probe_type(schema, &model.ir.name, expr).map_err(|e| invalid_fault(format!("{name}: computed {:?}: {e}", c.name)))?;
                if found.array || found.enum_idx.is_some() || !same_type(declared, found.ty) {
                    return invalid_in(name, format!("computed {:?}: declared {declared:?}, but the expression is {:?}{}", c.name, found.ty, if found.array { " array" } else { "" }));
                }
                Source::Expr(expr.clone())
            }
            (None, Some(f)) if !f.trim().is_empty() => Source::Batch(f.clone()),
            _ => return err_code_in(name, code::INVALID_VALUE, format!("computed '{}' sets expr or function, one of them", c.name)),
        };
        let sort = match &c.order_by {
            Some(_) if c.sortable == Some(false) => return invalid_in(name, format!("computed {:?}: \u{1}order_by\u{2} makes the column sortable, and sortable is false", c.name)),
            Some(by) => Some(Box::new(order_by_column(schema, idx, name, &c.name, by, nullable)?)),
            None => None,
        };
        if sort.is_some() {
            sortable.push(c.name.clone());
        } else if c.sortable == Some(true) {
            if matches!(source, Source::Batch(_)) {
                return invalid_in(name, format!("computed {:?}: a column of a host function cannot sort; the database does not know its values (\u{1}order_by\u{2} names a field or an expression to sort by)", c.name));
            }
            if nullable || matches!(declared, ColType::Json | ColType::Decimal) {
                return invalid_in(name, format!("computed {:?}: a sort column is not null (set nullable to false) and not json or decimal", c.name));
            }
            sortable.push(c.name.clone());
        }
        columns.push(Column { label: c.name.clone(), path: Vec::new(), field: c.name.clone(), ty: ValueType::scalar(declared), nullable, source, view: None, sort });
    }
    for a in &list.aggregates {
        taken(&columns, &a.name)?;
        let Ok((rel, target)) = model.relation(&a.relation) else {
            return err_code_in(name, code::RELATION_UNKNOWN, format!("aggregate {:?}: {} has no relation {:?}", a.name, model.ir.name, a.relation));
        };
        if rel.kind != RelKind::Many {
            return invalid_in(name, format!("aggregate {:?}: {:?} is a to-one relation; an aggregate reads a to-many relation", a.name, a.relation));
        }
        let expr = match (a.function.as_str(), &a.field) {
            ("count", None) => serde_json::json!({"t": "func", "name": "count", "rel": [a.relation]}),
            ("count", Some(_)) => return invalid_in(name, format!("aggregate {:?}: count takes no field", a.name)),
            ("sum" | "min" | "max" | "avg", Some(field)) => {
                if schema.model(target).field(field).is_err() {
                    return invalid_in(name, format!("aggregate {:?}: {} has no field {field:?}", a.name, schema.model(target).ir.name));
                }
                serde_json::json!({"t": "func", "name": a.function, "args": [{"t": "col", "path": [a.relation], "name": field}]})
            }
            ("sum" | "min" | "max" | "avg", None) => return invalid_in(name, format!("aggregate {:?}: {} needs a field", a.name, a.function)),
            (other, _) => return invalid_in(name, format!("aggregate {:?}: function {other:?} is not count, sum, min, max or avg", a.name)),
        };
        let found = probe_type(schema, &model.ir.name, &expr).map_err(|e| invalid_fault(format!("{name}: aggregate {:?}: {e}", a.name)))?;
        columns.push(Column { label: a.name.clone(), path: Vec::new(), field: a.name.clone(), ty: found, nullable: a.function != "count", source: Source::Expr(expr), view: None, sort: None });
    }
    for (c, label) in list.computed.iter().map(|c| (&c.name, &c.label)).chain(list.aggregates.iter().map(|a| (&a.name, &a.label))) {
        if label.as_ref().is_some_and(|l| l.trim().is_empty() || l.chars().count() > 80) {
            return invalid_in(name, format!("the label of {c:?} has 1 to 80 characters"));
        }
    }
    Ok(Extras { columns, sortable })
}

/// The column that `order_by` of a computed column names: a field path (at most one to-one hop) or an expression.
/// A keyset page compares the value, so it is never NULL and has a type that compares.
fn order_by_column(schema: &Schema, idx: usize, name: &str, computed: &str, by: &Value, nullable: bool) -> Result<Column, Fault> {
    let who = format!("{name}: computed {computed:?}: order_by");
    let orderable = |ty: ValueType| !ty.array && ty.enum_idx.is_none() && !matches!(ty.ty, ColType::Json | ColType::Decimal);
    match by {
        Value::String(path) => {
            let col = column(schema, idx, path).map_err(|e| Fault { text: format!("computed {computed:?}: order_by: {}", e.text), ..e })?;
            if col.path.len() > 1 {
                return invalid(format!("{who} {path:?} goes through {} relations; it takes at most one to-one relation", col.path.len()));
            }
            if col.nullable {
                return invalid(format!("{who} {path:?}: the field is nullable, and a keyset page cannot compare NULLs"));
            }
            if !orderable(col.ty) {
                return invalid(format!("{who} {path:?}: a field of this type cannot order a list"));
            }
            let mut cur = idx;
            for hop in &col.path {
                let model = schema.model(cur);
                let (rel, target) = model.relation(hop).map_err(|e| invalid_fault(format!("{who}: {e}")))?;
                if rel.is_conditioned() {
                    return invalid(format!("{who} {path:?}: a relation with fixed conditions or union members cannot order a list"));
                }
                if rel.from.iter().any(|f| model.ir.fields.iter().find(|x| x.column == *f || x.name == *f).is_none_or(|x| x.nullable)) {
                    return invalid(format!("{who} {path:?}: the relation {hop:?} is optional, so a row can have no value"));
                }
                cur = target;
            }
            if schema.model(cur).field(&col.field).is_ok_and(|f| f.write_sql.is_some()) {
                return invalid(format!("{who} {path:?}: a field of this type cannot order a list"));
            }
            Ok(col)
        }
        Value::Object(_) => {
            if has_binding(by) {
                return invalid(format!("{who}: an expression has no parameters or scope values"));
            }
            if nullable {
                return invalid(format!("{who}: an expression needs a column that declares nullable: false"));
            }
            let found = probe_type(schema, &schema.model(idx).ir.name, by).map_err(|e| invalid_fault(format!("{who}: {e}")))?;
            if !orderable(found) {
                return invalid(format!("{who}: an expression of this type cannot order a list"));
            }
            Ok(Column { label: format!("{computed}.sort"), path: Vec::new(), field: computed.to_owned(), ty: found, nullable: false, source: Source::Expr(by.clone()), view: None, sort: None })
        }
        _ => invalid(format!("{who} takes a field path or an expression")),
    }
}

/// A field that a filter can compare: not an array, a JSON value or a decimal.
fn filterable(c: &Column) -> bool {
    !c.ty.array && !matches!(c.ty.ty, ColType::Json | ColType::Decimal)
}

/// The date field of the drill-down bar: a date or datetime field, with one relation hop allowed.
/// A warning, not an error, when no index leads with it: each count then reads the whole filtered table.
fn date_hierarchy_of(schema: &Schema, idx: usize, name: &str, list: &ListConfig, warnings: &mut Vec<String>) -> Result<Option<DateHierarchy>, Fault> {
    let Some(config) = &list.date_hierarchy else { return Ok(None) };
    let field = config.field();
    let c = column(schema, idx, field).map_err(|e| Fault { text: format!("{}: {}", opt("date_hierarchy"), e.text), ..e })?;
    if c.path.len() > 1 {
        return invalid_in(name, format!("\u{1}date_hierarchy\u{2} {field:?}: one relation hop is the most"));
    }
    if c.ty.array || c.ty.enum_idx.is_some() || !matches!(c.ty.ty, ColType::Date | ColType::DateTime) {
        return invalid_in(name, format!("\u{1}date_hierarchy\u{2} {field:?}: the field is {}, and \u{1}date_hierarchy\u{2} takes a date or datetime field", type_name(&c)));
    }
    let mut at = idx;
    for hop in &c.path {
        at = schema.model(at).relation(hop).map_err(|e| invalid_fault(e.to_string()))?.1;
    }
    let model = schema.model(at);
    if !model.field(&c.field).is_ok_and(|f| leads_btree(&model.ir, f, false)) {
        warnings.push(format!("{name}: date_hierarchy field {field:?} has no index that leads with it; add an index on it, or each count reads the whole filtered table"));
    }
    let counts = match config {
        DateHierarchyConfig::Field(_) => true,
        DateHierarchyConfig::Full(f) => f.counts.unwrap_or(true),
    };
    Ok(Some(DateHierarchy { field: field.to_owned(), column: c, counts }))
}

fn filter_specs(schema: &Schema, idx: usize, name: &str, list: &ListConfig, date: Option<&DateHierarchy>) -> Result<Vec<Filter>, Fault> {
    let model = schema.model(idx);
    let mut out: Vec<Filter> = Vec::new();
    let extra = date.map(|d| d.field.clone()).filter(|f| !list.filters.contains(f));
    for spec in list.filters.iter().chain(&extra) {
        if out.iter().any(|f| f.name == *spec) {
            return err_code_in(name, code::DUPLICATE, format!("filter {spec:?} is listed twice"));
        }
        if !spec.contains('.') && model.relation(spec).is_ok() {
            out.push(Filter { name: spec.clone(), kind: relation_filter(schema, idx, name, spec)? });
            continue;
        }
        let c = column(schema, idx, spec).map_err(|e| Fault { text: e.text.replace("list column", "filter"), ..e })?;
        if !filterable(&c) {
            return err_code_in(name, code::UNSUPPORTED, format!("filter {spec:?}: a filter does not take an array, json or decimal field"));
        }
        out.push(Filter { name: spec.clone(), kind: FilterKind::Field(c) });
    }
    for n in list.named_filters.iter().map(|f| &f.name) {
        if out.iter().any(|f| f.name == *n) {
            return invalid_in(name, format!("named filter {n:?} has the name of a filter"));
        }
    }
    Ok(out)
}

/// A to-one relation as a filter: the foreign key fields on the model compare with a key of the target.
fn relation_filter(schema: &Schema, idx: usize, name: &str, spec: &str) -> Result<FilterKind, Fault> {
    let model = schema.model(idx);
    let (rel, target_idx) = model.relation(spec).map_err(|e| invalid_fault(e.to_string()))?;
    if rel.kind != RelKind::One {
        return invalid_in(name, format!("filter {spec:?}: a filter takes a to-one relation, and {spec:?} is to-many"));
    }
    if rel.is_conditioned() {
        return invalid_in(name, format!("filter {spec:?}: a relation with fixed conditions or union members is not a filter"));
    }
    let target = schema.model(target_idx);
    let keys = target.key_fields();
    if rel.to.len() != keys.len() {
        return invalid_in(name, format!("filter {spec:?}: the relation does not lead to the key of {}", target.ir.name));
    }
    let (mut from, mut key) = (Vec::new(), Vec::new());
    for k in &keys {
        let Some(n) = rel.to.iter().position(|t| *t == k.column || *t == k.name) else {
            return invalid_in(name, format!("filter {spec:?}: the relation does not lead to the key of {}", target.ir.name));
        };
        let Some(f) = model.ir.fields.iter().find(|f| f.column == rel.from[n] || f.name == rel.from[n]) else {
            return err_code_in(name, code::UNKNOWN_FIELD, format!("filter {spec:?}: the foreign key field {:?} is not a field", rel.from[n]));
        };
        from.push(column(schema, idx, &f.name)?);
        key.push(column(schema, target_idx, &k.name)?);
    }
    Ok(FilterKind::Relation { target: target.ir.name.clone(), from, key })
}

fn named_filters(schema: &Schema, idx: usize, name: &str, list: &ListConfig) -> Result<Vec<NamedFilter>, Fault> {
    let model = schema.model(idx);
    let mut out: Vec<NamedFilter> = Vec::new();
    for f in &list.named_filters {
        if !ident_ok(&f.name) || out.iter().any(|o| o.name == f.name) {
            return invalid_in(name, format!("named filter {:?} has 1 to 40 letters, digits and _, and a name of its own", f.name));
        }
        if f.choices.is_empty() {
            return invalid_in(name, format!("named filter {:?} has no choices", f.name));
        }
        let mut choices: Vec<NamedChoice> = Vec::new();
        for c in &f.choices {
            if !ident_ok(&c.name) || c.name == "all" || choices.iter().any(|o| o.name == c.name) {
                return invalid_in(name, format!("named filter {:?}: choice {:?} has 1 to 40 letters, digits and _, is not \"all\" (that is every row) and has a name of its own", f.name, c.name));
            }
            if has_binding(&c.condition) {
                return invalid_in(name, format!("named filter {:?}: choice {:?}: an expression has no parameters or scope values", f.name, c.name));
            }
            let key = model.key_fields().first().map(|k| serde_json::json!({"t": "col", "path": [], "name": k.name}));
            probe(schema, json_select(&model.ir.name, key.as_ref(), Some(&c.condition))).map_err(|e| invalid_fault(format!("{name}: named filter {:?}: choice {:?}: {e}", f.name, c.name)))?;
            choices.push(NamedChoice { name: c.name.clone(), label: c.label.clone().unwrap_or_else(|| meta::humanize(&c.name)), expr: c.condition.clone() });
        }
        if let Some(d) = &f.default {
            if !choices.iter().any(|c| c.name == *d) {
                return invalid_in(name, format!("named filter {:?}: default {d:?} is not a choice", f.name));
            }
        }
        out.push(NamedFilter { name: f.name.clone(), label: f.label.clone().unwrap_or_else(|| meta::humanize(&f.name)), choices, default: f.default.clone() });
    }
    Ok(out)
}

/// The names of the read defaults of the model that read a `scope.<name>` value.
pub(crate) fn scope_default_names(model: &monorm_core::schema::Model) -> Vec<String> {
    model.read_defaults.parts.iter().filter(|p| !p.scopes.is_empty()).map(|p| p.identity.name.clone()).collect()
}

/// The name of a host function: letters, digits, `_`, `.`, `:`, `-` and `@` (the inline tag), at most 80 characters.
fn check_function(what: &str, name: &str) -> Result<(), Fault> {
    let ok = !name.is_empty() && name.chars().count() <= 80 && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-' | '@'));
    if ok { Ok(()) } else { invalid(format!("{what} {name:?} is not a function name")) }
}

pub(crate) fn queryset_of(schema: &Schema, idx: usize, name: &str, config: Option<&QuerysetConfig>) -> Result<Option<Queryset>, Fault> {
    let Some(q) = config else { return Ok(None) };
    let mut slots = Vec::new();
    for s in &q.slots {
        slots.push(match s.as_str() {
            "user" => Slot::User,
            other => return invalid_in(name, format!("queryset slot {other:?} is not \"user\"")),
        });
    }
    match (&q.filter, &q.function) {
        (Some(_), Some(_)) => return invalid_in(name, "queryset has a filter or a function, not both".to_string()),
        (None, Some(f)) => {
            check_function(&format!("{name}: queryset.function"), f)?;
            if !slots.is_empty() {
                return invalid_in(name, "a queryset function takes no slots: it gets the user".to_string());
            }
        }
        (Some(f), None) => {
            let model = &schema.model(idx).ir.name;
            let key = schema.model(idx).key_fields().first().map(|k| serde_json::json!({"t": "col", "path": [], "name": k.name}));
            probe_with(schema, json_select(model, key.as_ref(), Some(f)), &ProbeParams(slots.len())).map_err(|e| invalid_fault(format!("{name}: queryset filter: {e}")))?;
            if has_scope(f) {
                return invalid_in(name, "queryset filter: a scope value has no value in the admin".to_string());
            }
            if let Some(i) = max_param(f).filter(|&i| i >= slots.len()) {
                return invalid_in(name, format!("queryset filter: parameter {i} has no slot"));
            }
        }
        (None, None) => return invalid_in(name, "queryset has a filter or a function".to_string()),
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

fn can_rules(schema: &Schema, name: &str, config: &BTreeMap<String, Value>) -> Result<Vec<(String, Value)>, Fault> {
    let mut out = Vec::new();
    for rule in CAN_NAMES {
        let Some(expr) = config.get(rule) else { continue };
        check_can(schema, name, expr, &format!("{name}: can {rule:?}"))?;
        out.push((rule.to_owned(), expr.clone()));
    }
    if let Some(other) = config.keys().find(|k| !CAN_NAMES.contains(&k.as_str())) {
        return invalid_in(name, format!("can has {other:?}, not one of {}", CAN_NAMES.join(", ")));
    }
    Ok(out)
}

fn check_can(schema: &Schema, model: &str, expr: &Value, what: &str) -> Result<(), Fault> {
    if forbidden_binding(expr, true) {
        return invalid(format!("{what}: only param(\"user\") is allowed; no in_param or scope values"));
    }
    let plan = probe_with(schema, json_select(model, Some(expr), None), &ProbeParams(1))
        .map_err(|e| invalid_fault(format!("{what}: {e}")))?;
    if !plan.types.first().is_some_and(|ty| ty.ty == ColType::Bool && !ty.array) {
        return invalid(format!("{what}: the expression is not a boolean"));
    }
    Ok(())
}

/// The actions and their flags for list and detail reads.
fn actions_of(schema: &Schema, name: &str, config: &[ActionConfig], can: &mut Vec<(String, Value)>, set_password: bool) -> Result<Vec<Action>, Fault> {
    let mut out = Vec::new();
    let change = can.iter().find(|(rule, _)| rule == "change").map(|(_, expr)| expr.clone());
    let mut password_rule = None;
    for a in config {
        if set_password && a.name == crate::set_password::NAME {
            if a.replace_change {
                return invalid_in(name, "\u{1}set_password\u{2} always requires the resource change rule".to_string());
            }
            if let Some(expr) = &a.can {
                check_can(schema, name, expr, &format!("{name}: set_password: can"))?;
                password_rule = Some(match &change {
                    Some(change) => serde_json::json!({"t": "and", "items": [expr, change]}),
                    None => expr.clone(),
                });
            }
            continue;
        }
        if a.name == BULK_DELETE {
            if a.can.is_some() {
                return invalid_in(name, format!("{BULK_DELETE}: can is not allowed on the built-in action; use the resource delete rule"));
            }
            return invalid_in(name, format!("{BULK_DELETE:?} is the name of the built-in action: bulk_delete=False removes it"));
        }
        let placement = match &a.placement {
            Some(p) => Some(Placement::parse(p).ok_or_else(|| invalid_fault(format!("{name}: action {:?} has placement {p:?}, not one of {}", a.name, Placement::NAMES.join(", "))))?),
            None => None,
        };
        if a.replace_change && a.can.is_none() {
            return invalid_in(name, format!("action {:?}: \u{1}replace_change\u{2} needs can", a.name));
        }
        if placement.is_none() && (a.confirm.is_some() || !a.inputs.is_empty() || a.applies_to.is_some() || a.can.is_some()) {
            return invalid_in(name, format!("action {:?} has no placement: only a placed action takes a confirmation, inputs, \u{1}applies_to\u{2} or can", a.name));
        }
        if let Some(label) = &a.label {
            if label.is_empty() || label.chars().count() > 80 {
                return invalid_in(name, format!("action {:?}: the label has 1 to 80 characters", a.name));
            }
        }
        if let Some(variant) = &a.variant {
            if !VARIANTS.contains(&variant.as_str()) {
                return invalid_in(name, format!("action {:?} has variant {variant:?}, not one of {}", a.name, VARIANTS.join(", ")));
            }
        }
        if let Some(icon) = &a.icon {
            if !crate::meta::icon_ok(icon) {
                return invalid_in(name, format!("action {:?}: icon {icon:?} must be a lucide icon name such as \"refresh-cw\"", a.name));
            }
        }
        if let Some(menu) = &a.menu {
            if menu.is_empty() || menu.chars().count() > 40 {
                return invalid_in(name, format!("action {:?}: the menu has 1 to 40 characters", a.name));
            }
            if placement != Some(Placement::Detail) {
                return invalid_in(name, format!("action {:?}: a menu needs the placement \"detail\"", a.name));
            }
        }
        crate::hostwrite::check_action_inputs(name, &a.name, &a.inputs)?;
        let rule = if let Some(expr) = &a.can {
            check_can(schema, name, expr, &format!("{name}: action {:?}: can", a.name))?;
            Some(match &change {
                Some(change) if !a.replace_change => serde_json::json!({"t": "and", "items": [expr, change]}),
                _ => expr.clone(),
            })
        } else {
            None
        };
        let action = Action { name: a.name.clone(), label: a.label.clone().unwrap_or_else(|| crate::meta::humanize(&a.name)), placement, variant: a.variant.clone(), icon: a.icon.clone(), menu: a.menu.clone(), permission: a.permission.clone(), confirm: a.confirm.clone(), inputs: a.inputs.clone(), applies: a.applies_to.clone(), can: rule, sync: a.sync, threaded: a.threaded };
        if let Some(expr) = &a.applies_to {
            if has_binding(expr) {
                return invalid_in(name, format!("action {:?}: \u{1}applies_to\u{2} has no parameters or scope values", a.name));
            }
            let ty = probe_type(schema, name, expr).map_err(|e| invalid_fault(format!("{name}: action {:?}: \u{1}applies_to\u{2}: {e}", a.name)))?;
            if ty.ty != ColType::Bool || ty.array {
                return invalid_in(name, format!("action {:?}: \u{1}applies_to\u{2} is not a boolean condition", a.name));
            }
            can.push((action.flag(), expr.clone()));
        }
        if let Some(expr) = action.can.as_ref().filter(|_| placement.is_some_and(Placement::single)) {
            can.push((action.can_flag(), expr.clone()));
        }
        out.push(action);
    }
    if set_password {
        let action = crate::set_password::action(password_rule);
        if let Some(expr) = &action.can {
            can.push((action.can_flag(), expr.clone()));
        }
        out.push(action);
    }
    Ok(out)
}
