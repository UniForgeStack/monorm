use crate::row::CacheData;
use crate::*;
use monorm_core::ir;
use std::marker::PhantomData;

/// Queryable does not imply a cache. No cached() method exists on this state.
pub struct RelatedSet<T> {
    key: Vec<BindValue>,
    to: Vec<String>,
    _target: PhantomData<fn() -> T>,
}
impl<T> RelatedSet<T> {
    fn read(row: &DecodedRow<'_>, from: &[&str], to: &[&str]) -> Result<Self> {
        Ok(Self {
            key: from
                .iter()
                .map(|name| row.data.key(name).cloned())
                .collect::<Result<_>>()?,
            to: to.iter().map(|name| (*name).into()).collect(),
            _target: PhantomData,
        })
    }
}
impl<T: Model> RelatedSet<T> {
    pub fn query(&self) -> Query<T> {
        let query = Query::<T>::new().initialized();
        let tree = if self.key.iter().any(|key| key == &key.as_null()) {
            Tree::Expr(ir::Expr::Const { value: false })
        } else {
            Tree::And(
                self.to
                    .iter()
                    .zip(&self.key)
                    .map(|(name, key)| {
                        Tree::Compare(
                            ir::Expr::Col {
                                path: vec![],
                                name: name.clone(),
                                sub: vec![],
                                alias: vec![],
                            },
                            ir::CmpOp::Eq,
                            key.clone(),
                        )
                    })
                    .collect(),
            )
        };
        Query::from_parts(query.select, vec![tree])
    }
}
pub struct LoadedMany<T> {
    set: RelatedSet<T>,
    rows: Vec<T>,
}
impl<T> LoadedMany<T> {
    pub fn cached(&self) -> &[T] {
        &self.rows
    }
}
impl<T: Model> LoadedMany<T> {
    pub fn query(&self) -> Query<T> {
        self.set.query()
    }
}
pub struct LoadedOne<T> {
    set: RelatedSet<T>,
    row: Option<Box<T>>,
}
impl<T> LoadedOne<T> {
    pub fn cached(&self) -> Option<&T> {
        self.row.as_deref()
    }
}
impl<T: Model> LoadedOne<T> {
    pub fn query(&self) -> Query<T> {
        self.set.query()
    }
}
pub struct NotCached;
pub struct Cached<T>(PhantomData<fn() -> T>);
pub struct BareCached<T>(PhantomData<fn() -> T>);
pub enum NormalTarget {}
pub enum SelectedTarget {}
pub struct DefaultCached<T>(PhantomData<fn() -> T>);
impl crate::sealed::Sealed for NotCached {}
impl<T> crate::sealed::Sealed for Cached<T> {}
impl<T> crate::sealed::Sealed for BareCached<T> {}
impl<T> crate::sealed::Sealed for DefaultCached<T> {}

