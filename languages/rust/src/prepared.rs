//! Reusable read templates. Every binding plans against the current database.
use crate::placeholder::{check_slots, substitute};
use crate::*;

/// Owns a reusable lazy template, rather than a cached result or an executed plan.
/// `K` lists the placeholders the template uses; `bind` needs a pack that has all of them.
pub struct Prepared<Q, K = Nil> {
    query: Q,
    _slots: PhantomData<K>,
}
impl<Q: Clone, K> Clone for Prepared<Q, K> {
    fn clone(&self) -> Self {
        Self::new(self.query.clone())
    }
}
impl<Q, K> Prepared<Q, K> {
    fn new(query: Q) -> Self {
        Self {
            query,
            _slots: PhantomData,
        }
    }
}
impl<M: Model, R, L, D> Query<M, R, L, D> {
    pub fn prepare(self) -> Prepared<Self> {
        Prepared::new(self)
    }
}
impl<M: Model, P: Projection<M::Source>, R, L, D> ProjectedQuery<M, P, R, L, D> {
    pub fn prepare(self) -> Prepared<Self> {
        Prepared::new(self)
    }
}
impl<M: Model, R, L, D, K> Prepared<Query<M, R, L, D>, K> {
    pub fn require(self) -> Prepared<Query<M, Required, L, D>, K> {
        Prepared::new(self.query.require())
    }
    pub fn filter<K2>(
        self,
        p: Predicate<M::Source, K2>,
    ) -> Prepared<Query<M, R, L, D>, <K as Join<K2>>::Out>
    where
        K: Join<K2>,
    {
        Prepared::new(self.query.push_tree(p.tree))
    }
    /// Sets a scope value for every execution; `bind(..)?.scope(..)` replaces it for one.
    pub fn scope<T: Codec>(self, name: &str, value: T) -> Self {
        Self::new(self.query.scope(name, value))
    }
    /// Substitutes the pack into a copy of the template. The copy is an ordinary lazy query.
    pub fn bind<A: Pack + Provides<K>>(&self, args: &A) -> Result<Query<M, R, L, D>> {
        let mut query = self.query.clone();
        check_slots(&query.filters, &mut Vec::new())?;
        substitute(&mut query.filters, &args.values())?;
        Ok(query)
    }
}
impl<M: Model, R, L, D> Prepared<Query<M, R, L, D>, Nil> {
    pub fn using<'db>(&self, db: &'db Database<M::Schema>) -> Bound<'db, M, R, L> {
        self.query.clone().using(db)
    }
}
impl<M: Model, P: Projection<M::Source> + Clone, R, L, D, K>
    Prepared<ProjectedQuery<M, P, R, L, D>, K>
{
    pub fn require(self) -> Prepared<ProjectedQuery<M, P, Required, L, D>, K> {
        Prepared::new(self.query.require())
    }
    #[allow(clippy::type_complexity)]
    pub fn filter<K2>(
        self,
        p: Predicate<M::Source, K2>,
    ) -> Prepared<ProjectedQuery<M, P, R, L, D>, <K as Join<K2>>::Out>
    where
        K: Join<K2>,
    {
        Prepared::new(self.query.map_query(|q| q.push_tree(p.tree)))
    }
    pub fn scope<T: Codec>(self, name: &str, value: T) -> Self {
        Self::new(self.query.map_query(|q| q.scope(name, value)))
    }
    pub fn bind<A: Pack + Provides<K>>(&self, args: &A) -> Result<ProjectedQuery<M, P, R, L, D>> {
        let mut query = self.query.clone();
        let trees = query.trees_mut();
        check_slots(trees, &mut Vec::new())?;
        substitute(trees, &args.values())?;
        Ok(query)
    }
}
impl<M: Model, P: Projection<M::Source> + Clone, R, L, D>
    Prepared<ProjectedQuery<M, P, R, L, D>, Nil>
{
    pub fn using<'db>(&self, db: &'db Database<M::Schema>) -> BoundProjection<'db, M, P, R, L, D> {
        self.query.clone().using(db)
    }
}
