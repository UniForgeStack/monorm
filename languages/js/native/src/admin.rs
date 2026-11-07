//! The admin service (feature `admin`): `monorm-admin` planned from the prepared schema and
//! called in memory. This file only converts between JS values and the crate's JSON.

use std::sync::Arc;

use monorm_admin::{Config, Logger, Server, Site};
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

    /// Binds the socket and serves this site: `settings_json` is the text that `adminSettings` gave.
    /// Rejects when an admin table is absent or the socket cannot bind.
    #[napi]
    pub fn serve<'env>(&self, env: &'env Env, engine: &Engine, settings_json: String) -> napi::Result<PromiseRaw<'env, AdminServer>> {
        let config: Config = serde_json::from_str(&settings_json).map_err(|e| napi::Error::from_reason(format!("settings are not JSON: {e}")))?;
        let driver = engine.driver.clone();
        let site = self.site.clone();
        env.spawn_future(async move {
            let server = monorm_admin::serve(site, driver, config.serve_config(), Logger::stderr()).await.map_err(napi::Error::from_reason)?;
            Ok(AdminServer { server: Arc::new(server) })
        })
    }
}

/// A running server. `stop` drains it: readiness answers 503, new connections stop, open requests finish.
#[napi]
pub struct AdminServer {
    server: Arc<Server>,
}

#[napi]
impl AdminServer {
    #[napi(getter)]
    pub fn address(&self) -> String {
        self.server.addr().to_string()
    }

    #[napi]
    pub fn stop<'env>(&self, env: &'env Env) -> napi::Result<PromiseRaw<'env, ()>> {
        let server = self.server.clone();
        env.spawn_future(async move { server.stop().await.map_err(napi::Error::from_reason) })
    }

    #[napi]
    pub fn wait<'env>(&self, env: &'env Env) -> napi::Result<PromiseRaw<'env, ()>> {
        let server = self.server.clone();
        env.spawn_future(async move { server.wait().await.map_err(napi::Error::from_reason) })
    }
}

/// The `[admin]` settings of the project in `dir`, as JSON. Throws the text of a wrong setting.
#[napi]
pub fn admin_settings(dir: String) -> napi::Result<String> {
    let config = Config::load(std::path::Path::new(&dir), |name| std::env::var(name).ok()).map_err(napi::Error::from_reason)?;
    serde_json::to_string(&config).map_err(|e| napi::Error::from_reason(e.to_string()))
}

/// Plans the resources: throws the precise text of a wrong config.
#[napi]
pub fn admin_site(schema: &JsSchema, config_json: String) -> napi::Result<AdminSite> {
    let site = Site::build(schema.inner.clone(), &config_json).map_err(|e| napi::Error::from_reason(e.to_string()))?;
    Ok(AdminSite { site: Arc::new(site) })
}
