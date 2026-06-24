//! Project configuration value types.
//!
//! Split into cohesive submodules by responsibility (component attachments,
//! remote file/log pins, database, smoke checks, API/auth). Re-exported
//! flat here so existing `crate::core::project::*` paths stay stable.

mod api;
mod component;
mod database;
mod remote;
mod smoke;

pub use api::*;
pub use component::*;
pub use database::*;
pub use remote::*;
pub use smoke::*;
