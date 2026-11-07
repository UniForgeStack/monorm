//! `Site::build`: the resources of an admin, checked against the prepared schema.

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use monorm_core::ir::{ColType, RelKind, ValueType};
use monorm_core::schema::Schema;
use serde::Deserialize;

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
}

pub struct Site {
    pub schema: Arc<Schema>,
    pub prefix: String,
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
        let mut resources = Vec::new();
        if config.resources.is_empty() {
            for (idx, m) in schema.models.iter().enumerate() {
                if concrete(m) {
                    resources.push(resource(&schema, idx, &ListConfig::default())?);
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
                resources.push(resource(&schema, idx, &r.list)?);
            }
        }
        Ok(Site { schema, prefix, resources })
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

fn resource(schema: &Schema, idx: usize, list: &ListConfig) -> Result<Resource, ConfigError> {
    let model = schema.model(idx);
    let name = model.ir.name.clone();
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
    Ok(Resource { model: idx, name, columns, order, page_size, order_positions })
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
