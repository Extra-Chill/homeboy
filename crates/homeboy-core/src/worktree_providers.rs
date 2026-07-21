use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::defaults::{
    self, HomeboyConfig, WorktreeProviderConfig, WorktreeProviderKind,
    WorktreeProviderListResultMapping,
};
use crate::error::{CommandEvidence, Error, Result};

#[derive(Debug, Clone)]
pub struct WorktreeProviderCleanupOptions {
    pub provider: Vec<String>,
    pub all_providers: bool,
    pub apply: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeProviderCleanupMode {
    Preview,
    Apply,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WorktreeProviderCleanupOutput {
    pub command: &'static str,
    pub mode: WorktreeProviderCleanupMode,
    pub provider_count: usize,
    pub success_count: usize,
    pub failure_count: usize,
    pub providers: Vec<WorktreeProviderCleanupResult>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WorktreeProviderCleanupResult {
    pub provider_id: String,
    pub success: bool,
    pub mode: WorktreeProviderCleanupMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_run: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<i32>,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub stdout: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub stderr: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parsed_payload: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_progress: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub run_refs: Vec<WorktreeProviderRunRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub follow_up_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorktreeProviderRunRef {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_command: Option<String>,
}

/// A workspace returned by a command worktree provider's `list` command.
///
/// The configured result mapping must project every field below so Homeboy
/// never guesses safety state for an externally managed destination.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct WorktreeProviderHandle {
    pub handle: String,
    pub path: String,
    pub branch: String,
    pub safety: WorktreeProviderHandleSafety,
}

/// A provider-managed workspace together with the provider that resolved it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeProviderResolution {
    pub provider_id: String,
    pub worktree: WorktreeProviderHandle,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct WorktreeProviderHandleSafety {
    pub dirty: bool,
    pub unpushed: bool,
    pub primary: bool,
}

/// Resolve an externally managed worktree handle without creating or adopting
/// a Homeboy record. This is intentionally a lookup-only boundary.
pub fn resolve_worktree_provider_handle(handle: &str) -> Result<WorktreeProviderHandle> {
    resolve_worktree_provider(handle).map(|resolution| resolution.worktree)
}

pub fn resolve_worktree_provider_handle_from_config(
    handle: &str,
    config: &HomeboyConfig,
) -> Result<WorktreeProviderHandle> {
    resolve_worktree_provider_from_config(handle, config).map(|resolution| resolution.worktree)
}

/// Resolve a provider-managed workspace and retain the selected provider id.
pub fn resolve_worktree_provider(handle: &str) -> Result<WorktreeProviderResolution> {
    resolve_worktree_provider_from_config(handle, &defaults::load_config())
}

pub fn resolve_worktree_provider_from_config(
    handle: &str,
    config: &HomeboyConfig,
) -> Result<WorktreeProviderResolution> {
    resolve_worktree_provider_with_policy_from_config(handle, config, false, None, None)
}

/// Resolve a workspace only from providers explicitly authorized for apply operations.
pub fn resolve_apply_enabled_worktree_provider_from_config(
    handle: &str,
    config: &HomeboyConfig,
    gate_feedback_baseline: Option<&serde_json::Value>,
) -> Result<WorktreeProviderResolution> {
    resolve_apply_enabled_worktree_provider_with_trusted_unpushed_destination_from_config(
        handle,
        config,
        gate_feedback_baseline,
        None,
    )
}

/// A clean immutable candidate may be its own destination before Homeboy's
/// finalizer pushes it. The exception remains bound to this exact checkout and
/// commit; every other destination safety requirement still applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedUnpushedWorktree {
    pub path: std::path::PathBuf,
    pub head: String,
}

pub fn resolve_apply_enabled_worktree_provider_with_trusted_unpushed_destination_from_config(
    handle: &str,
    config: &HomeboyConfig,
    gate_feedback_baseline: Option<&serde_json::Value>,
    trusted_unpushed_destination: Option<&TrustedUnpushedWorktree>,
) -> Result<WorktreeProviderResolution> {
    resolve_worktree_provider_with_policy_from_config(
        handle,
        config,
        true,
        gate_feedback_baseline,
        trusted_unpushed_destination,
    )
}

fn resolve_worktree_provider_with_policy_from_config(
    handle: &str,
    config: &HomeboyConfig,
    require_apply_enabled: bool,
    gate_feedback_baseline: Option<&serde_json::Value>,
    trusted_unpushed_destination: Option<&TrustedUnpushedWorktree>,
) -> Result<WorktreeProviderResolution> {
    let mut provider_ids = config
        .worktree_providers
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    provider_ids.sort();
    let mut attempted = Vec::new();
    let mut not_apply_enabled = Vec::new();

    for provider_id in provider_ids.iter().cloned() {
        let provider = &config.worktree_providers[&provider_id];
        if !provider.enabled {
            continue;
        }
        if require_apply_enabled && !provider.apply_enabled {
            not_apply_enabled.push(provider_id);
            continue;
        }
        if let Some(command) = provider.commands.resolve.as_ref() {
            attempted.push(provider_id.clone());
            let worktrees = run_provider_resolve_command(&provider_id, provider, command, handle)?;
            if let Some(worktree) = worktrees.into_iter().find(|item| item.handle == handle) {
                validate_provider_handle(
                    &provider_id,
                    &worktree,
                    gate_feedback_baseline,
                    trusted_unpushed_destination,
                )?;
                return Ok(WorktreeProviderResolution {
                    provider_id,
                    worktree,
                });
            }
            continue;
        }
        let Some(command) = provider.commands.list.as_ref() else {
            continue;
        };
        attempted.push(provider_id.clone());
        let worktrees = run_provider_list_command(&provider_id, provider, command)?;
        if let Some(worktree) = worktrees.into_iter().find(|item| item.handle == handle) {
            validate_provider_handle(
                &provider_id,
                &worktree,
                gate_feedback_baseline,
                trusted_unpushed_destination,
            )?;
            return Ok(WorktreeProviderResolution {
                provider_id,
                worktree,
            });
        }
    }

    let configured = if provider_ids.is_empty() {
        "no worktree providers are configured".to_string()
    } else {
        format!(
            "configured provider(s): {}; checked provider(s): {}{}",
            provider_ids.join(", "),
            if attempted.is_empty() {
                "none with an enabled resolve or list command".to_string()
            } else {
                attempted.join(", ")
            },
            if not_apply_enabled.is_empty() {
                String::new()
            } else {
                format!(
                    "; not apply-enabled provider(s): {}",
                    not_apply_enabled.join(", ")
                )
            },
        )
    };
    Err(Error::validation_invalid_argument(
        "to_worktree",
        format!(
            "worktree handle `{handle}` is not a Homeboy task worktree and was not returned by a configured worktree provider ({configured})"
        ),
        Some(handle.to_string()),
        Some(vec![
            "Create the destination through its workspace provider, or use an existing Homeboy task worktree handle.".to_string(),
            if require_apply_enabled {
                "Configure an enabled, apply-enabled worktree provider commands.list command that returns typed worktree path, branch, and safety metadata.".to_string()
            } else {
                "Configure an enabled worktree provider commands.list command that returns typed worktree path, branch, and safety metadata.".to_string()
            },
        ]),
    ))
}

fn run_provider_resolve_command(
    provider_id: &str,
    provider: &WorktreeProviderConfig,
    command: &[String],
    handle: &str,
) -> Result<Vec<WorktreeProviderHandle>> {
    let command = command
        .iter()
        .map(|argument| argument.replace("{handle}", handle))
        .collect::<Vec<_>>();
    run_provider_lookup_command(provider_id, provider, &command, "resolve")
}

fn run_provider_list_command(
    provider_id: &str,
    provider: &WorktreeProviderConfig,
    command: &[String],
) -> Result<Vec<WorktreeProviderHandle>> {
    run_provider_lookup_command(provider_id, provider, command, "list")
}

fn run_provider_lookup_command(
    provider_id: &str,
    provider: &WorktreeProviderConfig,
    command: &[String],
    operation: &str,
) -> Result<Vec<WorktreeProviderHandle>> {
    let (program, args) = command
        .split_first()
        .filter(|(program, _)| !program.trim().is_empty())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                format!("worktree_providers.commands.{operation}"),
                format!(
                    "worktree provider `{provider_id}` {operation} command must include an executable"
                ),
                Some(provider_id.to_string()),
                None,
            )
        })?;
    let output = Command::new(program).args(args).output().map_err(|error| {
        Error::validation_invalid_argument(
            "to_worktree",
            format!(
                "worktree provider `{provider_id}` {operation} command could not start: {error}"
            ),
            Some(provider_id.to_string()),
            None,
        )
    })?;
    if !output.status.success() {
        return Err(Error::validation_invalid_argument_with_evidence(
            "to_worktree",
            format!(
                "worktree provider `{provider_id}` {operation} command failed with {}",
                output
                    .status
                    .code()
                    .map(|code| format!("exit code {code}"))
                    .unwrap_or_else(|| "a signal".to_string())
            ),
            Some(provider_id.to_string()),
            None,
            Some(provider_command_evidence(command, &output)),
        ));
    }
    let value: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
        Error::validation_invalid_argument(
            "to_worktree",
            format!(
                "worktree provider `{provider_id}` {operation} command returned invalid JSON: {error}"
            ),
            Some(provider_id.to_string()),
            None,
        )
    })?;
    let mapping = provider.list_result_mapping.as_ref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "worktree_providers.list_result_mapping",
            format!(
                "worktree provider `{provider_id}` must configure an explicit list_result_mapping"
            ),
            Some(provider_id.to_string()),
            None,
        )
    })?;
    map_provider_list_result(provider_id, mapping, &value)
}

