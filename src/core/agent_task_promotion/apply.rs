use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use crate::core::agent_task_gate::{
    run_gate_command, run_gate_command_with_policy, AgentTaskGateReport, AgentTaskGateRevealPolicy,
    AgentTaskGateVisibility,
};
use crate::core::command_invocation::CommandInvocation;
use crate::core::stream_capture::StreamCaptureMetadata;
use crate::core::{Error, Result};

use super::types::{
    AgentTaskPromotionCommandCapture, AgentTaskPromotionCommandReport, AgentTaskPromotionOptions,
};

/// Environment variable carrying the promotion workspace provider command.
const PROMOTION_PROVIDER_COMMAND_ENV: &str = "HOMEBOY_AGENT_TASK_PROMOTION_COMMAND";

/// Maximum number of bytes retained per captured stream in a promotion command
/// report. Mirrors the bounded-capture cap used by extension self-checks so the
/// retained evidence cannot grow without bound (#5077).
const PROMOTION_CAPTURE_LIMIT_BYTES: usize = 65_536;

/// Bound a captured stream to a retained-byte cap, keeping the trailing bytes
/// (most relevant tail) and returning the retained text plus truncation
/// metadata. Mirrors the `BoundedCapture` pattern in extension self-checks.
fn bound_captured_stream(bytes: &[u8], limit: usize) -> (String, StreamCaptureMetadata) {
    let seen = bytes.len();
    let retained: &[u8] = if seen > limit {
        &bytes[seen - limit..]
    } else {
        bytes
    };
    let metadata = StreamCaptureMetadata {
        limit_bytes: limit,
        seen_bytes: seen,
        retained_bytes: retained.len(),
        truncated: seen > retained.len(),
    };
    (String::from_utf8_lossy(retained).to_string(), metadata)
}

pub(crate) const AGENT_TASK_PROMOTION_APPLY_REQUEST_SCHEMA: &str =
    "homeboy/agent-task-promotion-apply-request/v1";
