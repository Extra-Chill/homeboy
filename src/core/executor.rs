// Unified command execution - routes to local or SSH based on project config

use crate::context::resolve_project_ssh;
use crate::project::Project;
use crate::ssh::{execute_local_command, execute_local_command_interactive, CommandOutput};
use crate::Result;

/// Execute a command for a project - routes to local or SSH based on server_id config.
///
/// When `server_id` is not configured: executes command locally via shell
/// When `server_id` is configured: executes command via SSH to that server
///
/// This is the same pattern used by cli_tool.rs for module CLI commands.
pub fn execute_for_project(project: &Project, command: &str) -> Result<CommandOutput> {
    if project.server_id.as_ref().is_none_or(|s| s.is_empty()) {
        // Local execution
        Ok(execute_local_command(command))
    } else {
        // SSH execution
        let ctx = resolve_project_ssh(&project.id)?;
        Ok(ctx.client.execute(command))
    }
}

/// Execute an interactive command for a project (e.g., `tail -f`).
/// Returns exit code.
///
/// When `server_id` is not configured: executes locally with inherited stdio
/// When `server_id` is configured: executes via SSH interactive session
pub fn execute_for_project_interactive(project: &Project, command: &str) -> Result<i32> {
    if project.server_id.as_ref().is_none_or(|s| s.is_empty()) {
        // Local interactive execution
        Ok(execute_local_command_interactive(command, None, None))
    } else {
        // SSH interactive execution
        let ctx = resolve_project_ssh(&project.id)?;
        Ok(ctx.client.execute_interactive(Some(command)))
    }
}
