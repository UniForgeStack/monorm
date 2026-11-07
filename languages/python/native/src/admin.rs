//! The admin service (feature `admin`): `monorm-admin` planned from the prepared schema and
//! called in memory. This file only converts between Python values and the crate's JSON.

use std::sync::{Arc, Mutex};

use monorm_admin::{AuthConfig, BoxFuture, BridgeError, CallContext, Config, HostBridge, Logger, Routed, Server, Site};
use monorm_engine::db::Executor;
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde_json::Value;
use tokio::sync::oneshot;

use crate::{Engine, PySchema, Trace, Transaction};

type Outcome = Result<String, BridgeError>;

/// The done-callback of the `concurrent.futures.Future` that `run_coroutine_threadsafe` returns.
#[pyclass(frozen)]
struct Done(Mutex<Option<oneshot::Sender<Outcome>>>);

#[pymethods]
impl Done {
    fn __call__(&self, fut: &Bound<'_, PyAny>) {
        let outcome = outcome_of(fut).unwrap_or_else(|e| Err(BridgeError::Bridge(e.to_string())));
        if let Some(sender) = self.0.lock().unwrap().take() {
            let _ = sender.send(outcome);
        }
    }
}

fn outcome_of(fut: &Bound<'_, PyAny>) -> PyResult<Outcome> {
    if fut.call_method0("cancelled")?.is_truthy()? {
        return Ok(Err(BridgeError::Bridge("host call cancelled".into())));
    }
    let exc = fut.call_method0("exception")?;
    if !exc.is_none() {
        let kind = exc.get_type().name()?.to_string();
        return Ok(Err(BridgeError::Host { kind, message: exc.str()?.to_string() }));
    }
    Ok(Ok(fut.call_method0("result")?.extract::<String>()?))
}

/// Cancels the host task when the request future is dropped (the request timeout) before the call ended.
struct CancelOnDrop {
    fut: Py<PyAny>,
    done: bool,
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if !self.done {
            Python::attach(|py| {
                let _ = self.fut.call_method1(py, "cancel", ());
            });
        }
    }
}

/// Runs host functions (async functions of `(database, args)`) on the app's asyncio loop, each one
/// inside `Database.bind_request`, so it sees the request's transaction, scope and tenant.
struct PyBridge {
    event_loop: Py<PyAny>,
    submit: Py<PyAny>,
    database: Py<PyAny>,
    host: Py<PyDict>,
}

impl PyBridge {
    fn new(py: Python<'_>, database: Bound<'_, PyAny>, host: Bound<'_, PyDict>) -> PyResult<PyBridge> {
        let event_loop = py.import("asyncio")?.call_method0("get_running_loop")?.unbind();
        let submit = py.import("monorm._admin_bridge")?.getattr("submit")?.unbind();
        Ok(PyBridge { event_loop, submit, database: database.unbind(), host: host.unbind() })
    }

    fn start(&self, name: &str, payload: &Value, ctx: CallContext) -> PyResult<(CancelOnDrop, oneshot::Receiver<Outcome>)> {
        Python::attach(|py| {
            let Some(function) = self.host.bind(py).get_item(name)? else {
                return Err(PyValueError::new_err(format!("no host function {name}")));
            };
            let tx = match ctx.tx {
                Some(inner) => Some(Py::new(py, Transaction { inner, failure: Mutex::new(None) })?),
                None => None,
            };
            let tenant = ctx.tenant.map(|t| t.to_string());
            let args = (&self.event_loop, function, &self.database, tx, payload.to_string(), ctx.scope.to_string(), tenant);
            let fut = self.submit.call1(py, args)?;
            let (sender, receiver) = oneshot::channel();
            fut.call_method1(py, "add_done_callback", (Done(Mutex::new(Some(sender))),))?;
            Ok((CancelOnDrop { fut, done: false }, receiver))
        })
    }
}

