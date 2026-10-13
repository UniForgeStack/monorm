//! Configured defaults are schema branded live pools, never ambient transactions.
use crate::*;
use std::{
    any::{Any, TypeId},
    collections::HashMap,
    sync::{OnceLock, RwLock},
};
type Pools = HashMap<TypeId, Box<dyn Any + Send + Sync>>;
static DEFAULTS: OnceLock<RwLock<Pools>> = OnceLock::new();
impl<S: SchemaSpec> Database<S> {
    pub fn set_default(&self) {
        DEFAULTS
            .get_or_init(Default::default)
            .write()
            .expect("default pool lock poisoned")
            .insert(TypeId::of::<S>(), Box::new(self.clone()));
    }
    pub fn clear_default() {
        DEFAULTS
            .get_or_init(Default::default)
            .write()
            .expect("default pool lock poisoned")
            .remove(&TypeId::of::<S>());
    }
    fn configured() -> Result<Self> {
        DEFAULTS
            .get_or_init(Default::default)
            .read()
            .map_err(|_| Error::Compatibility("default pool lock poisoned".into()))?
            .get(&TypeId::of::<S>())
            .and_then(|pool| pool.downcast_ref::<Self>())
            .cloned()
            .ok_or_else(|| {
                Error::Compatibility(
                    "no default database configured for this generated schema".into(),
                )
            })
    }
}
impl<M: Model, R: Presence, L, D> Query<M, R, L, D> {
    pub async fn first(self) -> Result<R::Single<M>> {
        let db = Database::<M::Schema>::configured()?;
        self.using(&db).first().await
    }
    pub async fn last(self) -> Result<R::Single<M>> {
        let db = Database::<M::Schema>::configured()?;
        self.using(&db).last().await
    }
    pub async fn all(self) -> Result<Vec<M>> {
        let db = Database::<M::Schema>::configured()?;
        self.using(&db).all().await
    }
    pub async fn count(self) -> Result<i64> {
        let db = Database::<M::Schema>::configured()?;
        self.using(&db).count().await
    }
    pub async fn exists(self) -> Result<bool> {
        let db = Database::<M::Schema>::configured()?;
        self.using(&db).exists().await
    }
    pub async fn get<V: KeyValues>(
        self,
        key: Unique<M::Source, V>,
        value: V::Input,
    ) -> Result<R::Single<M>> {
        let db = Database::<M::Schema>::configured()?;
        self.using(&db).get(key, value).await
    }
}
impl<M: Model> QueryRoot<M> {
    pub async fn first(&self) -> Result<Option<M>> {
        self.query().first().await
    }
    pub async fn last(&self) -> Result<Option<M>> {
        self.query().last().await
    }
    pub async fn all(&self) -> Result<Vec<M>> {
        self.query().all().await
    }
    pub async fn count(&self) -> Result<i64> {
        self.query().count().await
    }
    pub async fn exists(&self) -> Result<bool> {
        self.query().exists().await
    }
    pub async fn get<V: KeyValues>(
        &self,
        key: Unique<M::Source, V>,
        value: V::Input,
    ) -> Result<Option<M>> {
        self.query().get(key, value).await
    }
}
