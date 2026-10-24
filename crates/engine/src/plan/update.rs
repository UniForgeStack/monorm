//! Row assignments are missing from SeaQuery's UPDATE API. Keep expressions in
//! its AST and render only the UPDATE envelope here, so bindings and inspection
//! still traverse the same subqueries and parameters.
use sea_query::{
    Alias, Cond, ConditionHolder, Expr, IntoIden, Query, QueryBuilder, QueryStatementBuilder,
    QueryStatementWriter, ReturningClause, SqlWriter, SubQueryStatement, WithClause,
};

#[derive(Clone, Debug)]
pub struct RowUpdate {
    table: String,
    values: Vec<(Vec<String>, Expr)>,
    filters: Vec<Expr>,
    returning: Option<ReturningClause>,
    with: Option<WithClause>,
}

impl RowUpdate {
    pub fn new(table: &str) -> Self {
        Self {
            table: table.to_owned(),
            values: vec![],
            filters: vec![],
            returning: None,
            with: None,
        }
    }
    pub fn value(&mut self, column: String, value: Expr) {
        self.row_value(vec![column], value);
    }
    pub fn row_value(&mut self, columns: Vec<String>, value: Expr) {
        self.values.push((columns, value));
    }
    pub fn and_where(&mut self, filter: Expr) {
        self.filters.push(filter);
    }
    pub fn returning(&mut self, returning: ReturningClause) {
        self.returning = Some(returning);
    }
    pub fn with_cte(&mut self, with: WithClause) {
        self.with = Some(with);
    }

    pub fn has_row_values(&self) -> bool {
        self.values.iter().any(|(columns, _)| columns.len() > 1)
    }

    pub fn into_statement(self) -> sea_query::UpdateStatement {
        assert!(!self.has_row_values(), "row assignments cannot become a SeaQuery update");
        self.inspection_statement()
    }

    /// Inspection traverses each RHS once; column arity does not affect its
    /// subqueries or dependencies. This projection is never executed.
    pub(crate) fn inspection_statement(self) -> sea_query::UpdateStatement {
        let mut stmt = Query::update();
        stmt.table(Alias::new(self.table));
        for (columns, value) in self.values {
            stmt.value(Alias::new(&columns[0]), value);
        }
        for filter in self.filters {
            stmt.and_where(filter);
        }
        if let Some(returning) = self.returning {
            stmt.returning(returning);
        }
        if let Some(with) = self.with {
            stmt.with_cte(with);
        }
        stmt
    }
}

// SeaQuery requires this conversion for QueryStatementBuilder. Only scalar
// assignments are representable; row assignments must use our writer directly.
impl From<RowUpdate> for SubQueryStatement {
    fn from(update: RowUpdate) -> Self {
        update.into_statement().into()
    }
}

impl QueryStatementBuilder for RowUpdate {
    fn build_collect_any_into(&self, builder: &impl QueryBuilder, sql: &mut impl SqlWriter) {
        if let Some(with) = &self.with {
            builder.prepare_with_clause(with, sql);
        }
        sql.write_str("UPDATE ").unwrap();
        builder.prepare_iden(&Alias::new(&self.table).into_iden(), sql);
        sql.write_str(" SET ").unwrap();
        for (i, (columns, value)) in self.values.iter().enumerate() {
            if i > 0 {
                sql.write_str(", ").unwrap();
            }
            if columns.len() > 1 {
                sql.write_str("(").unwrap();
            }
            for (j, column) in columns.iter().enumerate() {
                if j > 0 {
                    sql.write_str(", ").unwrap();
                }
                builder.prepare_iden(&Alias::new(column).into_iden(), sql);
            }
            if columns.len() > 1 {
                sql.write_str(")").unwrap();
            }
            sql.write_str(" = ").unwrap();
            builder.prepare_expr(value, sql);
        }
        // Use SeaQuery's condition precedence and RETURNING rendering.
        let condition = ConditionHolder::new_with_condition(
            self.filters
                .iter()
                .cloned()
                .fold(Cond::all(), |c, f| c.add(f)),
        );
        builder.prepare_update_condition(&[], &condition, sql);
        builder.prepare_returning(&self.returning, sql);
    }
}

impl QueryStatementWriter for RowUpdate {
    fn build_collect_into<T: QueryBuilder>(&self, builder: T, sql: &mut impl SqlWriter) {
        self.build_collect_any_into(&builder, sql);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "row assignments cannot become a SeaQuery update")]
    fn row_assignments_cannot_silently_lose_columns() {
        let mut update = RowUpdate::new("parent");
        update.row_value(vec!["n".into(), "total".into()], Expr::cust("(SELECT 1, 2)"));
        let _ = update.into_statement();
    }
}
