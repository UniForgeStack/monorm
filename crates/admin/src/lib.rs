//! The admin service: resources planned from the prepared schema, keyset lists and an
//! in-memory HTTP router, and the served form of it (`serve`).
//!
//! [`Site::build`] reads the prepared schema and the config once and rejects a wrong
//! config. A request then goes through [`server::call`]: the router, the list planner of
//! [`list`] and the engine. Only [`serve`] binds a socket, and nothing here names a host language.

pub mod config;
pub mod json;
pub mod list;
pub mod scope;
pub mod serve;
pub mod server;
pub mod service;
pub mod site;
pub mod tables;

pub use config::Config;
pub use scope::Scoped;
pub use serve::{serve, ServeConfig, Server};
pub use server::{call, router, User};
pub use service::{service, Logger, Options};
pub use site::{ConfigError, Site};
