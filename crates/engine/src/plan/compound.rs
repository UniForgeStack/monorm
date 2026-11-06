//! `UNION` / `UNION ALL` of `select(...)` queries.
//!
//! Each operand is planned as an ordinary `select(...)` with canonical column aliases
//! (`_u0`, `_u1`, ...) and rendered as a derived table, so that its own order and slice
//! apply before the set operation on both databases. A set node is a derived table too:
//! its own order and slice apply to the whole node, and nested groups keep their meaning.
use sea_query::UnionType;

use super::*;
use monorm_core::ir::{Compound, CompoundTerminal, SetExpr, SetOperator};

/// One planned operand or set node.
struct Node {
    names: Vec<String>,
    /// `None`: a `NULL` literal in every operand below, until a sibling gives it a type.
    types: Vec<Option<ValueType>>,
    kind: Kind,
}

enum Kind {
    Leaf { stmt: Box<SelectStatement>, nulls: Vec<bool> },
    Set { operator: SetOperator, left: Box<Node>, right: Box<Node>, order: Vec<(usize, bool)>, limit: Option<Count>, offset: Option<Count> },
}

impl Node {
    fn sliced(&self) -> bool {
        matches!(&self.kind, Kind::Set { order, limit, offset, .. } if !order.is_empty() || limit.is_some() || offset.is_some())
    }
}

pub(super) fn plan<'s>(schema: &'s Schema, target: Target, c: &Compound, params: &'s dyn Params) -> Result<Plan> {
    crate::db::require_dialect(target.dialect)?;
    let SetExpr::Set { .. } = &c.expr else {
        return Err(Error::query("a compound query needs a set operation: union() or union_all()"));
    };
    let node = node(schema, target, &c.expr, params)?;
    let mut types = Vec::with_capacity(node.types.len());
    for (t, name) in node.types.iter().zip(&node.names) {
        types.push(t.ok_or_else(|| Error::query(format!("column {name:?} has no type: every query gives NULL there; select a typed value")))?);
    }
    let model = first_model(schema, &c.expr)?;
    let mut counter = Counter::default();
    let stmt = render(target, node, &types, &mut counter, params)?;
    let width = types.len();
    Ok(match c.terminal {
        CompoundTerminal::Read => {
            within_cap(target, &stmt)?;
            Plan::Select(SelectPlan { stmt, types, output: Output::Rows { model, items: vec![None; width] }, filtered: false, computed: vec![], prefetch: vec![] })
        }
        CompoundTerminal::Count => {
            let mut outer = Query::select();
            outer.expr(SExpr::cust("COUNT(*)")).from_subquery(stmt, Alias::new("counted"));
            within_cap(target, &outer)?;
            Plan::Count(outer)
        }
        CompoundTerminal::Exists => {
            let mut inner = Query::select();
            inner.expr(SExpr::cust("1")).from_subquery(stmt, Alias::new("existing")).limit(1);
            let mut outer = Query::select();
            outer.expr(SExpr::exists(inner));
            within_cap(target, &outer)?;
            Plan::Exists(outer)
        }
    })
}

fn first_model(schema: &Schema, e: &SetExpr) -> Result<usize> {
    match e {
        SetExpr::Leaf(q) => schema.model_idx(&q.model).map_err(query_err),
        SetExpr::Set { left, .. } => first_model(schema, left),
    }
}

fn within_cap(target: Target, stmt: &SelectStatement) -> Result<()> {
    let n = crate::db::build(target.dialect, stmt).1.len();
    if n > target.caps.max_params {
        return Err(Error::query(format!(
            "this query binds {n} parameters, above the {} the database allows in one statement; use fewer values or smaller sets",
            target.caps.max_params
        )));
    }
    Ok(())
}

