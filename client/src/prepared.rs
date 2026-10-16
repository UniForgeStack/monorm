//! Reusable read templates. Every binding plans against the current database.
use crate::*;

/// Owns a reusable lazy template, rather than a cached result or an executed plan.
/// Parameter-free templates are currently supported; typed placeholders are pending.
#[derive(Clone)]
pub struct Prepared<Q> {
    query: Q,
}
impl<M: Model, R, L, D> Query<M, R, L, D> {
    pub fn prepare(self) -> Prepared<Self> {
        Prepared { query: self }
    }
}
impl<M: Model, P: Projection<M::Source>, R, L, D> ProjectedQuery<M, P, R, L, D> {
    pub fn prepare(self) -> Prepared<Self> {
        Prepared { query: self }
    }
}
impl<M: Model, R, L, D> Prepared<Query<M, R, L, D>> {
    pub fn using<'db>(&self, db: &'db Database<M::Schema>) -> Bound<'db, M, R, L> {
        self.query.clone().using(db)
    }
    pub fn require(self) -> Prepared<Query<M, Required, L, D>> {
        Prepared {
            query: self.query.require(),
        }
    }
    pub fn filter(self, predicate: Predicate<M::Source>) -> Self {
        Self {
            query: self.query.filter(predicate),
        }
    }
}
impl<M: Model, P: Projection<M::Source> + Clone, R, L, D> Prepared<ProjectedQuery<M, P, R, L, D>> {
    pub fn using<'db>(&self, db: &'db Database<M::Schema>) -> BoundProjection<'db, M, P, R, L, D> {
        self.query.clone().using(db)
    }
    pub fn require(self) -> Prepared<ProjectedQuery<M, P, Required, L, D>> {
        Prepared {
            query: self.query.require(),
        }
    }
    pub fn filter(self, predicate: Predicate<M::Source>) -> Self {
        Self {
            query: self.query.filter(predicate),
        }
    }
}
