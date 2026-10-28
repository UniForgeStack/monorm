//! Generated, typed Rust reads over the shared ORM planner and drivers.
mod defaults;
mod dynamic;
mod named;
mod placeholder;
mod prepared;
mod projection;
mod query;
mod relation;
mod row;
mod runtime;
mod selection;
mod subquery;
mod unify;
pub use named::*;
pub use placeholder::{Cons, Has, Join, Nil, Pack, Param, Provides, Slot};
pub use relation::*;
pub use row::RowData;
pub use runtime::*;
pub use selection::*;
pub use subquery::*;
pub use unify::*;
pub mod value;
pub use monorm_core::dialect::Dialect;
use monorm_core::{ir::SchemaIr, schema::Schema};
pub use monorm_engine::db::Cell;
pub use prepared::*;
pub use projection::*;
pub use query::*;
#[doc(hidden)]
pub use sea_query::Value as BindValue;
use std::{marker::PhantomData, sync::Arc};
pub use value::Codec;
pub use {bigdecimal, chrono, serde, serde_json, uuid};

pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug)]
pub enum Error {
    Engine(monorm_engine::Error),
    Decode(String),
    NotFound,
    MultipleRows,
    Compatibility(String),
    Unloaded(String),
}
impl From<monorm_engine::Error> for Error {
    fn from(e: monorm_engine::Error) -> Self {
        Self::Engine(e)
    }
}
impl From<monorm_engine::db::DbError> for Error {
    fn from(e: monorm_engine::db::DbError) -> Self {
        Self::Engine(e.into())
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Engine(e) => e.fmt(f),
            Self::Decode(e) | Self::Compatibility(e) => f.write_str(e),
            Self::Unloaded(name) => write!(f, "field or cache {name} is unloaded"),
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
    type Value: Send + Sync;
    const LOADED: bool;
    fn read(row: &DecodedRow<'_>, field: &str) -> Result<Self::Value>;
}
impl<T: Codec + Send + Sync> State<T> for Loaded {
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
    pub(crate) driver: Arc<dyn monorm_engine::db::Driver>,
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
    pub async fn connect(url: &str, pool_size: usize) -> Result<Self> {
        let ir = SchemaIr::from_json(S::JSON).map_err(Error::Compatibility)?;
        if ir.dialect != S::Dialect::DIALECT {
            return Err(Error::Compatibility(
                "schema dialect does not match generated context".into(),
            ));
        }
        let schema = Schema::from_ir(ir).map_err(Error::Compatibility)?;
        let driver = monorm_engine::db::connect(url, pool_size).await?;
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
/// Private logical key data, constructible only from an executed model row.
#[doc(hidden)]
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Identity {
    value: Vec<BindValue>,
}

/// Decoder witness: only the client can construct one from an executed plan.
/// Generated codecs can read it but callers cannot forge a row or private identity.
pub struct DecodedRow<'a> {
    pub(crate) data: &'a RowData,
}
impl DecodedRow<'_> {
    #[doc(hidden)]
    pub fn check_model(&self, name: &str) -> Result<()> {
        if self.data.model == name {
            Ok(())
        } else {
            Err(Error::Compatibility("wrong decoded model".into()))
        }
    }

    #[doc(hidden)]
    pub fn identity(&self) -> Result<Identity> {
        Ok(self.data.identity.clone())
    }
    #[doc(hidden)]
    pub fn data(&self) -> RowData {
        self.data.clone()
    }
    pub fn read<T: Codec>(&self, name: &str) -> Result<T> {
        self.data.read(name)
    }
}
/// Implemented by the schema generator; shapes disappear before async execution.
pub trait Model: Sized + Send + Sync + 'static {
    type Schema: SchemaSpec;
    type Source: Send + 'static;
    type Normal: Model<Schema = Self::Schema, Source = Self::Source>;
    const NAME: &'static str;
    fn fields() -> Vec<String>;
    fn configure(_: &mut monorm_core::ir::Select) {}
    #[doc(hidden)]
    fn selection(q: &mut SelectionPlan) {
        q.model_fields = Some(Self::fields());
    }

    fn data(&self) -> &RowData;
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

#[doc(hidden)]
pub type SelectionPlan = monorm_core::ir::Select;
