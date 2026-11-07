//! The admin service: resources planned from the prepared schema, keyset lists and an
//! in-memory HTTP router, and the served form of it (`serve`).
//!
//! [`Site::build`] reads the prepared schema and the config once and rejects a wrong
//! config. A request then goes through [`server::call`]: the router, the list planner of
//! [`list`] and the engine. Only [`serve`] binds a socket, and nothing here names a host language.

pub mod audit;
pub mod auth;
pub mod bridge;
pub mod config;
pub mod guard;
pub mod inproc;
pub mod json;
pub mod list;
pub mod revoke;
pub mod scope;
pub mod security;
pub mod serve;
pub mod server;
pub mod service;
pub mod site;
pub mod sql;
pub mod tables;

pub use auth::AuthConfig;
pub use bridge::{BoxFuture, BridgeError, CallContext, HostBridge};
pub use config::{security_options, security_with, Config};
pub use inproc::{Reply, Routed};
pub use scope::Scoped;
pub use security::{Security, SecurityOptions};
pub use serve::{serve, ServeConfig, Server};
pub use server::{call, router, User};
pub use service::{service, Logger, Options};
pub use site::{ConfigError, Site};
