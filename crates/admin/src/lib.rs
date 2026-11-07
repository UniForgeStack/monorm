//! The admin service: resources planned from the prepared schema, keyset lists and an
//! in-memory HTTP router.
//!
//! [`Site::build`] reads the prepared schema and the config once and rejects a wrong
//! config. A request then goes through [`server::call`]: the router, the list planner of
//! [`list`] and the engine. Nothing here reads a socket or names a host language.

pub mod json;
pub mod list;
pub mod server;
pub mod site;

pub use server::{call, router, User};
pub use site::{ConfigError, Site};