fn provider_command_evidence(command: &[String], output: &std::process::Output) -> CommandEvidence {
    let (stdout, stdout_truncated) = bounded_provider_output(&output.stdout);
    let (stderr, stderr_truncated) = bounded_provider_output(&output.stderr);
    CommandEvidence {
        command: command.join(" "),
        cwd: None,
        location: Some("local".to_string()),
        exit_code: output.status.code().unwrap_or(-1),
        stdout,
        stderr,
        truncated: stdout_truncated || stderr_truncated,
    }
}

fn bounded_provider_output(output: &[u8]) -> (String, bool) {
    const MAX_OUTPUT_CHARS: usize = 8_192;

    let output = String::from_utf8_lossy(output);
    let truncated = output.chars().count() > MAX_OUTPUT_CHARS;
    (output.chars().take(MAX_OUTPUT_CHARS).collect(), truncated)
}

fn map_provider_list_result(
    provider_id: &str,
    mapping: &WorktreeProviderListResultMapping,
    value: &Value,
) -> Result<Vec<WorktreeProviderHandle>> {
    let items = required_jsonpath_value(provider_id, "items", &mapping.items, value)?;
    let items = items.as_array().ok_or_else(|| {
        mapping_error(
            provider_id,
            "items",
            &mapping.items,
            "must resolve to an array",
        )
    })?;

    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            Ok(WorktreeProviderHandle {
                handle: required_string(provider_id, index, "handle", &mapping.handle, item)?,
                path: required_string(provider_id, index, "path", &mapping.path, item)?,
                branch: required_string(provider_id, index, "branch", &mapping.branch, item)?,
                // Safety flags are advisory hints a provider raises to BLOCK an
                // unsafe destination (dirty/unpushed/primary => refuse). A
                // provider that does not report one is making no claim of
                // unsafety, so an absent value defaults to `false` (permissive)
                // rather than failing the whole cook closed — the DMC worktree
                // provider legitimately omits `safety.dirty` (#7886). A value
                // that IS present but not a boolean is still a contract error.
                safety: WorktreeProviderHandleSafety {
                    dirty: optional_bool(provider_id, index, "dirty", &mapping.dirty, item)?,
                    unpushed: optional_bool(
                        provider_id,
                        index,
                        "unpushed",
                        &mapping.unpushed,
                        item,
                    )?,
                    primary: optional_bool(provider_id, index, "primary", &mapping.primary, item)?,
                },
            })
        })
        .collect()
}

fn required_string(
    provider_id: &str,
    index: usize,
    field: &str,
    path: &str,
    item: &Value,
) -> Result<String> {
    required_jsonpath_value(provider_id, &format!("items[{index}].{field}"), path, item)?
        .as_str()
        .map(ToString::to_string)
        .ok_or_else(|| mapping_error(provider_id, field, path, "must resolve to a string"))
}

fn required_bool(
    provider_id: &str,
    index: usize,
    field: &str,
    path: &str,
    item: &Value,
) -> Result<bool> {
    required_jsonpath_value(provider_id, &format!("items[{index}].{field}"), path, item)?
        .as_bool()
        .ok_or_else(|| mapping_error(provider_id, field, path, "must resolve to a boolean"))
}

/// Resolve an advisory boolean safety flag. An absent value defaults to `false`
/// (the provider makes no claim of unsafety), so a provider that omits the field
/// does not block the cook (#7886). A value that resolves but is not a boolean
/// remains a contract error.
fn optional_bool(
    provider_id: &str,
    index: usize,
    field: &str,
    path: &str,
    item: &Value,
) -> Result<bool> {
    match optional_jsonpath_value(provider_id, &format!("items[{index}].{field}"), path, item)? {
        None => Ok(false),
        Some(value) => value
            .as_bool()
            .ok_or_else(|| mapping_error(provider_id, field, path, "must resolve to a boolean")),
    }
}

fn required_jsonpath_value<'a>(
    provider_id: &str,
    field: &str,
    expression: &str,
    value: &'a Value,
) -> Result<&'a Value> {
    optional_jsonpath_value(provider_id, field, expression, value)?
        .ok_or_else(|| mapping_error(provider_id, field, expression, "did not resolve a value"))
}