fn node<'s>(schema: &'s Schema, target: Target, e: &SetExpr, params: &'s dyn Params) -> Result<Node> {
    match e {
        SetExpr::Leaf(q) => leaf(schema, target, q, params),
        SetExpr::Set { operator, left, right, order, limit, offset } => {
            let (left, right) = (node(schema, target, left, params)?, node(schema, target, right, params)?);
            if left.names.len() != right.names.len() {
                return Err(Error::query(format!(
                    "a set operation needs the same columns on both sides: the left query has {} columns, the right one has {}",
                    left.names.len(),
                    right.names.len()
                )));
            }
            let mut types = Vec::with_capacity(left.types.len());
            for (i, name) in left.names.iter().enumerate() {
                if *name != right.names[i] {
                    return Err(Error::query(format!(
                        "a set operation needs the same column names in the same order: column {i} is {name:?} on the left and {:?} on the right; name the columns alike with the same labels",
                        right.names[i]
                    )));
                }
                types.push(match (left.types[i], right.types[i]) {
                    (Some(a), Some(b)) if a == b => Some(a),
                    (Some(a), Some(b)) => {
                        return Err(Error::query(format!(
                            "column {name:?} has type {} on the left and {} on the right; a set operation does not convert types",
                            describe(a),
                            describe(b)
                        )))
                    }
                    (a, b) => a.or(b),
                });
            }
            let order = order.iter().map(|o| order_name(&left.names, o)).collect::<Result<Vec<_>>>()?;
            let names = left.names.clone();
            Ok(Node { names, types, kind: Kind::Set { operator: *operator, left: Box::new(left), right: Box::new(right), order, limit: *limit, offset: *offset } })
        }
    }
}

fn describe(t: ValueType) -> String {
    let mut s = format!("{:?}", t.ty).to_lowercase();
    if t.array {
        s += "[]";
    }
    if t.enum_idx.is_some() {
        s += " (enum)";
    }
    s
}

fn order_name(names: &[String], o: &str) -> Result<(usize, bool)> {
    let (name, desc) = match o.strip_prefix('-') {
        Some(rest) => (rest, true),
        None => (o, false),
    };
    if name.is_empty() {
        return Err(Error::query("order_by() of a set operation takes output names, with a leading '-' for descending"));
    }
    match names.iter().position(|n| n == name) {
        Some(i) => Ok((i, desc)),
        None => Err(Error::query(format!("order_by() names {name:?}, which is missing from the output columns ({})", names.join(", ")))),
    }
}

fn leaf<'s>(schema: &'s Schema, target: Target, q: &Select, params: &'s dyn Params) -> Result<Node> {
    validate_named_bypass(schema, &q.model, &q.without_defaults)?;
    if q.lock.is_some() {
        return Err(Error::query("lock() can't be used in a set operation"));
    }
    if !q.with.is_empty() || q.from.is_some() || !q.joins.is_empty() {
        return Err(Error::query("a set operation can't use a CTE"));
    }
    if !q.scope.is_empty() {
        return Err(Error::query("scope values belong to the whole compound query, not to one operand"));
    }
    let Some(items) = &q.columns else {
        return Err(Error::query("a set operation takes select(...) queries"));
    };
    let mut names: Vec<String> = Vec::with_capacity(items.len());
    let mut renamed = Vec::with_capacity(items.len());
    let mut nulls = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let SelectItem::Expr { expr, name } = item else {
            return Err(Error::query("a set operation takes columns and expressions, not model instances"));
        };
        let name = name.clone().filter(|n| !n.is_empty()).ok_or_else(|| Error::query("a set operation needs a name for each column"))?;
        if <[String]>::contains(&names, &name) {
            return Err(Error::query(format!("a set operation needs distinct column names, {name:?} is used twice")));
        }
        names.push(name);
        nulls.push(matches!(expr, Expr::Null));
        renamed.push(SelectItem::Expr { expr: expr.clone(), name: Some(format!("_u{i}")) });
    }
    let defaulted = with_default_order(schema, q)?;
    let q = defaulted.as_ref().unwrap_or(q);
    let mut p = Planner::new(schema, &[], target, &q.model, None, params, vec![], 0)?;
    let defaults = p.with_read_defaults(q)?;
    let plan = p.select_columns(defaults.as_ref(), &renamed, true)?;
    if !plan.computed.is_empty() {
        return Err(Error::query("computed outputs can't be used in a set operation"));
    }
    let mut types = Vec::with_capacity(plan.types.len());
    for ((t, name), (item, null)) in plan.types.iter().zip(&names).zip(items.iter().zip(&nulls)) {
        if *null {
            types.push(None);
            continue;
        }
        if t.array {
            return Err(Error::query(format!("column {name:?} is an array; array columns can't be used in a set operation")));
        }
        if t.ty == ColType::Json {
            return Err(Error::query(format!("column {name:?} is json; json columns can't be used in a set operation")));
        }
        if let SelectItem::Expr { expr: Expr::Col { path, name: field, sub, .. }, .. } = item {
            let m = p.walk(p.root, path)?;
            if sub.is_empty() && p.model(m).field(field).ok().is_some_and(|f| f.read_sql.is_some()) {
                return Err(Error::query(format!("column {name:?} has a read conversion; it can't be used in a set operation")));
            }
        }
        types.push(Some(*t));
    }
    Ok(Node { names, types, kind: Kind::Leaf { stmt: Box::new(plan.stmt), nulls } })
}

