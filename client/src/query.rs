use crate::{value::Values, *};
use orm_core::ir::{self, Expr, Operation};
use std::{
    future::{Future, IntoFuture},
    pin::Pin,
};

pub enum One {}
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
    fn expr(&self) -> Expr {
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
    pub fn eq_field(self, other: Column<M, T, C>) -> Predicate<M> {
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
#[derive(Clone)]
enum Tree {
    Expr(Expr),
    Compare(Expr, ir::CmpOp, sea_query::Value),
    In(Expr, Vec<sea_query::Value>),
    And(Vec<Tree>),
    Or(Vec<Tree>),
    Not(Box<Tree>),
}
impl Tree {
    fn erase(self, params: &mut Values) -> Expr {
        let mut bind = |v| {
            let i = params.0.len();
            params.0.push(v);
            Expr::Param { i }
        };
        match self {
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
                items: items.into_iter().map(|t| t.erase(params)).collect(),
            },
            Self::Or(items) => Expr::Or {
                items: items.into_iter().map(|t| t.erase(params)).collect(),
            },
            Self::Not(t) => Expr::Not {
                item: Box::new(t.erase(params)),
            },
        }
    }
}
pub struct Predicate<M> {
    tree: Tree,
    _model: PhantomData<M>,
}
impl<M> Clone for Predicate<M> {
    fn clone(&self) -> Self {
        Self {
            tree: self.tree.clone(),
            _model: PhantomData,
        }
    }
}
impl<M> Predicate<M> {
    pub fn and(self, other: Self) -> Self {
        Self {
            tree: Tree::And(vec![self.tree, other.tree]),
            _model: PhantomData,
        }
    }
    pub fn or(self, other: Self) -> Self {
        Self {
            tree: Tree::Or(vec![self.tree, other.tree]),
            _model: PhantomData,
        }
    }
}
impl<M> std::ops::Not for Predicate<M> {
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
    fields: &'static [&'static str],
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
pub struct Query<M: Model, R = Optional, L = Unsliced> {
    select: ir::Select,
    filters: Vec<Tree>,
    _type: PhantomData<(M, R, L)>,
}
impl<M: Model, R, L> Clone for Query<M, R, L> {
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
impl<M: Model, R, L> Query<M, R, L> {
    pub fn filter(mut self, p: Predicate<M::Source>) -> Self {
        self.filters.push(p.tree);
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
    pub fn require(self) -> Query<M, Required, L> {
        Query {
            select: self.select,
            filters: self.filters,
            _type: PhantomData,
        }
    }
    pub fn limit(mut self, n: u64) -> Query<M, R, Sliced> {
        self.select.limit = Some(ir::Count::Value(n));
        Query {
            select: self.select,
            filters: self.filters,
            _type: PhantomData,
        }
    }
    pub fn offset(mut self, n: u64) -> Query<M, R, Sliced> {
        self.select.offset = Some(ir::Count::Value(n));
        Query {
            select: self.select,
            filters: self.filters,
            _type: PhantomData,
        }
    }
    pub fn slice(self, start: u64, end: u64) -> Result<Query<M, R, Sliced>> {
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
        Bound { query: self, db }
    }
    fn erase(self) -> (ir::Select, Values) {
        let mut values = Values::default();
        let mut select = self.select;
        // The associated constant is a lazy root, with no allocations at access time.
        select.model = M::NAME.into();
        if select.model_fields.is_none() && select.columns.is_none() {
            select.model_fields = Some(M::fields());
        }
        select.filters = self
            .filters
            .into_iter()
            .map(|t| t.erase(&mut values))
            .collect();
        (select, values)
    }
}
impl<M: Model, R> Query<M, R, Unsliced> {
    pub fn or(mut self, other: Self) -> Result<Self> {
        // The left keeps its order; the right supplies only one grouped filter.
        if !other.select.order.is_empty() || self.filters.len() > 1 || other.filters.len() > 1 {
            return Err(Error::Compatibility(
                "queryset or requires one filter per side and filters only on the right".into(),
            ));
        }
        self.filters = vec![Tree::Or(vec![
            Tree::And(self.filters),
            Tree::And(other.filters),
        ])];
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
            let (_, base) = self.clone().erase();
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
                let (mut q, params) = query.erase();
                if !M::fields().iter().any(|f| f == key.fields[0]) {
                    q.model_helpers.push(key.fields[0].into());
                }
                let selected = run_select(db, q, params).await?;
                for i in 0..selected.rows.len() {
                    let row = witness(db, &selected, i)?;
                    out.insert(row.read::<V>(key.fields[0])?, M::decode(&row)?);
                }
            }
            if out.is_empty() {
                R::collection::<M>(vec![])?;
            }
            Ok(out)
        }
    }
}
impl<M: Model, R: Presence, L> Query<M, R, L> {
    pub async fn all(self) -> Result<Vec<M>> {
        let db = Database::<M::Schema>::configured()?;
        self.using(&db).all().await
    }
    pub async fn first(self) -> Result<R::Single<M>> {
        let db = Database::<M::Schema>::configured()?;
        self.using(&db).first().await
    }
    pub async fn last(self) -> Result<R::Single<M>> {
        let db = Database::<M::Schema>::configured()?;
        self.using(&db).last().await
    }
    pub async fn get<V: KeyValues>(self, key: Unique<M::Source,V>, value: V::Input) -> Result<R::Single<M>> {
        let db = Database::<M::Schema>::configured()?;
        self.using(&db).get(key,value).await
    }
    pub async fn count(self) -> Result<i64> {
        let db = Database::<M::Schema>::configured()?;
        self.using(&db).count().await
    }
    pub async fn exists(self) -> Result<bool> {
        let db = Database::<M::Schema>::configured()?;
        self.using(&db).exists().await
    }
}
impl<M: Model, R: Presence, L: Send + 'static> IntoFuture for Query<M,R,L> {
    type Output = Result<Vec<M>>;
    type IntoFuture = Pin<Box<dyn Future<Output=Self::Output> + Send>>;
    fn into_future(self) -> Self::IntoFuture { Box::pin(self.all()) }
}
pub struct Bound<'a, M: Model, R = Optional, L = Unsliced> {
    query: Query<M, R, L>,
    db: &'a Database<M::Schema>,
}
impl<'a, M: Model, R: Presence, L> Bound<'a, M, R, L> {
    pub async fn all(self) -> Result<Vec<M>> {
        let (q, p) = self.query.erase();
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
        let (mut q, p) = self.query.erase();
        if q.order.is_empty() {
            let m = self
                .db
                .schema
                .models
                .iter()
                .find(|m| m.ir.name == M::NAME)
                .ok_or_else(|| Error::Compatibility("unknown model".into()))?;
            q.order.push(ir::Order {
                expr: Expr::Col {
                    path: vec![],
                    name: m.pk_field().name.clone(),
                },
                desc: false,
                nulls: None,
            });
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
        let (q, p) = self.query.erase();
        match execute(self.db, Operation::Count(q), p).await? {
            orm_engine::exec::Outcome::Count(n) => Ok(n),
            _ => Err(Error::Decode("unexpected count result".into())),
        }
    }
    pub async fn exists(self) -> Result<bool> {
        let (q, p) = self.query.erase();
        match execute(self.db, Operation::Exists(q), p).await? {
            orm_engine::exec::Outcome::Exists(v) => Ok(v),
            _ => Err(Error::Decode("unexpected exists result".into())),
        }
    }
    pub fn statement(self) -> Result<Statement<'a, M, R>> {
        let (q, p) = self.query.erase();
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
fn plan<S: SchemaSpec>(
    db: &Database<S>,
    op: Operation,
    p: &Values,
) -> Result<orm_engine::plan::Plan> {
    if let Operation::Select(q) | Operation::Count(q) | Operation::Exists(q) = &op {
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
async fn run_select<S: SchemaSpec>(
    db: &Database<S>,
    q: ir::Select,
    p: Values,
) -> Result<Box<orm_engine::exec::Selected>> {
    match execute(db, Operation::Select(q), p).await? {
        orm_engine::exec::Outcome::Select(s) => Ok(s),
        _ => Err(Error::Decode("unexpected read result".into())),
    }
}
fn witness<'a, S: SchemaSpec>(
    db: &'a Database<S>,
    s: &'a orm_engine::exec::Selected,
    index: usize,
) -> Result<DecodedRow<'a>> {
    if let orm_engine::plan::Output::Instances { model, .. } = s.plan.output {
        Ok(DecodedRow {
            selected: s,
            schema: &db.schema,
            index,
            model,
        })
    } else {
        Err(Error::Decode("expected model rows".into()))
    }
}
fn decode<M: Model>(db: &Database<M::Schema>, s: &orm_engine::exec::Selected) -> Result<Vec<M>> {
    (0..s.rows.len())
        .map(|i| M::decode(&witness(db, s, i)?))
        .collect()
}
