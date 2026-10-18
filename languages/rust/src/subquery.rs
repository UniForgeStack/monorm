//! Correlated scalar subqueries. A parent handle names the query that encloses a subquery,
//! so every nesting level has its own alias, even when two levels read the same model.
use crate::{
    placeholder::Nil,
    projection::{column_offset, NullableCodec, ProjectionCardinality},
    query::{Erase, Tree},
    *,
};
use orm_core::ir::{self, Expr};
use std::sync::{
    atomic::{AtomicU64, Ordering as AtomicOrdering},
    Arc,
};

/// A predicate that reads only its own query. Only standalone predicates filter an executable query.
pub enum Standalone {}
/// A predicate that reads an enclosing query. Only a subquery accepts it.
pub enum Correlated {}
/// Combines the reach of two predicates: one correlated side makes the result correlated.
pub trait Reach<B> {
    type Out;
}
impl Reach<Standalone> for Standalone {
    type Out = Standalone;
}
impl Reach<Correlated> for Standalone {
    type Out = Correlated;
}
impl<B> Reach<B> for Correlated {
    type Out = Correlated;
}

/// A subquery that provably returns at most one row.
pub enum AtMostOne {}
/// A subquery whose row count is not proven.
pub enum Unbounded {}

/// A column of an enclosing query. The depth is known only when the subquery is erased.
#[doc(hidden)]
#[derive(Clone)]
pub struct OuterRef {
    pub(crate) id: u64,
    pub(crate) path: Vec<String>,
    pub(crate) name: &'static str,
}

type Scope<'a, T> = PhantomData<(&'a (), fn() -> T)>;

/// The enclosing query of a subquery. Its lifetime keeps the handle inside the closure of `scalar`.
pub struct Parent<'a, M: Model> {
    id: u64,
    _scope: Scope<'a, M>,
}
impl<M: Model> Clone for Parent<'_, M> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<M: Model> Copy for Parent<'_, M> {}
impl<'a, M: Model> Parent<'a, M> {
    /// A to-one column of the enclosing query. To-many paths do not compile.
    pub fn col<T: Codec, C: ProjectionCardinality<T>>(
        &self,
        column: Column<M::Source, T, C>,
    ) -> OuterColumn<'a, T> {
        OuterColumn {
            outer: column.outer_ref(self.id),
            _scope: PhantomData,
        }
    }
}

pub struct OuterColumn<'a, T> {
    outer: OuterRef,
    _scope: Scope<'a, T>,
}

impl<S, T: Codec, C: ProjectionCardinality<T>> Column<S, T, C> {
    fn cmp_outer(self, other: OuterColumn<'_, T>, op: ir::CmpOp) -> Predicate<S, Nil, Correlated> {
        Predicate::from_tree(Tree::Outer(self.expr(), op, other.outer))
    }
    pub fn eq_outer(self, other: OuterColumn<'_, T>) -> Predicate<S, Nil, Correlated> {
        self.cmp_outer(other, ir::CmpOp::Eq)
    }
    pub fn ne_outer(self, other: OuterColumn<'_, T>) -> Predicate<S, Nil, Correlated> {
        self.cmp_outer(other, ir::CmpOp::Ne)
    }
    pub fn lt_outer(self, other: OuterColumn<'_, T>) -> Predicate<S, Nil, Correlated> {
        self.cmp_outer(other, ir::CmpOp::Lt)
    }
    pub fn le_outer(self, other: OuterColumn<'_, T>) -> Predicate<S, Nil, Correlated> {
        self.cmp_outer(other, ir::CmpOp::Le)
    }
    pub fn gt_outer(self, other: OuterColumn<'_, T>) -> Predicate<S, Nil, Correlated> {
        self.cmp_outer(other, ir::CmpOp::Gt)
    }
    pub fn ge_outer(self, other: OuterColumn<'_, T>) -> Predicate<S, Nil, Correlated> {
        self.cmp_outer(other, ir::CmpOp::Ge)
    }
}