#[derive(Default)]
struct Counter {
    branch: usize,
    node: usize,
}

fn unit(i: usize) -> Alias {
    Alias::new(format!("_u{i}"))
}

/// `SELECT _u0, _u1, ... FROM (<inner>) AS <alias>`.
fn wrap(inner: SelectStatement, width: usize, alias: String) -> SelectStatement {
    let mut s = Query::select();
    for i in 0..width {
        s.column(unit(i));
    }
    s.from_subquery(inner, Alias::new(alias));
    s
}

fn render(target: Target, n: Node, types: &[ValueType], counter: &mut Counter, params: &dyn Params) -> Result<SelectStatement> {
    match n.kind {
        Kind::Leaf { stmt, nulls } => {
            let mut s = Query::select();
            for (i, t) in types.iter().enumerate() {
                if nulls[i] && target.dialect == Dialect::Postgres {
                    if t.enum_idx.is_some() {
                        return Err(Error::query(format!("NULL can't stand for the enum column {:?} in a set operation", n.names[i])));
                    }
                    let cast = template(target.dialect, format!("CAST($1 AS {})", pg_type(*t)), vec![SExpr::col(unit(i))]);
                    s.expr_as(cast, unit(i));
                } else {
                    s.column(unit(i));
                }
            }
            s.from_subquery(*stmt, Alias::new(format!("_b{}", counter.branch)));
            counter.branch += 1;
            Ok(s)
        }
        Kind::Set { operator, left, right, order, limit, offset } => {
            let width = types.len();
            let mut members = vec![];
            for child in [*left, *right] {
                let sliced = child.sliced();
                let mut s = render(target, child, types, counter, params)?;
                if sliced {
                    s = wrap(s, width, format!("_b{}", counter.branch));
                    counter.branch += 1;
                }
                members.push(s);
            }
            let right = members.pop().expect("two members");
            let mut union = members.pop().expect("two members");
            union.union(
                match operator {
                    SetOperator::Union => UnionType::Distinct,
                    SetOperator::UnionAll => UnionType::All,
                },
                right,
            );
            let mut s = wrap(union, width, format!("_c{}", counter.node));
            counter.node += 1;
            for (i, desc) in order {
                s.order_by(unit(i), if desc { SOrder::Desc } else { SOrder::Asc });
            }
            if let Some(c) = limit {
                s.limit(count(params, c)?);
            }
            if let Some(c) = offset {
                s.offset(count(params, c)?);
            }
            Ok(s)
        }
    }
}

/// The Postgres type of a `NULL` that takes the type of its sibling operand.
fn pg_type(t: ValueType) -> &'static str {
    match t.ty {
        ColType::BigInt => "bigint",
        ColType::Int => "integer",
        ColType::Float => "double precision",
        ColType::Bool => "boolean",
        ColType::String | ColType::Text => "text",
        ColType::DateTime => "timestamptz",
        ColType::Date => "date",
        ColType::Uuid => "uuid",
        ColType::Json => "jsonb",
        ColType::Decimal => "numeric",
    }
}
