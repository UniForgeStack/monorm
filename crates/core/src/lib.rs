//! Language-independent ORM core.
//!
//! ```text
//! schema.prisma ──dsl──▶ SchemaIr ──schema──▶ validated Schema ──migrate──▶ snapshots, DDL
//!                           │                        └──codegen──▶ models for each language
//!                           └──(JSON)──▶ language bindings (Python today, JS next)
//! ```
//!
//! Nothing here knows about Python, JS or a database driver: bindings and the `orm`
//! CLI are thin layers over these modules.

pub mod codegen;
pub mod dialect;
pub mod dsl;
pub mod ext;
pub mod ir;
pub mod migrate;
pub mod partition;
pub mod schema;

pub mod features;

pub mod behavior;
pub mod hooks;
pub mod loader;

pub mod ownership;

pub mod field_adapters;
pub mod file_storage;
pub mod identity;
pub mod client_default;
pub mod writes;

pub mod result_check;
pub mod view;
pub mod selection;
