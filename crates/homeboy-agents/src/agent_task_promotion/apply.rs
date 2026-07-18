use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use crate::agent_task_gate::{
    run_gate_command, run_gate_command_with_policy,
    run_gate_command_with_policy_and_runtime_tmpdir, AgentTaskGateReport,
    AgentTaskGateRevealPolicy, AgentTaskGateVisibility,
};
use homeboy_core::command_invocation::CommandInvocation;
use homeboy_core::stream_capture::StreamCaptureMetadata;
use homeboy_core::{worktree_providers, Error, Result};

use super::types::{
    AgentTaskPromotionCommandCapture, AgentTaskPromotionCommandReport, AgentTaskPromotionOptions,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct TrustedUnpushedCandidateDestination {
    pub(crate) path: PathBuf,
    pub(crate) head: String,
}

/// Environment variable carrying the promotion workspace provider command.
const PROMOTION_PROVIDER_COMMAND_ENV: &str = "HOMEBOY_AGENT_TASK_PROMOTION_COMMAND";

/// Maximum number of bytes retained per captured stream in a promotion command
/// report. Mirrors the bounded-capture cap used by extension self-checks so the
/// retained evidence cannot grow without bound (#5077).
const PROMOTION_CAPTURE_LIMIT_BYTES: usize = 65_536;
const PROMOTION_PROVIDER_RESPONSE_LIMIT_BYTES: usize = 1_048_576;

struct ProviderCapturedStream {
    response: Vec<u8>,
    retained: Vec<u8>,
    seen: usize,
    overflowed: bool,
}

fn capture_provider_stream(
    mut reader: impl Read,
    response_limit: Option<usize>,
    overflow_signal: Option<mpsc::Sender<()>>,
) -> std::io::Result<ProviderCapturedStream> {
    let mut response = Vec::new();
    let mut retained = Vec::new();
    let mut seen = 0;
    let mut buffer = [0_u8; 8_192];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        seen += count;
        retained.extend_from_slice(&buffer[..count]);
        if retained.len() > PROMOTION_CAPTURE_LIMIT_BYTES {
            let discard = retained.len() - PROMOTION_CAPTURE_LIMIT_BYTES;
            retained.drain(..discard);
        }
        if let Some(limit) = response_limit {
            if response.len() + count > limit {
                if let Some(signal) = overflow_signal.as_ref() {
                    let _ = signal.send(());
                }
                return Ok(ProviderCapturedStream {
                    response,
                    retained,
                    seen,
                    overflowed: true,
                });
            }
            response.extend_from_slice(&buffer[..count]);
        }
    }
    Ok(ProviderCapturedStream {
        response,
        retained,
        seen,
        overflowed: false,
    })
}

fn captured_stream_report(stream: &ProviderCapturedStream) -> (String, StreamCaptureMetadata) {
    (
        String::from_utf8_lossy(&stream.retained).to_string(),
        StreamCaptureMetadata {
            limit_bytes: PROMOTION_CAPTURE_LIMIT_BYTES,
            seen_bytes: stream.seen,
            retained_bytes: stream.retained.len(),
            truncated: stream.seen > stream.retained.len(),
        },
    )
}

pub(crate) const AGENT_TASK_PROMOTION_APPLY_REQUEST_SCHEMA: &str =
    "homeboy/agent-task-promotion-apply-request/v1";
pub(crate) const AGENT_TASK_PROMOTION_APPLY_RESPONSE_SCHEMA: &str =
    "homeboy/agent-task-promotion-apply-response/v1";

/// Confirm that the controller can promote into a managed workspace before it
/// spends a provider attempt. Explicit provider commands remain authoritative
/// and intentionally bypass this configured-provider lookup.
pub fn preflight_configured_workspace_provider(to_workspace: &str) -> Result<()> {
    let config = homeboy_core::defaults::load_config();
    preflight_configured_workspace_provider_with_config(to_workspace, &config)
}

pub(crate) fn preflight_configured_workspace_provider_with_config(
    to_workspace: &str,
    config: &homeboy_core::defaults::HomeboyConfig,
) -> Result<()> {
    worktree_providers::resolve_apply_enabled_worktree_provider_from_config(
        to_workspace,
        config,
        None,
    )?;
    Ok(())
}

