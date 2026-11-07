//! `@@protected_write`: ORM writes to a protected table run only inside a scope that
//! allows them. An application-level check; raw SQL and other clients still write.
//!
//! Protection and permission go by table: a proxy writes the table of its root, and a
//! multi-table write also writes the tables of its storage ancestors.

use crate::error::{Error, Result};
use monorm_core::schema::Schema;

/// The call-independent part of the write check of one model: a prepared write builds it
/// once and runs [`Writable::check`] per call.
pub struct Writable {
    /// The model name when the model is a SQL view.
    view: Option<String>,
    /// For each written table that a model protects: the table and that model's name.
    protected: Vec<(String, String)>,
}

impl Writable {
    pub fn of(schema: &Schema, model: &str) -> Result<Self> {
        let idx = schema.model_idx(model).map_err(Error::Query)?;
        let view = schema.model(idx).ir.sql_view.is_some().then(|| model.to_owned());
        let protected = written_tables(schema, idx).into_iter()
            .filter_map(|table| protector(schema, table).map(|owner| (table.to_owned(), owner.to_owned())))
            .collect();
        Ok(Self { view, protected })
    }

    /// Fails with [`Error::ReadOnlyModel`] for a SQL view, and with [`Error::WriteProtected`] when the write
    /// writes a protected table that no model in `allowed` (model names) writes.
    pub fn check(&self, schema: &Schema, allowed: &[String]) -> Result<()> {
        if let Some(view) = &self.view {
            return Err(Error::ReadOnlyModel(view.clone()));
        }
        for (table, owner) in &self.protected {
            let permitted = allowed.iter().filter_map(|name| schema.model_idx(name).ok()).any(|m| schema.model(m).table() == table);
            if !permitted {
                return Err(Error::WriteProtected(owner.clone()));
            }
        }
        Ok(())
    }
}

/// Fails with [`Error::ReadOnlyModel`] for a SQL view, and with [`Error::WriteProtected`] when a write of model `model` writes a protected
/// table that no model in `allowed` (model names) writes.
pub fn ensure_writable(schema: &Schema, model: &str, allowed: &[String]) -> Result<()> {
    Writable::of(schema, model)?.check(schema, allowed)
}

/// The model whose `@@protected_write` protects `table`, if one does.
fn protector<'s>(schema: &'s Schema, table: &str) -> Option<&'s str> {
    schema.models.iter().chain(&schema.physical().models)
        .find(|m| m.ir.protected_write && m.table() == table)
        .map(|m| m.ir.name.as_str())
}

/// The tables a write of model `idx` writes: its own, and the
/// tables of its storage ancestors.
fn written_tables(schema: &Schema, idx: usize) -> Vec<&str> {
    let model = schema.model(idx);
    let mut tables = vec![model.table()];
    if model.has_foreign_storage {
        let owners = &schema.physical().models;
        let mut current = model.owner;
        while let Some(link) = schema.owner_links.iter().find(|l| l.child == current) {
            tables.push(owners[link.parent.0].table());
            current = link.parent;
        }
    }
    tables
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> Schema {
        let ir = serde_json::json!({"models": [
            {"name": "Post", "table": "posts", "protected_write": true, "fields": [{"name": "id", "column": "id", "type": "int", "primary_key": true}]},
            {"name": "Tag", "table": "tags", "fields": [{"name": "id", "column": "id", "type": "int", "primary_key": true}]},
            {"name": "PostAlias", "table": "posts", "fields": [{"name": "id", "column": "id", "type": "int", "primary_key": true}]}
        ]});
        Schema::from_ir(serde_json::from_value(ir).unwrap()).unwrap()
    }

    #[test]
    fn a_protected_table_needs_a_model_that_writes_it_in_the_scope() {
        let schema = schema();
        assert!(matches!(ensure_writable(&schema, "Post", &[]), Err(Error::WriteProtected(m)) if m == "Post"));
        assert!(matches!(ensure_writable(&schema, "Post", &["Tag".into()]), Err(Error::WriteProtected(_))));
        assert!(ensure_writable(&schema, "Post", &["Post".into()]).is_ok());
        assert!(ensure_writable(&schema, "Tag", &[]).is_ok());
        // A second model of the same table (as a proxy is) has the same protection.
        assert!(matches!(ensure_writable(&schema, "PostAlias", &[]), Err(Error::WriteProtected(m)) if m == "Post"));
        assert!(ensure_writable(&schema, "PostAlias", &["Post".into()]).is_ok());
        assert!(ensure_writable(&schema, "Post", &["PostAlias".into(), "Unknown".into()]).is_ok());
    }

    /// `Writable::of(..).check(..)` gives the result of `ensure_writable`.
    fn same(schema: &Schema, model: &str, allowed: &[&str]) {
        let allowed: Vec<String> = allowed.iter().map(|a| a.to_string()).collect();
        let a = ensure_writable(schema, model, &allowed).map_err(|e| e.to_string());
        let b = Writable::of(schema, model).and_then(|w| w.check(schema, &allowed)).map_err(|e| e.to_string());
        assert_eq!(a, b, "{model} with {allowed:?}");
    }

    #[test]
    fn a_held_check_gives_the_result_of_ensure_writable() {
        let schema = schema();
        for model in ["Post", "Tag", "PostAlias"] {
            for allowed in [&[][..], &["Tag"], &["Post"], &["PostAlias", "Unknown"]] {
                same(&schema, model, allowed);
            }
        }
        let held = Writable::of(&schema, "Tag").unwrap();
        assert!(held.protected.is_empty() && held.view.is_none());
        assert!(matches!(Writable::of(&schema, "Nope"), Err(Error::Query(_))));
    }

    #[test]
    fn a_held_check_of_a_sql_view_fails_first() {
        let ir = serde_json::json!({"models": [
            {"name": "Live", "table": "live", "sql_view": {"body": "SELECT 1 AS id", "depends_on": []},
             "fields": [{"name": "id", "column": "id", "type": "int", "primary_key": true}]}
        ]});
        let schema = Schema::from_ir(serde_json::from_value(ir).unwrap()).unwrap();
        same(&schema, "Live", &["Live"]);
        assert!(matches!(Writable::of(&schema, "Live").unwrap().check(&schema, &[]), Err(Error::ReadOnlyModel(m)) if m == "Live"));
    }

    #[test]
    fn a_sql_view_is_never_writable() {
        let ir = serde_json::json!({"models": [
            {"name": "Live", "table": "live", "sql_view": {"body": "SELECT 1 AS id", "depends_on": []},
             "fields": [{"name": "id", "column": "id", "type": "int", "primary_key": true}]}
        ]});
        let schema = Schema::from_ir(serde_json::from_value(ir).unwrap()).unwrap();
        assert!(matches!(ensure_writable(&schema, "Live", &["Live".into()]), Err(Error::ReadOnlyModel(m)) if m == "Live"));
    }
}
