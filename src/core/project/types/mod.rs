//! Project configuration value types.
//!
//! Split into cohesive submodules by responsibility (component attachments,
//! remote file/log pins, database, smoke checks, API/auth, tools). Re-exported
//! flat here so existing `crate::core::project::*` paths stay stable.

mod api;
mod component;
mod database;
mod remote;
mod smoke;
mod tools;

pub use api::*;
pub use component::*;
pub use database::*;
pub use remote::*;
pub use smoke::*;
pub use tools::*;