impl HostBridge for PyBridge {
    fn call(&self, name: &str, payload: Value, ctx: CallContext) -> BoxFuture<Result<Value, BridgeError>> {
        let started = self.start(name, &payload, ctx).map_err(|e| BridgeError::Bridge(e.to_string()));
        Box::pin(async move {
            let (mut guard, receiver) = started?;
            let outcome = receiver.await.map_err(|_| BridgeError::Bridge("host call dropped".into()))?;
            guard.done = true;
            serde_json::from_str(&outcome?).map_err(|e| BridgeError::Bridge(e.to_string()))
        })
    }
}

/// The login state of a served or in-memory router: the signing secret, the session options, the host.
fn auth_config(py: Python<'_>, database: Bound<'_, PyAny>, host: Bound<'_, PyDict>, security: Result<Arc<monorm_admin::Security>, String>) -> PyResult<AuthConfig> {
    let security = security.map_err(PyValueError::new_err)?;
    Ok(AuthConfig { security, bridge: Arc::new(PyBridge::new(py, database, host)?) })
}

/// A site: the resources of one schema and one config. It holds no connection.
#[pyclass(frozen, module = "monorm._native")]
pub struct AdminSite {
    site: Arc<Site>,
}

#[pymethods]
impl AdminSite {
    /// The permission names of the chosen strategy: the gate first.
    #[getter]
    fn permissions(&self) -> Vec<String> {
        self.site.permission_names()
    }

    /// The labels that the permission names use: the site label and the label of each resource.
    #[getter]
    fn labels(&self) -> Vec<String> {
        self.site.labels()
    }

    /// The detail URL of a record. `key_json` is a scalar, a list in key order or an object of key fields.
    /// Raises `ValueError` when the model is no resource or the key does not fit.
    fn url(&self, resource: &str, key_json: &str) -> PyResult<String> {
        let key: Value = serde_json::from_str(key_json).map_err(|e| PyValueError::new_err(format!("key is not JSON: {e}")))?;
        self.site.url(resource, &key).map_err(PyValueError::new_err)
    }

    /// The `(status, body)` of one in-memory request. `user_json` is the JSON of the user, or None.
    #[pyo3(signature = (engine, method, path, user_json = None, trace = None))]
    fn request<'py>(
        &self,
        py: Python<'py>,
        engine: &Bound<'py, Engine>,
        method: String,
        path: String,
        user_json: Option<String>,
        trace: Option<&Bound<'py, Trace>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let user = user_json.map(|j| serde_json::from_str(&j).map_err(|e| PyValueError::new_err(format!("user is not JSON: {e}")))).transpose()?;
        let driver: Arc<dyn Executor> = engine.get().driver.clone();
        let conn = monorm_engine::trace::wrap(driver, trace.map(|t| &t.get().0));
        let site = self.site.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move { Ok(monorm_admin::call(site, conn, &method, &path, user).await) })
    }

    /// The router that `serve` runs, called in memory, with login. `options_json` is `{"idle_timeout", "session_timeout", "origins"}`.
    /// Raises `ValueError` when an option is wrong or the secret is missing or short. Call it from a running event loop.
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (engine, database, host, secret = None, options_json = "{}".to_string(), trace = None))]
    fn client<'py>(
        &self,
        py: Python<'py>,
        engine: &Bound<'py, Engine>,
        database: Bound<'py, PyAny>,
        host: Bound<'py, PyDict>,
        secret: Option<String>,
        options_json: String,
        trace: Option<&Bound<'py, Trace>>,
    ) -> PyResult<AdminRouter> {
        let options = monorm_admin::security_options(&options_json).map_err(PyValueError::new_err)?;
        let security = monorm_admin::security_with(secret.as_deref(), options);
        let auth = auth_config(py, database, host, security)?;
        let driver: Arc<dyn Executor> = engine.get().driver.clone();
        let conn = monorm_engine::trace::wrap(driver, trace.map(|t| &t.get().0));
        Ok(AdminRouter { routed: Routed::new(self.site.clone(), conn, auth, None) })
    }

    /// Binds the socket and serves this site: `settings_json` is the text that `admin_settings` gave.
    /// Raises `RuntimeError` when an admin table is absent or the socket cannot bind.
    /// `host` maps a host function name to an async function of `(database, args)`; `secret` is `MONORM_ADMIN_SECRET`.
    /// Raises `ValueError` when the settings are wrong or the secret is missing or short.
    #[pyo3(signature = (engine, settings_json, database, host, secret = None))]
    fn serve<'py>(
        &self,
        py: Python<'py>,
        engine: &Bound<'py, Engine>,
        settings_json: String,
        database: Bound<'py, PyAny>,
        host: Bound<'py, PyDict>,
        secret: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let config: Config = serde_json::from_str(&settings_json).map_err(|e| PyValueError::new_err(format!("settings are not JSON: {e}")))?;
        let mut serve_config = config.serve_config();
        serve_config.auth = Some(auth_config(py, database, host, config.security(secret.as_deref()))?);
        let driver = engine.get().driver.clone();
        let site = self.site.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let server = monorm_admin::serve(site, driver, serve_config, Logger::stderr()).await.map_err(PyRuntimeError::new_err)?;
            Ok(AdminServer { server: Arc::new(server) })
        })
    }
}

