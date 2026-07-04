use crate::core::engine::shell;
use crate::core::error::{Error, ErrorCode, Result};
use crate::core::extension;
use crate::core::output::MergeOutput;
use crate::core::server::{self, SshClient};

use serde_json::Value;
use std::collections::BTreeMap;
#[cfg(test)]
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::super::{load, merge, remote_runner_homeboy_path};
use super::super::{
    materialize_runner_extension, RunnerExtensionMaterializationRequest,
    RunnerExtensionMaterializationSource,
};
use super::{Runner, RunnerKind};

pub(super) fn required_extensions_for_command(
    command: &[String],
    explicit: &[String],
) -> Vec<String> {
    let mut extensions = explicit
        .iter()
        .filter(|extension| !extension.trim().is_empty())
        .cloned()
        .collect::<Vec<_>>();

    let mut args = command.iter();
    while let Some(arg) = args.next() {
        if arg == "--extension" {
            if let Some(extension) = args.next().filter(|value| !value.trim().is_empty()) {
                push_unique(&mut extensions, extension.to_string());
            }
            continue;
        }
        if let Some(extension) = arg.strip_prefix("--extension=") {
            if !extension.trim().is_empty() {
                push_unique(&mut extensions, extension.to_string());
            }
        }
    }

    extensions
}

pub(super) fn requested_setting_keys_for_command(command: &[String]) -> Vec<String> {
    let mut keys = Vec::new();
    let mut args = command.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--setting" | "--setting-json" => {
                if let Some(value) = args.next() {
                    push_setting_key(&mut keys, value);
                }
            }
            _ => {
                if let Some(value) = arg.strip_prefix("--setting=") {
                    push_setting_key(&mut keys, value);
                } else if let Some(value) = arg.strip_prefix("--setting-json=") {
                    push_setting_key(&mut keys, value);
                }
            }
        }
    }

    keys
}

fn push_unique(items: &mut Vec<String>, item: String) {
    if !items.contains(&item) {
        items.push(item);
    }
}

fn push_setting_key(keys: &mut Vec<String>, value: &str) {
    let Some((key, _)) = value.split_once('=') else {
        return;
    };
    let key = key.trim();
    if !key.is_empty() {
        push_unique(keys, key.to_string());
    }
}

pub(super) fn validate_runner_extension_parity(
    runner_id: &str,
    runner: &Runner,
    cwd: &str,
    required_extensions: &[String],
    requested_setting_keys: &[String],
) -> Result<()> {
    for extension_id in required_extensions {
        validate_runner_extension(runner_id, runner, cwd, extension_id, requested_setting_keys)?;
    }

    Ok(())
}

fn validate_runner_extension(
    runner_id: &str,
    runner: &Runner,
    cwd: &str,
    extension_id: &str,
    requested_setting_keys: &[String],
) -> Result<()> {
    let homeboy_path = remote_runner_homeboy_path(runner, "runner extension parity preflight")?;
    let output = show_runner_extension(runner, cwd, homeboy_path, extension_id)?;

    if output.success {
        validate_runner_extension_ready(runner_id, homeboy_path, extension_id, &output.stdout)?;
        validate_runner_extension_settings(
            runner_id,
            homeboy_path,
            extension_id,
            &output.stdout,
            requested_setting_keys,
        )?;
        validate_runner_extension_core_compatibility(
            runner_id,
            homeboy_path,
            extension_id,
            &output.stdout,
        )?;
        if let Err(err) = validate_runner_extension_revision(
            runner_id,
            runner,
            homeboy_path,
            extension_id,
            &output.stdout,
        ) {
            if !is_stale_runner_extension_parity_error(&err) {
                return Err(err);
            }
            sync_runner_extension_revision(
                runner_id,
                runner,
                cwd,
                homeboy_path,
                extension_id,
                err,
            )?;
            let refreshed = show_runner_extension(runner, cwd, homeboy_path, extension_id)?;
            if refreshed.success {
                validate_runner_extension_parity_status(
                    runner_id,
                    homeboy_path,
                    extension_id,
                    &refreshed.stdout,
                )?;
                validate_runner_extension_settings(
                    runner_id,
                    homeboy_path,
                    extension_id,
                    &refreshed.stdout,
                    requested_setting_keys,
                )?;
                validate_runner_extension_core_compatibility(
                    runner_id,
                    homeboy_path,
                    extension_id,
                    &refreshed.stdout,
                )?;
                return Ok(());
            }
            return Err(missing_runner_extension_error(
                runner_id,
                homeboy_path,
                extension_id,
                &refreshed.stderr,
                &refreshed.stdout,
            ));
        }
        return Ok(());
    }

    Err(missing_runner_extension_error(
        runner_id,
        homeboy_path,
        extension_id,
        &output.stderr,
        &output.stdout,
    ))
}

/// Runs the full readiness + source-revision parity validation against a single
/// `extension show` stdout payload. Both the initial preflight and the
/// post-sync refresh re-check the same two parity invariants against their
/// respective stdout, so they share this helper.
fn validate_runner_extension_parity_status(
    runner_id: &str,
    homeboy_path: &str,
    extension_id: &str,
    remote_stdout: &str,
) -> Result<()> {
    validate_runner_extension_ready(runner_id, homeboy_path, extension_id, remote_stdout)?;
    let runner = super::super::load(runner_id)?;
    validate_runner_extension_revision(
        runner_id,
        &runner,
        homeboy_path,
        extension_id,
        remote_stdout,
    )
}

fn validate_runner_extension_settings(
    runner_id: &str,
    homeboy_path: &str,
    extension_id: &str,
    remote_stdout: &str,
    requested_setting_keys: &[String],
) -> Result<()> {
    if requested_setting_keys.is_empty() {
        return Ok(());
    }

    let metadata = remote_extension_metadata(remote_stdout);
    let declared = remote_extension_settings(remote_stdout);
    for key in requested_setting_keys {
        if !runner_extension_setting_declared(&declared, key) {
            return Err(unsupported_runner_extension_setting_error(
                runner_id,
                homeboy_path,
                extension_id,
                key,
                &metadata,
            ));
        }
    }

    Ok(())
}

