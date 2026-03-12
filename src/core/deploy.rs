use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::time::SystemTime;

use crate::build;
use crate::component::{self, Component};
use crate::config;
use crate::context::{resolve_project_ssh_with_base_path, RemoteProjectContext};
use crate::defaults;
use crate::error::{Error, Result};
use crate::extension::build::resolve_artifact_path;
use crate::extension::{
    self, load_all_extensions, DeployOverride, DeployVerification, ExtensionManifest,
};
use crate::git;
use crate::hooks::{self, HookFailureMode};
use crate::permissions;
use crate::paths as base_path;
use crate::project::{self, Project};
use crate::ssh::SshClient;
use crate::engine::shell;
use crate::engine::template::{render_map, TemplateVars};
use crate::version;

include!("deploy/safety_and_artifact.rs");
include!("deploy/transfer.rs");
include!("deploy/orchestration.rs");
include!("deploy/execution.rs");
include!("deploy/planning.rs");
include!("deploy/version_overrides.rs");
include!("deploy/types.rs");
