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
use crate::extension::{
    self, load_all_extensions, DeployOverride, DeployVerification, ExtensionManifest,
};
use crate::git;
use crate::hooks::{self, HookFailureMode};
use crate::permissions;
use crate::project::{self, Project};
use crate::ssh::SshClient;
use crate::utils::base_path;
use crate::utils::parser;
use crate::utils::shell;
use crate::utils::template::{render_map, TemplateVars};
use crate::utils::{self, artifact};
use crate::version;

include!("deploy/safety_and_artifact.inc");
include!("deploy/transfer.inc");
include!("deploy/orchestration.inc");
include!("deploy/execution.inc");
include!("deploy/planning.inc");
include!("deploy/version_overrides.inc");
include!("deploy/types.inc");
