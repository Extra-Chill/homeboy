//! File operations.
//!
//! Provides file browsing, reading, writing, and searching.
//! Routes to local or SSH execution based on project configuration.

mod edit;
mod find;
mod helpers;
mod types;

pub use edit::*;
pub use find::*;
pub use helpers::*;
pub use types::*;


use serde::Serialize;
use std::io::{self, Read};

use crate::context::{require_project_base_path, resolve_project_ssh_with_base_path};
use crate::defaults;
use crate::engine::executor::execute_for_project;
use crate::engine::text;
use crate::engine::{command, shell};
use crate::error::{Error, Result};
use crate::paths::{self as base_path, resolve_path_string};
use crate::project;

use std::path::Path;
use std::process::Command;
