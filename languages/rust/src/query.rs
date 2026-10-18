use crate::{
    placeholder::{Cons, Join, Nil, Param, Slot, SlotRef},
    subquery::{Reach, Standalone},
    value::Values,
    *,
};
use orm_core::ir::{self, Expr, Operation};
use std::{
    future::{Future, IntoFuture},
    pin::Pin,
};

pub enum One {}
/// A to-one path whose join can be absent. Filter operands keep their field type.
pub enum OptionalOne {}
pub enum Many {}
pub trait Combine<C> {
    type Output;
}
impl Combine<One> for One {
    type Output = One;
}
impl Combine<Many> for One {
    type Output = Many;
}
impl Combine<One> for Many {
    type Output = Many;
}
impl Combine<Many> for Many {
    type Output = Many;
}
impl Combine<One> for OptionalOne {
    type Output = OptionalOne;
}
impl Combine<OptionalOne> for One {
    type Output = OptionalOne;
}
impl Combine<OptionalOne> for OptionalOne {
    type Output = OptionalOne;
}
impl Combine<Many> for OptionalOne {
    type Output = Many;
}
impl Combine<OptionalOne> for Many {
    type Output = Many;
}
/// Field/path tokens have a root source, a value type, and relation cardinality.
pub struct Column<M, T, C = One> {
    name: &'static str,
    path: Vec<String>,
    _type: PhantomData<(M, T, C)>,
}
impl<M, T, C> Clone for Column<M, T, C> {
    fn clone(&self) -> Self {
        Self {
            name: self.name,
            path: self.path.clone(),
            _type: PhantomData,
        }
    }
}
impl<M, T, C> Column<M, T, C> {
    #[doc(hidden)]
    pub fn generated(name: &'static str, path: &[&str]) -> Self {
        Self {
            name,
            path: path.iter().map(|s| (*s).into()).collect(),
            _type: PhantomData,
        }
    }
    #[doc(hidden)]
    pub const fn field(name: &'static str) -> Self {
        Self {
            name,
            path: Vec::new(),
            _type: PhantomData,
        }
    }
    #[doc(hidden)]
    pub fn path(name: &'static str, path: Vec<String>) -> Self {
        Self {
            name,
            path,
            _type: PhantomData,
        }
    }
    pub(crate) fn outer_ref(&self, id: u64) -> crate::subquery::OuterRef {
        crate::subquery::OuterRef {
            id,
            path: self.path.clone(),
            name: self.name,
        }
    }
    pub(crate) fn expr(&self) -> Expr {
        Expr::Col {
            path: self.path.clone(),
            name: self.name.into(),
        }
    }
    fn cmp(self, value: T, op: ir::CmpOp) -> Predicate<M>
    where
        T: Codec,
    {
        Predicate {
            tree: Tree::Compare(self.expr(), op, value.encode()),
            _model: PhantomData,
        }
    }
    fn cmp_param<S: Slot<Value = T>>(
        self,
        _: Param<S>,
        op: ir::CmpOp,
    ) -> Predicate<M, Cons<S, Nil>> {
        Predicate {
            tree: Tree::Param(self.expr(), op, SlotRef::of::<S>()),
            _model: PhantomData,
        }
    }
    pub fn eq_param<S: Slot<Value = T>>(self, p: Param<S>) -> Predicate<M, Cons<S, Nil>> {
        self.cmp_param(p, ir::CmpOp::Eq)
    }
    pub fn ne_param<S: Slot<Value = T>>(self, p: Param<S>) -> Predicate<M, Cons<S, Nil>> {
        self.cmp_param(p, ir::CmpOp::Ne)
    }
    pub fn lt_param<S: Slot<Value = T>>(self, p: Param<S>) -> Predicate<M, Cons<S, Nil>> {
        self.cmp_param(p, ir::CmpOp::Lt)
    }
    pub fn le_param<S: Slot<Value = T>>(self, p: Param<S>) -> Predicate<M, Cons<S, Nil>> {
        self.cmp_param(p, ir::CmpOp::Le)
    }
    pub fn gt_param<S: Slot<Value = T>>(self, p: Param<S>) -> Predicate<M, Cons<S, Nil>> {
        self.cmp_param(p, ir::CmpOp::Gt)
    }
    pub fn ge_param<S: Slot<Value = T>>(self, p: Param<S>) -> Predicate<M, Cons<S, Nil>> {
        self.cmp_param(p, ir::CmpOp::Ge)
    }
    pub fn eq(self, value: T) -> Predicate<M>
    where
        T: Codec,
    {
        self.cmp(value, ir::CmpOp::Eq)
    }
    pub fn ne(self, value: T) -> Predicate<M>
    where
        T: Codec,
    {
        self.cmp(value, ir::CmpOp::Ne)
    }
    pub fn lt(self, value: T) -> Predicate<M>
    where
        T: Codec,
    {
        self.cmp(value, ir::CmpOp::Lt)
    }
    pub fn le(self, value: T) -> Predicate<M>
    where
        T: Codec,
    {
        self.cmp(value, ir::CmpOp::Le)
    }
    pub fn gt(self, value: T) -> Predicate<M>
    where
        T: Codec,
    {
        self.cmp(value, ir::CmpOp::Gt)
    }
    pub fn ge(self, value: T) -> Predicate<M>
    where
        T: Codec,
    {
        self.cmp(value, ir::CmpOp::Ge)
    }
    pub fn in_values(self, values: impl IntoIterator<Item = T>) -> Predicate<M>
    where
        T: Codec,
    {
        Predicate {
            tree: Tree::In(self.expr(), values.into_iter().map(T::encode).collect()),
            _model: PhantomData,
        }
    }
    pub fn eq_field<C2>(self, other: Column<M, T, C2>) -> Predicate<M> {
        Predicate {
            tree: Tree::Expr(Expr::Cmp {
                op: ir::CmpOp::Eq,
                l: Box::new(self.expr()),
                r: Box::new(other.expr()),
            }),
            _model: PhantomData,
        }
    }
}
impl<M, T, C> Column<M, Option<T>, C> {
    pub fn is_null(self) -> Predicate<M> {
        Predicate {
            tree: Tree::Expr(Expr::IsNull {
                item: Box::new(self.expr()),
                neg: false,
            }),
            _model: PhantomData,
        }
    }
}
impl<M, T> Column<M, T, One> {
    pub fn asc(self) -> Ordering<M> {
        Ordering {
            order: ir::Order {
                expr: self.expr(),
                desc: false,
                nulls: None,
            },
            _model: PhantomData,
        }
    }
    pub fn desc(self) -> Ordering<M> {
        Ordering {
            order: ir::Order {
                expr: self.expr(),
                desc: true,
                nulls: None,
            },
            _model: PhantomData,
        }
    }
}
impl<M, T> Column<M, T, OptionalOne> {
    pub fn asc(self) -> Ordering<M> {
        Column::<M, T, One>::path(self.name, self.path).asc()
    }
    pub fn desc(self) -> Ordering<M> {
        Column::<M, T, One>::path(self.name, self.path).desc()
    }
}
#[derive(Clone)]
#[doc(hidden)]
pub enum Tree {
    Expr(Expr),
    Compare(Expr, ir::CmpOp, sea_query::Value),
    In(Expr, Vec<sea_query::Value>),
    And(Vec<Tree>),
    Or(Vec<Tree>),
    Not(Box<Tree>),
    Nested(String, Vec<Tree>),
    /// A typed placeholder, replaced by its bound value before planning.
    Param(Expr, ir::CmpOp, crate::placeholder::SlotRef),
    /// A scope value for `scope.<name>` defaults, bound at execution.
    Scope(String, sea_query::Value),
    /// A comparison with a column of an enclosing query, resolved to a depth at erasure.
    Outer(Expr, ir::CmpOp, crate::subquery::OuterRef),
    /// `item [NOT] IN (SELECT one_column ...)`; the build pushes its own outer scope.
    InSub(Expr, bool, crate::subquery::BuildSelect),
    /// `EXISTS (SELECT ...)`.
    Exists(crate::subquery::BuildSelect),
}
/// State of one erasure: the bound values and the enclosing-query handles of the current subquery.
#[doc(hidden)]
pub struct Erase<'a> {
    pub(crate) values: &'a mut Values,
    pub(crate) scopes: Vec<u64>,
    pub(crate) error: Option<String>,
}
impl<'a> Erase<'a> {
    pub(crate) fn new(values: &'a mut Values) -> Self {
        Self {
            values,
            scopes: Vec::new(),
            error: None,
        }
    }
    pub(crate) fn finish(self) -> Result<()> {
        match self.error {
            Some(message) => Err(Error::Compatibility(message)),
            None => Ok(()),
        }
    }
}
impl Tree {
    pub(crate) fn erase(self, erase: &mut Erase) -> Expr {
        let mut bind = |v| {
            let i = erase.values.0.len();
            erase.values.0.push(v);
            Expr::Param { i }
        };
        match self {
            Self::Nested(_, _) | Self::Scope(_, _) => {
                unreachable!("nested and scope filters are erased with their select")
            }
            Self::Outer(l, op, outer) => {
                let depth = erase
                    .scopes
                    .iter()
                    .rposition(|id| *id == outer.id)
                    .map(|at| erase.scopes.len() - at);
                let Some(depth) = depth else {
                    erase.error.get_or_insert_with(|| {
                        format!("unresolved outer source for {}", outer.name)
                    });
                    return Expr::Const { value: false };
                };
                Expr::Cmp {
                    op,
                    l: Box::new(l),
                    r: Box::new(Expr::Outer {
                        depth,
                        path: outer.path,
                        name: outer.name.into(),
                    }),
                }
            }
            Self::InSub(item, neg, build) => Expr::InSelect {
                item: Box::new(item),
                select: Box::new(build(erase)),
                neg,
            },
            Self::Exists(build) => Expr::Exists {
                select: Box::new(build(erase)),
            },
            Self::Param(_, _, slot) => {
                unreachable!("placeholder {} is bound before planning", slot.name)
            }
            Self::Expr(e) => e,
            Self::Compare(l, op, v)
                if v == v.as_null() && matches!(op, ir::CmpOp::Eq | ir::CmpOp::Ne) =>
            {
                Expr::IsNull {
                    item: Box::new(l),
                    neg: matches!(op, ir::CmpOp::Ne),
                }
            }
            Self::Compare(l, op, v) => Expr::Cmp {
                op,
                l: Box::new(l),
                r: Box::new(bind(v)),
            },
            Self::In(item, vs) => Expr::In {
                item: Box::new(item),
                values: vs.into_iter().map(bind).collect(),
                neg: false,
            },
            Self::And(items) => Expr::And {
                items: items.into_iter().map(|t| t.erase(erase)).collect(),
            },
            Self::Or(items) => Expr::Or {
                items: items.into_iter().map(|t| t.erase(erase)).collect(),
            },
            Self::Not(t) => Expr::Not {
                item: Box::new(t.erase(erase)),
            },
        }
    }
}
/// `K` lists the placeholders; `X` is `Correlated` when the predicate reads an enclosing query.
pub struct Predicate<M, K = crate::placeholder::Nil, X = Standalone> {
    pub(crate) tree: Tree,
    _model: PhantomData<(M, K, X)>,
}
impl<M, K, X> Clone for Predicate<M, K, X> {
    fn clone(&self) -> Self {
        Self {
            tree: self.tree.clone(),
            _model: PhantomData,
        }
    }
}
impl<M, K, X> Predicate<M, K, X> {
    pub(crate) fn from_tree(tree: Tree) -> Self {
        Self {
            tree,
            _model: PhantomData,
        }
    }
    pub fn and<K2, X2>(
        self,
        other: Predicate<M, K2, X2>,
    ) -> Predicate<M, <K as Join<K2>>::Out, <X as Reach<X2>>::Out>
    where
        K: Join<K2>,
        X: Reach<X2>,
    {
        Predicate {
            tree: Tree::And(vec![self.tree, other.tree]),
            _model: PhantomData,
        }
    }
    pub fn or<K2, X2>(
        self,
        other: Predicate<M, K2, X2>,
    ) -> Predicate<M, <K as Join<K2>>::Out, <X as Reach<X2>>::Out>
    where
        K: Join<K2>,
        X: Reach<X2>,
    {
        Predicate {
            tree: Tree::Or(vec![self.tree, other.tree]),
            _model: PhantomData,
        }
    }
}
impl<M, K, X> std::ops::Not for Predicate<M, K, X> {
    type Output = Self;
    fn not(self) -> Self {
        Self {
            tree: Tree::Not(Box::new(self.tree)),
            _model: PhantomData,
        }
    }
}
pub struct Ordering<M> {
    order: ir::Order,
    _model: PhantomData<M>,
}
/// Generated unique lookup token. Runtime get still checks cardinality.
pub struct Unique<M, V> {
    pub(crate) fields: &'static [&'static str],
    _type: PhantomData<(M, V)>,
}
impl<M, V> Unique<M, V> {
    #[doc(hidden)]
    pub const fn generated(fields: &'static [&'static str]) -> Self {
        Self {
            fields,
            _type: PhantomData,
        }
    }
}
pub trait KeyValues {
    type Input;
    fn values(input: Self::Input) -> Vec<sea_query::Value>;
}
impl<T: Codec> KeyValues for T {
    type Input = Self;
    fn values(input: Self::Input) -> Vec<sea_query::Value> {
        vec![input.encode()]
    }
}
impl<M, V: KeyValues> Unique<M, V> {
    fn predicate(&self, values: V::Input) -> Predicate<M> {
        let trees = self
            .fields
            .iter()
            .zip(V::values(values))
            .map(|(name, v)| {
                Tree::Compare(
                    Expr::Col {
                        path: vec![],
                        name: (*name).into(),
                    },
                    ir::CmpOp::Eq,
                    v,
                )
            })
            .collect();
        Predicate {
            tree: Tree::And(trees),
            _model: PhantomData,
        }
    }
}
pub enum Optional {}
pub enum Required {}
pub trait Presence: Send + 'static {
    type Single<T>;
    fn single<T>(row: Option<T>) -> Result<Self::Single<T>>;
    fn collection<T>(rows: Vec<T>) -> Result<Vec<T>>;
}
impl Presence for Optional {
    type Single<T> = Option<T>;
    fn single<T>(row: Option<T>) -> Result<Option<T>> {
        Ok(row)
    }
    fn collection<T>(rows: Vec<T>) -> Result<Vec<T>> {
        Ok(rows)
    }
}
impl Presence for Required {
    type Single<T> = T;
    fn single<T>(row: Option<T>) -> Result<T> {
        row.ok_or(Error::NotFound)
    }
    fn collection<T>(rows: Vec<T>) -> Result<Vec<T>> {
        if rows.is_empty() {
            Err(Error::NotFound)
        } else {
            Ok(rows)
        }
    }
}
pub enum Unsliced {}
pub enum Sliced {}
/// Lazy owned builder. Clone explicitly to reuse it.
pub struct Query<M: Model, R = Optional, L = Unsliced, D = DefaultSelection> {
    pub(crate) select: ir::Select,
    pub(crate) filters: Vec<Tree>,
    pub(crate) _type: PhantomData<(M, R, L, D)>,
}
impl<M: Model, R, L, D> Clone for Query<M, R, L, D> {
    fn clone(&self) -> Self {
        Self {
            select: self.select.clone(),
            filters: self.filters.clone(),
            _type: PhantomData,
        }
    }
}
impl<M: Model> Default for Query<M> {
    fn default() -> Self {
        Self::new()
    }
}
impl<M: Model> Query<M> {
    pub const fn new() -> Self {
        Self {
            select: ir::Select {
                model: String::new(),
                model_fields: None,
                with_default_fields: false,
                model_helpers: vec![],
                scope: std::collections::BTreeMap::new(),
                without_defaults: false,
                without_related: false,
                with: vec![],
                from: None,
                joins: vec![],
                windows: vec![],
                filters: vec![],
                order: vec![],
                limit: None,
                offset: None,
                select_related: vec![],
                related_fields: vec![],
                prefetch: vec![],
                lock: None,
                columns: None,
                group_by: vec![],
                having: vec![],
                distinct: false,
                distinct_on: vec![],
            },
            filters: vec![],
            _type: PhantomData,
        }
    }
}
impl<M: Model, R, L, D> Query<M, R, L, D> {
    /// Retained for callers that explicitly start a query from a model root.
    pub fn query(self) -> Self {
        self
    }
    // Shape transitions and nested plans need defaults before changing M. Ordinary
    // roots defer model setup until erasure. An empty model marks a fresh root.
    pub(crate) fn initialized(mut self) -> Self {
        if self.select.model.is_empty() {
            self.select.model = M::NAME.into();
            self.select.model_fields = Some(M::fields());
            M::configure(&mut self.select);
        }
        self
    }
    pub fn filter(mut self, p: Predicate<M::Source>) -> Self {
        self.filters.push(p.tree);
        self
    }
    pub(crate) fn push_tree(mut self, tree: Tree) -> Self {
        self.filters.push(tree);
        self
    }
    /// The value `scope.<name>` reads in default filters for this execution; a later value replaces an earlier one.
    pub fn scope<T: Codec>(mut self, name: &str, value: T) -> Self {
        self.filters
            .retain(|t| !matches!(t, Tree::Scope(n, _) if n == name));
        self.filters.push(Tree::Scope(name.into(), value.encode()));
        self
    }
    pub fn exclude(self, p: Predicate<M::Source>) -> Self {
        self.filter(!p)
    }
    pub fn order_by(mut self, order: Ordering<M::Source>) -> Self {
        self.select.order = vec![order.order];
        self
    }
    pub fn then_by(mut self, order: Ordering<M::Source>) -> Self {
        self.select.order.push(order.order);
        self
    }
    pub fn require(self) -> Query<M, Required, L, D> {
        Query {
            select: self.select,
            filters: self.filters,
            _type: PhantomData,
        }
    }
    pub fn limit(mut self, n: u64) -> Query<M, R, Sliced, D> {
        self.select.limit = Some(ir::Count::Value(n));
        Query {
            select: self.select,
            filters: self.filters,
            _type: PhantomData,
        }
    }
    pub fn offset(mut self, n: u64) -> Query<M, R, Sliced, D> {
        self.select.offset = Some(ir::Count::Value(n));
        Query {
            select: self.select,
            filters: self.filters,
            _type: PhantomData,
        }
    }
    pub fn slice(self, start: u64, end: u64) -> Result<Query<M, R, Sliced, D>> {
        let offset = match self.select.offset {
            Some(ir::Count::Value(n)) => n,
            _ => 0,
        };
        let offset = offset
            .checked_add(start)
            .ok_or_else(|| Error::Engine(orm_engine::Error::value("slice offset overflow")))?;
        let requested = end.saturating_sub(start);
        let limit = match self.select.limit {
            Some(ir::Count::Value(n)) => n.saturating_sub(start).min(requested),
            _ => requested,
        };
        Ok(self.offset(offset).limit(limit))
    }