/// Apply a promotion-provider request to an already materialized Git workspace.
///
/// Lab uses this adapter after it has safely staged the target workspace on the
/// runner, so promotion stays within the runner without requiring a controller
/// workspace provider.
pub fn apply_materialized_workspace_patch(workspace: &Path, request_json: &str) -> Result<String> {
    let request: AgentTaskPromotionApplyRequest =
        serde_json::from_str(request_json).map_err(|error| {
            Error::validation_invalid_json(
                error,
                Some("agent-task promotion provider request".to_string()),
                None,
            )
        })?;
    if request.schema != AGENT_TASK_PROMOTION_APPLY_REQUEST_SCHEMA {
        return Err(Error::validation_invalid_argument(
            "promotion_provider.request.schema",
            format!(
                "expected {}, got {}",
                AGENT_TASK_PROMOTION_APPLY_REQUEST_SCHEMA, request.schema
            ),
            None,
            None,
        ));
    }
    validate_provider_workspace_path(workspace)?;
    // The provider can run in a different materialized workspace from the
    // producer, so prefer the patch carried through the stdin request.
    let inline_patch = request
        .patch
        .as_deref()
        .map(|patch| {
            let mut file = NamedTempFile::new().map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some("create inline agent-task promotion patch".to_string()),
                )
            })?;
            file.write_all(patch.as_bytes()).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some("write inline agent-task promotion patch".to_string()),
                )
            })?;
            Ok::<_, Error>(file)
        })
        .transpose()?;
    let patch_path = inline_patch
        .as_ref()
        .map(|file| file.path().to_string_lossy().into_owned())
        .unwrap_or_else(|| request.patch_path.clone());
    if request
        .trusted_unpushed_candidate_destination
        .as_ref()
        .is_some_and(|trusted| trusted_candidate_destination_matches(workspace, trusted))
        && patch_is_already_applied(workspace, &patch_path)?
    {
        return serde_json::to_string(&AgentTaskPromotionApplyResponse {
            schema: AGENT_TASK_PROMOTION_APPLY_RESPONSE_SCHEMA.to_string(),
            workspace_path: workspace.display().to_string(),
            command_evidence: Vec::new(),
        })
        .map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize promotion provider response".to_string()),
            )
        });
    }
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(workspace)
        .args(["apply", "--whitespace=nowarn", &patch_path]);
    if request.dry_run {
        command.arg("--check");
    }
    let output = command.output().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("apply agent-task promotion patch".to_string()),
        )
    })?;
    if !output.status.success() {
        return Err(Error::validation_invalid_argument(
            "promotion_provider.patch",
            format!(
                "could not apply promotion patch in {}: {}",
                workspace.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            Some(request.patch_path),
            None,
        ));
    }
    serde_json::to_string(&AgentTaskPromotionApplyResponse {
        schema: AGENT_TASK_PROMOTION_APPLY_RESPONSE_SCHEMA.to_string(),
        workspace_path: workspace.display().to_string(),
        command_evidence: Vec::new(),
    })
    .map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize promotion provider response".to_string()),
        )
    })
}

fn trusted_candidate_destination_matches(
    workspace: &Path,
    trusted: &TrustedUnpushedCandidateDestination,
) -> bool {
    let Ok(workspace) = std::fs::canonicalize(workspace) else {
        return false;
    };
    let Ok(path) = std::fs::canonicalize(&trusted.path) else {
        return false;
    };
    workspace == path
        && Command::new("git")
            .args([
                "-C",
                workspace.to_str().unwrap_or_default(),
                "rev-parse",
                "--verify",
                "HEAD^{commit}",
            ])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .is_some_and(|output| String::from_utf8_lossy(&output.stdout).trim() == trusted.head)
}

fn patch_is_already_applied(workspace: &Path, patch_path: &str) -> Result<bool> {
    Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args([
            "apply",
            "--reverse",
            "--check",
            "--whitespace=nowarn",
            patch_path,
        ])
        .status()
        .map(|status| status.success())
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("verify already-applied agent-task promotion patch".to_string()),
            )
        })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct AgentTaskPromotionApplyRequest {
    pub(crate) schema: String,
    pub(crate) to_workspace: String,
    /// Inline patch payload for providers that do not share artifact storage
    /// with the producer. `patch_path` remains provenance and a legacy fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) patch: Option<String>,
    pub(crate) patch_path: String,
    pub(crate) changed_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) gate_feedback_baseline: Option<serde_json::Value>,
    #[serde(default)]
    pub(crate) dry_run: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) trusted_unpushed_candidate_destination: Option<TrustedUnpushedCandidateDestination>,
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

    fn verify_with_runtime_tmpdir(
        &mut self,
        cwd: &Path,
        index: usize,
        command: &str,
        visibility: AgentTaskGateVisibility,
        reveal_policy: AgentTaskGateRevealPolicy,
        _runtime_tmpdir: &Path,
    ) -> Result<AgentTaskGateReport> {
        self.verify(cwd, index, command, visibility, reveal_policy)
    }
}