/// Resolve a JSONPath to at most one value: `Ok(None)` when it resolves nothing,
/// `Ok(Some(value))` for exactly one, and an error only for invalid JSONPath or
/// an ambiguous multi-match.
fn optional_jsonpath_value<'a>(
    provider_id: &str,
    field: &str,
    expression: &str,
    value: &'a Value,
) -> Result<Option<&'a Value>> {
    let path = serde_json_path::JsonPath::parse(expression).map_err(|error| {
        mapping_error(
            provider_id,
            field,
            expression,
            &format!("is not valid JSONPath: {error}"),
        )
    })?;
    let matches = path.query(value).all();
    match matches.as_slice() {
        [value] => Ok(Some(*value)),
        [] => Ok(None),
        _ => Err(mapping_error(
            provider_id,
            field,
            expression,
            "resolved more than one value",
        )),
    }
}

fn mapping_error(provider_id: &str, field: &str, path: &str, detail: &str) -> Error {
    Error::validation_invalid_argument(
        "worktree_providers.list_result_mapping",
        format!("worktree provider `{provider_id}` mapping `{field}` ({path}) {detail}"),
        Some(provider_id.to_string()),
        None,
    )
}

fn validate_provider_handle(
    provider_id: &str,
    worktree: &WorktreeProviderHandle,
    gate_feedback_baseline: Option<&serde_json::Value>,
    trusted_unpushed_destination: Option<&TrustedUnpushedWorktree>,
) -> Result<()> {
    let path = std::path::PathBuf::from(&worktree.path);
    if !path.is_dir() {
        return Err(Error::validation_invalid_argument(
            "to_worktree",
            format!(
                "worktree provider `{provider_id}` resolved `{}` to a missing directory {}",
                worktree.handle,
                path.display()
            ),
            Some(worktree.handle.clone()),
            None,
        ));
    }
    if worktree.branch.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "to_worktree",
            format!(
                "worktree provider `{provider_id}` resolved `{}` without a branch",
                worktree.handle
            ),
            Some(worktree.handle.clone()),
            None,
        ));
    }
    let verified_gate_feedback_baseline = worktree.safety.dirty
        && gate_feedback_baseline
            .map(|baseline| {
                crate::gate_feedback_baseline::validate_gate_feedback_candidate_baseline(
                    &path, baseline,
                )
            })
            .transpose()?
            .is_some();
    let blocked = [
        (
            worktree.safety.dirty && !verified_gate_feedback_baseline,
            "dirty",
        ),
        (
            worktree.safety.unpushed
                && !trusted_unpushed_destination_matches(&path, trusted_unpushed_destination),
            "unpushed",
        ),
        (worktree.safety.primary, "primary"),
    ]
    .into_iter()
    .filter_map(|(blocked, name)| blocked.then_some(name))
    .collect::<Vec<_>>();
    if !blocked.is_empty() {
        let baseline_state = if worktree.safety.dirty {
            if gate_feedback_baseline.is_some() {
                "gate_feedback_baseline=present"
            } else {
                "gate_feedback_baseline=missing"
            }
        } else {
            "gate_feedback_baseline=not_required"
        };
        return Err(Error::validation_invalid_argument(
            "to_worktree",
            format!("worktree provider `{provider_id}` marked `{}` as {}; {baseline_state}; refusing to cook into an unsafe destination", worktree.handle, blocked.join(", ")),
            Some(worktree.handle.clone()),
            Some(vec!["Use a clean, pushed, non-primary provider-managed worktree on the intended branch.".to_string()]),
        ));
    }
    if crate::git::current_branch(&path).as_deref() != Some(worktree.branch.as_str()) {
        return Err(Error::validation_invalid_argument(
            "to_worktree",
            format!("worktree provider `{provider_id}` branch metadata for `{}` does not match the checkout branch", worktree.handle),
            Some(worktree.handle.clone()),
            Some(vec![format!("Provider reported branch `{}`; refresh provider metadata and retry.", worktree.branch)]),
        ));
    }
    Ok(())
}

fn trusted_unpushed_destination_matches(
    path: &std::path::Path,
    trusted: Option<&TrustedUnpushedWorktree>,
) -> bool {
    let Some(trusted) = trusted else {
        return false;
    };
    let Ok(path) = std::fs::canonicalize(path) else {
        return false;
    };
    let Ok(trusted_path) = std::fs::canonicalize(&trusted.path) else {
        return false;
    };
    path == trusted_path
        && crate::git::run_git(
            &path,
            &["rev-parse", "--verify", "HEAD^{commit}"],
            "verify trusted unpushed worktree HEAD",
        )
        .ok()
        .is_some_and(|head| head.trim() == trusted.head)
}

pub fn cleanup_worktree_providers_from_config(
    options: WorktreeProviderCleanupOptions,
    config: HomeboyConfig,
) -> Result<WorktreeProviderCleanupOutput> {
    validate_selection(&options)?;

    let mode = if options.apply {
        WorktreeProviderCleanupMode::Apply
    } else {
        WorktreeProviderCleanupMode::Preview
    };

    let providers = selected_providers(&options, &config)?;
    let mut results = Vec::new();

    for (provider_id, provider_config) in providers {
        results.push(run_provider_cleanup(
            &provider_id,
            provider_config,
            mode.clone(),
        ));
    }

    let success_count = results.iter().filter(|row| row.success).count();
    let failure_count = results.len().saturating_sub(success_count);

    Ok(WorktreeProviderCleanupOutput {
        command: "cleanup.worktrees",
        mode,
        provider_count: results.len(),
        success_count,
        failure_count,
        providers: results,
    })
}

fn validate_selection(options: &WorktreeProviderCleanupOptions) -> Result<()> {
    if options.all_providers && !options.provider.is_empty() {
        return Err(Error::validation_invalid_argument(
            "provider",
            "--provider cannot be combined with --all-providers",
            None,
            None,
        ));
    }
    if !options.all_providers && options.provider.is_empty() {
        return Err(Error::validation_missing_argument(vec![
            "--provider <id> or --all-providers".to_string(),
        ]));
    }
    Ok(())
}

fn selected_providers<'a>(
    options: &WorktreeProviderCleanupOptions,
    config: &'a HomeboyConfig,
) -> Result<Vec<(String, &'a WorktreeProviderConfig)>> {
    if options.all_providers {
        let sorted: BTreeMap<_, _> = config.worktree_providers.iter().collect();
        return Ok(sorted
            .into_iter()
            .filter_map(|(id, provider)| provider.enabled.then_some((id.clone(), provider)))
            .collect());
    }

    let mut providers = Vec::new();
    for provider_id in &options.provider {
        let Some(provider_config) = config.worktree_providers.get(provider_id) else {
            return Err(Error::validation_invalid_argument(
                "provider",
                format!("unknown worktree provider '{provider_id}'"),
                Some(provider_id.clone()),
                Some(config.worktree_providers.keys().cloned().collect()),
            ));
        };
        providers.push((provider_id.clone(), provider_config));
    }
    Ok(providers)
}

