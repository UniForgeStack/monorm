//! Typed projection results over the ordinary shared SELECT planner.
use crate::{
    query::{plan, run_select},
    value::Values,
    *,
};
use orm_core::ir::{self, Operation};
use std::{
    future::{Future, IntoFuture},
    marker::PhantomData,
    pin::Pin,
};

/// An item in a projection. To-many paths deliberately do not implement this.
pub trait ProjectionItem<S>: Send + Sync + 'static {
    type Value: Send + 'static;
    #[doc(hidden)]
    fn item(&self, erase: &mut Erase) -> ir::SelectItem;
    #[doc(hidden)]
    fn read(
        &self,
        schema: &orm_core::schema::Schema,
        rows: &orm_engine::exec::Selected,
        row: usize,
        col: usize,
    ) -> Result<Self::Value>;
}
impl<S: Send + Sync + 'static, T: Codec + Send + Sync + 'static, C: ProjectionCardinality<T>>
    ProjectionItem<S> for Column<S, T, C>
{
    type Value = C::Value;
    fn item(&self, _erase: &mut Erase) -> ir::SelectItem {
        ir::SelectItem::Expr {
            expr: self.expr(),
            name: None,
        }
    }
    fn read(
        &self,
        _schema: &orm_core::schema::Schema,
        rows: &orm_engine::exec::Selected,
        row: usize,
        col: usize,
    ) -> Result<C::Value> {
        let col = column_offset(rows, col);
        C::Value::decode(rows.rows.cell(row, col, rows.plan.types[col])?)
    }
}
/// One or more typed items. A single item still produces a one-element tuple.
pub trait Projection<S>: Send + Sync + 'static {
    type Row: Send + 'static;
    #[doc(hidden)]
    fn items(&self, erase: &mut Erase) -> Vec<ir::SelectItem>;
    #[doc(hidden)]
    fn read(
        &self,
        schema: &orm_core::schema::Schema,
        rows: &orm_engine::exec::Selected,
        row: usize,
    ) -> Result<Self::Row>;
}
impl<S: Send + Sync + 'static, T: Codec + Send + Sync + 'static, C: ProjectionCardinality<T>>
    Projection<S> for Column<S, T, C>
{
    type Row = (C::Value,);
    fn items(&self, erase: &mut Erase) -> Vec<ir::SelectItem> {
        vec![self.item(erase)]
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
macro_rules! tuples {
    ($($t:ident:$i:tt),+) => {
        impl<S, $($t:ProjectionItem<S>),+> Projection<S> for ($($t,)+) {
            type Row=($($t::Value,)+);
            fn items(&self,erase:&mut Erase)->Vec<ir::SelectItem> { vec![$(self.$i.item(erase)),+] }
            fn read(&self,schema:&orm_core::schema::Schema,rows:&orm_engine::exec::Selected,row:usize)->Result<Self::Row> { Ok(($(self.$i.read(schema,rows,row,$i)?,)+)) }
        }
    };
}
tuples!(A:0);
tuples!(A:0,B:1);
tuples!(A:0,B:1,C:2);
tuples!(A:0,B:1,C:2,D:3);
tuples!(A:0,B:1,C:2,D:3,E:4);
tuples!(A:0,B:1,C:2,D:3,E:4,F:5);
tuples!(A:0,B:1,C:2,D:3,E:4,F:5,G:6);
tuples!(A:0,B:1,C:2,D:3,E:4,F:5,G:6,H:7);

/// A single column is the only projection supporting scalar terminals.
pub trait ScalarProjection<S>: Projection<S, Row = (Self::Value,)> {
    type Value;
}
impl<S: Send + Sync + 'static, T: Codec + Send + Sync + 'static, C: ProjectionCardinality<T>>
    ScalarProjection<S> for Column<S, T, C>
{
    type Value = C::Value;
}
impl<S, P: ProjectionItem<S>> ScalarProjection<S> for (P,) {
    type Value = P::Value;
}

/// Projection mapping is explicit and preserves the source query and requiredness.
#[derive(Clone)]
pub struct MappedProjection<P, F> {
    projection: P,
    map: F,
}
impl<S, P: Projection<S>, F, T> Projection<S> for MappedProjection<P, F>
where
    F: Fn(P::Row) -> T + Send + Sync + 'static,
    T: Send + 'static,
{
    type Row = T;
    fn items(&self, erase: &mut Erase) -> Vec<ir::SelectItem> {
        self.projection.items(erase)
    }
    fn read(
        &self,
        schema: &orm_core::schema::Schema,
        rows: &orm_engine::exec::Selected,
        row: usize,
    ) -> Result<T> {
        Ok((self.map)(self.projection.read(schema, rows, row)?))
    }
}
/// Owned lazy projection. Transformations keep the underlying model source.
pub struct ProjectedQuery<M: Model, P, R = Optional, L = Unsliced, D = DefaultSelection> {
    query: Query<M, R, L, D>,
    projection: P,
}
impl<M: Model, P: Clone, R, L, D> Clone for ProjectedQuery<M, P, R, L, D> {
    fn clone(&self) -> Self {
        Self {
            query: self.query.clone(),
            projection: self.projection.clone(),
        }
    }
}
impl<M: Model, R, L, D> Query<M, R, L, D> {
    pub fn select<P: Projection<M::Source>>(self, projection: P) -> ProjectedQuery<M, P, R, L, D> {
        ProjectedQuery {
            query: self,
            projection,
        }
    }
}
impl<M: Model, P: Projection<M::Source>, R, L, D> ProjectedQuery<M, P, R, L, D> {
    pub(crate) fn map_query(self, f: impl FnOnce(Query<M, R, L, D>) -> Query<M, R, L, D>) -> Self {
        Self {
            query: f(self.query),
            projection: self.projection,
        }
    }
    pub(crate) fn trees_mut(&mut self) -> &mut Vec<Tree> {
        &mut self.query.filters
    }
    pub fn scope<T: Codec>(self, name: &str, value: T) -> Self {
        self.map_query(|q| q.scope(name, value))
    }
    pub fn filter(mut self, p: Predicate<M::Source>) -> Self {
        self.query = self.query.filter(p);
        self
    }
    pub fn order_by(mut self, o: Ordering<M::Source>) -> Self {
        self.query = self.query.order_by(o);
        self
    }
    pub fn then_by(mut self, o: Ordering<M::Source>) -> Self {
        self.query = self.query.then_by(o);
        self
    }
    pub fn require(self) -> ProjectedQuery<M, P, Required, L, D> {
        ProjectedQuery {
            query: self.query.require(),
            projection: self.projection,
        }
    }
    pub fn limit(self, n: u64) -> ProjectedQuery<M, P, R, Sliced, D> {
        ProjectedQuery {
            query: self.query.limit(n),
            projection: self.projection,
        }
    }
    pub fn offset(self, n: u64) -> ProjectedQuery<M, P, R, Sliced, D> {
        ProjectedQuery {
            query: self.query.offset(n),
            projection: self.projection,
        }
    }
    pub fn map<F, T>(self, map: F) -> ProjectedQuery<M, MappedProjection<P, F>, R, L, D>
    where
        F: Fn(P::Row) -> T + Send + Sync + 'static,
        T: Send + 'static,
    {
        ProjectedQuery {
            query: self.query,
            projection: MappedProjection {
                projection: self.projection,
                map,
            },
        }
    }
    pub fn using(self, db: &Database<M::Schema>) -> BoundProjection<'_, M, P, R, L, D> {
        BoundProjection { query: self, db }
    }
    fn erase(self) -> Result<(ir::Select, Values, P)> {
        let mut values = Values::default();
        let mut erase = Erase::new(&mut values);
        let mut q = self.query.erase_with(&mut erase);
        q.columns = Some(self.projection.items(&mut erase));
        erase.finish()?;
        Ok((q, values, self.projection))
    }
}
pub struct BoundProjection<'a, M: Model, P, R = Optional, L = Unsliced, D = DefaultSelection> {
    query: ProjectedQuery<M, P, R, L, D>,
    db: &'a Database<M::Schema>,
}
fn decode<S, P: Projection<S>>(
    schema: &orm_core::schema::Schema,
    projection: &P,
    rows: &orm_engine::exec::Selected,
) -> Result<Vec<P::Row>> {
    (0..rows.rows.len())
        .map(|row| projection.read(schema, rows, row))
        .collect()
}
impl<'a, M: Model, P: Projection<M::Source>, R: Presence, L, D> BoundProjection<'a, M, P, R, L, D> {
    pub async fn all(self) -> Result<Vec<P::Row>> {
        let (q, p, projection) = self.query.erase()?;
        R::collection(decode(
            &self.db.schema,
            &projection,
            &*run_select(self.db, q, p).await?,
        )?)
    }
    pub async fn first(self) -> Result<R::Single<P::Row>> {
        let (mut q, p, projection) = self.query.erase()?;
        q.limit = Some(ir::Count::Value(match q.limit {
            Some(ir::Count::Value(n)) => n.min(1),
            _ => 1,
        }));
        let rows = run_select(self.db, q, p).await?;
        R::single(
            decode(&self.db.schema, &projection, &rows)?
                .into_iter()
                .next(),
        )
    }
    pub fn statement(self) -> Result<ProjectionStatement<'a, M, P, R>> {
        let (q, p, projection) = self.query.erase()?;
        Ok(ProjectionStatement {
            plan: plan(self.db, Operation::Select(q), &p)?,
            db: self.db,
            projection,
            _type: PhantomData,
        })
    }
}
impl<M: Model, P: ScalarProjection<M::Source>, R: Presence, L, D>
    BoundProjection<'_, M, P, R, L, D>
{
    /// Absence is the outer Option; a nullable SQL value keeps its inner Option.
    pub async fn scalar(self) -> Result<R::Single<P::Value>> {
        let (mut q, p, projection) = self.query.erase()?;
        q.limit = Some(ir::Count::Value(match q.limit {
            Some(ir::Count::Value(n)) => n.min(1),
            _ => 1,
        }));
        let rows = run_select(self.db, q, p).await?;
        let value = if rows.rows.is_empty() {
            None
        } else {
            Some(projection.read(&self.db.schema, &rows, 0)?.0)
        };
        R::single(value)
    }
    pub async fn scalars(self) -> Result<Vec<P::Value>> {
        Ok(self
            .all()
            .await?
            .into_iter()
            .map(|(value,)| value)
            .collect())
    }
}
impl<'a, M: Model, P: Projection<M::Source>, R: Presence, L: Send + 'a, D: Send + 'a> IntoFuture
    for BoundProjection<'a, M, P, R, L, D>
{
    type Output = Result<Vec<P::Row>>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send + 'a>>;
    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.all())
    }
}
pub struct ProjectionStatement<'a, M: Model, P, R = Optional> {
    plan: orm_engine::plan::Plan,
    db: &'a Database<M::Schema>,
    projection: P,
    _type: PhantomData<R>,
}
impl<M: Model, P: Projection<M::Source>, R: Presence> ProjectionStatement<'_, M, P, R> {
    /// Debug SQL follows the existing engine's literal-rendering semantics.
    pub fn sql(&self) -> String {
        orm_engine::exec::sql(
            orm_core::dialect::Target::new(self.db.driver.dialect()),
            &self.plan,
        )
    }
    pub fn inspect(&self) -> orm_engine::inspect::Inspection {
        orm_engine::inspect::Inspection::plans(std::slice::from_ref(&self.plan), false)
    }
    pub async fn execute(self) -> Result<Vec<P::Row>> {
        match orm_engine::exec::run(
            self.db.driver.as_ref(),
            orm_core::dialect::Target::new(self.db.driver.dialect()),
            self.plan,
        )
        .await?
        {
            orm_engine::exec::Outcome::Select(rows) => {
                R::collection(decode(&self.db.schema, &self.projection, &rows)?)
            }
            _ => Err(Error::Decode("unexpected projection result".into())),
        }
    }
}
/// Define an exportable result with inferred generic field types.
#[macro_export]
macro_rules! named_result {
    ($vis:vis struct $name:ident { $($field:ident),+ $(,)? }) => {
        #[allow(non_camel_case_types)]
        $vis struct $name<$($field),+> {$(pub $field:$field),+}
    };
}
/// Literal labels create ordinary result fields; duplicates are compiler errors.
/// Use `query => path::Result` with a named_result! declaration for exported rows.
#[macro_export]
macro_rules! select_named {
    ($query:expr => $name:path; $($field:ident => $value:expr),+ $(,)?) => {{
        use $name as __OrmNamedResult;
        ($query).select(($($value,)+)).map(|($($field,)+)| __OrmNamedResult { $($field),+ })
    }};
    ($query:expr; $($field:ident => $value:expr),+ $(,)?) => {{
        $crate::named_result! { pub struct ProjectionResult { $($field),+ } }
        $crate::select_named!($query => ProjectionResult; $($field => $value),+)
    }};
}
impl<M: Model, P: Projection<M::Source>, R: Presence, L, D> ProjectedQuery<M, P, R, L, D> {
    pub async fn all(self) -> Result<Vec<P::Row>> {
        let db = Database::<M::Schema>::configured()?;
        self.using(&db).all().await
    }
    pub async fn first(self) -> Result<R::Single<P::Row>> {
        let db = Database::<M::Schema>::configured()?;
        self.using(&db).first().await
    }
}
impl<M: Model, P: ScalarProjection<M::Source>, R: Presence, L, D> ProjectedQuery<M, P, R, L, D> {
    pub async fn scalar(self) -> Result<R::Single<P::Value>> {
        let db = Database::<M::Schema>::configured()?;
        self.using(&db).scalar().await
    }
    pub async fn scalars(self) -> Result<Vec<P::Value>> {
        let db = Database::<M::Schema>::configured()?;
        self.using(&db).scalars().await
    }
}
impl<M: Model, P: Projection<M::Source>, R: Presence, L: Send + 'static, D: Send + 'static>
    IntoFuture for ProjectedQuery<M, P, R, L, D>
{
    type Output = Result<Vec<P::Row>>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send>>;
    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.all())
    }
}