pub(crate) struct ExternalPromotionWorkspaceProvider {
    invocation: Option<CommandInvocation>,
    provenance: Option<serde_json::Value>,
    configured_fallback: Option<ConfiguredPromotionWorkspaceProvider>,
}

struct ConfiguredPromotionWorkspaceProvider {
    config: homeboy_core::defaults::HomeboyConfig,
    executable: Option<PathBuf>,
}

impl ExternalPromotionWorkspaceProvider {
    pub(crate) fn from_options(options: &AgentTaskPromotionOptions) -> Self {
        let config = homeboy_core::defaults::load_config();
        Self::from_options_with_config_and_environment(
            options,
            &config,
            None,
            std::env::var(PROMOTION_PROVIDER_COMMAND_ENV).ok(),
        )
    }

    pub(super) fn from_options_with_config_and_environment(
        options: &AgentTaskPromotionOptions,
        config: &homeboy_core::defaults::HomeboyConfig,
        executable: Option<PathBuf>,
        promotion_command: Option<String>,
    ) -> Self {
        if let Some(invocation) = options.provider_invocation.clone() {
            return Self {
                invocation: Some(invocation),
                provenance: None,
                configured_fallback: None,
            };
        }
        if let Some(command) = options.provider_command.clone().or(promotion_command) {
            eprintln!(
                "Warning: agent-task promotion provider string command is deprecated; use argv-capable provider invocation instead"
            );
            return Self {
                invocation: Some(CommandInvocation {
                    argv: command.split_whitespace().map(str::to_string).collect(),
                    display: Some(command),
                    ..Default::default()
                }),
                provenance: None,
                configured_fallback: None,
            };
        }

        Self {
            invocation: None,
            provenance: None,
            configured_fallback: Some(ConfiguredPromotionWorkspaceProvider {
                config: config.clone(),
                executable,
            }),
        }
    }

    pub(crate) fn provenance(&self) -> Option<&serde_json::Value> {
        self.provenance.as_ref()
    }

    pub(super) fn invocation(&self) -> Option<&CommandInvocation> {
        self.invocation.as_ref()
    }

    fn resolve_configured_fallback(
        &mut self,
        request: &AgentTaskPromotionApplyRequest,
    ) -> Result<()> {
        let configured_fallback = self.configured_fallback.as_ref().ok_or_else(|| {
            Error::validation_invalid_argument(
                "promotion_provider",
                format!(
                    "agent-task promotion requires a workspace provider command; pass --provider-command or set {PROMOTION_PROVIDER_COMMAND_ENV}"
                ),
                None,
                None,
            )
        })?;
        let trusted_unpushed_destination = request
            .trusted_unpushed_candidate_destination
            .as_ref()
            .map(
                |trusted| homeboy_core::worktree_providers::TrustedUnpushedWorktree {
                    path: trusted.path.clone(),
                    head: trusted.head.clone(),
                },
            );
        let resolution = worktree_providers::resolve_apply_enabled_worktree_provider_with_trusted_unpushed_destination_from_config(
            &request.to_workspace,
            &configured_fallback.config,
            request.gate_feedback_baseline.as_ref(),
            trusted_unpushed_destination.as_ref(),
        )?;
        let executable = configured_fallback
            .executable
            .clone()
            .map(Ok)
            .unwrap_or_else(|| {
                std::env::current_exe().map_err(|error| {
                    Error::internal_io(
                        error.to_string(),
                        Some(
                            "resolve current Homeboy executable for promotion provider".to_string(),
                        ),
                    )
                })
            })?;
        let workspace = resolution.worktree.path;
        self.invocation = Some(CommandInvocation {
            argv: vec![
                executable.display().to_string(),
                "agent-task".to_string(),
                "promotion-provider".to_string(),
                "--workspace".to_string(),
                workspace.clone(),
            ],
            ..Default::default()
        });
        self.provenance = Some(serde_json::json!({
            "id": resolution.provider_id,
            "handle": resolution.worktree.handle,
            "path": workspace,
        }));
        Ok(())
    }

