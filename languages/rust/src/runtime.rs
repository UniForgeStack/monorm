use crate::row::CacheData;
use crate::*;
use std::marker::PhantomData;

/// An ordinary transition error keeps the consumed input available for retry.
pub struct TransitionError<T> {
    pub error: Error,
    pub original: Box<T>,
}
impl<T> std::fmt::Debug for TransitionError<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransitionError")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}
impl<T> std::fmt::Display for TransitionError<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}
impl<T> std::error::Error for TransitionError<T> {}

/// Fixed row type for all runtime selections of one generated model. Values and
/// flags stay private; unloaded access is fallible and never performs I/O.
pub struct RuntimeRow<M: Model> {
    data: RowData,
    _model: PhantomData<fn() -> M>,
}
impl<M: Model> serde::Serialize for RuntimeRow<M> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        self.data.serialize(s)
    }
}
impl<M: Model> RuntimeRow<M> {
    pub fn get<T: Codec>(&self, name: &str) -> Result<T> {
        self.data.read(name)
    }
    pub fn is_loaded(&self, name: &str) -> bool {
        self.data.values.contains_key(name) || self.data.relations.contains_key(name)
    }
    pub fn public_json(&self) -> Result<serde_json::Value> {
        self.data.public_json()
    }
    pub fn related<P: Model<Schema = M::Schema>>(
        &self,
        name: &str,
    ) -> Result<Vec<RuntimeRow<P::Normal>>> {
        let cache = self
            .data
            .relations
            .get(name)
            .ok_or_else(|| Error::Unloaded(name.into()))?;
        let rows: Vec<_> = match cache {
            CacheData::One(row) => row.as_deref().into_iter().collect(),
            CacheData::Many(rows) => rows.iter().collect(),
        };
        rows.into_iter()
            .map(|row| {
                if row.model != P::NAME {
                    return Err(Error::Compatibility("wrong runtime relation target".into()));
                }
                Ok(RuntimeRow {
                    data: row.clone(),
                    _model: PhantomData,
                })
            })
            .collect()
    }
    /// Checks public loaded flags, codecs, and nested cache shapes without fetching.
    pub fn try_into_static<P: Model<Schema = M::Schema, Source = M::Source>>(
        self,
    ) -> std::result::Result<P, TransitionError<Self>> {
        match P::decode(&DecodedRow { data: &self.data }) {
            Ok(row) => Ok(row),
            Err(error) => Err(TransitionError {
                error,
                original: Box::new(self),
            }),
        }
    }
    /// Mutable loading is restricted to runtime rows. Failure leaves the row intact.
    pub async fn load_runtime(
        &mut self,
        fields: &[&str],
        db: &Database<M::Schema>,
        reload: bool,
    ) -> Result<()> {
        let query = upgrade_query::<M>();
        let query = query.runtime(fields)?;
        let fetched = fetch_upgrade(&[&self.data], query, db).await?;
        let mut next = self.data.clone();
        next.merge(fetched.into_iter().next().ok_or(Error::NotFound)?, reload);
        self.data = next;
        Ok(())
    }
}
impl<M: Model> std::fmt::Debug for RuntimeRow<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeRow")
            .field("model", &self.data.model)
            .field("loaded", &self.data.public_json())
            .finish()
    }
}
impl<M: Model> Model for RuntimeRow<M> {
    type Schema = M::Schema;
    type Source = M::Source;
    type Normal = M::Normal;
    const NAME: &'static str = M::NAME;
    fn fields() -> Vec<String> {
        vec![]
    }
    fn selection(_: &mut SelectionPlan) {}
    fn data(&self) -> &RowData {
        &self.data
    }
    fn decode(row: &DecodedRow<'_>) -> Result<Self> {
        if row.data.model != M::NAME {
            return Err(Error::Compatibility("wrong runtime model".into()));
        }
        Ok(Self {
            data: row.data.clone(),
            _model: PhantomData,
        })
    }
}