/// Project the root model, retaining its generated associated shape traits.
/// The shared planner rejects relation loading combined with SQL projections.
/// Use a normal model query when loaded relation caches are needed.
pub struct ModelProjection<M>(PhantomData<fn() -> M>);
impl<M> Clone for ModelProjection<M> {
    fn clone(&self) -> Self {
        Self(PhantomData)
    }
}
pub fn model<M: Model>() -> ModelProjection<M> {
    ModelProjection(PhantomData)
}
pub(crate) fn column_offset(rows: &orm_engine::exec::Selected, item: usize) -> usize {
    match &rows.plan.output {
        orm_engine::plan::Output::Rows { items, .. } => {
            items[..item].iter().map(|width| width.unwrap_or(1)).sum()
        }
        _ => item,
    }
}
impl<M: Model> ProjectionItem<M::Source> for ModelProjection<M> {
    type Value = M;
    fn item(&self, _erase: &mut Erase) -> ir::SelectItem {
        ir::SelectItem::Model
    }
    fn read(
        &self,
        schema: &orm_core::schema::Schema,
        rows: &orm_engine::exec::Selected,
        row: usize,
        col: usize,
    ) -> Result<M> {
        let orm_engine::plan::Output::Rows { model, .. } = &rows.plan.output else {
            return Err(Error::Decode("expected projected model".into()));
        };
        let data = crate::row::instance(
            schema,
            *model,
            None,
            rows.rows.as_ref(),
            &rows.plan.types,
            row,
            column_offset(rows, col),
        )?;
        M::decode(&DecodedRow { data: &data })
    }
}