    fn with_configured_provider_provenance(&self, mut error: Error) -> Error {
        if let Some(provenance) = self.provenance() {
            if !error.details.is_object() {
                error.details = serde_json::json!({});
            }
            if let Some(details) = error.details.as_object_mut() {
                details.insert("worktree_provider".to_string(), provenance.clone());
            }
        }
        error
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
        if self.invocation.is_none() {
            self.resolve_configured_fallback(&request)?;
        }
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
            .map_err(|error| self.with_configured_provider_provenance(error))
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

    fn verify_with_runtime_tmpdir(
        &mut self,
        cwd: &Path,
        index: usize,
        command: &str,
        visibility: AgentTaskGateVisibility,
        reveal_policy: AgentTaskGateRevealPolicy,
        runtime_tmpdir: &Path,
    ) -> Result<AgentTaskGateReport> {
        run_gate_command_with_policy_and_runtime_tmpdir(
            cwd,
            index,
            command,
            visibility,
            reveal_policy,
            Some(runtime_tmpdir),
        )
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
    drop(process.stdin.take());
    let stdout = process.stdout.take().ok_or_else(|| {
        Error::internal_io(
            "provider command stdout was not available".to_string(),
            Some("capture agent-task promotion provider response".to_string()),
        )
    })?;
    let stderr = process.stderr.take().ok_or_else(|| {
        Error::internal_io(
            "provider command stderr was not available".to_string(),
            Some("capture agent-task promotion provider diagnostics".to_string()),
        )
    })?;
    let (overflow_sender, overflow_receiver) = mpsc::channel();
    let stdout_capture_thread = std::thread::spawn(move || {
        capture_provider_stream(
            stdout,
            Some(PROMOTION_PROVIDER_RESPONSE_LIMIT_BYTES),
            Some(overflow_sender),
        )
    });
    let stderr_capture_thread =
        std::thread::spawn(move || capture_provider_stream(stderr, None, None));
    let mut response_overflow = false;
    let status = loop {
        if overflow_receiver.try_recv().is_ok() {
            response_overflow = true;
            if let Some(status) = process.try_wait().map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some("wait for oversized agent-task promotion provider response".to_string()),
                )
            })? {
                break status;
            }
            process.kill().map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some("terminate oversized agent-task promotion provider response".to_string()),
                )
            })?;
            break process.wait().map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some("wait for terminated agent-task promotion provider command".to_string()),
                )
            })?;
        }
        if let Some(status) = process.try_wait().map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("wait for agent-task promotion provider command".to_string()),
            )
        })? {
            break status;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let stdout = stdout_capture_thread
        .join()
        .map_err(|_| {
            Error::internal_unexpected("promotion provider stdout capture thread panicked")
        })?
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("capture agent-task promotion provider response".to_string()),
            )
        })?;
    let stderr = stderr_capture_thread
        .join()
        .map_err(|_| {
            Error::internal_unexpected("promotion provider stderr capture thread panicked")
        })?
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("capture agent-task promotion provider diagnostics".to_string()),
            )
        })?;
    response_overflow |= stdout.overflowed;
    let exit_code = status.code().unwrap_or(1);
    let full_stdout = String::from_utf8_lossy(&stdout.response).to_string();
    let (retained_stdout, stdout_capture) = captured_stream_report(&stdout);
    let (retained_stderr, stderr_capture) = captured_stream_report(&stderr);
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
    if response_overflow {
        return Err(Error::validation_invalid_argument_with_evidence(
            "promotion_provider.response",
            format!(
                "promotion provider response exceeded {} bytes and was terminated",
                PROMOTION_PROVIDER_RESPONSE_LIMIT_BYTES
            ),
            None,
            None,
            Some(homeboy_core::error::CommandEvidence {
                command: display,
                cwd: invocation.cwd.clone(),
                location: Some("local".to_string()),
                exit_code,
                stdout: report.stdout.clone(),
                stderr: report.stderr.clone(),
                truncated: true,
            }),
        ));
    }
    if !status.success() {
        return Err(Error::validation_invalid_argument_with_evidence(
            "command",
            format!(
                "promotion provider command failed with exit code {}: {}",
                exit_code, display
            ),
            None,
            None,
            Some(homeboy_core::error::CommandEvidence {
                command: display,
                cwd: invocation.cwd.clone(),
                location: Some("local".to_string()),
                exit_code,
                stdout: report.stdout.clone(),
                stderr: report.stderr.clone(),
                truncated: report.capture.stdout.truncated || report.capture.stderr.truncated,
            }),
        ));
    }
    let response_value: serde_json::Value =
        serde_json::from_str(&full_stdout).map_err(|error| {
            Error::validation_invalid_json(
                error,
                Some("agent-task promotion provider response".to_string()),
                Some(report.stdout.clone()),
            )
        })?;
    // Homeboy commands emit the standard result envelope. Accept its data
    // payload while retaining direct provider responses as the generic contract.
    let response_value = response_value
        .get("data")
        .cloned()
        .unwrap_or(response_value);
    let response: AgentTaskPromotionApplyResponse = serde_json::from_value(response_value)
        .map_err(|error| {
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