/// A query that only a scalar expression can embed. It has no terminals, so it never runs alone.
pub struct Subquery<M: Model, Card = Unbounded> {
    query: Query<M>,
    _card: PhantomData<Card>,
}
impl<M: Model> Subquery<M> {
    pub fn new(query: Query<M>) -> Self {
        Self {
            query,
            _card: PhantomData,
        }
    }
}
impl<M: Model, Card> Subquery<M, Card> {
    pub fn filter<X>(mut self, predicate: Predicate<M::Source, Nil, X>) -> Self {
        self.query = self.query.push_tree(predicate.tree);
        self
    }
    pub fn order_by(mut self, order: Ordering<M::Source>) -> Self {
        self.query = self.query.order_by(order);
        self
    }
    pub fn then_by(mut self, order: Ordering<M::Source>) -> Self {
        self.query = self.query.then_by(order);
        self
    }
    pub fn limit_one(mut self) -> Subquery<M, AtMostOne> {
        self.query.select.limit = Some(ir::Count::Value(1));
        Subquery {
            query: self.query,
            _card: PhantomData,
        }
    }
    /// A lookup on a single-field unique key: at most one row by the schema.
    pub fn unique<V: Codec>(
        mut self,
        key: Unique<M::Source, V>,
        outer: OuterColumn<'_, V>,
    ) -> Subquery<M, AtMostOne> {
        let field = Expr::Col {
            path: Vec::new(),
            name: key.fields[0].into(),
        };
        self.query = self
            .query
            .push_tree(Tree::Outer(field, ir::CmpOp::Eq, outer.outer));
        Subquery {
            query: self.query,
            _card: PhantomData,
        }
    }
    /// One column only: `P` is a scalar projection.
    pub fn select<P: ScalarProjection<M::Source>>(self, projection: P) -> SubSelect<M, P, Card> {
        SubSelect {
            query: self.query,
            projection,
            _card: PhantomData,
        }
    }
}
pub struct SubSelect<M: Model, P, Card = Unbounded> {
    query: Query<M>,
    projection: P,
    _card: PhantomData<Card>,
}

type Build = dyn Fn(&mut Erase) -> ir::Select + Send + Sync;
#[doc(hidden)]
pub type BuildSelect = Arc<Build>;
/// `(SELECT one_column ...)` as a projection item. It is `NULL` when the subquery has no row.
pub struct ScalarSubquery<M, T> {
    build: Arc<Build>,
    _type: PhantomData<fn() -> (M, T)>,
}
impl<M, T> Clone for ScalarSubquery<M, T> {
    fn clone(&self) -> Self {
        Self {
            build: self.build.clone(),
            _type: PhantomData,
        }
    }
}

fn next_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, AtomicOrdering::Relaxed)
}
fn embed<M, N, P, Card>(
    id: u64,
    sub: SubSelect<N, P, Card>,
) -> ScalarSubquery<M, <P::Value as NullableCodec>::Nullable>
where
    M: Model,
    N: Model,
    P: ScalarProjection<N::Source>,
    P::Value: NullableCodec,
{
    let SubSelect {
        query, projection, ..
    } = sub;
    ScalarSubquery {
        build: Arc::new(move |erase: &mut Erase| {
            erase.scopes.push(id);
            let mut select = query.clone().erase_with(erase);
            select.columns = Some(projection.items(erase));
            erase.scopes.pop();
            select
        }),
        _type: PhantomData,
    }
}
/// A scalar subquery that provably returns at most one row (`limit_one`, `unique`).
/// Annotate the parent: `scalar(|a: Parent<Author>| ...)`.
pub fn scalar<M, N, P, F>(build: F) -> ScalarSubquery<M, <P::Value as NullableCodec>::Nullable>
where
    M: Model,
    N: Model,
    P: ScalarProjection<N::Source>,
    P::Value: NullableCodec,
    F: for<'a> FnOnce(Parent<'a, M>) -> SubSelect<N, P, AtMostOne>,
{
    let id = next_id();
    embed(
        id,
        build(Parent {
            id,
            _scope: PhantomData,
        }),
    )
}
/// A scalar subquery without a proven row count. The database rejects a second row where it
/// does (PostgreSQL); SQLite reads the first row.
pub fn scalar_checked<M, N, P, Card, F>(
    build: F,
) -> ScalarSubquery<M, <P::Value as NullableCodec>::Nullable>
where
    M: Model,
    N: Model,
    P: ScalarProjection<N::Source>,
    P::Value: NullableCodec,
    F: for<'a> FnOnce(Parent<'a, M>) -> SubSelect<N, P, Card>,
{
    let id = next_id();
    embed(
        id,
        build(Parent {
            id,
            _scope: PhantomData,
        }),
    )
}

