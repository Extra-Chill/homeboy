use std::collections::HashMap;

use crate::core::engine::resource::ExtensionChildResourceSummary;

use super::ManagedSshSession;

mod delegated;
mod host;
mod local_exec;
mod resource_monitor;
mod ssh_client;
#[cfg(test)]
mod tests;

pub(crate) use delegated::DELEGATED_RUN_STATUS_FILE_ENV;
pub use local_exec::{
    execute_local_command, execute_local_command_in_dir, execute_local_command_interactive,
    execute_local_command_passthrough,
};
pub(crate) use local_exec::{
    execute_local_command_in_dir_with_timeout, execute_local_command_passthrough_with_timeout,
    execute_local_command_stderr_passthrough,
    execute_local_command_stderr_passthrough_with_timeout,
};

pub struct SshClient {
    pub host: String,
    pub user: String,
    pub port: u16,
    pub identity_file: Option<String>,
    pub auth: Option<ManagedSshSession>,
    /// When true, all commands run locally instead of over SSH.
    /// Set automatically when the server host is localhost/127.0.0.1/::1.
    pub is_local: bool,
    /// Environment variables to inject before remote commands.
    /// Values are passed through the shell, so `$PATH`-style expansion works.
    pub env: HashMap<String, String>,
}

pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
    pub exit_code: i32,
    pub timed_out: bool,
    pub child_resource: Option<ExtensionChildResourceSummary>,
}
