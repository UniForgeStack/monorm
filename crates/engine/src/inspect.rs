//! On-demand inspection of the generated SeaQuery structure. Nothing is collected
//! while planning or executing ordinary queries, and no SQL strings are parsed.
use crate::plan::{Plan, PrefetchPlan};
use sea_query::*;
use serde::Serialize;
use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
};

#[derive(serde::Deserialize)]
pub struct InsertOptions {
    #[serde(default)]
    pub scope: std::collections::BTreeMap<String, usize>,
    pub conflict: Option<Vec<String>>,
    pub update: Option<Vec<String>>,
    pub set: Vec<orm_core::ir::Assignment>,
    pub filter: Option<orm_core::ir::Expr>,
}

#[derive(Default, Debug, Serialize)]
pub struct Statement {
    pub has_batching: bool,
    pub joins_count: usize,
    pub correlated_count: usize,
    pub uncorrelated_count: usize,
    pub cte_count: usize,
}
#[derive(Default, Debug, Serialize)]
pub struct Inspection {
    pub statement_count: usize,
    pub has_batching: bool,
    pub statements: Vec<Statement>,
}
impl Inspection {
    pub fn new(statements: Vec<Statement>) -> Self {
        Self {
            statement_count: statements.len(),
            has_batching: statements.iter().any(|s| s.has_batching),
            statements,
        }
    }
    pub fn plans(plans: &[Plan], batching: bool) -> Self {
        let mut statements = vec![];
        for plan in plans {
            append(plan, batching, &mut statements);
        }
        Self::new(statements)
    }
}

#[derive(Default)]
struct Scope {
    tables: HashSet<String>,
    columns: HashSet<String>,
    children: Vec<Scope>,
    union_branch: bool,
    cte: bool,
}

