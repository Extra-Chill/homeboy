//! component_set_flags — extracted from component.rs.

use clap::{Args, Subcommand};
use serde::Serialize;
use serde_json::Value;
use std::path::Path;
use homeboy::component::{self, Component};
use homeboy::project::{self, Project};
use homeboy::EntityCrudOutput;
use super::super::{CmdResult, DynamicSetArgs};


/// Dedicated flags for common component fields on `component set`.
pub(crate) struct ComponentSetFlags {
    local_path: Option<String>,
    remote_path: Option<String>,
    build_artifact: Option<String>,
    extract_command: Option<String>,
    changelog_target: Option<String>,
}
