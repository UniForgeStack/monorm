//! The seam between the HTTP handlers and host code (a Python coroutine or a Node Promise).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use monorm_engine::db::Transaction;
use serde_json::Value;

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// What the bridge binds into the host's context values before a call.
#[derive(Clone)]
pub struct CallContext {
    pub request_id: u64,
    /// The tenant id (a JSON number or string) the host binds with `db.tenant`; none for an admin call.
    pub tenant: Option<Value>,
    /// The `scope.<name>` values of default filters.
    pub scope: Value,
    /// The request's transaction; `None` for a call that needs no database.
    pub tx: Option<Arc<dyn Transaction>>,
}

/// A failed call. A host exception keeps its type name and message; the handler never panics.
#[derive(Debug, Clone, PartialEq)]
pub enum BridgeError {
    Host { kind: String, message: String },
    Bridge(String),
}

impl BridgeError {
    pub fn kind(&self) -> &str {
        match self {
            Self::Host { kind, .. } => kind,
            Self::Bridge(_) => "BridgeError",
        }
    }
    pub fn message(&self) -> &str {
        match self {
            Self::Host { message, .. } | Self::Bridge(message) => message,
        }
    }
}

/// Calls host code by name. The call has no timeout of its own; the request timeout drops the
/// future, and an implementation cancels the host task in `Drop` where the host allows it.
pub trait HostBridge: Send + Sync + 'static {
    fn call(&self, name: &str, payload: Value, ctx: CallContext) -> BoxFuture<Result<Value, BridgeError>>;
}
