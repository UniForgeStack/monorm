use monorm_engine::db::{DbError, ErrorKind};
use monorm_engine::{Error, ValidationError};
use pyo3::create_exception;
use pyo3::exceptions::{PyException, PyTypeError, PyUserWarning, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use pyo3::PyErr;

create_exception!(_native, DatabaseError, PyException, "Error reported by the database or driver.");
create_exception!(
    _native,
    IntegrityError,
    DatabaseError,
    "Unique, foreign key, check, exclusion or not-null violation."
);
create_exception!(
    _native,
    LockNotAvailable,
    DatabaseError,
    "A row lock taken with `nowait` (or a lock timeout) found the rows locked."
);
create_exception!(
    _native,
    TransactionAborted,
    DatabaseError,
    "A statement or commit ran in a transaction that an earlier error aborted; `__cause__` is that error."
);
create_exception!(
    _native,
    TransactionTimeoutError,
    DatabaseError,
    "The deadline of a transaction passed; `outcome` is `\"rolled_back\"` or `\"unknown\"`."
);
create_exception!(_native, QueryError, PyValueError, "The query IR does not match the schema.");
create_exception!(_native, SchemaError, PyValueError, "The schema file or IR is invalid.");
create_exception!(_native, SchemaWarning, PyUserWarning, "A schema expression compiles, but probably does not do what it says.");
create_exception!(
    _native,
    MigrationError,
    PyException,
    "The migrations directory and the database's migration history disagree."
);

create_exception!(
    _native,
    ReadOnlyModel,
    PyException,
    "An ORM write to a SQL view model; a view is read-only."
);

create_exception!(
    _native,
    WriteProtected,
    PyException,
    "An ORM write to a `@@protected_write` model outside `monorm.allow_writes(...)`."
);

pub fn db_err(e: DbError) -> PyErr {
    let err = match e.kind {
        ErrorKind::Query => QueryError::new_err(e.message),
        ErrorKind::Integrity => IntegrityError::new_err(e.message),
        ErrorKind::LockNotAvailable => LockNotAvailable::new_err(e.message),
        ErrorKind::TransactionAborted => TransactionAborted::new_err(e.message),
        ErrorKind::TransactionTimeout => TransactionTimeoutError::new_err(e.message),
        ErrorKind::Other => DatabaseError::new_err(e.message),
    };
    if let Some(cause) = e.cause {
        let cause = db_err((*cause).clone());
        Python::attach(|py| err.set_cause(py, Some(cause)));
    }
    if e.sqlstate.is_none() && e.constraint.is_none() && e.detail.is_none() && e.outcome.is_none() {
        return err;
    }
    Python::attach(|py| {
        let value = err.value(py);
        let _ = value.setattr("outcome", e.outcome.map(|o| o.as_str()));
        // A failed setattr leaves the class default (None); the error itself still raises.
        let _ = value.setattr("sqlstate", e.sqlstate);
        let _ = value.setattr("constraint", e.constraint);
        let _ = value.setattr("detail", e.detail);
    });
    err
}

/// `sqlstate`, `constraint` and `detail` default to None on every database error.
pub fn add_defaults(py: Python<'_>) -> PyResult<()> {
    let cls = py.get_type::<DatabaseError>();
    for name in ["sqlstate", "constraint", "detail"] {
        cls.setattr(name, py.None())?;
    }
    py.get_type::<TransactionTimeoutError>().setattr("outcome", py.None())?;
    Ok(())
}

pub fn query_err(msg: String) -> PyErr {
    QueryError::new_err(msg)
}

pub fn schema_err(msg: String) -> PyErr {
    SchemaError::new_err(msg)
}

/// The Python exception for an engine error; Python errors raised while converting
/// parameters come back as they were.
pub fn engine_err(e: Error) -> PyErr {
    match e {
        Error::Query(m) => QueryError::new_err(m),
        Error::Schema(m) => SchemaError::new_err(m),
        Error::Migration(m) => MigrationError::new_err(m),
        Error::Db(e) => db_err(e),
        Error::Value(m) => PyTypeError::new_err(m),
        Error::ReadOnlyModel(m) => ReadOnlyModel::new_err(format!("{m} is a view; it can't be written")),
        Error::WriteProtected(m) => WriteProtected::new_err(format!("{m} is write-protected (@@protected_write); write it inside monorm.allow_writes({m})")),
        Error::Validation(v) => validation_to_py(&v),
        Error::Binding(e) => match e.downcast::<PyErr>() {
            Ok(e) => *e,
            Err(e) => PyTypeError::new_err(e.to_string()),
        },
    }
}

/// A Python error raised while converting a value, carried through the engine unchanged.
pub fn binding(e: PyErr) -> Error {
    Error::Binding(Box::new(e))
}

/// `monorm.errors.ValidationError`, the class of the Python layer: Rust builds and reads it by its `fields` and `non_field_messages`.
fn validation_class<'py>(py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
    py.import("monorm.errors")?.getattr("ValidationError")
}

/// The fields and messages of a `ValidationError` raised in Python; `None` for any other exception.
pub fn validation_from_py(e: &PyErr) -> Option<ValidationError> {
    Python::attach(|py| {
        let class = validation_class(py).ok()?;
        let value = e.value(py);
        if !value.is_instance(&class).unwrap_or(false) {
            return None;
        }
        let fields = value.getattr("fields").ok()?.cast_into::<PyDict>().ok()?;
        let mut out = ValidationError::new();
        for (path, list) in fields.iter() {
            let path: String = path.extract().ok()?;
            for message in list.extract::<Vec<String>>().ok()? {
                out.add(path.clone(), message);
            }
        }
        for message in value.getattr("non_field_messages").ok()?.extract::<Vec<String>>().ok()? {
            out.add_non_field_message(message);
        }
        Some(out)
    })
}

/// A new Python `ValidationError` with the paths and messages of `v`.
pub fn validation_to_py(v: &ValidationError) -> PyErr {
    Python::attach(|py| {
        let build = || -> PyResult<PyErr> {
            let fields = PyDict::new(py);
            for (path, list) in v.fields() {
                fields.set_item(path, PyList::new(py, list)?)?;
            }
            let kwargs = PyDict::new(py);
            kwargs.set_item("non_field_messages", PyList::new(py, v.non_field_messages())?)?;
            let value = validation_class(py)?.call((fields,), Some(&kwargs))?;
            Ok(PyErr::from_value(value))
        };
        build().unwrap_or_else(|e| e)
    })
}