#[doc(hidden)]
#[macro_export]
macro_rules! impl_projection_column {
    ($token:ty,$source:ty,$value:ty,$field:literal) => {
        impl $token {
            pub fn column(self) -> $crate::Column<$source, $value> {
                $crate::Column::field($field)
            }
        }
        impl $crate::ProjectionItem<$source> for $token {
            type Value = $value;
            fn item(&self, erase: &mut $crate::Erase) -> $crate::ProjectionSelectItem {
                $crate::ProjectionItem::item(&$crate::Column::<$source, $value>::field($field), erase)
            }
            fn read(
                &self,
                schema: &$crate::ProjectionSchema,
                rows: &$crate::ProjectionRows,
                row: usize,
                col: usize,
            ) -> $crate::Result<$value> {
                $crate::ProjectionItem::read(
                    &$crate::Column::<$source, $value>::field($field),
                    schema,
                    rows,
                    row,
                    col,
                )
            }
        }
        impl $crate::Projection<$source> for $token {
            type Row = ($value,);
            fn items(&self, erase: &mut $crate::Erase) -> ::std::vec::Vec<$crate::ProjectionSelectItem> {
                ::std::vec![$crate::ProjectionItem::item(self, erase)]
            }
            fn read(
                &self,
                schema: &$crate::ProjectionSchema,
                rows: &$crate::ProjectionRows,
                row: usize,
            ) -> $crate::Result<Self::Row> {
                Ok(($crate::ProjectionItem::read(self, schema, rows, row, 0)?,))
            }
        }
        impl $crate::ScalarProjection<$source> for $token {
            type Value = $value;
        }
    };
}
#[doc(hidden)]
pub use orm_core::{ir::SelectItem as ProjectionSelectItem, schema::Schema as ProjectionSchema};
#[doc(hidden)]
pub use orm_engine::exec::Selected as ProjectionRows;

