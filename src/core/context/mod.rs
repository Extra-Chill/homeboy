mod local_context_detection;
mod project_server_context;
mod types;

pub use local_context_detection::*;
pub use project_server_context::*;
pub use types::*;

use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::component;
use crate::error::{Error, Result};
use crate::extension;
use crate::project::{self, Project};
use crate::server::SshClient;
use crate::server::{self, Server};

pub mod report;

pub use report::build_report;

// === Local Context Detection (homeboy context command) ===

// === Project/Server Context Resolution ===