/// Closed cache states. To-one caches are always optional because target policy
/// can hide an otherwise required physical reference.
pub trait RelationState<T, C>: crate::sealed::Sealed {
    type Value: Send + Sync;
    const DEFAULT: bool = false;
    const LOADED: bool = false;
    fn read(row: &DecodedRow<'_>, name: &str, from: &[&str], to: &[&str]) -> Result<Self::Value>;
    fn configure(_: &mut SelectionPlan, _: &str) {}
    #[doc(hidden)]
    fn merge_shape(_: &mut SelectionPlan, _: &str) {}
}
impl<T, C, P> RelationState<T, C> for DefaultCached<P>
where
    Cached<P>: RelationState<T, C>,
{
    type Value = <Cached<P> as RelationState<T, C>>::Value;
    const DEFAULT: bool = true;
    const LOADED: bool = true;
    fn read(row: &DecodedRow<'_>, name: &str, from: &[&str], to: &[&str]) -> Result<Self::Value> {
        Cached::<P>::read(row, name, from, to)
    }
    fn configure(q: &mut SelectionPlan, name: &str) {
        Cached::<P>::configure(q, name)
    }
}
impl<T, C, P> RelationState<T, C> for BareCached<P>
where
    Cached<P>: RelationState<T, C>,
{
    type Value = <Cached<P> as RelationState<T, C>>::Value;
    const LOADED: bool = true;
    fn read(row: &DecodedRow<'_>, name: &str, from: &[&str], to: &[&str]) -> Result<Self::Value> {
        Cached::<P>::read(row, name, from, to)
    }
    fn configure(q: &mut SelectionPlan, name: &str) {
        Cached::<P>::configure(q, name)
    }
    fn merge_shape(q: &mut SelectionPlan, name: &str) {
        Cached::<P>::merge_shape(q, name)
    }
}
pub trait StripDefault<T, C>: RelationState<T, C> {
    type Output: RelationState<T, C> + 'static;
}
impl<T, C> StripDefault<T, C> for NotCached {
    type Output = Self;
}
impl<T, C, P: 'static> StripDefault<T, C> for Cached<P>
where
    Self: RelationState<T, C>,
{
    type Output = Self;
}
impl<T, C, P: 'static> StripDefault<T, C> for BareCached<P>
where
    Self: RelationState<T, C>,
{
    type Output = Self;
}
impl<T, C, P> StripDefault<T, C> for DefaultCached<P>
where
    Self: RelationState<T, C>,
{
    type Output = NotCached;
}
impl<T, C> RelationState<T, C> for NotCached {
    type Value = RelatedSet<T>;
    fn read(row: &DecodedRow<'_>, _: &str, from: &[&str], to: &[&str]) -> Result<Self::Value> {
        RelatedSet::read(row, from, to)
    }
}
impl<T: Model, P: Model<Schema = T::Schema, Source = T::Source>> RelationState<T, Many>
    for Cached<P>
{
    type Value = LoadedMany<P>;
    const LOADED: bool = true;
    fn read(row: &DecodedRow<'_>, name: &str, from: &[&str], to: &[&str]) -> Result<Self::Value> {
        let Some(CacheData::Many(rows)) = row.data.relations.get(name) else {
            return Err(Error::Unloaded(name.into()));
        };
        Ok(LoadedMany {
            set: RelatedSet::read(row, from, to)?,
            rows: rows
                .iter()
                .map(|data| P::decode(&DecodedRow { data }))
                .collect::<Result<_>>()?,
        })
    }
    fn configure(q: &mut SelectionPlan, name: &str) {
        configure::<P>(q, name)
    }
    fn merge_shape(q: &mut SelectionPlan, name: &str) {
        merge_shape::<P>(q, name)
    }
}
impl<T: Model, P: Model<Schema = T::Schema, Source = T::Source>> RelationState<T, One>
    for Cached<P>
{
    type Value = LoadedOne<P>;
    const LOADED: bool = true;
    fn read(row: &DecodedRow<'_>, name: &str, from: &[&str], to: &[&str]) -> Result<Self::Value> {
        let Some(CacheData::One(target)) = row.data.relations.get(name) else {
            return Err(Error::Unloaded(name.into()));
        };
        Ok(LoadedOne {
            set: RelatedSet::read(row, from, to)?,
            row: target
                .as_deref()
                .map(|data| P::decode(&DecodedRow { data }).map(Box::new))
                .transpose()?,
        })
    }
    fn configure(q: &mut SelectionPlan, name: &str) {
        configure::<P>(q, name)
    }
    fn merge_shape(q: &mut SelectionPlan, name: &str) {
        merge_shape::<P>(q, name)
    }
}
fn merge_shape<P: Model>(q: &mut SelectionPlan, name: &str) {
    if let Some(node) = q
        .prefetch
        .iter_mut()
        .find(|p| p.relation == name && p.attr.is_none())
    {
        P::selection(&mut node.query);
    }
}
fn configure<P: Model>(q: &mut SelectionPlan, name: &str) {
    if q.prefetch
        .iter()
        .any(|p| p.relation == name && p.attr.is_none())
    {
        return;
    }
    q.prefetch.push(ir::Prefetch {
        relation: name.into(),
        attr: None,
        one: false,
        names: Vec::new(),
        query: Query::<P>::new().initialized().select,
    });
}

/// Target selection is finite and branded by the relation's source.
pub struct Relation<R, T, C, K, B = NormalTarget> {
    pub(crate) name: &'static str,
    pub(crate) query: Option<Box<SelectionPlan>>,
    pub(crate) filters: Vec<Tree>,
    pub(crate) _type: PhantomData<RelationTypes<R, T, C, K, B>>,
}
impl<R, T, C, K, B> Relation<R, T, C, K, B> {
    #[doc(hidden)]
    pub const fn generated(name: &'static str) -> Self {
        Self {
            name,
            query: None,
            filters: Vec::new(),
            _type: PhantomData,
        }
    }
}
impl<R, T: Model, C, K, B> Relation<R, T, C, K, B> {
    pub fn load<A>(self, a: A) -> Relation<R, Requested<T, A>, C, K, SelectedTarget>
    where
        A: Descriptor,
        DefaultSelection:
            SelectionBase<T, <A as Descriptor>::Kind> + NextSelection<<A as Descriptor>::Kind>,
        A: Apply<<DefaultSelection as SelectionBase<T, <A as Descriptor>::Kind>>::Base>,
    {
        let query = Query::<T>::new().load(a);
        Relation {
            name: self.name,
            query: Some(Box::new(query.select)),
            filters: query.filters,
            _type: PhantomData,
        }
    }
    pub fn as_prefetch<P: Model<Schema = T::Schema, Source = T::Source>, L, D: TargetSelection>(
        self,
        query: Query<P, Optional, L, D>,
    ) -> Relation<R, P, C, K, D::Target> {
        // Parameters are carried separately by the descriptor; see RelationQuery.
        let query = query.initialized();
        Relation {
            name: self.name,
            query: Some(Box::new(query.select)),
            filters: query.filters,
            _type: PhantomData,
        }
    }
    #[doc(hidden)]
    pub fn install(self, q: &mut SelectionPlan, filters: &mut Vec<Tree>, merge: bool) {
        let mut query = self
            .query
            .map(|q| *q)
            .unwrap_or_else(|| Query::<T>::new().initialized().select);
        query.model = T::NAME.into();
        filters.retain(|t| !matches!(t,Tree::Nested(name,_) if name==self.name));
        filters.push(Tree::Nested(self.name.into(), self.filters));
        if merge {
            if let Some(old) = q
                .prefetch
                .iter()
                .find(|p| p.relation == self.name && p.attr.is_none())
            {
                merge_selection(&mut query, &old.query);
            }
        }
        q.prefetch
            .retain(|p| !(p.relation == self.name && p.attr.is_none()));
        q.prefetch.push(ir::Prefetch {
            relation: self.name.into(),
            attr: None,
            one: false,
            names: Vec::new(),
            query,
        });
    }
}
impl<R, T, C, K, B> Descriptor for Relation<R, T, C, K, B> {
    type Kind = Relations;
}

fn merge_selection(query: &mut SelectionPlan, old: &SelectionPlan) {
    match (&mut query.model_fields, &old.model_fields) {
        (Some(fields), Some(previous)) => {
            for field in previous {
                if !fields.contains(field) {
                    fields.push(field.clone());
                }
            }
        }
        (fields, None) => *fields = None,
        _ => {}
    }
    for child in &old.prefetch {
        if let Some(next) = query
            .prefetch
            .iter_mut()
            .find(|p| p.relation == child.relation && p.attr == child.attr)
        {
            merge_selection(&mut next.query, &child.query);
        } else {
            query.prefetch.push(child.clone());
        }
    }
}

/// Generated relation tokens also expose their source-branded comparison path.
pub trait PathDescriptor {
    type Path;
    fn path() -> Self::Path;
}

impl<R, T, C, K: PathDescriptor, B> Relation<R, T, C, K, B> {
    pub fn path(self) -> K::Path {
        K::path()
    }
}

type RelationTypes<R, T, C, K, B> = fn() -> (R, T, C, K, B);