impl<M: Model, T: Codec + Send + 'static> ProjectionItem<M::Source> for ScalarSubquery<M, T> {
    type Value = T;
    fn item(&self, erase: &mut Erase) -> ir::SelectItem {
        ir::SelectItem::Expr {
            expr: Expr::Subquery {
                select: Box::new((self.build)(erase)),
            },
            name: None,
        }
    }
    fn read(
        &self,
        _schema: &orm_core::schema::Schema,
        rows: &orm_engine::exec::Selected,
        row: usize,
        col: usize,
    ) -> Result<T> {
        let col = column_offset(rows, col);
        T::decode(rows.rows.cell(row, col, rows.plan.types[col])?)
    }
}
impl<M: Model, T: Codec + Send + 'static> Projection<M::Source> for ScalarSubquery<M, T> {
    type Row = (T,);
    fn items(&self, erase: &mut Erase) -> Vec<ir::SelectItem> {
        vec![ProjectionItem::item(self, erase)]
    }
    fn read(
        &self,
        schema: &orm_core::schema::Schema,
        rows: &orm_engine::exec::Selected,
        row: usize,
    ) -> Result<Self::Row> {
        Ok((ProjectionItem::read(self, schema, rows, row, 0)?,))
    }
}
impl<M: Model, T: Codec + Send + 'static> ScalarProjection<M::Source> for ScalarSubquery<M, T> {
    type Value = T;
}

fn handle<'a, M: Model>(id: u64) -> Parent<'a, M> {
    Parent {
        id,
        _scope: PhantomData,
    }
}
/// `EXISTS (SELECT ...)` over a subquery that may read the enclosing row.
/// Annotate the parent: `exists(|u: Parent<User>| ...)`. Negate it with `!`.
pub fn exists<M, N, Card, F>(build: F) -> Predicate<M::Source>
where
    M: Model,
    N: Model,
    F: for<'a> FnOnce(Parent<'a, M>) -> Subquery<N, Card>,
{
    let id = next_id();
    let query = build(handle(id)).query;
    Predicate::from_tree(Tree::Exists(Arc::new(move |erase: &mut Erase| {
        erase.scopes.push(id);
        let select = query.clone().erase_with(erase);
        erase.scopes.pop();
        select
    })))
}
fn in_tree<M, N, P, Card, T, C, F>(
    column: Column<M::Source, T, C>,
    neg: bool,
    build: F,
) -> Predicate<M::Source>
where
    M: Model,
    N: Model,
    P: ScalarProjection<N::Source>,
    T: Codec,
    C: ProjectionCardinality<T>,
    F: for<'a> FnOnce(Parent<'a, M>) -> SubSelect<N, P, Card>,
{
    let id = next_id();
    let SubSelect {
        query, projection, ..
    } = build(handle(id));
    Predicate::from_tree(Tree::InSub(
        column.expr(),
        neg,
        Arc::new(move |erase: &mut Erase| {
            erase.scopes.push(id);
            let mut select = query.clone().erase_with(erase);
            select.columns = Some(projection.items(erase));
            erase.scopes.pop();
            select
        }),
    ))
}
/// `column IN (SELECT one_column ...)`. The subquery column must have the operand's type, and
/// either side may be nullable.
pub fn in_subquery<M, N, P, Card, T, C, F>(
    column: Column<M::Source, T, C>,
    build: F,
) -> Predicate<M::Source>
where
    M: Model,
    N: Model,
    P: ScalarProjection<N::Source>,
    T: NullableCodec,
    P::Value: NullableCodec<Nullable = T::Nullable>,
    C: ProjectionCardinality<T>,
    F: for<'a> FnOnce(Parent<'a, M>) -> SubSelect<N, P, Card>,
{
    in_tree(column, false, build)
}
/// `column NOT IN (SELECT one_column ...)`; a NULL in the subquery makes the predicate unknown.
pub fn not_in_subquery<M, N, P, Card, T, C, F>(
    column: Column<M::Source, T, C>,
    build: F,
) -> Predicate<M::Source>
where
    M: Model,
    N: Model,
    P: ScalarProjection<N::Source>,
    T: NullableCodec,
    P::Value: NullableCodec<Nullable = T::Nullable>,
    C: ProjectionCardinality<T>,
    F: for<'a> FnOnce(Parent<'a, M>) -> SubSelect<N, P, Card>,
{
    in_tree(column, true, build)
}
