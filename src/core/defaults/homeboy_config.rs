//! homeboy_config — extracted from defaults.rs.

use serde::{Deserialize, Serialize};


/// Root configuration structure for homeboy.json
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomeboyConfig {
    #[serde(default)]
    pub defaults: Defaults,

    /// Enable automatic update check on startup (default: true).
    /// Disable with `homeboy config set /update_check false`
    /// or set HOMEBOY_NO_UPDATE_CHECK=1.
    #[serde(default = "default_true")]
    pub update_check: bool,
}

impl Default for HomeboyConfig {
    fn default() -> Self {
        Self {
            defaults: Defaults::default(),
            update_check: true,
        }
    }
}
