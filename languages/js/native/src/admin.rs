//! The admin service (feature `admin`): `monorm-admin` planned from the prepared schema and
//! called in memory. This file only converts between JS values and the crate's JSON.

use std::sync::Arc;

use monorm_admin::Site;
use monorm_engine::db::Executor;
use napi::bindgen_prelude::PromiseRaw;
use napi::Env;
use napi_derive::napi;

use crate::{Engine, JsSchema, Trace};

/// A site: the resources of one schema and one config. It holds no connection.
#[napi]
pub struct AdminSite {
    site: Arc<Site>,
}

#[napi(object)]
pub struct AdminResponse {
    pub status: u32,
    pub body: String,
}

#[napi]
impl AdminSite {
    /// One in-memory request. `user_json` is the JSON of the user, or null.
    #[napi]
    pub fn request<'env>(
        &self,
        env: &'env Env,
        engine: &Engine,
        method: String,
        path: String,
        user_json: Option<String>,
        trace: Option<&Trace>,
    ) -> napi::Result<PromiseRaw<'env, AdminResponse>> {
        let user = user_json.map(|j| serde_json::from_str(&j).map_err(|e| napi::Error::from_reason(format!("user is not JSON: {e}")))).transpose()?;
        let driver: Arc<dyn Executor> = engine.driver.clone();
        let conn = monorm_engine::trace::wrap(driver, trace.map(|t| &t.0));
        let site = self.site.clone();
        env.spawn_future(async move {
            let (status, body) = monorm_admin::call(site, conn, &method, &path, user).await;
            Ok(AdminResponse { status: u32::from(status), body })
        })
    }
}

/// Plans the resources: throws the precise text of a wrong config.
#[napi]
pub fn admin_site(schema: &JsSchema, config_json: String) -> napi::Result<AdminSite> {
    let site = Site::build(schema.inner.clone(), &config_json).map_err(|e| napi::Error::from_reason(e.to_string()))?;
    Ok(AdminSite { site: Arc::new(site) })
}
