//! Generated, typed Rust reads over the shared ORM planner and drivers.
mod query;
pub mod value;
pub use orm_core::dialect::Dialect;
use orm_core::{ir::SchemaIr, schema::Schema};
pub use orm_engine::db::Cell;
pub use query::*;
#[doc(hidden)]
pub use sea_query::Value as BindValue;
use std::{
    any::{Any, TypeId},
    collections::HashMap,
    marker::PhantomData,
    sync::{Arc, Mutex, OnceLock},
};
pub use value::Codec;
pub use {bigdecimal, chrono, serde_json, uuid};

pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug)]
pub enum Error {
    Engine(orm_engine::Error),
    Decode(String),
    NotFound,
    MultipleRows,
    Compatibility(String),
}
impl From<orm_engine::Error> for Error {
    fn from(e: orm_engine::Error) -> Self {
        Self::Engine(e)
    }
}
impl From<orm_engine::db::DbError> for Error {
    fn from(e: orm_engine::db::DbError) -> Self {
        Self::Engine(e.into())
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Engine(e) => e.fmt(f),
            Self::Decode(e) | Self::Compatibility(e) => f.write_str(e),
            Self::NotFound => f.write_str("required query returned no rows"),
            Self::MultipleRows => f.write_str("get returned multiple rows"),
        }
    }
}
impl std::error::Error for Error {}
mod sealed {
    pub trait Sealed {}
}
#[derive(Clone, Copy, Debug)]
pub struct Loaded;
#[derive(Clone, Copy, Debug)]
pub struct Unloaded {
    _private: (),
}
impl sealed::Sealed for Loaded {}
impl sealed::Sealed for Unloaded {}
/// Closed field states: loaded values are ordinary values, unloaded values are markers.
pub trait State<T: Codec>: sealed::Sealed {
    type Value;
    const LOADED: bool;
    fn read(row: &DecodedRow<'_>, field: &str) -> Result<Self::Value>;
}
impl<T: Codec> State<T> for Loaded {
    type Value = T;
    const LOADED: bool = true;
    fn read(row: &DecodedRow<'_>, field: &str) -> Result<T> {
        row.read(field)
    }
}
impl<T: Codec> State<T> for Unloaded {
    type Value = Unloaded;
    const LOADED: bool = false;
    fn read(_: &DecodedRow<'_>, _: &str) -> Result<Unloaded> {
        Ok(Unloaded { _private: () })
    }
}
/// Generated schema identity. The associated dialect also brands execution handles.
pub trait SchemaSpec: Send + Sync + 'static {
    type Dialect: DatabaseDialect;
    const JSON: &'static str;
}
pub trait DatabaseDialect: sealed::Sealed + Send + Sync + 'static {
    const DIALECT: Dialect;
}
pub enum Postgres {}
pub enum Sqlite {}
impl sealed::Sealed for Postgres {}
impl sealed::Sealed for Sqlite {}
impl DatabaseDialect for Postgres {
    const DIALECT: Dialect = Dialect::Postgres;
}
impl DatabaseDialect for Sqlite {
    const DIALECT: Dialect = Dialect::Sqlite;
}

