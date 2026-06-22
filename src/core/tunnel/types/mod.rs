mod declaration;
mod runtime_state;

pub use declaration::*;
pub use runtime_state::*;

pub(in crate::core::tunnel) use declaration::{default_local_host, default_scheme};