/// A SQL text function with root identity and nullable result type retained.
pub struct TextExpression<S, T> {
    expr: ir::Expr,
    _type: PhantomData<fn() -> (S, T)>,
}
impl<S, T> Clone for TextExpression<S, T> {
    fn clone(&self) -> Self {
        Self {
            expr: self.expr.clone(),
            _type: PhantomData,
        }
    }
}
fn text_function<S, T>(name: &str, arg: ir::Expr) -> TextExpression<S, T> {
    TextExpression {
        expr: ir::Expr::Func {
            name: name.into(),
            args: vec![arg],
            rel: None,
            distinct: false,
            filter: None,
            order_by: vec![],
        },
        _type: PhantomData,
    }
}
// Private sealed value family prevents pretending a numeric value is SQL text.
mod text_value {
    pub trait Sealed {
        type Length;
    }
    impl Sealed for String {
        type Length = i32;
    }
    impl Sealed for Option<String> {
        type Length = Option<i32>;
    }
}
impl<S, T: text_value::Sealed> Column<S, T, One> {
    pub fn lower(self) -> TextExpression<S, T> {
        text_function("lower", self.expr())
    }
    pub fn upper(self) -> TextExpression<S, T> {
        text_function("upper", self.expr())
    }
    pub fn length(self) -> TextExpression<S, T::Length> {
        text_function("length", self.expr())
    }
}
impl<S, T: text_value::Sealed> TextExpression<S, T> {
    pub fn lower(self) -> Self {
        text_function("lower", self.expr)
    }
    pub fn upper(self) -> Self {
        text_function("upper", self.expr)
    }
    pub fn length(self) -> TextExpression<S, T::Length> {
        text_function("length", self.expr)
    }
}
impl<S: 'static, T: Codec + Send + 'static> ProjectionItem<S> for TextExpression<S, T> {
    type Value = T;
    fn item(&self, _erase: &mut Erase) -> ir::SelectItem {
        ir::SelectItem::Expr {
            expr: self.expr.clone(),
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
impl<S: 'static, T: Codec + Send + 'static> Projection<S> for TextExpression<S, T> {
    type Row = (T,);
    fn items(&self, erase: &mut Erase) -> Vec<ir::SelectItem> {
        vec![self.item(erase)]
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
impl<S: 'static, T: Codec + Send + 'static> ScalarProjection<S> for TextExpression<S, T> {
    type Value = T;
}

/// SQL NULL propagation flattens already nullable stored values.
#[doc(hidden)]
pub trait NullableCodec: Codec {
    type Nullable: Codec + Send + 'static;
}
macro_rules! nullable_codecs {($($ty:ty),+) => {$(impl NullableCodec for $ty {type Nullable=Option<Self>;})+};}
nullable_codecs!(
    i32,
    i64,
    f64,
    bool,
    String,
    chrono::NaiveDate,
    chrono::DateTime<chrono::Utc>,
    uuid::Uuid,
    serde_json::Value,
    bigdecimal::BigDecimal
);
impl<T: Codec + Send + 'static> NullableCodec for Option<T> {
    type Nullable = Self;
}
impl<T: Codec + Send + 'static> NullableCodec for Vec<T> {
    type Nullable = Option<Self>;
}
impl crate::sealed::Sealed for One {}
impl crate::sealed::Sealed for OptionalOne {}
#[doc(hidden)]
pub trait ProjectionCardinality<T: Codec>: crate::sealed::Sealed + Send + Sync + 'static {
    type Value: Codec + Send + 'static;
}
impl<T: Codec + Send + 'static> ProjectionCardinality<T> for One {
    type Value = T;
}
impl<T: NullableCodec> ProjectionCardinality<T> for OptionalOne {
    type Value = T::Nullable;
}
impl<S, T> Column<S, T, OptionalOne>
where
    T: NullableCodec,
    T::Nullable: text_value::Sealed,
{
    pub fn lower(self) -> TextExpression<S, T::Nullable> {
        text_function("lower", self.expr())
    }
    pub fn upper(self) -> TextExpression<S, T::Nullable> {
        text_function("upper", self.expr())
    }
    pub fn length(self) -> TextExpression<S, <T::Nullable as text_value::Sealed>::Length> {
        text_function("length", self.expr())
    }
}