    pub fn using(self, db: &Database<M::Schema>) -> Bound<'_, M, R, L> {
        Bound {
            query: Query {
                select: self.select,
                filters: self.filters,
                _type: PhantomData,
            },
            db,
        }
    }
    pub(crate) fn erase(self) -> Result<(ir::Select, Values)> {
        let mut values = Values::default();
        let mut erase = Erase::new(&mut values);
        let select = self.erase_with(&mut erase);
        erase.finish()?;
        Ok((select, values))
    }
    pub(crate) fn erase_with(self, erase: &mut Erase) -> ir::Select {
        let this = self.initialized();
        let mut select = this.select;
        erase_filters(&mut select, this.filters, erase);
        select
    }
}
impl<M: Model, R, D> Query<M, R, Unsliced, D> {
    pub fn or(mut self, other: impl Into<Self>) -> Result<Self> {
        let other = other.into();
        // The left keeps its order; the right supplies only one grouped filter.
        let root_filter_count = |filters: &[Tree]| {
            filters
                .iter()
                .filter(|t| !matches!(t, Tree::Nested(..) | Tree::Scope(..)))
                .count()
        };
        if !other.select.order.is_empty()
            || root_filter_count(&self.filters) > 1
            || root_filter_count(&other.filters) > 1
        {
            return Err(Error::Compatibility(
                "queryset or requires one filter per side and filters only on the right".into(),
            ));
        }
        // Nested filters and scope values stay at the root: they are not part of the OR group.
        let (mut nested, roots): (Vec<_>, Vec<_>) = self
            .filters
            .into_iter()
            .partition(|t| matches!(t, Tree::Nested(..) | Tree::Scope(..)));
        let mut right = Vec::new();
        for tree in other.filters {
            match tree {
                Tree::Nested(..) => {}
                Tree::Scope(name, value) => {
                    if nested
                        .iter()
                        .any(|t| matches!(t, Tree::Scope(n, _) if *n == name))
                    {
                        return Err(Error::Compatibility(format!(
                            "queryset or: scope {name} is set on both sides"
                        )));
                    }
                    nested.push(Tree::Scope(name, value));
                }
                tree => right.push(tree),
            }
        }
        nested.push(Tree::Or(vec![Tree::And(roots), Tree::And(right)]));
        self.filters = nested;
        Ok(self)
    }
    pub async fn in_bulk<V: Codec + Eq + std::hash::Hash + Clone>(
        self,
        key: Unique<M::Source, V>,
        ids: impl IntoIterator<Item = V>,
        db: &Database<M::Schema>,
    ) -> Result<std::collections::HashMap<V, M>>
    where
        R: Presence,
    {
        let ids: Vec<V> = ids.into_iter().collect();
        {
            if key.fields.len() != 1 {
                return Err(Error::Compatibility(
                    "in_bulk requires a scalar unique field".into(),
                ));
            }
            let mut out = std::collections::HashMap::new();
            // Account for parameters already bound by the base query.
            let (_, base) = self.clone().erase()?;
            let available = db
                .driver
                .dialect()
                .capabilities()
                .max_params
                .checked_sub(base.0.len())
                .filter(|n| *n > 0)
                .ok_or_else(|| {
                    Error::Engine(orm_engine::Error::query(
                        "in_bulk base filter exhausts the parameter budget",
                    ))
                })?;
            for chunk in ids.chunks(available.min(1000)) {
                let p = Column::<M::Source, V>::generated(key.fields[0], &[])
                    .in_values(chunk.iter().cloned());
                let query = self.clone().filter(p);
                let (mut q, params) = query.erase()?;
                if !M::fields().iter().any(|f| f == key.fields[0]) {
                    q.model_helpers.push(key.fields[0].into());
                }
                let selected = run_select(db, q, params).await?;
                for data in crate::row::snapshot(&db.schema, &selected)? {
                    let key_value = V::decode(crate::row::cell(data.key(key.fields[0])?)?)?;
                    out.insert(key_value, M::decode(&DecodedRow { data: &data })?);
                }
            }
            if out.is_empty() {
                R::collection::<M>(vec![])?;
            }
            Ok(out)
        }
    }
}
pub struct Bound<'a, M: Model, R = Optional, L = Unsliced> {
    query: Query<M, R, L>,
    db: &'a Database<M::Schema>,
}
impl<'a, M: Model, R: Presence, L> Bound<'a, M, R, L> {
    pub async fn all(self) -> Result<Vec<M>> {
        let (q, p) = self.query.erase()?;
        let rows = run_select(self.db, q, p).await?;
        R::collection(decode(self.db, &rows)?)
    }
    pub async fn first(self) -> Result<R::Single<M>> {
        self.one(false, false).await
    }
    pub async fn last(self) -> Result<R::Single<M>> {
        self.one(true, false).await
    }
    pub async fn get<V: KeyValues>(
        mut self,
        key: Unique<M::Source, V>,
        value: V::Input,
    ) -> Result<R::Single<M>> {
        self.query = self.query.filter(key.predicate(value));
        self.one(false, true).await
    }
    async fn one(self, last: bool, get: bool) -> Result<R::Single<M>> {
        let (mut q, p) = self.query.erase()?;
        if q.order.is_empty() {
            let m = self
                .db
                .schema
                .models
                .iter()
                .find(|m| m.ir.name == M::NAME)
                .ok_or_else(|| Error::Compatibility("unknown model".into()))?;
            q.order = orm_engine::plan::effective_order(&self.db.schema, &q)?;
            if q.order.is_empty() {
                for &pos in &m.key {
                    q.order.push(ir::Order {
                        expr: Expr::Col {
                            path: vec![],
                            name: m.fields()[pos].name.clone(),
                        },
                        desc: false,
                        nulls: None,
                    });
                }
            }
        }
        if last {
            for o in &mut q.order {
                o.desc = !o.desc;
                o.nulls = o.nulls.map(|n| match n {
                    ir::Nulls::First => ir::Nulls::Last,
                    ir::Nulls::Last => ir::Nulls::First,
                });
            }
        }
        // Preserve stricter slices; get inspects up to two rows, not one.
        let cap = if get { 2 } else { 1 };
        q.limit = Some(ir::Count::Value(if get {
            cap
        } else {
            match q.limit {
                Some(ir::Count::Value(n)) => n.min(cap),
                _ => cap,
            }
        }));
        let rows = run_select(self.db, q, p).await?;
        if get && rows.rows.len() > 1 {
            return Err(Error::MultipleRows);
        }
        R::single(decode(self.db, &rows)?.into_iter().next())
    }
    pub async fn count(self) -> Result<i64> {
        let (q, p) = self.query.erase()?;
        match execute(self.db, Operation::Count(q), p).await? {
            orm_engine::exec::Outcome::Count(n) => Ok(n),
            _ => Err(Error::Decode("unexpected count result".into())),
        }
    }
    pub async fn exists(self) -> Result<bool> {
        let (q, p) = self.query.erase()?;
        match execute(self.db, Operation::Exists(q), p).await? {
            orm_engine::exec::Outcome::Exists(v) => Ok(v),
            _ => Err(Error::Decode("unexpected exists result".into())),
        }
    }
    pub fn statement(self) -> Result<Statement<'a, M, R>> {
        let (q, p) = self.query.erase()?;
        let plan = plan(self.db, Operation::Select(q), &p)?;
        Ok(Statement {
            plan,
            db: self.db,
            _type: PhantomData,
        })
    }
}
impl<'a, M: Model, R: Presence, L: Send + 'a> IntoFuture for Bound<'a, M, R, L> {
    type Output = Result<Vec<M>>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send + 'a>>;
    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.all())
    }
}
/// A planned statement executes once by consuming itself.
pub struct Statement<'a, M: Model, R = Optional> {
    plan: orm_engine::plan::Plan,
    db: &'a Database<M::Schema>,
    _type: PhantomData<(M, R)>,
}
impl<M: Model, R: Presence> Statement<'_, M, R> {
    pub fn sql(&self) -> String {
        orm_engine::exec::sql(
            orm_core::dialect::Target::new(self.db.driver.dialect()),
            &self.plan,
        )
    }
    pub fn inspect(&self) -> orm_engine::inspect::Inspection {
        orm_engine::inspect::Inspection::plans(std::slice::from_ref(&self.plan), false)
    }
    pub async fn execute(self) -> Result<Vec<M>> {
        let target = orm_core::dialect::Target::new(self.db.driver.dialect());
        match orm_engine::exec::run(self.db.driver.as_ref(), target, self.plan).await? {
            orm_engine::exec::Outcome::Select(rows) => R::collection(decode(self.db, &rows)?),
            _ => Err(Error::Decode("unexpected read result".into())),
        }
    }
}
pub(crate) fn plan<S: SchemaSpec>(
    db: &Database<S>,
    op: Operation,
    p: &Values,
) -> Result<orm_engine::plan::Plan> {
    if let Operation::Select(q) | Operation::Count(q) | Operation::Exists(q) = &op {
        validate_views(&db.schema, q)?;
        for count in [q.limit, q.offset].into_iter().flatten() {
            if let ir::Count::Value(n) = count {
                if n > i64::MAX as u64 {
                    return Err(Error::Engine(orm_engine::Error::value(
                        "row count exceeds signed 64-bit range",
                    )));
                }
            }
        }
    }
    Ok(orm_engine::plan::Planner::plan(
        &db.schema,
        orm_core::dialect::Target::new(db.driver.dialect()),
        &op,
        p,
    )?)
}
// Substantial async execution is erased at this boundary.
async fn execute<S: SchemaSpec>(
    db: &Database<S>,
    op: Operation,
    p: Values,
) -> Result<orm_engine::exec::Outcome> {
    let plan = plan(db, op, &p)?;
    Ok(orm_engine::exec::run(
        db.driver.as_ref(),
        orm_core::dialect::Target::new(db.driver.dialect()),
        plan,
    )
    .await?)
}
pub(crate) async fn run_select<S: SchemaSpec>(
    db: &Database<S>,
    q: ir::Select,
    p: Values,
) -> Result<Box<orm_engine::exec::Selected>> {
    match execute(db, Operation::Select(q), p).await? {
        orm_engine::exec::Outcome::Select(s) => Ok(s),
        _ => Err(Error::Decode("unexpected read result".into())),
    }
}
fn decode<M: Model>(db: &Database<M::Schema>, s: &orm_engine::exec::Selected) -> Result<Vec<M>> {
    crate::row::snapshot(&db.schema, s)?
        .iter()
        .map(|data| M::decode(&DecodedRow { data }))
        .collect()
}