/// SeaQuery hides statement fields. Its QueryBuilder traversal visits the actual
/// expressions (including conditions, windows and CTEs). This builder discards SQL
/// and records scope boundaries, table aliases and qualified column references.
#[derive(Default)]
struct Visitor {
    scopes: RefCell<Vec<Scope>>,
    joins: RefCell<usize>,
    ctes: Cell<usize>,
    next_is_cte: Cell<bool>,
}
impl Visitor {
    fn nested(&self, query: &SubQueryStatement, sql: &mut impl SqlWriter) {
        self.scopes.borrow_mut().push(Scope {
            cte: self.next_is_cte.replace(false),
            ..Scope::default()
        });
        match query {
            SubQueryStatement::SelectStatement(s) => self.prepare_select_statement(s, sql),
            SubQueryStatement::InsertStatement(s) => self.prepare_insert_statement(s, sql),
            SubQueryStatement::UpdateStatement(s) => self.prepare_update_statement(s, sql),
            SubQueryStatement::DeleteStatement(s) => self.prepare_delete_statement(s, sql),
            SubQueryStatement::WithStatement(s) => self.prepare_with_query(s, sql),
        }
        let child = self.scopes.borrow_mut().pop().unwrap();
        self.scopes
            .borrow_mut()
            .last_mut()
            .unwrap()
            .children
            .push(child);
    }
    fn finish(self, batching: bool) -> Statement {
        fn dependencies(
            scope: Scope,
            enclosing: &HashSet<String>,
            counts: &mut Statement,
        ) -> HashSet<String> {
            let visible = enclosing.union(&scope.tables).cloned().collect();
            let mut references = scope.columns;
            for child in scope.children {
                let counted = !child.union_branch && !child.cte;
                let free = dependencies(child, &visible, counts);
                if counted {
                    if free.iter().any(|r| visible.contains(r)) {
                        counts.correlated_count += 1;
                    } else {
                        counts.uncorrelated_count += 1;
                    }
                }
                references.extend(free);
            }
            references.retain(|r| !scope.tables.contains(r));
            references
        }
        let mut result = Statement {
            has_batching: batching,
            joins_count: self.joins.into_inner(),
            cte_count: self.ctes.get(),
            ..Statement::default()
        };
        dependencies(
            self.scopes.into_inner().pop().unwrap(),
            &HashSet::new(),
            &mut result,
        );
        result
    }
}
impl QuotedBuilder for Visitor {
    fn quote(&self) -> Quote {
        Quote::new(b'"')
    }
}
impl EscapeBuilder for Visitor {}
impl TableRefBuilder for Visitor {}
impl PrecedenceDecider for Visitor {
    fn inner_expr_well_known_greater_precedence(&self, _: &Expr, _: &Oper) -> bool {
        false
    }
}
impl OperLeftAssocDecider for Visitor {
    fn well_known_left_associative(&self, _: &BinOper) -> bool {
        false
    }
}
impl QueryBuilder for Visitor {
    fn placeholder(&self) -> (&'static str, bool) {
        ("$", true)
    }
    fn prepare_query_statement(&self, query: &SubQueryStatement, sql: &mut impl SqlWriter) {
        self.nested(query, sql);
    }
    fn prepare_with_query_clause_materialization(
        &self,
        _: &CommonTableExpression,
        _: &mut impl SqlWriter,
    ) {
        // SeaQuery invokes this hook once per definition, immediately before
        // visiting its query or VALUES body (the CTE fields are private).
        self.ctes.set(self.ctes.get() + 1);
        self.next_is_cte.set(true);
    }
    fn prepare_values_rows(&self, _: &[Values], _: &mut impl SqlWriter) {
        // A VALUES CTE has no query scope to consume this marker.
        self.next_is_cte.set(false);
    }
    fn prepare_select_into(&self, _: &SelectInto, _: &mut impl SqlWriter) {}
    fn prepare_explain_statement(&self, _: &ExplainStatement, _: &mut impl SqlWriter) {}
    fn prepare_value(&self, _: Value, _: &mut impl SqlWriter) {}
    fn prepare_constant(&self, _: &Value, _: &mut impl SqlWriter) {}
    fn prepare_bin_oper(&self, _: &BinOper, _: &mut impl SqlWriter) {}
    fn prepare_function_name(&self, _: &Func, _: &mut impl SqlWriter) {}
    fn prepare_column_ref(&self, column: &ColumnRef, _: &mut impl SqlWriter) {
        let table = match column {
            ColumnRef::Column(ColumnName(table, _)) | ColumnRef::Asterisk(table) => table,
            _ => return,
        };
        if let Some(TableName(_, name)) = table {
            self.scopes
                .borrow_mut()
                .last_mut()
                .unwrap()
                .columns
                .insert(name.to_string());
        }
    }
    fn prepare_table_ref(&self, table: &TableRef, sql: &mut impl SqlWriter) {
        let alias = match table {
            TableRef::Table(TableName(_, name), alias) => alias.as_ref().unwrap_or(name),
            TableRef::SubQuery(_, alias)
            | TableRef::ValuesList(_, alias)
            | TableRef::FunctionCall(_, alias) => alias,
            _ => return,
        };
        self.scopes
            .borrow_mut()
            .last_mut()
            .unwrap()
            .tables
            .insert(alias.to_string());
        match table {
            TableRef::SubQuery(query, _) => {
                self.nested(&SubQueryStatement::SelectStatement((**query).clone()), sql)
            }
            TableRef::FunctionCall(func, _) => self.prepare_function_arguments(func, sql),
            _ => (),
        }
    }
    fn prepare_join_table_ref(&self, join: &JoinExpr, sql: &mut impl SqlWriter) {
        *self.joins.borrow_mut() += 1;
        self.prepare_table_ref(&join.table, sql);
    }
    fn prepare_union_statement(
        &self,
        _: UnionType,
        select: &SelectStatement,
        sql: &mut impl SqlWriter,
    ) {
        // UNION branches are peers, not nested subqueries. Keep their aliases in
        // separate scopes without adding a subquery count for the branch itself.
        self.scopes.borrow_mut().push(Scope::default());
        self.prepare_select_statement(select, sql);
        let branch = self.scopes.borrow_mut().pop().unwrap();
        let mut scopes = self.scopes.borrow_mut();
        let parent = scopes.last_mut().unwrap();
        parent.children.push(Scope {
            union_branch: true,
            ..branch
        });
    }
    fn prepare_expr(&self, expr: &Expr, sql: &mut impl SqlWriter) {
        // Visit template operands once, regardless of placeholder repetition.
        if let Expr::CustomWithExpr(_, exprs) = expr {
            for expr in exprs {
                self.prepare_expr(expr, sql);
            }
        } else {
            self.prepare_expr_common(expr, sql);
        }
    }
}