fn runner_extension_setting_declared(declared: &BTreeMap<String, String>, key: &str) -> bool {
    if declared.contains_key(key) {
        return true;
    }

    let Some((parent, _)) = key.split_once('.') else {
        return false;
    };

    matches!(declared.get(parent).map(String::as_str), Some("object"))
}

fn validate_runner_extension_core_compatibility(
    runner_id: &str,
    homeboy_path: &str,
    extension_id: &str,
    remote_stdout: &str,
) -> Result<()> {
    let Some(report) = remote_extension_core_compatibility(remote_stdout) else {
        return Ok(());
    };
    if report.status != "incompatible" {
        return Ok(());
    }

    let constraint = report.requires_homeboy.as_deref().unwrap_or("<undeclared>");
    let source_revision = report.source_revision.as_deref().unwrap_or("<missing>");
    let command = format!("{homeboy_path} upgrade");
    Err(Error::new(
        ErrorCode::ValidationInvalidArgument,
        format!(
            "Invalid argument 'runner_extension': Runner '{runner_id}' has homeboy-core incompatible extension parity for '{extension_id}' before command execution"
        ),
        serde_json::json!({
            "field": "runner_extension",
            "problem": "homeboy_core.incompatible",
            "diagnostic": {
                "code": "homeboy_core.incompatible",
                "runner_id": runner_id,
                "extension_id": extension_id,
                "installed_homeboy": report.installed_homeboy,
                "requires_homeboy": constraint,
                "source_revision": source_revision,
                "remediation_command": command,
            },
            "tried": [
                format!("Runner homeboy version: {}", report.installed_homeboy),
                format!("Declared homeboy constraint: {constraint}"),
                format!("Runner extension source_revision: {source_revision}"),
                format!("Remediation: {command}"),
            ]
        }),
    ))
}

#[derive(Debug, Clone, serde::Deserialize)]
struct RemoteExtensionCoreCompatibility {
    status: String,
    installed_homeboy: String,
    requires_homeboy: Option<String>,
    source_revision: Option<String>,
}

fn remote_extension_core_compatibility(stdout: &str) -> Option<RemoteExtensionCoreCompatibility> {
    let value: Value = serde_json::from_str(stdout.trim()).ok()?;
    let extension = value.get("data").and_then(|data| data.get("extension"))?;
    serde_json::from_value(extension.get("core_compatibility")?.clone()).ok()
}