/// Typed loading owns the row until completion. Dropping its future drops that row;
/// cancellation cannot return ownership. These reads promise no write rollback.
pub trait Instance: Model {
    fn into_runtime(self) -> RuntimeRow<Self::Normal> {
        RuntimeRow {
            data: self.data().clone(),
            _model: PhantomData,
        }
    }
    fn public_json(&self) -> Result<serde_json::Value> {
        self.data().public_json()
    }
    fn load<A: Apply<Self> + Send>(
        self,
        a: A,
        db: &Database<Self::Schema>,
    ) -> impl std::future::Future<Output = std::result::Result<A::Output, TransitionError<Self>>> + Send
    {
        async move { self.transition(a, db, false).await }
    }
    fn reload<A: Apply<Self> + Send>(
        self,
        a: A,
        db: &Database<Self::Schema>,
    ) -> impl std::future::Future<Output = std::result::Result<A::Output, TransitionError<Self>>> + Send
    {
        async move { self.transition(a, db, true).await }
    }
    fn transition<A: Apply<Self> + Send>(
        self,
        a: A,
        db: &Database<Self::Schema>,
        reload: bool,
    ) -> impl std::future::Future<Output = std::result::Result<A::Output, TransitionError<Self>>> + Send
    {
        async move {
            let needs_data = reload || a.needs_data(self.data());
            if !needs_data {
                return A::Output::decode(&DecodedRow { data: self.data() }).map_err(|error| {
                    TransitionError {
                        error,
                        original: Box::new(self),
                    }
                });
            }
            let mut query = upgrade_query::<Self>();
            a.apply(&mut query.select, &mut query.filters);
            let result = async {
                let mut data = self.data().clone();
                let fetched = fetch_upgrade(&[&data], query, db).await?;
                data.merge(fetched.into_iter().next().ok_or(Error::NotFound)?, reload);
                A::Output::decode(&DecodedRow { data: &data })
            }
            .await;
            result.map_err(|error| TransitionError {
                error,
                original: Box::new(self),
            })
        }
    }
    fn load_all_scalars(
        self,
        db: &Database<Self::Schema>,
    ) -> impl std::future::Future<Output = std::result::Result<Self::Full, TransitionError<Self>>> + Send
    where
        Self: ResetScalars,
    {
        async move {
            let result = async {
                let data = self.data();
                let query = upgrade_query::<Self>().all_scalars();
                let fetched = fetch_upgrade(&[data], query, db).await?;
                let mut data = data.clone();
                data.merge(fetched.into_iter().next().ok_or(Error::NotFound)?, false);
                Self::Full::decode(&DecodedRow { data: &data })
            }
            .await;
            result.map_err(|error| TransitionError {
                error,
                original: Box::new(self),
            })
        }
    }
}
impl<M: Model> Instance for M {}

pub trait BatchLoad<M: Model>: Sized {
    fn prefetch<A: Apply<M> + Send>(
        self,
        a: A,
        db: &Database<M::Schema>,
    ) -> impl std::future::Future<Output = std::result::Result<Vec<A::Output>, TransitionError<Self>>>
           + Send;
}
impl<M: Model> BatchLoad<M> for Vec<M> {
    async fn prefetch<A: Apply<M> + Send>(
        self,
        a: A,
        db: &Database<M::Schema>,
    ) -> std::result::Result<Vec<A::Output>, TransitionError<Self>> {
        if self.is_empty() {
            return Ok(vec![]);
        }
        let result = async {
            if self.iter().all(|row| !a.needs_data(row.data())) {
                return self
                    .iter()
                    .map(|row| A::Output::decode(&DecodedRow { data: row.data() }))
                    .collect::<Result<Vec<_>>>();
            }
            let mut query = upgrade_query::<M>();
            a.apply(&mut query.select, &mut query.filters);
            let originals: Vec<_> = self.iter().map(Model::data).collect();
            let fetched: std::collections::HashMap<_, _> = fetch_upgrade(&originals, query, db)
                .await?
                .into_iter()
                .map(|row| (row.identity.clone(), row))
                .collect();
            originals
                .into_iter()
                .map(|old| {
                    let next = fetched.get(&old.identity).ok_or(Error::NotFound)?;
                    let mut data = old.clone();
                    data.merge(next.clone(), false);
                    A::Output::decode(&DecodedRow { data: &data })
                })
                .collect::<Result<Vec<_>>>()
        }
        .await;
        result.map_err(|error| TransitionError {
            error,
            original: Box::new(self),
        })
    }
}

