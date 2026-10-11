//! Hand-written CDP schemas, organized by domain.
//!
//! Protocol responses include fields that are not yet read internally. Keep
//! their wire definitions without turning them into public API obligations.
#![allow(dead_code)]

use serde::Serialize;

mod browser;
pub use browser::*;
mod page;
pub use page::*;
mod input;
pub use input::*;
mod network;
pub use network::*;
mod dom;
pub use dom::*;
mod runtime;
pub use runtime::*;
mod emulation;
pub use emulation::*;

/// Empty params for CDP commands that take no arguments
#[derive(Debug, Clone, Default, Serialize)]
pub struct Empty {}

#[cfg(test)]
mod tests;
