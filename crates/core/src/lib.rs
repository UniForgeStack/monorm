//! Language-independent ORM core.
//!
//! ```text
//! schema.prisma ──dsl──▶ SchemaIr ──schema──▶ validated Schema ──migrate──▶ snapshots, DDL
//!                           │                        └──codegen──▶ models for each language
//!                           └──(JSON)──▶ language bindings (Python today, JS next)
//! ```
//!
//! Nothing here knows about Python, JS or a database driver: bindings and the `monorm`
//! CLI are thin layers over these modules.

pub mod codegen;
pub mod dialect;
pub mod dsl;
pub mod duration;
pub mod ext;
pub mod ir;
pub mod json_util;
pub mod migrate;
pub mod model_ref;
pub mod partition;
pub mod password;
pub mod schema;
pub mod settings;

pub mod features;

pub mod behavior;
pub mod hooks;
pub mod loader;

pub mod ownership;
pub mod owning;
pub mod linked;

pub mod field_adapters;
pub mod identity;
pub mod client_default;
pub mod writes;
mod write_rules;

pub mod result_check;
pub mod view;
pub mod selection;
