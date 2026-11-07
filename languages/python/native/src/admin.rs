//! The admin service (feature `admin`): `monorm-admin` planned from the prepared schema and
//! called in memory. This file only converts between Python values and the crate's JSON.

use std::sync::Arc;

use monorm_admin::Site;
use monorm_engine::db::Executor;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

use crate::{Engine, PySchema, Trace};

/// A site: the resources of one schema and one config. It holds no connection.
#[pyclass(frozen, module = "monorm._native")]
pub struct AdminSite {
    site: Arc<Site>,
}

#[pymethods]
impl AdminSite {
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
}

/// Plans the resources: raises `ValueError` with the precise text of a wrong config.
#[pyfunction]
fn admin_site(schema: &Bound<'_, PySchema>, config_json: &str) -> PyResult<AdminSite> {
    let site = Site::build(schema.get().inner.clone(), config_json).map_err(|e| PyValueError::new_err(e.to_string()))?;
    Ok(AdminSite { site: Arc::new(site) })
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(admin_site, m)?)?;
    m.add_class::<AdminSite>()?;
    Ok(())
}
