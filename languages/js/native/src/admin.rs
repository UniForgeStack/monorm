//! The admin service (feature `admin`): `monorm-admin` planned from the prepared schema and
//! called in memory. This file only converts between JS values and the crate's JSON.

use std::sync::Arc;

use monorm_admin::{AuthConfig, BoxFuture, BridgeError, CallContext, Config, HostBridge, Logger, Routed, Server, Site};
use monorm_engine::db::Executor;
use napi::bindgen_prelude::{FnArgs, Function, Promise, PromiseRaw};
use napi::threadsafe_function::ThreadsafeFunction;
use napi::{Env, Status};
use napi_derive::napi;
use serde_json::Value;

use crate::{Engine, JsSchema, Trace, Transaction};

/// `(name, transaction or null, payload JSON, scope JSON, tenant JSON or null)`, spread into five JS arguments
/// (`FnArgs`; a plain tuple would arrive as one array). The Promise gives an envelope JSON.
type Args = FnArgs<(String, Option<Transaction>, String, String, Option<String>)>;
type Dispatch = ThreadsafeFunction<Args, Promise<String>, Args, Status, false, true>;

/// Calls the host functions (async functions of `(database, args)`) through one JS dispatch function that runs
/// each one in `Database.bindRequest`. A Promise cannot be cancelled: a request that times out stops waiting
/// and the JS work runs to its end; the handler rolls the transaction back.
struct NodeBridge {
    dispatch: Arc<Dispatch>,
}

impl HostBridge for NodeBridge {
    fn call(&self, name: &str, payload: Value, ctx: CallContext) -> BoxFuture<Result<Value, BridgeError>> {
        let args: Args = (
            name.to_owned(),
            ctx.tx.map(|inner| Transaction { inner }),
            payload.to_string(),
            ctx.scope.to_string(),
            ctx.tenant.map(|t| t.to_string()),
        )
            .into();
        let dispatch = self.dispatch.clone();
        Box::pin(async move {
            let promise = dispatch.call_async_catch(args).await.map_err(|e| BridgeError::Bridge(e.reason.clone()))?;
            let envelope = promise.await.map_err(|e| BridgeError::Bridge(e.reason.clone()))?;
            let envelope: Value = serde_json::from_str(&envelope).map_err(|e| BridgeError::Bridge(e.to_string()))?;
            if envelope["ok"].as_bool() == Some(true) {
                return Ok(envelope["value"].clone());
            }
            let text = |key: &str| envelope[key].as_str().unwrap_or_default().to_owned();
            Err(BridgeError::Host { kind: text("kind"), message: text("message") })
        })
    }
}

/// The login state of a served or in-memory router. The dispatch function is weak: it does not keep Node alive.
fn auth_config(dispatch: Function<'_, Args, Promise<String>>, security: Result<Arc<monorm_admin::Security>, String>) -> napi::Result<AuthConfig> {
    let security = security.map_err(napi::Error::from_reason)?;
    let dispatch = dispatch.build_threadsafe_function::<Args>().callee_handled::<false>().weak::<true>().build()?;
    Ok(AuthConfig { security, bridge: Arc::new(NodeBridge { dispatch: Arc::new(dispatch) }) })
}

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
    /// The permission names of the chosen strategy: the gate first.
    #[napi(getter)]
    pub fn permissions(&self) -> Vec<String> {
        self.site.permission_names()
    }

    /// The labels that the permission names use: the site label and the label of each resource.
    #[napi(getter)]
    pub fn labels(&self) -> Vec<String> {
        self.site.labels()
    }

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

    /// The router that `serve` runs, called in memory, with login. `options_json` is `{"idle_timeout", "session_timeout", "origins"}`.
    /// Throws when an option is wrong or the secret is missing or short. `dispatch` runs one host call.
    #[napi]
    pub fn client<'env>(
        &self,
        engine: &Engine,
        dispatch: Function<'env, Args, Promise<String>>,
        secret: Option<String>,
        options_json: String,
        trace: Option<&Trace>,
    ) -> napi::Result<AdminRouter> {
        let options = monorm_admin::security_options(&options_json).map_err(napi::Error::from_reason)?;
        let auth = auth_config(dispatch, monorm_admin::security_with(secret.as_deref(), options))?;
        let driver: Arc<dyn Executor> = engine.driver.clone();
        let conn = monorm_engine::trace::wrap(driver, trace.map(|t| &t.0));
        Ok(AdminRouter { routed: Routed::new(self.site.clone(), conn, auth, None) })
    }

    /// Binds the socket and serves this site: `settings_json` is the text that `adminSettings` gave.
    /// Rejects when an admin table is absent or the socket cannot bind. `dispatch` runs one host call;
    /// `secret` is `MONORM_ADMIN_SECRET`. Throws when the settings are wrong or the secret is missing or short.
    #[napi]
    pub fn serve<'env>(
        &self,
        env: &'env Env,
        engine: &Engine,
        settings_json: String,
        dispatch: Function<'env, Args, Promise<String>>,
        secret: Option<String>,
    ) -> napi::Result<PromiseRaw<'env, AdminServer>> {
        let config: Config = serde_json::from_str(&settings_json).map_err(|e| napi::Error::from_reason(format!("settings are not JSON: {e}")))?;
        let mut serve_config = config.serve_config();
        serve_config.auth = Some(auth_config(dispatch, config.security(secret.as_deref()))?);
        let driver = engine.driver.clone();
        let site = self.site.clone();
        env.spawn_future(async move {
            let server = monorm_admin::serve(site, driver, serve_config, Logger::stderr()).await.map_err(napi::Error::from_reason)?;
            Ok(AdminServer { server: Arc::new(server) })
        })
    }
}

#[napi(object)]
pub struct RouterReply {
    pub status: u32,
    /// `[name, value]` pairs; every `set-cookie` stays.
    pub headers: Vec<Vec<String>>,
    pub body: String,
}

/// The served router with login, called in memory (the guard, CSRF, cookies and headers run as on a socket).
#[napi]
pub struct AdminRouter {
    routed: Routed,
}

#[napi]
impl AdminRouter {
    /// `headers` is a list of `[name, value]` pairs.
    #[napi]
    pub fn request<'env>(
        &self,
        env: &'env Env,
        method: String,
        path: String,
        headers: Vec<Vec<String>>,
        body: Option<String>,
        ip: String,
    ) -> napi::Result<PromiseRaw<'env, RouterReply>> {
        let routed = self.routed.clone();
        let headers: Vec<(String, String)> = headers.into_iter().filter_map(|mut p| (p.len() == 2).then(|| (std::mem::take(&mut p[0]), std::mem::take(&mut p[1])))).collect();
        env.spawn_future(async move {
            let reply = routed.request(&method, &path, &headers, body, &ip).await.map_err(napi::Error::from_reason)?;
            Ok(RouterReply { status: u32::from(reply.status), headers: reply.headers.into_iter().map(|(k, v)| vec![k, v]).collect(), body: reply.body })
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
