//! `Site::build`: the resources of an admin, checked against the prepared schema.

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use monorm_core::ir::{ColType, RelKind, ValueType};
use monorm_core::schema::Schema;
use serde::Deserialize;

use crate::permissions::{Op, Strategy};

pub const DEFAULT_PREFIX: &str = "/admin/api";
pub const DEFAULT_PAGE_SIZE: usize = 25;
pub const MAX_PAGE_SIZE: usize = 100;

/// A wrong config: the text names the model and the field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError(pub String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

fn err<T>(message: String) -> Result<T, ConfigError> {
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
    /// The registered resources. None: every concrete model is a read-only resource.
    #[serde(default)]
    resources: Vec<ResourceConfig>,
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
    /// Names of field permissions (`see_cost`): `<label>.<name>`.
    #[serde(default)]
    field_permissions: Vec<String>,
    /// Names of actions: `<label>.<action>_<resource>`.
    #[serde(default)]
    actions: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ListConfig {
    /// Field names; `author.name` reaches a field through to-one relations.
    columns: Option<Vec<String>>,
    /// Field names of the model, `-created_at` for descending.
    order: Option<Vec<String>>,
    page_size: Option<usize>,
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
}

#[derive(Debug, Clone)]
pub struct OrderColumn {
    pub column: Column,
    pub desc: bool,
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
}

pub struct Site {
    pub schema: Arc<Schema>,
    pub prefix: String,
    pub strategy: Strategy,
    pub resources: Vec<Resource>,
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
        if config.resources.is_empty() {
            for (idx, m) in schema.models.iter().enumerate() {
                if concrete(m) {
                    resources.push(resource(&schema, idx, &ResourceConfig::of(&schema.model(idx).ir.name), strategy, label)?);
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
                resources.push(resource(&schema, idx, r, strategy, label)?);
            }
        }
        Ok(Site { schema, prefix, strategy, resources })
    }

    pub fn resource(&self, name: &str) -> Option<&Resource> {
        self.resources.iter().find(|r| r.name == name)
    }
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
        ResourceConfig { model: model.to_owned(), list: ListConfig::default(), allow: None, perms: None, label: None, field_permissions: Vec::new(), actions: Vec::new() }
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

fn resource(schema: &Schema, idx: usize, config: &ResourceConfig, strategy: Strategy, site_label: &str) -> Result<Resource, ConfigError> {
    let list = &config.list;
    let model = schema.model(idx);
    let name = model.ir.name.clone();
    let PermissionOptions { label, allow, split_delete } = permission_options(&name, config, strategy, site_label)?;
    let mut columns = match &list.columns {
        Some(names) if names.is_empty() => return err(format!("{name}: list.columns is empty")),
        Some(names) => names.iter().map(|n| column(schema, idx, n)).collect::<Result<Vec<_>, _>>()?,
        None => model.ir.fields.iter().filter(|f| listable(f)).map(|f| column(schema, idx, &f.name)).collect::<Result<Vec<_>, _>>()?,
    };
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
    })
}

fn listable(f: &monorm_core::ir::FieldIr) -> bool {
    f.generated.is_none()
}

/// A column from `author.name`: each name before the last is a to-one relation.
fn column(schema: &Schema, root: usize, spec: &str) -> Result<Column, ConfigError> {
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
    Ok(Column { label: spec.to_owned(), path, field: field.name.clone(), ty, nullable: field.nullable })
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
