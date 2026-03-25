mod default_value_functions;
mod defaults;
mod homeboy_config;
mod loading_functions;
mod types;

pub use default_value_functions::*;
pub use defaults::*;
pub use homeboy_config::*;
pub use loading_functions::*;
pub use types::*;

use serde::{Deserialize, Serialize};
use std::fs;

use crate::engine::local_files;
use crate::paths;

// =============================================================================
// Default value functions (match current hardcoded behavior)
// =============================================================================

// =============================================================================
// Loading functions
// =============================================================================