fn run_provider_cleanup(
    provider_id: &str,
    provider_config: &WorktreeProviderConfig,
    mode: WorktreeProviderCleanupMode,
) -> WorktreeProviderCleanupResult {
    if !provider_config.enabled {
        return provider_failure(provider_id, mode, "provider is disabled");
    }

    match provider_config.kind {
        WorktreeProviderKind::Command => {
            run_command_provider_cleanup(provider_id, provider_config, mode)
        }
    }
}

fn run_command_provider_cleanup(
    provider_id: &str,
    provider_config: &WorktreeProviderConfig,
    mode: WorktreeProviderCleanupMode,
) -> WorktreeProviderCleanupResult {
    if mode == WorktreeProviderCleanupMode::Apply && !provider_config.apply_enabled {
        return provider_failure(provider_id, mode, "provider apply is not enabled");
    }

    let command = match mode {
        WorktreeProviderCleanupMode::Preview => &provider_config.commands.cleanup_preview,
        WorktreeProviderCleanupMode::Apply => &provider_config.commands.cleanup_apply,
    };

    let Some(command) = command.as_ref() else {
        return provider_failure(
            provider_id,
            mode,
            "provider cleanup command is not configured",
        );
    };
    if command.is_empty() || command[0].trim().is_empty() {
        return provider_failure(
            provider_id,
            mode,
            "provider command argv must include an executable",
        );
    }

    match Command::new(&command[0])
        .args(&command[1..])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(mut child) => {
            let stdout = child.stdout.take();
            let stderr = child.stderr.take();
            let stdout_lines = Arc::new(Mutex::new(Vec::new()));
            let stderr_lines = Arc::new(Mutex::new(Vec::new()));

            let stdout_handle = stdout.map(|stream| {
                collect_provider_stream(
                    provider_id.to_string(),
                    "stdout",
                    stream,
                    Arc::clone(&stdout_lines),
                )
            });
            let stderr_handle = stderr.map(|stream| {
                collect_provider_stream(
                    provider_id.to_string(),
                    "stderr",
                    stream,
                    Arc::clone(&stderr_lines),
                )
            });

            let wait_result = child.wait();
            if let Some(handle) = stdout_handle {
                let _ = handle.join();
            }
            if let Some(handle) = stderr_handle {
                let _ = handle.join();
            }

            let stdout = joined_lines(&stdout_lines);
            let stderr = joined_lines(&stderr_lines);
            let parsed_payload = parse_json_stdout(&stdout);
            let phase = provider_phase(&parsed_payload, &mode);
            let last_progress = provider_last_progress(&parsed_payload)
                .or_else(|| last_non_empty_line(&stdout))
                .or_else(|| last_non_empty_line(&stderr));
            let run_refs = provider_run_refs(&parsed_payload);
            let follow_up_command = provider_follow_up_command(&run_refs);

            let output_status = match wait_result {
                Ok(status) => status,
                Err(err) => {
                    return WorktreeProviderCleanupResult {
                        provider_id: provider_id.to_string(),
                        success: false,
                        mode,
                        command_run: Some(command.clone()),
                        status: None,
                        stdout,
                        stderr,
                        parsed_payload,
                        phase,
                        last_progress,
                        run_refs,
                        follow_up_command,
                        error: Some(format!("failed to wait for provider command: {err}")),
                    };
                }
            };
            let status = output_status.code();
            WorktreeProviderCleanupResult {
                provider_id: provider_id.to_string(),
                success: output_status.success(),
                mode,
                command_run: Some(command.clone()),
                status,
                stdout,
                stderr,
                parsed_payload,
                phase,
                last_progress,
                run_refs,
                follow_up_command,
                error: output_status
                    .success()
                    .then_some(())
                    .is_none()
                    .then(|| "provider command failed".to_string()),
            }
        }
        Err(err) => {
            let phase = Some(mode_phase(&mode).to_string());
            WorktreeProviderCleanupResult {
                provider_id: provider_id.to_string(),
                success: false,
                mode,
                command_run: Some(command.clone()),
                status: None,
                stdout: String::new(),
                stderr: String::new(),
                parsed_payload: None,
                phase,
                last_progress: None,
                run_refs: Vec::new(),
                follow_up_command: None,
                error: Some(format!("failed to execute provider command: {err}")),
            }
        }
    }
}

fn collect_provider_stream<R>(
    provider_id: String,
    stream_name: &'static str,
    stream: R,
    lines: Arc<Mutex<Vec<String>>>,
) -> thread::JoinHandle<()>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let reader = BufReader::new(stream);
        for line in reader.lines().map_while(std::result::Result::ok) {
            eprintln!("[cleanup.worktrees provider={provider_id} stream={stream_name}] {line}");
            if let Ok(mut guard) = lines.lock() {
                guard.push(line);
            }
        }
    })
}

fn joined_lines(lines: &Arc<Mutex<Vec<String>>>) -> String {
    lines
        .lock()
        .map(|guard| guard.join("\n"))
        .unwrap_or_default()
}

fn last_non_empty_line(output: &str) -> Option<String> {
    output
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(ToString::to_string)
}

fn provider_failure(
    provider_id: &str,
    mode: WorktreeProviderCleanupMode,
    error: &str,
) -> WorktreeProviderCleanupResult {
    let phase = Some(mode_phase(&mode).to_string());
    WorktreeProviderCleanupResult {
        provider_id: provider_id.to_string(),
        success: false,
        mode,
        command_run: None,
        status: None,
        stdout: String::new(),
        stderr: String::new(),
        parsed_payload: None,
        phase,
        last_progress: None,
        run_refs: Vec::new(),
        follow_up_command: None,
        error: Some(error.to_string()),
    }
}

fn parse_json_stdout(stdout: &str) -> Option<Value> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str(trimmed).ok().or_else(|| {
        trimmed
            .lines()
            .rev()
            .map(str::trim)
            .find_map(|line| serde_json::from_str(line).ok())
    })
}

fn provider_phase(payload: &Option<Value>, mode: &WorktreeProviderCleanupMode) -> Option<String> {
    payload
        .as_ref()
        .and_then(|payload| first_string_for_keys(payload, &["phase", "state", "status"]))
        .or_else(|| Some(mode_phase(mode).to_string()))
}

fn provider_last_progress(payload: &Option<Value>) -> Option<String> {
    payload.as_ref().and_then(|payload| {
        first_string_for_keys(
            payload,
            &[
                "last_progress",
                "progress",
                "message",
                "summary",
                "last_observed_progress",
            ],
        )
    })
}

fn provider_run_refs(payload: &Option<Value>) -> Vec<WorktreeProviderRunRef> {
    let Some(payload) = payload else {
        return Vec::new();
    };
    let mut run_ids = Vec::new();
    let mut status_commands = Vec::new();
    collect_strings_for_keys(
        payload,
        &["run_id", "runId", "durable_run_id"],
        &mut run_ids,
    );
    collect_strings_for_keys(
        payload,
        &["status_command", "statusCommand", "status_cmd"],
        &mut status_commands,
    );
    collect_status_commands_from_arrays(payload, &mut status_commands);

    let len = run_ids.len().max(status_commands.len());
    (0..len)
        .map(|index| WorktreeProviderRunRef {
            run_id: run_ids.get(index).cloned(),
            status_command: status_commands.get(index).cloned(),
        })
        .collect()
}