fn upgrade_query<M: Model>() -> Query<M> {
    let mut query = Query::<M>::new().initialized();
    query.select.model_fields = Some(vec![]);
    query.select.prefetch.clear();
    query
}

async fn fetch_upgrade<M: Model, R, L, D>(
    rows: &[&RowData],
    query: Query<M, R, L, D>,
    db: &Database<M::Schema>,
) -> Result<Vec<RowData>> {
    use orm_engine::{exec::Outcome, plan::Output};
    let (mut q, p) = query.erase()?;
    crate::query::validate_views(&db.schema, &q)?;
    let model = db
        .schema
        .models
        .iter()
        .find(|m| m.ir.name == M::NAME)
        .ok_or_else(|| Error::Compatibility("unknown model".into()))?;
    let mut result = Vec::new();
    let budget = db
        .driver
        .dialect()
        .capabilities()
        .max_params
        .saturating_sub(p.0.len())
        .min(1000);
    if budget < model.key.len() {
        return Err(Error::Compatibility(
            "load parameters exhaust database budget".into(),
        ));
    }
    let relations = std::mem::take(&mut q.prefetch);
    q.select_related.clear();
    q.related_fields.clear();
    q.without_defaults = true.into();
    q.without_related = true;
    for chunk in rows.chunks(budget / model.key.len()) {
        let mut params = p.clone();
        let mut root = q.clone();
        root.filters.push(orm_core::ir::Expr::Or {
            items: chunk
                .iter()
                .map(|row| orm_core::ir::Expr::And {
                    items: model
                        .key
                        .iter()
                        .zip(&row.identity.value)
                        .map(|(&pos, value)| {
                            let i = params.0.len();
                            params.0.push(value.clone());
                            orm_core::ir::Expr::Cmp {
                                l: Box::new(orm_core::ir::Expr::Col {
                                    path: vec![],
                                    name: model.fields()[pos].name.clone(),
                                }),
                                op: orm_core::ir::CmpOp::Eq,
                                r: Box::new(orm_core::ir::Expr::Param { i }),
                            }
                        })
                        .collect(),
                })
                .collect(),
        });
        let mut selected = crate::query::run_select(db, root, params.clone()).await?;
        let mut child = q.clone();
        child.prefetch = relations.clone();
        child.without_defaults = false.into();
        let mut plans = orm_engine::plan::plan_prefetch_only(
            &db.schema,
            orm_core::dialect::Target::new(db.driver.dialect()),
            &child,
            &params,
        )?;
        if let Output::Instances {
            shape: Some(shape), ..
        } = &selected.plan.output
        {
            for plan in &mut plans {
                plan.key_pos = plan
                    .key_pos
                    .iter()
                    .map(|&pos| {
                        shape
                            .fields
                            .iter()
                            .position(|f| f.field.position == pos)
                            .ok_or_else(|| Error::Decode("missing prefetch parent key".into()))
                    })
                    .collect::<Result<_>>()?;
            }
        }
        match orm_engine::exec::prefetch(
            db.driver.as_ref(),
            orm_core::dialect::Target::new(db.driver.dialect()),
            selected.rows,
            plans,
        )
        .await?
        {
            Outcome::Prefetched {
                parents,
                prefetched,
            } => {
                selected.rows = parents;
                selected.prefetched = prefetched;
            }
            _ => return Err(Error::Decode("unexpected prefetch result".into())),
        }
        result.extend(crate::row::snapshot(&db.schema, &selected)?);
    }
    Ok(result)
}

impl<M: Model> BypassShape for RuntimeRow<M> {
    type Output = Self;
}
