//! The SQL text of the admin's own tables: names quoted, a placeholder per dialect, and the table and columns of a model.

use monorm_core::dialect::Dialect;

use crate::site::Site;

pub fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// The placeholder of the `i`-th (from 1) argument.
pub fn mark(dialect: Dialect, i: usize) -> String {
    if dialect == Dialect::Postgres {
        format!("${i}")
    } else {
        "?".to_string()
    }
}

/// The table of a model and the column of each field, as the prepared schema names them.
pub struct Table {
    pub name: String,
    model: String,
    fields: Vec<(String, String)>,
}

impl Table {
    pub fn of(site: &Site, model: &str) -> Result<Table, String> {
        let idx = site.schema.model_idx(model).map_err(|_| format!("the schema has no {model} model"))?;
        let m = site.schema.model(idx);
        Ok(Table { name: m.table().to_string(), model: model.to_string(), fields: m.fields().iter().map(|f| (f.name.to_string(), f.column.to_string())).collect() })
    }

    /// The quoted table name.
    pub fn quoted(&self) -> String {
        quote(&self.name)
    }

    /// The quoted column of a field.
    pub fn column(&self, field: &str) -> Result<String, String> {
        self.fields.iter().find(|(n, _)| n == field).map(|(_, c)| quote(c)).ok_or_else(|| format!("{} has no field {field}", self.model))
    }
}
