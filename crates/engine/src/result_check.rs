//! Result checks recorded by extension passes, inside existing planning/materialization.
use crate::db::{Cell, DbResult, RowSet};
use monorm_core::{behavior::ResultShape, ir::ValueType, result_check::{Category, EnumScalar, PreparedChecks, PreparedField, Warning, Warnings}};

/// Check public instance columns only. An absent LEFT JOIN object is not a loaded
/// null field and produces no warning. Row order/count/values remain untouched.
pub fn inspect<'a>(checks: &'a PreparedChecks, shape: Option<&ResultShape>, start: usize,
    presence: Option<(usize, ValueType)>, rows: &dyn RowSet, warnings: &mut Warnings<'a>) -> DbResult<()> {
    for field in &checks.fields {
        let column = match shape {
            Some(shape) => match shape.fields.iter().find(|f| f.field.position == field.position && f.public).and_then(|f| f.physical) {
                Some(column) => column,
                None => continue,
            },
            None => start + field.position,
        };
        for row in 0..rows.len() {
            if let Some((column, ty)) = presence {
                if matches!(rows.cell(row, column, ty)?, Cell::Null) { continue; }
            }
            let cell = rows.cell(row, column, field.ty)?;
            if matches!(cell, Cell::Null) {
                if field.expected.non_null { warnings.record(checks, field, Category::Null); }
            } else if field.allowed.is_some() && !accepts(field, &cell) {
                warnings.record(checks, field, Category::EnumSubset);
            }
        }
    }
    Ok(())
}
fn accepts(field: &PreparedField, cell: &Cell<'_>) -> bool {
    match cell {
        Cell::Null => true,
        Cell::Text(value) => field.accepts_enum(EnumScalar::Text(value)),
        Cell::Int(value) => field.accepts_enum(EnumScalar::Int(i64::from(*value))),
        Cell::BigInt(value) => field.accepts_enum(EnumScalar::Int(*value)),
        Cell::Array(values) => values.iter().all(|v| accepts(field, v)),
        _ => false,
    }
}

/// Collect once for an operation. Projection/count results never acquire diagnostics.
pub fn diagnostics(all: &[Option<PreparedChecks>], out: &crate::exec::Outcome) -> DbResult<Vec<Warning>> {
    use crate::{exec::{Fetched, Outcome}, plan::Output};
    if all.is_empty() { return Ok(vec![]); }
    fn output<'a>(all: &'a [Option<PreparedChecks>], output: &Output, rows: &dyn RowSet,
        types: &[ValueType], warnings: &mut Warnings<'a>) -> DbResult<()> {
        if let Output::Instances { model, joins, .. } = output {
            if let Some(c) = &all[*model] { inspect(c, None, 0, None, rows, warnings)?; }
            for join in joins {
                let pk = join.start + join.pk_pos;
                if let Some(c) = &all[join.model] { inspect(c, None, join.start, Some((pk, types[pk])), rows, warnings)?; }
            }
        }
        Ok(())
    }
    fn fetched<'a>(all: &'a [Option<PreparedChecks>], fetches: &[Fetched], warnings: &mut Warnings<'a>) -> DbResult<()> {
        for fetch in fetches {
            output(all, &fetch.plan.output, fetch.rows.as_ref(), &fetch.plan.types, warnings)?;
            fetched(all, &fetch.children, warnings)?;
        }
        Ok(())
    }
    let mut warnings = Warnings::default();
    match out {
        Outcome::Select(selected) => {
            output(all, &selected.plan.output, selected.rows.as_ref(), &selected.plan.types, &mut warnings)?;
            fetched(all, &selected.prefetched, &mut warnings)?;
        }
        Outcome::Rows { model, rows, .. } => if let Some(c) = &all[*model] { inspect(c, None, 0, None, rows.as_ref(), &mut warnings)?; },
        Outcome::Prefetched { prefetched, .. } => fetched(all, prefetched, &mut warnings)?,
        _ => {}
    }
    Ok(warnings.finish())
}

pub fn emit(warnings: &[Warning]) {
    for warning in warnings {
        // Structured metadata only, one record per query/model/field/category.
        // Both bindings use the same native sink; stderr is capturable by hosts.
        if let Ok(record) = serde_json::to_string(warning) { eprintln!("{record}"); }
    }
}
