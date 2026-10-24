//! Versioned host contracts. This crate depends on neither bindings nor extensions.
pub mod dialect;
pub mod version;
pub mod abi;
pub mod resolve;
pub mod ir;
pub mod extension;
pub mod features;

pub mod native;

pub mod identity;

pub mod generic;


pub mod writes;

pub mod tracking;
