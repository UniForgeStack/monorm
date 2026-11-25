//! The admin service: resources planned from the prepared schema, keyset lists and an
//! in-memory HTTP router, and the served form of it (`serve`).
//!
//! [`Site::build`] reads the prepared schema and the config once and rejects a wrong
//! config. A request then goes through [`server::call`]: the router, the list planner of
//! [`list`] and the engine. Only [`serve`] binds a socket, and nothing here names a host language.

pub mod actions;
pub mod audit;
pub mod auth;
pub mod bridge;
pub mod builder;
pub mod catalog;
pub mod config;
pub mod csv;
pub mod export;
pub mod files;
pub mod form;
mod frames;
pub mod guard;
pub mod hostwrite;
pub mod inlinewrite;
pub mod inproc;
pub mod json;
pub mod key;
pub mod list;
pub mod listwrite;
pub mod meta;
pub mod oidc;
pub mod packages;
pub mod permissions;
pub mod relations;
pub mod revoke;
pub mod routes;
pub mod scope;
pub mod security;
pub mod serve;
pub mod sight;
pub mod server;
pub mod service;
pub mod error;
pub mod site;
pub mod site_config;
mod set_password;
pub mod sql;
pub mod tables;
pub mod ui;
pub mod view;
pub mod write;

pub use auth::AuthConfig;
pub use builder::AdminConfigBuilder;
pub use bridge::{BoxFuture, BridgeError, CallContext, HostBridge};
pub use config::{security_options, security_with, Config};
pub use inproc::{Reply, Routed};
pub use scope::Scoped;
pub use security::{Security, SecurityOptions};
pub use serve::{serve, ServeConfig, Server};
pub use server::{call, call_body, call_full, call_raw, call_with, router, User};
pub use service::{service, Logger, Options};
pub use error::{code, AdminError, PathPart, Spelling};
pub use site::Site;