#[derive(Default)]
struct RemoteExtensionMetadata {
    path: Option<String>,
    source_revision: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct DevSyncExtensionOverlay {
    id: String,
    source_path: String,
    content_hash: String,
}

fn dev_sync_extension_overlay(
    runner: &Runner,
    extension_id: &str,
) -> Option<DevSyncExtensionOverlay> {
    let extensions = runner
        .resources
        .get("dev_sync")?
        .get("extensions")?
        .as_array()?;
    extensions.iter().find_map(|value| {
        let overlay: DevSyncExtensionOverlay = serde_json::from_value(value.clone()).ok()?;
        (overlay.id == extension_id).then_some(overlay)
    })
}

fn unsupported_runner_extension_setting_error(
    runner_id: &str,
    homeboy_path: &str,
    extension_id: &str,
    setting_key: &str,
    metadata: &RemoteExtensionMetadata,
) -> Error {
    let runner_path = metadata.path.as_deref().unwrap_or("<unknown>");
    let runner_revision = metadata.source_revision.as_deref().unwrap_or("<unknown>");
    Error::new(
        ErrorCode::ValidationInvalidArgument,
        format!(
            "Invalid argument 'runner_extension_setting': unsupported_setting: runner extension '{extension_id}' does not declare requested setting '{setting_key}'"
        ),
        serde_json::json!({
            "field": "runner_extension_setting",
            "problem": "unsupported_setting",
            "id": extension_id,
            "diagnostic": {
                "code": "runner_extension.unsupported_setting",
                "runner_id": runner_id,
                "extension_id": extension_id,
                "unsupported_setting_key": setting_key,
                "runner_extension_path": metadata.path,
                "runner_extension_source_revision": metadata.source_revision,
                "repair_hint": format!(
                    "Update, relink, or refresh the active runner extension so its manifest declares `{setting_key}` before dispatch: {homeboy_path} extension update {extension_id} or {homeboy_path} extension relink {extension_id} <source>"
                )
            },
            "tried": [
                format!("Runner extension id: {extension_id}"),
                format!("Runner extension path: {runner_path}"),
                format!("Runner extension source_revision: {runner_revision}"),
                format!("Unsupported setting key: {setting_key}"),
                format!("Repair: update, relink, or refresh the active runner extension so its manifest declares `{setting_key}` before dispatch."),
            ]
        }),
    )
}

fn show_runner_extension(
    runner: &Runner,
    cwd: &str,
    homeboy_path: &str,
    extension_id: &str,
) -> Result<server::CommandOutput> {
    let command = format!(
        "cd {} && {} extension show {}",
        shell::quote_path(cwd),
        shell::quote_path(homeboy_path),
        shell::quote_arg(extension_id)
    );
    execute_runner_command(runner, &command)
}

fn missing_runner_extension_error(
    runner_id: &str,
    homeboy_path: &str,
    extension_id: &str,
    stderr: &str,
    stdout: &str,
) -> Error {
    Error::validation_invalid_argument(
        "runner_extension",
        format!(
            "Runner '{runner_id}' is missing required extension parity for '{extension_id}' before command execution"
        ),
        Some(extension_id.to_string()),
        Some(vec![
            format!(
                "Install the extension on the runner before dispatch: {homeboy_path} extension install <source> --id {extension_id}"
            ),
            format!("Remote preflight command failed: {homeboy_path} extension show {extension_id}"),
            extension_parity_diagnostic_tail(stderr, stdout),
        ]),
    )
}

fn sync_runner_extension_revision(
    runner_id: &str,
    runner: &Runner,
    _cwd: &str,
    homeboy_path: &str,
    extension_id: &str,
    parity_error: Error,
) -> Result<()> {
    let local_revision = extension::read_source_revision(extension_id)
        .filter(|revision| !revision.trim().is_empty())
        .ok_or_else(|| parity_error.clone())?;
    let source = extension::resolve_source_url(extension_id).map_err(|err| {
        controller_extension_metadata_required_error(
            runner_id,
            homeboy_path,
            extension_id,
            &local_revision,
            err,
        )
    })?;
    let materialization_source =
        if let Some(local_source_path) = controller_local_source_path(&source.url) {
            RunnerExtensionMaterializationSource::ControllerSnapshot {
                local_path: local_source_path,
            }
        } else if !looks_like_remote_source(&source.url) {
            RunnerExtensionMaterializationSource::RunnerPath {
                path: source.url.clone(),
            }
        } else {
            RunnerExtensionMaterializationSource::RemoteGit {
                url: source.url.clone(),
                git_ref: local_revision.clone(),
            }
        };
    let records_dev_overlay = matches!(
        &materialization_source,
        RunnerExtensionMaterializationSource::ControllerSnapshot { .. }
    );
    let provenance = materialize_runner_extension(
        runner,
        homeboy_path,
        &RunnerExtensionMaterializationRequest {
            id: extension_id.to_string(),
            revision: local_revision,
            source: materialization_source,
        },
    )
    .map_err(|err| {
        runner_extension_materialization_error(
            runner_id,
            homeboy_path,
            extension_id,
            err,
            parity_error,
        )
    })?;
    if records_dev_overlay {
        record_materialized_extension_overlay(runner_id, provenance)?;
    }
    Ok(())
}

fn record_materialized_extension_overlay(
    runner_id: &str,
    provenance: impl serde::Serialize,
) -> Result<()> {
    let mut runner = load(runner_id)?;
    let mut dev_sync = runner
        .resources
        .remove("dev_sync")
        .unwrap_or_else(|| serde_json::json!({ "schema": "homeboy/runner-dev-sync/v1" }));
    let mut extensions = dev_sync
        .get("extensions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let provenance_value = serde_json::to_value(provenance)
        .map_err(|err| Error::internal_json(err.to_string(), None))?;
    let extension_id = provenance_value
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    extensions
        .retain(|entry| entry.get("id").and_then(Value::as_str) != Some(extension_id.as_str()));
    extensions.push(provenance_value);
    dev_sync["extensions"] = Value::Array(extensions);
    runner.resources.insert("dev_sync".to_string(), dev_sync);
    let patch = serde_json::json!({ "resources": runner.resources });
    let _updated = matches!(
        merge(Some(runner_id), &patch.to_string(), &[])?,
        MergeOutput::Single(_)
    );
    Ok(())
}

fn controller_extension_metadata_required_error(
    runner_id: &str,
    homeboy_path: &str,
    extension_id: &str,
    local_revision: &str,
    source_error: Error,
) -> Error {
    Error::new(
        ErrorCode::ValidationInvalidArgument,
        format!(
            "Invalid argument 'runner_extension': Controller-local extension metadata is required to materialize runner job extension parity for '{extension_id}' on runner '{runner_id}'"
        ),
        serde_json::json!({
            "field": "runner_extension",
            "problem": "controller_extension_metadata_required",
            "id": extension_id,
            "diagnostic": {
                "code": "runner_extension.controller_extension_metadata_required",
                "location": "controller",
                "runner_id": runner_id,
                "extension_id": extension_id,
                "homeboy_path": homeboy_path,
                "local_source_revision": local_revision,
                "required_for": "stale runner extension parity auto-sync before runner job dispatch",
                "source_error": {
                    "code": source_error.code.as_str(),
                    "message": source_error.message,
                    "details": source_error.details,
                },
                "next_commands": [
                    format!("{homeboy_path} extension show {extension_id}"),
                    format!("{homeboy_path} extension diff-installed {extension_id}"),
                    format!("{homeboy_path} extension install <runner-resolvable-source> --id {extension_id} --replace")
                ]
            },
            "tried": [
                format!("Controller-local extension source_revision: {local_revision}"),
                "Controller-local extension source metadata is required because the runner extension is stale and Homeboy needs a runner-resolvable source/ref to refresh it before dispatch.",
                "Runner-local extension readiness was checked first; this controller metadata is only used to build the runner-side refresh job.",
                format!("Repair controller metadata or sync manually on the runner: {homeboy_path} extension refresh <runner-resolvable-source> --id {extension_id} --ref {local_revision}")
            ]
        }),
    )
}

fn runner_extension_materialization_error(
    runner_id: &str,
    homeboy_path: &str,
    extension_id: &str,
    materialization_error: Error,
    parity_error: Error,
) -> Error {
    Error::new(
        ErrorCode::ValidationInvalidArgument,
        format!(
            "Invalid argument 'runner_extension': Runner '{runner_id}' could not auto-materialize stale extension parity for '{extension_id}' before command execution"
        ),
        serde_json::json!({
            "field": "runner_extension",
            "problem": "runner_extension_materialization_failed",
            "id": extension_id,
            "diagnostic": {
                "code": "runner_extension.materialization_failed",
                "runner_id": runner_id,
                "extension_id": extension_id,
                "homeboy_path": homeboy_path,
                "original_error": parity_error.message,
                "materialization_error": {
                    "code": materialization_error.code.as_str(),
                    "message": materialization_error.message,
                    "details": materialization_error.details,
                },
                "next_commands": [
                    format!("{homeboy_path} extension diff-installed {extension_id}"),
                    format!("{homeboy_path} extension show {extension_id}")
                ]
            },
            "tried": [
                "Runner extension parity was stale before dispatch.",
                "Homeboy attempted to materialize the controller extension source on the runner automatically.",
                format!("Original parity error: {}", parity_error.message),
            ]
        }),
    )
}

fn controller_local_source_path(source: &str) -> Option<PathBuf> {
    if looks_like_remote_source(source) {
        return None;
    }

    let expanded = shellexpand::tilde(source).to_string();
    let path = Path::new(&expanded);
    path.is_dir().then(|| path.canonicalize().ok()).flatten()
}

fn looks_like_remote_source(source: &str) -> bool {
    let lower = source.to_ascii_lowercase();
    lower.contains("://")
        || lower.starts_with("git@")
        || source.contains('@') && source.contains(':')
}

fn execute_runner_command(runner: &Runner, command: &str) -> Result<server::CommandOutput> {
    match runner.kind {
        RunnerKind::Local => Ok(server::execute_local_command(command)),
        RunnerKind::Ssh => {
            let client = ssh_client_for_runner_extension_parity(runner)?;
            Ok(client.execute(command))
        }
    }
}

#[cfg(test)]
fn runner_extension_sync_command(
    cwd: &str,
    homeboy_path: &str,
    source_url: &str,
    extension_id: &str,
    local_revision: &str,
) -> String {
    format!(
        "cd {} && {} extension refresh {} --id {} --ref {}",
        shell::quote_path(cwd),
        shell::quote_path(homeboy_path),
        shell::quote_arg(source_url),
        shell::quote_arg(extension_id),
        shell::quote_arg(local_revision)
    )
}

fn is_stale_runner_extension_parity_error(err: &Error) -> bool {
    err.message.contains("stale extension parity")
}

fn validate_runner_extension_ready(
    runner_id: &str,
    homeboy_path: &str,
    extension_id: &str,
    remote_stdout: &str,
) -> Result<()> {
    let Some(status) = remote_extension_ready_status(remote_stdout) else {
        return Ok(());
    };
    if status.ready {
        return Ok(());
    }

    let mut tried = vec![format!("Runner extension ready: false")];
    if let Some(reason) = status.reason.filter(|value| !value.trim().is_empty()) {
        tried.push(format!("Ready reason: {reason}"));
    }
    if let Some(detail) = status.detail.filter(|value| !value.trim().is_empty()) {
        tried.push(format!("Ready detail: {detail}"));
    }

    Err(Error::validation_invalid_argument(
        "runner_extension",
        format!(
            "Runner '{runner_id}' has unready extension parity for '{extension_id}' before command execution"
        ),
        Some(extension_id.to_string()),
        Some(vec![
            format!("Run extension setup on the runner before dispatch: {homeboy_path} extension setup {extension_id}"),
            format!("If setup remains stale, update or relink the extension on the runner: {homeboy_path} extension update {extension_id} or {homeboy_path} extension relink {extension_id} <source>"),
            tried.join("\n"),
        ]),
    ))
}

struct RemoteExtensionReadyStatus {
    ready: bool,
    reason: Option<String>,
    detail: Option<String>,
}

fn remote_extension_ready_status(stdout: &str) -> Option<RemoteExtensionReadyStatus> {
    let value: Value = serde_json::from_str(stdout.trim()).ok()?;
    let extension = value.get("data").and_then(|data| data.get("extension"))?;
    Some(RemoteExtensionReadyStatus {
        ready: extension.get("ready").and_then(Value::as_bool)?,
        reason: extension
            .get("ready_reason")
            .and_then(Value::as_str)
            .map(str::to_string),
        detail: extension
            .get("ready_detail")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

fn validate_runner_extension_revision(
    runner_id: &str,
    runner: &Runner,
    homeboy_path: &str,
    extension_id: &str,
    remote_stdout: &str,
) -> Result<()> {
    if let Some(overlay) = dev_sync_extension_overlay(runner, extension_id) {
        return validate_dev_overlay_extension_revision(
            runner_id,
            homeboy_path,
            extension_id,
            remote_stdout,
            &overlay,
        );
    }
    let local_revision = extension::read_source_revision(extension_id);
    let remote_revision = remote_extension_source_revision(remote_stdout);
    let Some(local_revision) = local_revision.filter(|revision| !revision.trim().is_empty()) else {
        return Ok(());
    };
    let Some(remote_revision) = remote_revision.filter(|revision| !revision.trim().is_empty())
    else {
        return Err(Error::validation_invalid_argument(
            "runner_extension",
            format!(
                "Runner '{runner_id}' has stale extension parity for '{extension_id}' before command execution"
            ),
            Some(extension_id.to_string()),
            Some(vec![
                format!("Local extension source_revision: {local_revision}"),
                "Runner extension source_revision: <missing>".to_string(),
                format!(
                    "Relink or update the extension on the runner before dispatch: {homeboy_path} extension relink {extension_id} <source>"
                ),
            ]),
        ));
    };

    if local_revision == remote_revision {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "runner_extension",
        format!(
            "Runner '{runner_id}' has stale extension parity for '{extension_id}' before command execution"
        ),
        Some(extension_id.to_string()),
        Some(vec![
            format!("Local extension source_revision: {local_revision}"),
            format!("Runner extension source_revision: {remote_revision}"),
            format!(
                "Relink or update the extension on the runner before dispatch: {homeboy_path} extension relink {extension_id} <source>"
            ),
        ]),
    ))
}

fn validate_dev_overlay_extension_revision(
    runner_id: &str,
    homeboy_path: &str,
    extension_id: &str,
    remote_stdout: &str,
    overlay: &DevSyncExtensionOverlay,
) -> Result<()> {
    let current_hash =
        super::super::extension_source_content_hash(Path::new(&overlay.source_path))?;
    if current_hash != overlay.content_hash {
        return Err(dev_overlay_mismatch_error(
            runner_id,
            homeboy_path,
            extension_id,
            overlay,
            &current_hash,
            remote_extension_source_revision(remote_stdout).as_deref(),
        ));
    }

    let remote_revision = remote_extension_source_revision(remote_stdout);
    if remote_revision.as_deref() == Some(overlay.content_hash.as_str()) {
        return Ok(());
    }

    Err(dev_overlay_mismatch_error(
        runner_id,
        homeboy_path,
        extension_id,
        overlay,
        &current_hash,
        remote_revision.as_deref(),
    ))
}

fn dev_overlay_mismatch_error(
    runner_id: &str,
    _homeboy_path: &str,
    extension_id: &str,
    overlay: &DevSyncExtensionOverlay,
    current_hash: &str,
    remote_revision: Option<&str>,
) -> Error {
    let command = format!(
        "homeboy runner dev-sync {} --extensions {}={}",
        shell::quote_arg(runner_id),
        shell::quote_arg(extension_id),
        shell::quote_arg(&overlay.source_path)
    );
    Error::new(
        ErrorCode::ValidationInvalidArgument,
        format!(
            "Invalid argument 'runner_extension': Runner '{runner_id}' has stale dev-overlay extension parity for '{extension_id}' before command execution"
        ),
        serde_json::json!({
            "field": "runner_extension",
            "problem": "dev_overlay_content_hash_mismatch",
            "id": extension_id,
            "diagnostic": {
                "code": "runner_extension.dev_overlay_content_hash_mismatch",
                "runner_id": runner_id,
                "extension_id": extension_id,
                "recorded_content_hash": overlay.content_hash,
                "current_content_hash": current_hash,
                "runner_extension_source_revision": remote_revision.unwrap_or("<missing>"),
                "source_path": overlay.source_path,
                "remediation_command": command,
            },
            "tried": [
                format!("Recorded dev overlay content_hash: {}", overlay.content_hash),
                format!("Current local extension content_hash: {current_hash}"),
                format!("Runner extension source_revision: {}", remote_revision.unwrap_or("<missing>")),
                format!("Re-sync the dev overlay before dispatch: {command}"),
            ]
        }),
    )
}

fn remote_extension_source_revision(stdout: &str) -> Option<String> {
    remote_extension_metadata(stdout).source_revision
}

fn remote_extension_metadata(stdout: &str) -> RemoteExtensionMetadata {
    let Ok(value) = serde_json::from_str::<Value>(stdout.trim()) else {
        return RemoteExtensionMetadata::default();
    };
    let Some(extension) = value.get("data").and_then(|data| data.get("extension")) else {
        return RemoteExtensionMetadata::default();
    };

    RemoteExtensionMetadata {
        path: extension
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_string),
        source_revision: extension
            .get("source_revision")
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

#[cfg(test)]
fn remote_extension_setting_ids(stdout: &str) -> BTreeSet<String> {
    remote_extension_settings(stdout).into_keys().collect()
}

fn remote_extension_settings(stdout: &str) -> BTreeMap<String, String> {
    let Ok(value) = serde_json::from_str::<Value>(stdout.trim()) else {
        return BTreeMap::new();
    };
    let Some(settings) = value
        .get("data")
        .and_then(|data| data.get("extension"))
        .and_then(|extension| extension.get("settings"))
    else {
        return BTreeMap::new();
    };

    if let Some(array) = settings.as_array() {
        return array
            .iter()
            .filter_map(|setting| {
                let id = setting.get("id").and_then(Value::as_str)?;
                let setting_type = setting
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("string");
                Some((id.to_string(), setting_type.to_string()))
            })
            .collect();
    }

    settings
        .as_object()
        .into_iter()
        .flat_map(|settings| settings.iter())
        .map(|(id, setting)| {
            let setting_type = setting
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("string");
            (id.to_string(), setting_type.to_string())
        })
        .collect()
}

fn ssh_client_for_runner_extension_parity(runner: &Runner) -> Result<SshClient> {
    let server_id = runner.server_id.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "server_id",
            "SSH runners require server_id for runner extension parity preflight",
            Some(runner.id.clone()),
            None,
        )
    })?;
    let server = server::load(server_id)?;
    let mut client = SshClient::from_server(&server, server_id)?;
    client.env.extend(runner.env.clone());
    Ok(client)
}

fn extension_parity_diagnostic_tail(stderr: &str, stdout: &str) -> String {
    let output = if stderr.trim().is_empty() {
        stdout
    } else {
        stderr
    };
    let tail = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .rev()
        .take(3)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");

    if tail.is_empty() {
        "Runner extension parity preflight produced no diagnostic output.".to_string()
    } else {
        format!("Runner extension parity preflight output:\n{tail}")
    }
}

#[cfg(test)]
mod tests {
    use super::{
        controller_extension_metadata_required_error, controller_local_source_path,
        remote_extension_core_compatibility, remote_extension_ready_status,
        remote_extension_setting_ids, remote_extension_source_revision,
        requested_setting_keys_for_command, runner_extension_sync_command,
        validate_runner_extension_core_compatibility, validate_runner_extension_ready,
        validate_runner_extension_revision, validate_runner_extension_settings,
    };
    use crate::test_support::with_isolated_home;

    use crate::core::runner::{Runner, RunnerKind};
    use std::collections::HashMap;
    use std::fs;

    fn runner_with_overlay(extension_id: &str, source_path: &str, content_hash: &str) -> Runner {
        let mut resources = HashMap::new();
        resources.insert(
            "dev_sync".to_string(),
            serde_json::json!({
                "schema": "homeboy/runner-dev-sync/v1",
                "extensions": [{
                    "id": extension_id,
                    "source_path": source_path,
                    "content_hash": content_hash,
                }]
            }),
        );
        Runner {
            id: "homeboy-lab".to_string(),
            kind: RunnerKind::Local,
            server_id: None,
            workspace_root: Some("/runner/ws".to_string()),
            settings: Default::default(),
            env: HashMap::new(),
            secret_env: HashMap::new(),
            resources,
            policy: Default::default(),
        }
    }

    #[test]
    fn remote_extension_source_revision_reads_extension_show_output() {
        let stdout = r#"{"success":true,"data":{"extension":{"id":"wordpress","source_revision":"abc1234"}}}"#;

        assert_eq!(
            remote_extension_source_revision(stdout).as_deref(),
            Some("abc1234")
        );
    }

    #[test]
    fn remote_extension_core_compatibility_reads_extension_show_output() {
        let stdout = r#"{"success":true,"data":{"extension":{"id":"wordpress","core_compatibility":{"status":"incompatible","installed_homeboy":"0.1.0","requires_homeboy":">=999.0.0","source_revision":"abc1234","remediation_command":"homeboy upgrade"}}}}"#;

        let report = remote_extension_core_compatibility(stdout).expect("core compatibility");

        assert_eq!(report.status, "incompatible");
        assert_eq!(report.installed_homeboy, "0.1.0");
        assert_eq!(report.requires_homeboy.as_deref(), Some(">=999.0.0"));
        assert_eq!(report.source_revision.as_deref(), Some("abc1234"));
    }

    #[test]
    fn runner_extension_core_compatibility_fails_fast_with_remediation() {
        let stdout = r#"{"success":true,"data":{"extension":{"id":"wordpress","core_compatibility":{"status":"incompatible","installed_homeboy":"0.1.0","requires_homeboy":">=999.0.0","source_revision":"abc1234","remediation_command":"homeboy upgrade"}}}}"#;

        let err = validate_runner_extension_core_compatibility(
            "lab",
            "/usr/local/bin/homeboy",
            "wordpress",
            stdout,
        )
        .expect_err("incompatible runner core should fail");

        assert_eq!(
            err.details["diagnostic"]["code"],
            "homeboy_core.incompatible"
        );
        assert_eq!(
            err.details["diagnostic"]["remediation_command"],
            "/usr/local/bin/homeboy upgrade"
        );
        assert!(err.message.contains("homeboy-core incompatible"));
    }

    #[test]
    fn requested_setting_keys_for_command_reads_string_and_json_flags() {
        let command = vec![
            "homeboy".to_string(),
            "test".to_string(),
            "--extension".to_string(),
            "example".to_string(),
            "--setting".to_string(),
            "profile=fast".to_string(),
            "--setting-json=env={\"A\":true}".to_string(),
            "--setting".to_string(),
            "profile=slow".to_string(),
        ];

        assert_eq!(
            requested_setting_keys_for_command(&command),
            vec!["profile".to_string(), "env".to_string()]
        );
    }

    #[test]
    fn remote_extension_setting_ids_reads_extension_show_output() {
        let stdout = r#"{"success":true,"data":{"extension":{"id":"example","settings":[{"id":"profile","type":"string","label":"Profile"},{"id":"env","type":"object","label":"Env"}]}}}"#;

        let settings = remote_extension_setting_ids(stdout);

        assert!(settings.contains("profile"));
        assert!(settings.contains("env"));
    }

    #[test]
    fn setting_parity_rejects_unsupported_runner_extension_setting() {
        let remote_stdout = r#"{"success":true,"data":{"extension":{"id":"example","path":"/runner/extensions/example","source_revision":"abc1234","settings":[{"id":"profile","type":"string","label":"Profile"}]}}}"#;

        let err = validate_runner_extension_settings(
            "homeboy-lab",
            "homeboy",
            "example",
            remote_stdout,
            &["missing_setting".to_string()],
        )
        .expect_err("unsupported setting should fail before execution");

        assert!(err.to_string().contains("unsupported_setting"));
        assert_eq!(
            err.details["diagnostic"]["code"].as_str(),
            Some("runner_extension.unsupported_setting")
        );
        assert_eq!(
            err.details["diagnostic"]["extension_id"].as_str(),
            Some("example")
        );
        assert_eq!(
            err.details["diagnostic"]["unsupported_setting_key"].as_str(),
            Some("missing_setting")
        );
        assert_eq!(
            err.details["diagnostic"]["runner_extension_path"].as_str(),
            Some("/runner/extensions/example")
        );
        assert_eq!(
            err.details["diagnostic"]["runner_extension_source_revision"].as_str(),
            Some("abc1234")
        );
        assert!(err.details["diagnostic"]["repair_hint"]
            .as_str()
            .unwrap()
            .contains("extension update example"));
    }

    #[test]
    fn setting_parity_accepts_dotted_children_of_declared_object_settings() {
        let remote_stdout = r#"{"success":true,"data":{"extension":{"id":"wordpress","path":"/runner/extensions/wordpress","source_revision":"abc1234","settings":[{"id":"bench_env","type":"object","label":"Bench env"},{"id":"profile","type":"string","label":"Profile"}]}}}"#;

        validate_runner_extension_settings(
            "homeboy-lab",
            "homeboy",
            "wordpress",
            remote_stdout,
            &[
                "bench_env.SSI_FIXTURE_MATRIX_FIXTURE_ROOT".to_string(),
                "bench_env.SSI_FIXTURE_MATRIX_VISUAL_PARITY_FULL_PAGE".to_string(),
            ],
        )
        .expect("declared object setting should cover dotted child overrides");

        validate_runner_extension_settings(
            "homeboy-lab",
            "homeboy",
            "wordpress",
            remote_stdout,
            &["profile.name".to_string()],
        )
        .expect_err("string settings should not cover dotted child overrides");
    }

    #[test]
    fn setting_parity_accepts_lab_bench_command_dotted_object_settings() {
        let remote_stdout = r#"{
          "success": true,
          "data": {
            "command": "extension.show",
            "extension": {
              "id": "wordpress",
              "path": "/home/chubes/.config/homeboy/extensions/wordpress",
              "source_revision": "3e3e4c41",
              "settings": [
                {"default": [], "id": "validation_dependencies", "label": "Validation Dependencies", "type": "array"},
                {"default": {}, "id": "wp_config_defines", "label": "wp-config additions", "type": "object"},
                {"default": {}, "id": "bench_env", "label": "Bench env passthrough", "type": "object"},
                {"default": "", "id": "wp_codebox_core_module", "label": "WP Codebox core module", "type": "string"},
                {"default": [], "id": "wp_codebox_workloads", "label": "Bench workloads", "type": "array"},
                {"default": {}, "id": "bench_browser_target", "label": "Bench browser target handoff", "type": "object"}
              ]
            }
          }
        }"#;
        let command = vec![
            "homeboy".to_string(),
            "bench".to_string(),
            "static-site-importer".to_string(),
            "--path".to_string(),
            "/Users/chubes/Developer/static-site-importer@fix-codebox-validation-provider".to_string(),
            "--runner".to_string(),
            "homeboy-lab".to_string(),
            "--lab-only".to_string(),
            "--extension".to_string(),
            "wordpress".to_string(),
            "--rig".to_string(),
            "static-site-importer-fixture-matrix".to_string(),
            "--shared-state".to_string(),
            "/home/chubes/Developer/_lab_artifacts/ssi-fast-loop-shared".to_string(),
            "--iterations".to_string(),
            "1".to_string(),
            "--warmup".to_string(),
            "0".to_string(),
            "--run-id".to_string(),
            "ssi-onepager-coffee-wp-codebox-0-12-fullpage".to_string(),
            "--setting".to_string(),
            "bench_env.SSI_FIXTURE_MATRIX_FIXTURE_ROOT=/Users/chubes/Developer/blocks-engine@fixtures-static-import-corpus/fixtures/websites/2-onepager-coffee".to_string(),
            "--setting".to_string(),
            "bench_env.SSI_FIXTURE_MATRIX_BLOCKS_ENGINE_PHP_TRANSFORMER_PATH=/Users/chubes/Developer/blocks-engine@fixtures-static-import-corpus".to_string(),
            "--setting".to_string(),
            "bench_env.SSI_FIXTURE_MATRIX_VISUAL_PARITY_FULL_PAGE=1".to_string(),
            "--".to_string(),
            "--max-depth".to_string(),
            "0".to_string(),
            "--batch-size".to_string(),
            "1".to_string(),
            "--run".to_string(),
        ];
        let requested_setting_keys = requested_setting_keys_for_command(&command);

        validate_runner_extension_settings(
            "homeboy-lab",
            "homeboy",
            "wordpress",
            remote_stdout,
            &requested_setting_keys,
        )
        .expect("declared bench_env object should cover Lab bench dotted overrides");
    }

    #[test]
    fn setting_parity_accepts_dotted_children_from_settings_object_map() {
        let remote_stdout = r#"{"success":true,"data":{"command":"extension.show","extension":{"id":"wordpress","path":"/home/chubes/.config/homeboy/extensions/wordpress","source_revision":"abc1234","settings":{"bench_env":{"default":{},"label":"Bench env passthrough","type":"object"},"profile":{"default":"","label":"Profile","type":"string"}}}}}"#;

        validate_runner_extension_settings(
            "homeboy-lab",
            "homeboy",
            "wordpress",
            remote_stdout,
            &["bench_env.SSI_FIXTURE_MATRIX_FIXTURE_ROOT".to_string()],
        )
        .expect("settings object maps should preserve object-child semantics");
    }

    #[test]
    fn remote_extension_ready_status_reads_extension_show_output() {
        let stdout = r#"{"success":true,"data":{"extension":{"id":"wordpress","ready":false,"ready_reason":"ready_check_failed","ready_detail":"missing generated asset"}}}"#;
        let status = remote_extension_ready_status(stdout).expect("ready status");

        assert!(!status.ready);
        assert_eq!(status.reason.as_deref(), Some("ready_check_failed"));
        assert_eq!(status.detail.as_deref(), Some("missing generated asset"));
    }

    #[test]
    fn readiness_parity_rejects_unready_runner_extension() {
        let remote_stdout = r#"{"success":true,"data":{"extension":{"id":"wordpress","ready":false,"ready_reason":"ready_check_failed","ready_detail":"missing generated asset"}}}"#;

        let err =
            validate_runner_extension_ready("homeboy-lab", "homeboy", "wordpress", remote_stdout)
                .expect_err("unready runner extension should fail parity");

        assert!(err.to_string().contains("unready extension parity"));
        assert!(err.details["tried"]
            .to_string()
            .contains("extension setup wordpress"));
        assert!(err.details["tried"]
            .to_string()
            .contains("missing generated asset"));
    }

    #[test]
    fn readiness_parity_accepts_ready_runner_extension() {
        let remote_stdout =
            r#"{"success":true,"data":{"extension":{"id":"wordpress","ready":true}}}"#;

        validate_runner_extension_ready("homeboy-lab", "homeboy", "wordpress", remote_stdout)
            .expect("ready runner extension should pass parity");
    }

    #[test]
    fn revision_parity_rejects_stale_runner_extension() {
        with_isolated_home(|home| {
            let extension_dir = home.path().join(".config/homeboy/extensions/wordpress");
            fs::create_dir_all(&extension_dir).expect("extension dir");
            fs::write(extension_dir.join(".source-revision"), "local123\n").expect("revision");
            let remote_stdout = r#"{"success":true,"data":{"extension":{"id":"wordpress","source_revision":"remote456"}}}"#;
            let runner = runner_with_overlay("other", "/tmp/other", "unused");

            let err = validate_runner_extension_revision(
                "homeboy-lab",
                &runner,
                "homeboy",
                "wordpress",
                remote_stdout,
            )
            .expect_err("stale runner extension should fail parity");

            assert!(err.to_string().contains("stale extension parity"));
            assert!(err.details["tried"].to_string().contains("local123"));
            assert!(err.details["tried"].to_string().contains("remote456"));
        });
    }

    #[test]
    fn revision_parity_rejects_runner_extension_without_source_revision() {
        with_isolated_home(|home| {
            let extension_dir = home.path().join(".config/homeboy/extensions/wordpress");
            fs::create_dir_all(&extension_dir).expect("extension dir");
            fs::write(extension_dir.join(".source-revision"), "local123\n").expect("revision");
            let remote_stdout = r#"{"success":true,"data":{"extension":{"id":"wordpress"}}}"#;
            let runner = runner_with_overlay("other", "/tmp/other", "unused");

            let err = validate_runner_extension_revision(
                "homeboy-lab",
                &runner,
                "homeboy",
                "wordpress",
                remote_stdout,
            )
            .expect_err("runner extension without revision should fail parity");

            assert!(err.to_string().contains("stale extension parity"));
            assert!(err.details["tried"].to_string().contains("local123"));
            assert!(err.details["tried"].to_string().contains("<missing>"));
        });
    }

    #[test]
    fn revision_parity_accepts_matching_dev_overlay_content_hash() {
        let tempdir = tempfile::tempdir().expect("source dir");
        fs::write(tempdir.path().join("rust.json"), r#"{"id":"rust"}"#).expect("manifest");
        fs::write(tempdir.path().join("run.sh"), "echo hi\n").expect("source file");
        let hash = crate::core::runner::extension_source_content_hash(tempdir.path())
            .expect("content hash");
        let runner = runner_with_overlay("rust", &tempdir.path().display().to_string(), &hash);
        let remote_stdout = format!(
            r#"{{"success":true,"data":{{"extension":{{"id":"rust","source_revision":"{hash}"}}}}}}"#
        );

        validate_runner_extension_revision(
            "homeboy-lab",
            &runner,
            "homeboy",
            "rust",
            &remote_stdout,
        )
        .expect("matching dev overlay hash should pass parity");
    }

    #[test]
    fn revision_parity_rejects_changed_dev_overlay_with_resync_command() {
        let tempdir = tempfile::tempdir().expect("source dir");
        fs::write(tempdir.path().join("rust.json"), r#"{"id":"rust"}"#).expect("manifest");
        fs::write(tempdir.path().join("run.sh"), "echo hi\n").expect("source file");
        let hash = crate::core::runner::extension_source_content_hash(tempdir.path())
            .expect("content hash");
        fs::write(tempdir.path().join("run.sh"), "echo changed\n").expect("mutate source");
        let runner = runner_with_overlay("rust", &tempdir.path().display().to_string(), &hash);
        let remote_stdout = format!(
            r#"{{"success":true,"data":{{"extension":{{"id":"rust","source_revision":"{hash}"}}}}}}"#
        );

        let err = validate_runner_extension_revision(
            "homeboy-lab",
            &runner,
            "homeboy",
            "rust",
            &remote_stdout,
        )
        .expect_err("changed dev overlay source should fail parity");

        assert_eq!(
            err.details["diagnostic"]["code"].as_str(),
            Some("runner_extension.dev_overlay_content_hash_mismatch")
        );
        assert!(err.details["diagnostic"]["remediation_command"]
            .as_str()
            .expect("command")
            .contains("homeboy runner dev-sync homeboy-lab --extensions rust="));
    }

    #[test]
    fn runner_extension_sync_command_refreshes_exact_local_revision() {
        let command = runner_extension_sync_command(
            "/tmp/project path",
            "/usr/local/bin/homeboy",
            "https://github.com/Extra-Chill/homeboy-extensions.git",
            "rust",
            "abc1234",
        );

        assert_eq!(
            command,
            "cd '/tmp/project path' && '/usr/local/bin/homeboy' extension refresh https://github.com/Extra-Chill/homeboy-extensions.git --id rust --ref abc1234"
        );
    }

    #[test]
    fn parity_auto_sync_classifies_controller_local_source_paths_for_snapshot() {
        let tempdir = tempfile::tempdir().expect("creates temp extension source");
        let local_source = tempdir.path().canonicalize().expect("canonical tempdir");

        assert_eq!(
            controller_local_source_path(tempdir.path().to_str().unwrap()).as_deref(),
            Some(local_source.as_path())
        );
    }

    #[test]
    fn parity_auto_sync_reports_controller_metadata_when_required_for_runner_job() {
        let source_error = crate::core::error::Error::validation_invalid_argument(
            "extension_id",
            "Extension 'rust' has no sourceUrl or .source-url metadata",
            Some("rust".to_string()),
            None,
        );

        let err = controller_extension_metadata_required_error(
            "homeboy-lab",
            "homeboy",
            "rust",
            "abc1234",
            source_error,
        );

        assert!(err
            .to_string()
            .contains("Controller-local extension metadata"));
        assert_eq!(
            err.details["diagnostic"]["code"].as_str(),
            Some("runner_extension.controller_extension_metadata_required")
        );
        assert_eq!(
            err.details["diagnostic"]["location"].as_str(),
            Some("controller")
        );
        assert!(err.details["diagnostic"]["required_for"]
            .as_str()
            .is_some_and(|value| value.contains("runner job dispatch")));
        let tried = err.details["tried"].to_string();
        assert!(tried.contains("Runner-local extension readiness was checked first"));
        assert!(tried.contains("extension refresh <runner-resolvable-source>"));
    }

    #[test]
    fn parity_auto_sync_classifies_only_controller_local_directories_as_local() {
        let tempdir = tempfile::tempdir().expect("creates temp extension source");
        let expected = tempdir.path().canonicalize().expect("canonical tempdir");

        assert_eq!(
            controller_local_source_path(tempdir.path().to_str().unwrap()).as_deref(),
            Some(expected.as_path())
        );
        assert!(controller_local_source_path("https://example.com/extensions.git").is_none());
        assert!(controller_local_source_path("git@example.com:org/extensions.git").is_none());
        assert!(controller_local_source_path("/runner/only/extensions/rust").is_none());
    }
}