/// A live driver checked against the generated schema and selected dialect.
pub struct Database<S: SchemaSpec> {
    pub(crate) driver: Arc<dyn orm_engine::db::Driver>,
    pub(crate) schema: Arc<Schema>,
    _schema: PhantomData<S>,
}
impl<S: SchemaSpec> Clone for Database<S> {
    fn clone(&self) -> Self {
        Self {
            driver: self.driver.clone(),
            schema: self.schema.clone(),
            _schema: PhantomData,
        }
    }
}
impl<S: SchemaSpec> Database<S> {
    /// Configure ordinary queries for this generated schema. This is never a transaction.
    pub fn set_default(&self) {
        defaults()
            .lock()
            .unwrap()
            .insert(TypeId::of::<S>(), Box::new(self.clone()));
    }
    /// Remove this schema's configured default without closing its driver.
    pub fn clear_default() {
        defaults().lock().unwrap().remove(&TypeId::of::<S>());
    }
    pub(crate) fn configured() -> Result<Self> {
        defaults()
            .lock()
            .unwrap()
            .get(&TypeId::of::<S>())
            .and_then(|db| db.downcast_ref::<Self>())
            .cloned()
            .ok_or_else(|| {
                Error::Compatibility("no default database configured for this generated schema".into())
            })
    }
    pub async fn connect(url: &str, pool_size: usize) -> Result<Self> {
        let ir = SchemaIr::from_json(S::JSON).map_err(Error::Compatibility)?;
        if ir.dialect != S::Dialect::DIALECT {
            return Err(Error::Compatibility(
                "schema dialect does not match generated context".into(),
            ));
        }
        let schema = Schema::from_ir(ir).map_err(Error::Compatibility)?;
        let driver = orm_engine::db::connect(url, pool_size).await?;
        if driver.dialect() != S::Dialect::DIALECT {
            driver.close().await;
            return Err(Error::Compatibility(
                "URL dialect does not match generated schema".into(),
            ));
        }
        Ok(Self {
            driver,
            schema: Arc::new(schema),
            _schema: PhantomData,
        })
    }
    pub async fn close(&self) {
        self.driver.close().await;
    }
}
type Defaults = Mutex<HashMap<TypeId, Box<dyn Any + Send + Sync>>>;
fn defaults() -> &'static Defaults {
    static DEFAULTS: OnceLock<Defaults> = OnceLock::new();
    DEFAULTS.get_or_init(|| Mutex::new(HashMap::new()))
}
/// Private logical key data, constructible only from an executed model row.
#[doc(hidden)]
#[derive(PartialEq, Eq, Hash)]
pub struct Identity {
    value: BindValue,
}

/// Decoder witness: only the client can construct one from an executed plan.
/// Generated codecs can read it but callers cannot forge a row or private identity.
pub struct DecodedRow<'a> {
    pub(crate) selected: &'a orm_engine::exec::Selected,
    pub(crate) schema: &'a Schema,
    pub(crate) index: usize,
    pub(crate) model: usize,
}
impl DecodedRow<'_> {
    #[doc(hidden)]
    pub fn identity(&self) -> Result<Identity> {
        let model = &self.schema.models[self.model];
        let slot = self.slot(model.pk)?;
        Ok(Identity {
            value: self
                .selected
                .rows
                .value(self.index, slot, self.selected.plan.types[slot])?,
        })
    }
    fn slot(&self, pos: usize) -> Result<usize> {
        use orm_engine::plan::Output;
        match &self.selected.plan.output {
            Output::Instances {
                shape: Some(shape), ..
            } => shape.fields.iter().position(|f| f.field.position == pos),
            Output::Instances { shape: None, .. } => Some(pos),
            _ => None,
        }
        .ok_or_else(|| Error::Decode("missing selected identity/field".into()))
    }

    pub fn read<T: Codec>(&self, name: &str) -> Result<T> {
        let model = &self.schema.models[self.model];
        let pos = model.field_pos(name).map_err(Error::Decode)?;
        let slot = self.slot(pos)?;
        T::decode(
            self.selected
                .rows
                .cell(self.index, slot, self.selected.plan.types[slot])?,
        )
    }
}
/// Implemented by the schema generator; shapes disappear before async execution.
pub trait Model: Sized + Send + 'static {
    type Schema: SchemaSpec;
    type Source: Send + 'static;
    const NAME: &'static str;
    fn fields() -> Vec<String>;
    fn decode(row: &DecodedRow<'_>) -> Result<Self>;
}
#[derive(Debug)]
pub struct FieldMetadata {
    pub name: &'static str,
    pub column: &'static str,
    pub nullable: bool,
    pub primary_key: bool,
}
#[derive(Debug)]
pub struct ModelMetadata {
    pub name: &'static str,
    pub table: &'static str,
    pub fields: &'static [FieldMetadata],
}