pub(crate) fn erase_filters(select: &mut ir::Select, filters: Vec<Tree>, erase: &mut Erase) {
    for tree in filters {
        match tree {
            Tree::Nested(attr, children) => {
                if let Some(node) = select
                    .prefetch
                    .iter_mut()
                    .find(|p| p.attr.as_deref().unwrap_or(&p.relation) == attr)
                {
                    erase_filters(&mut node.query, children, erase);
                }
            }
            Tree::Scope(name, _) if !erase.scopes.is_empty() => {
                erase.error.get_or_insert_with(|| {
                    format!("scope {name} on a subquery: set scopes on the outer query")
                });
            }
            Tree::Scope(name, value) => {
                select.scope.insert(name, erase.values.0.len());
                erase.values.0.push(value);
            }
            tree => select.filters.push(tree.erase(erase)),
        }
    }
}

pub(crate) fn validate_views(schema: &Schema, q: &ir::Select) -> Result<()> {
    let model = schema
        .models
        .iter()
        .find(|m| m.ir.name == q.model)
        .ok_or_else(|| Error::Compatibility("unknown relation root".into()))?;
    let mut labels = std::collections::BTreeSet::new();
    for p in &q.prefetch {
        if let Some(label) = &p.attr {
            if model.fields().iter().any(|f| f.name == *label)
                || model.ir.relations.iter().any(|r| r.name == *label)
                || !labels.insert(label)
                || label.starts_with("__orm_")
            {
                return Err(Error::Compatibility(format!(
                    "named relation view collision: {label}"
                )));
            }
        }
        let (_, target) = model.relation(&p.relation).map_err(Error::Compatibility)?;
        if schema.models[target].ir.name != p.query.model {
            return Err(Error::Compatibility("wrong prefetch target".into()));
        }
        validate_views(schema, &p.query)?;
    }
    Ok(())
}