/// The served router with login, called in memory (the guard, CSRF, cookies and headers run as on a socket).
#[pyclass(frozen, module = "monorm._native")]
pub struct AdminRouter {
    routed: Routed,
}

#[pymethods]
impl AdminRouter {
    /// `(status, headers, body)`; `headers` is a list of `(name, value)` pairs that keeps every `set-cookie`.
    fn request<'py>(&self, py: Python<'py>, method: String, path: String, headers: Vec<(String, String)>, body: Option<String>, ip: String) -> PyResult<Bound<'py, PyAny>> {
        let routed = self.routed.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let reply = routed.request(&method, &path, &headers, body, &ip).await.map_err(PyValueError::new_err)?;
            Ok((reply.status, reply.headers, reply.body))
        })
    }
}

/// A running server. `stop` drains it: readiness answers 503, new connections stop, open requests finish.
#[pyclass(frozen, module = "monorm._native")]
pub struct AdminServer {
    server: Arc<Server>,
}

#[pymethods]
impl AdminServer {
    #[getter]
    fn address(&self) -> String {
        self.server.addr().to_string()
    }

    fn stop<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let server = self.server.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move { server.stop().await.map_err(PyRuntimeError::new_err) })
    }

    fn wait<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let server = self.server.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move { server.wait().await.map_err(PyRuntimeError::new_err) })
    }
}

/// The `[admin]` settings of the project in `dir`, as JSON. Raises `ValueError` with the text of a wrong setting.
#[pyfunction]
fn admin_settings(dir: &str) -> PyResult<String> {
    let config = Config::load(std::path::Path::new(dir), |name| std::env::var(name).ok()).map_err(PyValueError::new_err)?;
    serde_json::to_string(&config).map_err(|e| PyValueError::new_err(e.to_string()))
}

/// Plans the resources: raises `ValueError` with the precise text of a wrong config.
#[pyfunction]
fn admin_site(schema: &Bound<'_, PySchema>, config_json: &str) -> PyResult<AdminSite> {
    let site = Site::build(schema.get().inner.clone(), config_json).map_err(|e| PyValueError::new_err(e.to_string()))?;
    Ok(AdminSite { site: Arc::new(site) })
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(admin_site, m)?)?;
    m.add_function(wrap_pyfunction!(admin_settings, m)?)?;
    m.add_class::<AdminSite>()?;
    m.add_class::<AdminServer>()?;
    m.add_class::<AdminRouter>()?;
    Ok(())
}
