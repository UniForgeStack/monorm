use crate::db::DbError;
use crate::validation::ValidationError;

pub type Result<T> = std::result::Result<T, Error>;

/// What went wrong, by the exception (or error class) a binding raises for it.
#[derive(Debug)]
pub enum Error {
    /// The query IR does not match the schema, or asks for something unsupported.
    Query(String),
    /// The schema file or IR is invalid.
    Schema(String),
    /// The migrations directory and the database's migration history disagree.
    Migration(String),
    /// Reported by the database or the driver.
    Db(DbError),
    /// A parameter value of the wrong type for its column (a `TypeError`).
    Value(String),
    /// A write was refused by an application rule or a database trigger.
    WriteProtected(WriteProtection),
    /// A statement needs `scope.<key>` of `model` and the scope has no value for it. `message` says what the statement was.
    ScopeMissing { model: String, key: String, message: String },
    /// A write sets the scope column of `model` to a value other than the scope value.
    ScopeMismatch { model: String, key: String, expected: String, actual: String },
    /// A write to a model that is a SQL view (named here); a view is read-only.
    ReadOnlyModel(String),
    /// App code rejected a value: messages per field path, and messages with no field.
    Validation(ValidationError),
    /// An error a binding raised itself while converting its values (e.g. a Python
    /// exception), handed back to it unchanged.
    Binding(Box<dyn std::error::Error + Send + Sync>),
}

#[derive(Debug)]
pub enum WriteProtection {
    Application { model: String },
    Database { message: String },
}

impl Error {
    pub fn query(msg: impl Into<String>) -> Self {
        Error::Query(msg.into())
    }

    pub fn value(msg: impl Into<String>) -> Self {
        Error::Value(msg.into())
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Query(m) | Error::Schema(m) | Error::Migration(m) | Error::Value(m) => f.write_str(m),
            Error::ScopeMissing { message, .. } => f.write_str(message),
            Error::ScopeMismatch { model, key, expected, actual } => write!(f, "{model} is scoped by {key}: the scope is {expected}, the write sets {actual}; write inside the scope, or use unscoped({key:?}) to write another scope"),
            Error::ReadOnlyModel(m) => write!(f, "{m} is a view; it can't be written"),
            Error::WriteProtected(WriteProtection::Application { model }) => write!(f, "{model} is write-protected; write it inside a scope that allows writes to {model}"),
            Error::WriteProtected(WriteProtection::Database { message }) => f.write_str(message),
            Error::Validation(e) => write!(f, "{e}"),
            Error::Db(e) => write!(f, "{e}"),
            Error::Binding(e) => write!(f, "{e}"),
        }
    }
}

impl From<ValidationError> for Error {
    fn from(e: ValidationError) -> Self {
        Error::Validation(e)
    }
}

impl From<DbError> for Error {
    fn from(e: DbError) -> Self {
        if e.write_rule().is_some() { Error::WriteProtected(WriteProtection::Database { message: e.message }) } else { Error::Db(e) }
    }
}

/// `QueryError` from a message, for `map_err` on the schema's `Result<_, String>`s.
pub(crate) fn query_err(msg: String) -> Error {
    Error::Query(msg)
}
