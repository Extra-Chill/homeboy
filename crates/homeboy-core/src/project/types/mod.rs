//! Project configuration value types.
//!
//! Split into cohesive submodules by responsibility (component attachments,
//! remote file pins, smoke checks, sub-targets). Re-exported
//! flat here so existing `crate::project::*` paths stay stable.

mod component;
mod remote;
mod smoke;
mod sub_target;

pub use component::*;
pub use remote::*;
pub use smoke::*;
pub use sub_target::*;
