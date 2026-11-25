//! `update_many`: rows whose cells are database expressions, and the fixed `update_values`.

use super::{is_aggregate, Expr};
use crate::error::{Error, Result};
use monorm_core::ir::When;

/// The rows of one `update_many` statement. `shape` holds each cell's expression (`None`: a plain value);
/// its numbered `Param` slots read `slots` of the row. Rows hold a placeholder at an expression cell.
pub struct ManyChunk<'a> {
    pub rows: &'a [Vec<sea_query::Value>],
    pub shape: Option<&'a [Option<Expr>]>,
    pub slots: &'a [Vec<sea_query::Value>],
}

/// What `Param` slots and `excluded()` read while a row expression is planned.
pub(crate) struct RowSource {
    /// `None` for `update_values`, whose parameters are the statement's.
    pub slots: Option<Vec<RowCell>>,
    pub excluded: Vec<(String, Option<RowCell>)>,
}

#[derive(Clone)]
pub(crate) enum RowCell {
    /// Column `n` of the `VALUES` list.
    Column(usize),
    /// The row's own value, bound in a `CASE` branch.
    Bound(sea_query::Value),
}

fn refuse(what: &str, kind: &str) -> Error {
    Error::query(format!("{what}: a value can't read {kind}"))
}

/// Checks that `e` holds only what a row value or `update_values` may hold. Sets `excluded` when it reads `excluded()`.
pub(crate) fn check_value_expr(e: &Expr, what: &str, allow_excluded: bool, excluded: &mut bool) -> Result<()> {
    let mut again = |e: &Expr| check_value_expr(e, what, allow_excluded, excluded);
    match e {
        Expr::Col { path, sub, .. } => {
            if !path.is_empty() {
                return Err(refuse(what, "a relation path"));
            }
            if !sub.is_empty() {
                return Err(refuse(what, "a nested subfield"));
            }
            Ok(())
        }
        Expr::Param { .. } | Expr::Const { .. } | Expr::Null | Expr::Int { .. } | Expr::Text { .. } | Expr::Scope { .. } => Ok(()),
        Expr::Cmp { l, r, .. } | Expr::Arith { l, r, .. } => again(l).and_then(|_| again(r)),
        Expr::And { items } | Expr::Or { items } => items.iter().try_for_each(again),
        Expr::Not { item } | Expr::IsNull { item, .. } | Expr::JsonPath { item, .. } => again(item),
        Expr::In { item, values, .. } => again(item).and_then(|_| values.iter().try_for_each(again)),
        Expr::Like { item, pattern, .. } => again(item).and_then(|_| again(pattern)),
        Expr::Case { whens, default } => {
            for When { cond, value } in whens {
                again(cond)?;
                again(value)?;
            }
            default.as_deref().map_or(Ok(()), again)
        }
        Expr::Func { name, args, rel, distinct, filter, order_by } => {
            if is_aggregate(name) || rel.is_some() || *distinct || filter.is_some() || !order_by.is_empty() {
                return Err(refuse(what, "an aggregate"));
            }
            args.iter().try_for_each(again)
        }
        Expr::Excluded { .. } if allow_excluded => {
            *excluded = true;
            Ok(())
        }
        Expr::Excluded { .. } => Err(refuse(what, "excluded()")),
        Expr::CteCol { .. } => Err(refuse(what, "a CTE column")),
        Expr::Exists { .. } | Expr::Subquery { .. } | Expr::InSelect { .. } => Err(refuse(what, "a subquery")),
        Expr::Outer { .. } => Err(refuse(what, "a column of an outer query")),
        Expr::InParam { .. } => Err(refuse(what, "a param() list")),
        Expr::Window { .. } => Err(refuse(what, "a window function")),
        Expr::Object { .. } => Err(refuse(what, "an object value")),
    }
}