fn provider_follow_up_command(refs: &[WorktreeProviderRunRef]) -> Option<String> {
    refs.iter().find_map(|row| row.status_command.clone())
}

fn mode_phase(mode: &WorktreeProviderCleanupMode) -> &'static str {
    match mode {
        WorktreeProviderCleanupMode::Preview => "preview",
        WorktreeProviderCleanupMode::Apply => "apply",
    }
}

fn first_string_for_keys(value: &Value, keys: &[&str]) -> Option<String> {
    match value {
        Value::Object(map) => {
            for key in keys {
                if let Some(value) = map.get(*key).and_then(Value::as_str) {
                    return Some(value.to_string());
                }
            }
            map.values()
                .find_map(|value| first_string_for_keys(value, keys))
        }
        Value::Array(values) => values
            .iter()
            .find_map(|value| first_string_for_keys(value, keys)),
        _ => None,
    }
}

fn collect_strings_for_keys(value: &Value, keys: &[&str], out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for key in keys {
                if let Some(value) = map.get(*key).and_then(Value::as_str) {
                    if !out.contains(&value.to_string()) {
                        out.push(value.to_string());
                    }
                }
            }
            for value in map.values() {
                collect_strings_for_keys(value, keys, out);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_strings_for_keys(value, keys, out);
            }
        }
        _ => {}
    }
}