pub fn statement(query: &SubQueryStatement, batching: bool) -> Statement {
    let visitor = Visitor::default();
    visitor.scopes.borrow_mut().push(Scope::default());
    let mut sink = String::new();
    match query {
        SubQueryStatement::SelectStatement(s) => visitor.prepare_select_statement(s, &mut sink),
        SubQueryStatement::InsertStatement(s) => visitor.prepare_insert_statement(s, &mut sink),
        SubQueryStatement::UpdateStatement(s) => visitor.prepare_update_statement(s, &mut sink),
        SubQueryStatement::DeleteStatement(s) => visitor.prepare_delete_statement(s, &mut sink),
        SubQueryStatement::WithStatement(s) => visitor.prepare_with_query(s, &mut sink),
    }
    visitor.finish(batching)
}
fn prefetch(plan: &PrefetchPlan, statements: &mut Vec<Statement>) {
    statements.push(statement(
        &plan.statement(vec![]).into_sub_query_statement(),
        true,
    ));
    for child in &plan.children {
        prefetch(child, statements);
    }
}
fn append(plan: &Plan, batching: bool, statements: &mut Vec<Statement>) {
    let query = match plan {
        Plan::Select(s) => {
            statements.push(statement(
                &s.stmt.clone().into_sub_query_statement(),
                batching,
            ));
            for p in &s.prefetch {
                prefetch(p, statements);
            }
            return;
        }
        Plan::Count(s) | Plan::Exists(s) => s.clone().into_sub_query_statement(),
        Plan::Update(s, _) => s.clone().into_sub_query_statement(),
        Plan::Delete(s, _) => s.clone().into_sub_query_statement(),
        Plan::Insert(s, _) => s.clone().into_sub_query_statement(),
        #[cfg(feature = "model-composition")]
        Plan::ComposedInsert(insert) => {
            statements.extend(insert.inspection());
            return;
        }
        #[cfg(feature = "model-composition")]
        Plan::ComposedMutation(mutation) => {
            statements.extend(mutation.inspection());
            return;
        }
    };
    statements.push(statement(&query, batching));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inspect(select: SelectStatement) -> Statement {
        statement(&select.into_sub_query_statement(), false)
    }

    #[test]
    fn lateral_and_from_placement_do_not_imply_correlation() {
        for lateral in [false, true] {
            for correlated in [false, true] {
                let mut child = Query::select();
                child
                    .column((Alias::new("child"), Alias::new("id")))
                    .from(Alias::new("child"));
                if correlated {
                    child.and_where(
                        Expr::col(("child", "parent_id")).eq(Expr::col(("parent", "id"))),
                    );
                }
                let mut root = Query::select();
                root.column((Alias::new("parent"), Alias::new("id")))
                    .from(Alias::new("parent"));
                if lateral {
                    root.join_lateral(
                        JoinType::LeftJoin,
                        child,
                        Alias::new("derived"),
                        Expr::val(true),
                    );
                } else {
                    root.from_subquery(child, Alias::new("derived"));
                }
                let result = inspect(root);
                assert_eq!(result.joins_count, usize::from(lateral));
                assert_eq!(result.correlated_count, usize::from(correlated));
                assert_eq!(result.uncorrelated_count, usize::from(!correlated));
            }
        }
    }

    #[test]
    fn union_branch_subqueries_keep_their_enclosing_aliases() {
        let nested = Query::select().expr(Expr::col(("b", "id"))).to_owned();
        let branch = Query::select()
            .expr(Expr::exists(nested))
            .from(Alias::new("b"))
            .to_owned();
        let derived = Query::select()
            .expr(Expr::val(1))
            .from(Alias::new("a"))
            .union(UnionType::All, branch)
            .to_owned();
        let root = Query::select().expr(Expr::exists(derived)).to_owned();
        let result = inspect(root);
        assert_eq!(result.correlated_count, 1);
        assert_eq!(result.uncorrelated_count, 1);
    }

    #[test]
    fn local_alias_shadows_an_enclosing_table() {
        let child = Query::select()
            .column((Alias::new("same"), Alias::new("id")))
            .from(Alias::new("same"))
            .to_owned();
        let root = Query::select()
            .expr(Expr::exists(child))
            .from(Alias::new("same"))
            .to_owned();
        let result = inspect(root);
        assert_eq!(result.correlated_count, 0);
        assert_eq!(result.uncorrelated_count, 1);
    }

    #[test]
    fn cte_definitions_are_separate_from_subqueries_in_their_bodies() {
        let child = Query::select().expr(Expr::col(("parent", "id"))).to_owned();
        let body = Query::select()
            .expr(Expr::exists(child))
            .from(Alias::new("parent"))
            .union(
                UnionType::All,
                Query::select().expr(Expr::val(1)).to_owned(),
            )
            .to_owned();
        let cte = CommonTableExpression::new()
            .table_name("chain")
            .query(body)
            .to_owned();
        let with = WithClause::new().recursive(true).cte(cte).to_owned();
        let root = Query::select()
            .column(Asterisk)
            .from(Alias::new("chain"))
            .with_cte(with)
            .to_owned();
        let result = inspect(root);
        assert_eq!(result.cte_count, 1);
        assert_eq!(result.correlated_count, 1);
        assert_eq!(result.uncorrelated_count, 0);
        let json = serde_json::to_value(result).unwrap();
        assert_eq!(json["cte_count"], 1);
        assert_eq!(json["correlated_count"], 1);
        assert!(json.get("subqueries").is_none());
    }

    #[test]
    fn values_cte_does_not_hide_a_following_subquery() {
        let cte = CommonTableExpression::new()
            .table_name("numbers")
            .values(vec![Values(vec![1.into()])])
            .to_owned();
        let with = WithClause::new().cte(cte).to_owned();
        let child = Query::select().expr(Expr::val(1)).to_owned();
        let root = Query::select()
            .expr(Expr::exists(child))
            .with_cte(with)
            .to_owned();
        let result = inspect(root);
        assert_eq!(result.cte_count, 1);
        assert_eq!(result.correlated_count, 0);
        assert_eq!(result.uncorrelated_count, 1);
    }
}