pub(crate) const AGENT_TASK_PROMOTION_APPLY_RESPONSE_SCHEMA: &str =
    "homeboy/agent-task-promotion-apply-response/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct AgentTaskPromotionApplyRequest {
    pub(crate) schema: String,
    pub(crate) to_workspace: String,
    pub(crate) patch_path: String,
    pub(crate) changed_files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct AgentTaskPromotionApplyResponse {
    #[serde(default)]
    schema: String,
    workspace_path: String,
    #[serde(default)]
    command_evidence: Vec<AgentTaskPromotionCommandReport>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentTaskPromotionWorkspace {
    pub(crate) path: PathBuf,
    pub(crate) command_evidence: Vec<AgentTaskPromotionCommandReport>,
}

pub(crate) trait AgentTaskPromotionWorkspaceProvider {
    fn apply_patch(
        &mut self,
        request: AgentTaskPromotionApplyRequest,
    ) -> Result<AgentTaskPromotionWorkspace>;
    fn verify(
        &mut self,
        cwd: &Path,
        index: usize,
        command: &str,
        visibility: AgentTaskGateVisibility,
        reveal_policy: AgentTaskGateRevealPolicy,
    ) -> Result<AgentTaskGateReport>;
}

pub(crate) struct ExternalPromotionWorkspaceProvider {
    invocation: Option<CommandInvocation>,
}

impl ExternalPromotionWorkspaceProvider {
    pub(crate) fn from_options(options: &AgentTaskPromotionOptions) -> Self {
        Self {
            invocation: options
                .provider_command
                .clone()
                .or_else(|| std::env::var(PROMOTION_PROVIDER_COMMAND_ENV).ok())
                .map(|command| {
                    eprintln!(
                        "Warning: agent-task promotion provider string command is deprecated; use argv-capable provider invocation instead"
                    );
                    CommandInvocation {
                        argv: command.split_whitespace().map(str::to_string).collect(),
                        display: Some(command),
                        ..Default::default()
                    }
                }),
        }
    }
}

fn invocation_program_and_args(invocation: &CommandInvocation) -> Option<(String, Vec<String>)> {
    let mut argv = invocation.argv.clone().into_iter();
    let program = argv.next()?;
    Some((program, argv.collect()))
}

impl AgentTaskPromotionWorkspaceProvider for ExternalPromotionWorkspaceProvider {
    fn apply_patch(
        &mut self,
        request: AgentTaskPromotionApplyRequest,
    ) -> Result<AgentTaskPromotionWorkspace> {
        let invocation = self.invocation.as_ref().ok_or_else(|| {
            Error::validation_invalid_argument(
                "promotion_provider",
                format!(
                    "agent-task promotion requires a workspace provider command; pass --provider-command or set {PROMOTION_PROVIDER_COMMAND_ENV}"
                ),
                None,
                None,
            )
        })?;
        run_provider_command(invocation, &request)
    }

    fn verify(
        &mut self,
        cwd: &Path,
        index: usize,
        command: &str,
        visibility: AgentTaskGateVisibility,
        reveal_policy: AgentTaskGateRevealPolicy,
    ) -> Result<AgentTaskGateReport> {
        if visibility == AgentTaskGateVisibility::Visible
            && reveal_policy == AgentTaskGateRevealPolicy::FullEvidence
        {
            return run_gate_command(cwd, index, command);
        }

        run_gate_command_with_policy(cwd, index, command, visibility, reveal_policy)
    }
}

pub(crate) fn run_provider_command(
    invocation: &CommandInvocation,
    request: &AgentTaskPromotionApplyRequest,
) -> Result<AgentTaskPromotionWorkspace> {
    let (program, args) = invocation_program_and_args(invocation).ok_or_else(|| {
        Error::validation_invalid_argument(
            "promotion_provider.argv",
            "promotion provider invocation argv must not be empty",
            None,
            None,
        )
    })?;
    let display = invocation.display.clone().unwrap_or_else(|| {
        std::iter::once(program.clone())
            .chain(args.clone())
            .collect::<Vec<_>>()
            .join(" ")
    });
    let request_json = serde_json::to_vec(request).map_err(|error| {
        Error::validation_invalid_json(
            error,
            Some("agent-task promotion provider request".to_string()),
            None,
        )
    })?;
    let mut command_builder = Command::new(&program);
    command_builder.args(&args);
    if let Some(cwd) = invocation.cwd.as_deref() {
        command_builder.current_dir(cwd);
    }

    let mut process = command_builder
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("start agent-task promotion provider command".to_string()),
            )
        })?;
    process
        .stdin
        .as_mut()
        .ok_or_else(|| {
            Error::internal_io(
                "provider command stdin was not available".to_string(),
                Some("write agent-task promotion provider request".to_string()),
            )
        })?
        .write_all(&request_json)
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("write agent-task promotion provider request".to_string()),
            )
        })?;
    let output = process.wait_with_output().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("run agent-task promotion provider command".to_string()),
        )
    })?;
    let exit_code = output.status.code().unwrap_or(1);
    // The full stdout is parsed for the provider response below, but the bytes
    // retained in the evidence report are bounded with truncation metadata so
    // a pathologically large stream cannot grow the report without bound.
    let full_stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let (retained_stdout, stdout_capture) =
        bound_captured_stream(&output.stdout, PROMOTION_CAPTURE_LIMIT_BYTES);
    let (retained_stderr, stderr_capture) =
        bound_captured_stream(&output.stderr, PROMOTION_CAPTURE_LIMIT_BYTES);
    let report = AgentTaskPromotionCommandReport {
        command: std::iter::once(program).chain(args).collect(),
        exit_code,
        stdout: retained_stdout,
        stderr: retained_stderr,
        capture: AgentTaskPromotionCommandCapture {
            stdout: stdout_capture,
            stderr: stderr_capture,
        },
    };
    if !output.status.success() {
        return Err(Error::validation_invalid_argument(
            "command",
            format!(
                "promotion provider command failed with exit code {}: {}",
                exit_code, display
            ),
            None,
            Some(vec![report.stderr.clone()]),
        ));
    }
    let response: AgentTaskPromotionApplyResponse =
        serde_json::from_str(&full_stdout).map_err(|error| {
            Error::validation_invalid_json(
                error,
                Some("agent-task promotion provider response".to_string()),
                Some(report.stdout.clone()),
            )
        })?;
    if !response.schema.is_empty() && response.schema != AGENT_TASK_PROMOTION_APPLY_RESPONSE_SCHEMA
    {
        return Err(Error::validation_invalid_argument(
            "promotion_provider.response.schema",
            format!(
                "expected {}, got {}",
                AGENT_TASK_PROMOTION_APPLY_RESPONSE_SCHEMA, response.schema
            ),
            None,
            None,
        ));
    }
    if response.workspace_path.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "promotion_provider.response.workspace_path",
            "promotion provider response must include a workspace_path",
            None,
            None,
        ));
    }

    let workspace_path = PathBuf::from(response.workspace_path);
    validate_provider_workspace_path(&workspace_path)?;

    let mut command_evidence = response.command_evidence;
    if command_evidence.is_empty() {
        command_evidence.push(report);
    }
    Ok(AgentTaskPromotionWorkspace {
        path: workspace_path,
        command_evidence,
    })
}

fn validate_provider_workspace_path(path: &Path) -> Result<()> {
    match git_output(path, &["rev-parse", "--is-inside-work-tree"]) {
        Some(value) if value == "true" => Ok(()),
        _ => Err(Error::validation_invalid_argument(
            "promotion_provider.response.workspace_path",
            format!(
                "promotion provider response workspace_path is not a git worktree: {}",
                path.display()
            ),
            None,
            None,
        )),
    }
}

fn git_output(cwd: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