fn collect_status_commands_from_arrays(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for key in [
                "status_commands",
                "statusCommands",
                "next_commands",
                "nextCommands",
            ] {
                if let Some(values) = map.get(key).and_then(Value::as_array) {
                    for value in values {
                        if let Some(command) = value.as_str().or_else(|| {
                            value
                                .get("command")
                                .and_then(Value::as_str)
                                .or_else(|| value.get("status_command").and_then(Value::as_str))
                        }) {
                            if !out.contains(&command.to_string()) {
                                out.push(command.to_string());
                            }
                        }
                    }
                }
            }
            for value in map.values() {
                collect_status_commands_from_arrays(value, out);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_status_commands_from_arrays(value, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;

    use serde_json::json;

    use super::*;
    use crate::defaults::WorktreeProviderCommands;

    #[test]
    fn deserializes_worktree_provider_config() {
        let config: HomeboyConfig = serde_json::from_value(json!({
            "worktree_providers": {
                "fixture": {
                    "enabled": true,
                    "kind": "command",
                    "apply_enabled": true,
                    "commands": {
                        "cleanup_preview": ["fixture-bin", "preview"],
                        "cleanup_apply": ["fixture-bin", "apply"],
                        "artifacts_preview": ["fixture-bin", "artifacts-preview"]
                    },
                    "list_result_mapping": {
                        "items": "$.result.items",
                        "handle": "$.id",
                        "path": "$.checkout.path",
                        "branch": "$.checkout.branch",
                        "dirty": "$.safety.dirty",
                        "unpushed": "$.safety.unpushed",
                        "primary": "$.safety.primary"
                    }
                }
            }
        }))
        .expect("config deserializes");

        let provider = config.worktree_providers.get("fixture").expect("provider");
        assert!(provider.enabled);
        assert_eq!(provider.kind, WorktreeProviderKind::Command);
        assert!(provider.apply_enabled);
        assert_eq!(
            provider.commands.cleanup_preview.as_ref().expect("command"),
            &vec!["fixture-bin".to_string(), "preview".to_string()]
        );
        assert_eq!(
            provider
                .list_result_mapping
                .as_ref()
                .expect("list result mapping")
                .items,
            "$.result.items"
        );
    }

    #[test]
    fn dry_run_executes_preview_command_and_parses_json() {
        let script = fake_provider_script();
        let output = cleanup_worktree_providers_from_config(
            WorktreeProviderCleanupOptions {
                provider: vec!["fixture".to_string()],
                all_providers: false,
                apply: false,
            },
            config_with_provider(WorktreeProviderConfig {
                enabled: true,
                kind: WorktreeProviderKind::Command,
                apply_enabled: false,
                commands: WorktreeProviderCommands {
                    cleanup_preview: Some(vec![script, "preview".to_string()]),
                    cleanup_apply: None,
                    ..Default::default()
                },
                list_result_mapping: None,
            }),
        )
        .expect("cleanup succeeds");

        assert_eq!(output.mode, WorktreeProviderCleanupMode::Preview);
        assert_eq!(output.success_count, 1);
        assert_eq!(output.failure_count, 0);
        assert_eq!(output.providers[0].status, Some(0));
        assert_eq!(
            output.providers[0].parsed_payload,
            Some(json!({ "mode": "preview" }))
        );
    }

    #[test]
    fn apply_refuses_when_provider_apply_is_disabled() {
        let script = fake_provider_script();
        let output = cleanup_worktree_providers_from_config(
            WorktreeProviderCleanupOptions {
                provider: vec!["fixture".to_string()],
                all_providers: false,
                apply: true,
            },
            config_with_provider(WorktreeProviderConfig {
                enabled: true,
                kind: WorktreeProviderKind::Command,
                apply_enabled: false,
                commands: WorktreeProviderCommands {
                    cleanup_apply: Some(vec![script, "apply".to_string()]),
                    ..Default::default()
                },
                list_result_mapping: None,
            }),
        )
        .expect("cleanup reports refusal");

        assert_eq!(output.success_count, 0);
        assert_eq!(output.failure_count, 1);
        assert_eq!(output.providers[0].command_run, None);
        assert_eq!(
            output.providers[0].error.as_deref(),
            Some("provider apply is not enabled")
        );
    }

    #[test]
    fn apply_executes_apply_command_when_enabled() {
        let script = fake_provider_script();
        let output = cleanup_worktree_providers_from_config(
            WorktreeProviderCleanupOptions {
                provider: vec!["fixture".to_string()],
                all_providers: false,
                apply: true,
            },
            config_with_provider(WorktreeProviderConfig {
                enabled: true,
                kind: WorktreeProviderKind::Command,
                apply_enabled: true,
                commands: WorktreeProviderCommands {
                    cleanup_apply: Some(vec![script, "apply".to_string()]),
                    ..Default::default()
                },
                list_result_mapping: None,
            }),
        )
        .expect("cleanup succeeds");

        assert_eq!(output.mode, WorktreeProviderCleanupMode::Apply);
        assert_eq!(output.success_count, 1);
        assert_eq!(
            output.providers[0].parsed_payload,
            Some(json!({ "mode": "apply" }))
        );
    }

    #[test]
    fn resolves_a_clean_provider_managed_handle_with_targeted_command() {
        let workspace = tempfile::tempdir().expect("workspace");
        git_init(workspace.path(), "cook-target");
        let script = fake_list_provider_script(serde_json::json!({
            "worktrees": [{
                "handle": "fixture@cook-target",
                "path": workspace.path(),
                "branch": "cook-target",
                "safety": { "dirty": false, "unpushed": false, "primary": false }
            }]
        }));
        let handle = resolve_worktree_provider_handle_from_config(
            "fixture@cook-target",
            &config_with_provider(WorktreeProviderConfig {
                enabled: true,
                kind: WorktreeProviderKind::Command,
                apply_enabled: false,
                commands: WorktreeProviderCommands {
                    resolve: Some(vec![script, "{handle}".to_string()]),
                    ..Default::default()
                },
                list_result_mapping: Some(worktrees_mapping()),
            }),
        )
        .expect("provider handle resolves");

        assert_eq!(handle.path, workspace.path().display().to_string());
        assert_eq!(handle.branch, "cook-target");
    }

    #[test]
    fn resolution_retains_the_selected_provider_identity() {
        let workspace = tempfile::tempdir().expect("workspace");
        git_init(workspace.path(), "cook-target");
        let script = fake_list_provider_script(serde_json::json!({
            "worktrees": [{
                "handle": "fixture@cook-target",
                "path": workspace.path(),
                "branch": "cook-target",
                "safety": { "dirty": false, "unpushed": false, "primary": false }
            }]
        }));

        let resolution = resolve_worktree_provider_from_config(
            "fixture@cook-target",
            &config_with_provider(list_provider(script, worktrees_mapping())),
        )
        .expect("provider handle resolves");

        assert_eq!(resolution.provider_id, "fixture");
        assert_eq!(
            resolution.worktree.path,
            workspace.path().display().to_string()
        );
    }

    #[test]
    fn unresolved_handle_reports_sorted_configured_provider_ids() {
        let mut config = HomeboyConfig::default();
        for provider_id in ["zeta", "alpha"] {
            config.worktree_providers.insert(
                provider_id.to_string(),
                WorktreeProviderConfig {
                    enabled: false,
                    kind: WorktreeProviderKind::Command,
                    apply_enabled: false,
                    commands: WorktreeProviderCommands::default(),
                    list_result_mapping: None,
                },
            );
        }

        let error = resolve_worktree_provider_from_config("fixture@missing", &config)
            .expect_err("missing handle fails");

        assert!(
            error
                .message
                .contains("configured provider(s): alpha, zeta"),
            "{}",
            error.message
        );
    }

    #[test]
    fn resolve_does_not_invoke_list_when_both_commands_are_configured() {
        let workspace = tempfile::tempdir().expect("workspace");
        git_init(workspace.path(), "cook-target");
        let marker = workspace.path().join("list-invoked");
        let resolve = fake_list_provider_script(serde_json::json!({
            "worktrees": [{
                "handle": "fixture@cook-target",
                "path": workspace.path(),
                "branch": "cook-target",
                "safety": { "dirty": false, "unpushed": false, "primary": false }
            }]
        }));
        let list =
            fake_list_provider_script_with_marker(serde_json::json!({ "worktrees": [] }), &marker);

        let handle = resolve_worktree_provider_handle_from_config(
            "fixture@cook-target",
            &config_with_provider(WorktreeProviderConfig {
                enabled: true,
                kind: WorktreeProviderKind::Command,
                apply_enabled: false,
                commands: WorktreeProviderCommands {
                    resolve: Some(vec![resolve, "{handle}".to_string()]),
                    list: Some(vec![list]),
                    ..Default::default()
                },
                list_result_mapping: Some(worktrees_mapping()),
            }),
        )
        .expect("targeted resolve succeeds");

        assert_eq!(handle.handle, "fixture@cook-target");
        assert!(!marker.exists(), "list command must not be invoked");
    }

    #[test]
    fn list_is_the_compatibility_fallback_when_resolve_is_unavailable() {
        let workspace = tempfile::tempdir().expect("workspace");
        git_init(workspace.path(), "cook-target");
        let marker = workspace.path().join("list-invoked");
        let list = fake_list_provider_script_with_marker(
            serde_json::json!({
                "worktrees": [{
                    "handle": "fixture@cook-target",
                    "path": workspace.path(),
                    "branch": "cook-target",
                    "safety": { "dirty": false, "unpushed": false, "primary": false }
                }]
            }),
            &marker,
        );

        resolve_worktree_provider_handle_from_config(
            "fixture@cook-target",
            &config_with_provider(list_provider(list, worktrees_mapping())),
        )
        .expect("list fallback succeeds");

        assert!(marker.exists(), "list fallback must be invoked");
    }

    #[test]
    fn resolve_failure_preserves_command_classification() {
        let script = fake_failing_provider_script();
        let err = resolve_worktree_provider_handle_from_config(
            "fixture@cook-target",
            &config_with_provider(WorktreeProviderConfig {
                enabled: true,
                kind: WorktreeProviderKind::Command,
                apply_enabled: false,
                commands: WorktreeProviderCommands {
                    resolve: Some(vec![script, "{handle}".to_string()]),
                    ..Default::default()
                },
                list_result_mapping: Some(worktrees_mapping()),
            }),
        )
        .expect_err("failed resolve must be reported");

        assert!(err
            .message
            .contains("resolve command failed with exit code 23"));
        assert!(!err.message.contains("invalid JSON"));
        assert_eq!(err.details["command_evidence"]["exit_code"], 23);
        assert_eq!(
            err.details["command_evidence"]["stderr"],
            "provider failed\n"
        );
    }

    #[test]
    fn rejects_provider_handles_with_unsafe_safety_metadata() {
        let workspace = tempfile::tempdir().expect("workspace");
        git_init(workspace.path(), "cook-target");
        let script = fake_list_provider_script(serde_json::json!({
            "worktrees": [{
                "handle": "fixture@cook-target",
                "path": workspace.path(),
                "branch": "cook-target",
                "safety": { "dirty": true, "unpushed": false, "primary": false }
            }]
        }));
        let err = resolve_worktree_provider_handle_from_config(
            "fixture@cook-target",
            &config_with_provider(WorktreeProviderConfig {
                enabled: true,
                kind: WorktreeProviderKind::Command,
                apply_enabled: false,
                commands: WorktreeProviderCommands {
                    list: Some(vec![script]),
                    ..Default::default()
                },
                list_result_mapping: Some(worktrees_mapping()),
            }),
        )
        .expect_err("dirty provider handle must be rejected");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("dirty"));
    }

    #[test]
    fn maps_differently_nested_provider_envelopes_from_configuration() {
        let workspace = tempfile::tempdir().expect("workspace");
        git_init(workspace.path(), "cook-target");
        let cases = [
            (
                json!({ "result": { "items": [{
                    "id": "fixture@cook-target", "checkout": { "path": workspace.path(), "branch": "cook-target" },
                    "state": { "dirty": false, "unpushed": false, "primary": false }
                }]}}),
                WorktreeProviderListResultMapping {
                    items: "$.result.items".to_string(),
                    handle: "$.id".to_string(),
                    path: "$.checkout.path".to_string(),
                    branch: "$.checkout.branch".to_string(),
                    dirty: "$.state.dirty".to_string(),
                    unpushed: "$.state.unpushed".to_string(),
                    primary: "$.state.primary".to_string(),
                },
            ),
            (
                json!({ "payload": [{
                    "name": "fixture@cook-target", "location": workspace.path(), "ref": "cook-target",
                    "dirty": false, "unpushed": false, "primary": false
                }]}),
                WorktreeProviderListResultMapping {
                    items: "$.payload".to_string(),
                    handle: "$.name".to_string(),
                    path: "$.location".to_string(),
                    branch: "$.ref".to_string(),
                    dirty: "$.dirty".to_string(),
                    unpushed: "$.unpushed".to_string(),
                    primary: "$.primary".to_string(),
                },
            ),
        ];

        for (payload, mapping) in cases {
            let script = fake_list_provider_script(payload);
            let handle = resolve_worktree_provider_handle_from_config(
                "fixture@cook-target",
                &config_with_provider(list_provider(script, mapping)),
            )
            .expect("configured envelope resolves");
            assert_eq!(handle.path, workspace.path().display().to_string());
        }
    }

    #[test]
    fn rejects_malformed_or_incomplete_provider_mappings() {
        let payload = json!({ "items": [{
            "handle": "fixture@cook-target", "path": "/tmp/fixture", "branch": "cook-target",
            "dirty": false, "unpushed": false, "primary": false
        }] });
        let cases = [
            ("items", "not a jsonpath", "is not valid JSONPath"),
            ("path", "$.missing", "did not resolve a value"),
            ("dirty", "$.handle", "must resolve to a boolean"),
        ];

        for (field, path, expected) in cases {
            let mut mapping = WorktreeProviderListResultMapping {
                items: "$.items".to_string(),
                handle: "$.handle".to_string(),
                path: "$.path".to_string(),
                branch: "$.branch".to_string(),
                dirty: "$.dirty".to_string(),
                unpushed: "$.unpushed".to_string(),
                primary: "$.primary".to_string(),
            };
            *match field {
                "items" => &mut mapping.items,
                "path" => &mut mapping.path,
                "dirty" => &mut mapping.dirty,
                _ => unreachable!(),
            } = path.to_string();
            let err = resolve_worktree_provider_handle_from_config(
                "fixture@cook-target",
                &config_with_provider(list_provider(
                    fake_list_provider_script(payload.clone()),
                    mapping,
                )),
            )
            .expect_err("invalid mapping must fail closed");
            assert!(err.message.contains(expected), "{}", err.message);
        }
    }

    #[test]
    fn absent_safety_flags_default_to_permissive_instead_of_failing_the_cook() {
        // The DMC worktree provider omits `safety.dirty` (and can omit the whole
        // `safety` object). A missing advisory safety flag is not a claim of
        // unsafety, so it must default to `false` and not reject the cook
        // pre-dispatch (#7886).
        let workspace = tempfile::tempdir().expect("workspace");
        git_init(workspace.path(), "cook-target");

        // Response omits `safety.dirty` (only reports unpushed/primary).
        let partial_safety = json!({ "worktrees": [{
            "handle": "fixture@cook-target", "path": workspace.path(), "branch": "cook-target",
            "safety": { "unpushed": false, "primary": false }
        }]});
        // Response omits the `safety` object entirely.
        let no_safety = json!({ "worktrees": [{
            "handle": "fixture@cook-target", "path": workspace.path(), "branch": "cook-target"
        }]});

        for payload in [partial_safety, no_safety] {
            let handle = resolve_worktree_provider_handle_from_config(
                "fixture@cook-target",
                &config_with_provider(list_provider(
                    fake_list_provider_script(payload),
                    worktrees_mapping(),
                )),
            )
            .expect("absent safety flags resolve without failing the cook");
            assert!(!handle.safety.dirty);
            assert!(!handle.safety.unpushed);
            assert!(!handle.safety.primary);
        }

        // A present-but-non-boolean safety value is still a contract error.
        let wrong_type = json!({ "worktrees": [{
            "handle": "fixture@cook-target", "path": workspace.path(), "branch": "cook-target",
            "safety": { "dirty": "yes", "unpushed": false, "primary": false }
        }]});
        let err = resolve_worktree_provider_handle_from_config(
            "fixture@cook-target",
            &config_with_provider(list_provider(
                fake_list_provider_script(wrong_type),
                worktrees_mapping(),
            )),
        )
        .expect_err("a non-boolean safety value is still rejected");
        assert!(
            err.message.contains("must resolve to a boolean"),
            "{}",
            err.message
        );
    }

    #[test]
    fn rejects_unsafe_and_mismatched_provider_metadata() {
        let workspace = tempfile::tempdir().expect("workspace");
        git_init(workspace.path(), "cook-target");
        for (field, value, expected) in [
            ("dirty", json!(true), "dirty"),
            ("unpushed", json!(true), "unpushed"),
            ("primary", json!(true), "primary"),
            ("branch", json!("wrong-branch"), "does not match"),
        ] {
            let mut row = json!({
                "handle": "fixture@cook-target", "path": workspace.path(), "branch": "cook-target",
                "safety": { "dirty": false, "unpushed": false, "primary": false }
            });
            if field == "branch" {
                row[field] = value;
            } else {
                row["safety"][field] = value;
            }
            let err = resolve_worktree_provider_handle_from_config(
                "fixture@cook-target",
                &config_with_provider(list_provider(
                    fake_list_provider_script(json!({ "worktrees": [row] })),
                    worktrees_mapping(),
                )),
            )
            .expect_err("unsafe metadata must fail closed");
            assert!(err.message.contains(expected), "{}", err.message);
        }
    }

    #[test]
    fn trusted_immutable_destination_is_the_only_unpushed_apply_exception() {
        let workspace = tempfile::tempdir().expect("workspace");
        git_init(workspace.path(), "cook-target");
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(workspace.path())
                .output()
                .expect("run git");
            assert!(output.status.success());
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        };
        git(&["config", "user.email", "agent@example.test"]);
        git(&["config", "user.name", "Agent"]);
        std::fs::write(workspace.path().join("candidate"), "candidate\n").expect("write candidate");
        git(&["add", "candidate"]);
        git(&["commit", "-m", "candidate"]);
        let head = git(&["rev-parse", "HEAD"]);
        let mut config = config_with_provider(list_provider(
            fake_list_provider_script(json!({ "worktrees": [{
                "handle": "fixture@cook-target", "path": workspace.path(), "branch": "cook-target",
                "safety": { "dirty": false, "unpushed": true, "primary": false }
            }]})),
            worktrees_mapping(),
        ));
        config
            .worktree_providers
            .get_mut("fixture")
            .expect("fixture provider")
            .apply_enabled = true;

        let rejected = resolve_apply_enabled_worktree_provider_from_config(
            "fixture@cook-target",
            &config,
            None,
        )
        .expect_err("ordinary unpushed destination remains blocked");
        assert!(rejected.message.contains("unpushed"));

        let resolved =
            resolve_apply_enabled_worktree_provider_with_trusted_unpushed_destination_from_config(
                "fixture@cook-target",
                &config,
                None,
                Some(&TrustedUnpushedWorktree {
                    path: workspace.path().to_path_buf(),
                    head: head.clone(),
                }),
            )
            .expect("exact immutable destination is allowed before finalizer push");
        assert_eq!(
            resolved.worktree.path,
            workspace.path().display().to_string()
        );

        let stale =
            resolve_apply_enabled_worktree_provider_with_trusted_unpushed_destination_from_config(
                "fixture@cook-target",
                &config,
                None,
                Some(&TrustedUnpushedWorktree {
                    path: workspace.path().to_path_buf(),
                    head: format!("{head}0"),
                }),
            )
            .expect_err("different candidate commit remains blocked");
        assert!(stale.message.contains("unpushed"));
    }

    #[test]
    fn provider_apply_captures_phase_progress_and_durable_refs() {
        let script = fake_provider_script_with_refs();
        let output = cleanup_worktree_providers_from_config(
            WorktreeProviderCleanupOptions {
                provider: vec!["fixture".to_string()],
                all_providers: false,
                apply: true,
            },
            config_with_provider(WorktreeProviderConfig {
                enabled: true,
                kind: WorktreeProviderKind::Command,
                apply_enabled: true,
                commands: WorktreeProviderCommands {
                    cleanup_apply: Some(vec![script]),
                    ..Default::default()
                },
                list_result_mapping: None,
            }),
        )
        .expect("cleanup succeeds");

        let provider = &output.providers[0];
        assert_eq!(provider.phase.as_deref(), Some("running"));
        assert_eq!(provider.last_progress.as_deref(), Some("removed 10/20"));
        assert_eq!(provider.run_refs.len(), 1);
        assert_eq!(
            provider.run_refs[0].run_id.as_deref(),
            Some("cleanup-run-1")
        );
        assert_eq!(
            provider.run_refs[0].status_command.as_deref(),
            Some("provider status cleanup-run-1")
        );
        assert_eq!(
            provider.follow_up_command.as_deref(),
            Some("provider status cleanup-run-1")
        );
    }

    fn config_with_provider(provider: WorktreeProviderConfig) -> HomeboyConfig {
        let mut providers = HashMap::new();
        providers.insert("fixture".to_string(), provider);
        HomeboyConfig {
            worktree_providers: providers,
            ..HomeboyConfig::default()
        }
    }

    fn worktrees_mapping() -> WorktreeProviderListResultMapping {
        WorktreeProviderListResultMapping {
            items: "$.worktrees".to_string(),
            handle: "$.handle".to_string(),
            path: "$.path".to_string(),
            branch: "$.branch".to_string(),
            dirty: "$.safety.dirty".to_string(),
            unpushed: "$.safety.unpushed".to_string(),
            primary: "$.safety.primary".to_string(),
        }
    }

    fn list_provider(
        script: String,
        mapping: WorktreeProviderListResultMapping,
    ) -> WorktreeProviderConfig {
        WorktreeProviderConfig {
            enabled: true,
            kind: WorktreeProviderKind::Command,
            apply_enabled: false,
            commands: WorktreeProviderCommands {
                list: Some(vec![script]),
                ..Default::default()
            },
            list_result_mapping: Some(mapping),
        }
    }

    /// Shared, process-wide root for fixture provider scripts.
    ///
    /// Each fixture script needs a stable on-disk path that outlives the helper
    /// that creates it (the test executes it later). Previously each helper
    /// `.keep()`-ed its own `tempfile::tempdir()`, which permanently disables
    /// `TempDir`'s `Drop` cleanup — leaking one directory per fixture on every
    /// run (see #9173 follow-up). Instead, anchor all fixture scripts under a
    /// single `TempDir` owned by this `OnceLock`: it is created once, cleans up
    /// when the test process exits normally, and is `hb-test-` prefixed so the
    /// startup sweep (#9177) reclaims it even if the process is killed.
    fn fixture_script_root() -> &'static std::path::Path {
        static ROOT: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
        ROOT.get_or_init(|| {
            tempfile::Builder::new()
                .prefix("hb-test-worktree-fixtures-")
                .tempdir()
                .expect("fixture script root tempdir")
        })
        .path()
    }

    /// Allocate a fresh, unique subdirectory under [`fixture_script_root`] for a
    /// single fixture script. Uniqueness avoids collisions between fixtures
    /// within one test run; cleanup is handled by the shared root.
    fn unique_fixture_script_dir() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = fixture_script_root().join(format!("fixture-{id}"));
        fs::create_dir_all(&dir).expect("create fixture script dir");
        dir
    }

    fn fake_provider_script() -> String {
        let dir = unique_fixture_script_dir();
        let script = dir.join("provider");
        fs::write(&script, "#!/bin/sh\nprintf '{\"mode\":\"%s\"}\n' \"$1\"\n")
            .expect("write script");
        make_executable(&script);
        script.to_string_lossy().to_string()
    }

    fn fake_provider_script_with_refs() -> String {
        let dir = unique_fixture_script_dir();
        let script = dir.join("provider");
        fs::write(
            &script,
            concat!(
                "#!/bin/sh\n",
                "printf 'starting cleanup\\n' >&2\n",
                "printf '{\"phase\":\"running\",\"last_progress\":\"removed 10/20\",\"run_id\":\"cleanup-run-1\",\"status_command\":\"provider status cleanup-run-1\"}\\n'\n"
            ),
        )
        .expect("write script");
        make_executable(&script);
        script.to_string_lossy().to_string()
    }

    fn fake_list_provider_script(output: Value) -> String {
        let dir = unique_fixture_script_dir();
        let script = dir.join("provider");
        fs::write(&script, format!("#!/bin/sh\nprintf '%s\\n' '{}'\n", output))
            .expect("write script");
        make_executable(&script);
        script.to_string_lossy().to_string()
    }

    fn fake_list_provider_script_with_marker(output: Value, marker: &std::path::Path) -> String {
        let dir = unique_fixture_script_dir();
        let script = dir.join("provider");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\ntouch '{}'\nprintf '%s\\n' '{}'\n",
                marker.display(),
                output
            ),
        )
        .expect("write script");
        make_executable(&script);
        script.to_string_lossy().to_string()
    }

    fn fake_failing_provider_script() -> String {
        let dir = unique_fixture_script_dir();
        let script = dir.join("provider");
        fs::write(
            &script,
            "#!/bin/sh\nprintf 'provider failed\\n' >&2\nexit 23\n",
        )
        .expect("write script");
        make_executable(&script);
        script.to_string_lossy().to_string()
    }

    fn git_init(path: &std::path::Path, branch: &str) {
        let output = std::process::Command::new("git")
            .args(["init", "-b", branch])
            .current_dir(path)
            .output()
            .expect("initialize git repository");
        assert!(output.status.success());
    }

    #[cfg(unix)]
    fn make_executable(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).expect("chmod");
    }

    #[cfg(not(unix))]
    fn make_executable(_path: &std::path::Path) {}
}
