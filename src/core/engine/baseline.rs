//! Generic baseline & ratchet primitive for drift detection.
//!
//! Captures a snapshot of "current state" (any set of fingerprintable items)
//! and compares future runs against it. Only NEW items (not in the baseline)
//! trigger a failure — resolved items are celebrated, same-state passes.
//!
//! Zero domain knowledge. The caller decides:
//! - What gets fingerprinted (via [`Fingerprintable`])
//! - What metadata to store (via the `M` type parameter)
//! - What key to use in `homeboy.json` (via [`BaselineConfig`])
//!
//! Baselines are stored in the project's `homeboy.json` under a `baselines`
//! key, keeping all component configuration in a single portable file.
//!
//! # Usage
//!
//! ```ignore
//! use homeboy::baseline::{self, Fingerprintable, BaselineConfig};
//!
//! impl Fingerprintable for MyFinding {
//!     fn fingerprint(&self) -> String {
//!         format!("{}::{}", self.category, self.file)
//!     }
//!     fn description(&self) -> String {
//!         self.message.clone()
//!     }
//!     fn context_label(&self) -> String {
//!         self.category.clone()
//!     }
//! }
//!
//! let config = BaselineConfig::new(source_path, "audit");
//! baseline::save(&config, "my-component", &items, my_metadata)?;
//! if let Some(saved) = baseline::load::<MyMeta>(&config)? {
//!     let comparison = baseline::compare(&items, &saved);
//!     if comparison.drift_increased {
//!         // CI fails — new findings introduced
//!     }
//! }
//! ```

mod baseline_config;
mod constants;
mod helpers;
mod leap_year;
mod read_json_empty;
mod save;
mod types;

pub use baseline_config::*;
pub use constants::*;
pub use helpers::*;
pub use leap_year::*;
pub use read_json_empty::*;
pub use save::*;
pub use types::*;


use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, Result};

impl BaselineConfig {
    pub fn new(root: impl Into<PathBuf>, key: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            key: key.into(),
        }
    }

    pub fn json_path(&self) -> PathBuf {
        self.root.join(HOMEBOY_JSON)
    }

    pub fn key(&self) -> &str {
        &self.key
    }
}
