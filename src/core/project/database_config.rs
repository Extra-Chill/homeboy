//! database_config — extracted from mod.rs.

use serde::{Deserialize, Serialize};
use crate::component::ScopedExtensionConfig;
use crate::config::{self, ConfigEntity};
use crate::engine::local_files::{self, FileSystem};
use crate::error::{Error, Result};
use crate::output::{CreateOutput, MergeOutput, RemoveResult};
use std::collections::HashMap;
use std::path::PathBuf;
use crate::core::project::default_db_host;
use crate::core::project::default_db_port;


#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct DatabaseConfig {
    #[serde(default = "default_db_host")]
    pub host: String,
    #[serde(default = "default_db_port")]
    pub port: u16,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub user: String,
    #[serde(default = "default_true")]
    pub use_ssh_tunnel: bool,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            host: default_db_host(),
            port: default_db_port(),
            name: String::new(),
            user: String::new(),
            use_ssh_tunnel: true,
        }
    }
}
