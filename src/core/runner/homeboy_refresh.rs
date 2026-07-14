use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::core::engine::shell::{quote_arg, quote_path};
use crate::core::error::{Error, Result};
use crate::core::git::{run_git, run_git_output};
use crate::core::output::MergeOutput;

use super::connection::active_jobs_before_daemon_replacement;
use super::{
    connect_with_orphan_adoption, disconnect, exec, load, materialize_runner_extension_with_env,
    merge, normalize_runner_command_env_for_homeboy_path, plan_controller_snapshot_extension,
    RunnerCapabilityPreflight, RunnerExecOptions, RunnerExecOutput,
    RunnerExtensionMaterializationRequest, RunnerExtensionMaterializationSource,
    RunnerFileTransfer, RunnerKind,
};

const DEFAULT_HOMEBOY_REMOTE: &str = "https://github.com/Extra-Chill/homeboy.git";
const DEFAULT_HOMEBOY_REF: &str = "main";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HomeboyBinaryRefreshMode {
    Materialize,
    Select { binary_path: String },
}

#[derive(Debug, Clone)]
pub struct HomeboyBinaryRefreshOptions {
    pub runner_id: String,
    pub mode: HomeboyBinaryRefreshMode,
    pub source: Option<String>,
    pub git_ref: Option<String>,
    pub target_dir: Option<String>,
    pub reconnect: bool,
    pub force: bool,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HomeboyBinaryRefreshPlan {
    pub runner_id: String,
    pub mode: String,
    pub source: Option<String>,
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
    pub target_dir: Option<String>,
    pub binary_path: String,
    pub script: String,
    pub reconnect: bool,
    pub followup_commands: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomeboyBinaryRefreshOutput {
    pub variant: &'static str,
    pub command: &'static str,
    pub runner_id: String,
    pub dry_run: bool,
    pub plan: HomeboyBinaryRefreshPlan,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<Value>,
    pub updated_fields: Vec<String>,
    pub daemon_refreshed: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interrupted_job_ids: Vec<String>,
    pub selected_binary_path: String,
    pub reconnect_required: bool,
    pub followup_commands: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<HomeboyBinaryRefreshFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomeboyBinaryRefreshFailure {
    pub exit_code: i32,
    pub failed_command: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_sha: Option<String>,
    pub build_path: String,
    pub stdout: String,
    pub stderr: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture: Option<crate::core::engine::command::CommandCaptureMetadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_record: Option<crate::core::runner_execution_envelope::RunnerExecutionRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mirror_run_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RunnerDevSyncOptions {
    pub runner_id: String,
    pub homeboy_source: Option<String>,
    pub homeboy_binary: Option<String>,
    pub extensions: Vec<String>,
    pub reconnect: bool,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerDevSyncBinaryProvenance {
    pub sha256: String,
    pub hash: String,
    pub local_binary: String,
    pub remote_binary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
    pub dirty: bool,
}

pub type RunnerDevSyncExtensionProvenance =
    super::extension_materialization::RunnerExtensionMaterializationProvenance;

pub type RunnerDevSyncExtensionPlan =
    super::extension_materialization::RunnerExtensionMaterializationPlan;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerDevSyncPlan {
    pub runner_id: String,
    pub workspace_root: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_binary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_binary: Option<String>,
    pub extensions: Vec<RunnerDevSyncExtensionPlan>,
    pub reconnect: bool,
    pub followup_commands: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerDevSyncOutput {
    pub variant: &'static str,
    pub command: &'static str,
    pub runner_id: String,
    pub dry_run: bool,
    pub plan: RunnerDevSyncPlan,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary: Option<RunnerDevSyncBinaryProvenance>,
    pub extensions: Vec<RunnerDevSyncExtensionProvenance>,
    pub extensions_deferred: Vec<String>,
    pub updated_fields: Vec<String>,
    pub daemon_refreshed: bool,
    pub reconnect_required: bool,
    pub next_actions: Vec<String>,
}

pub fn plan_homeboy_binary_refresh(
    options: &HomeboyBinaryRefreshOptions,
) -> Result<HomeboyBinaryRefreshPlan> {
    let runner = load(&options.runner_id)?;
    let runner_id = runner.id;
    match &options.mode {
        HomeboyBinaryRefreshMode::Select { binary_path } => {
            let binary_path = non_empty("select", binary_path)?;
            let script = identity_probe_script(binary_path);
            Ok(HomeboyBinaryRefreshPlan {
                runner_id: runner_id.clone(),
                mode: "select".to_string(),
                source: None,
                git_ref: None,
                target_dir: None,
                binary_path: binary_path.to_string(),
                script,
                reconnect: options.reconnect,
                followup_commands: refresh_followups(&runner_id, options.reconnect),
            })
        }
        HomeboyBinaryRefreshMode::Materialize => {
            let workspace_root = runner.workspace_root.as_deref().ok_or_else(|| {
                Error::validation_invalid_argument(
                    "target_dir",
                    "runner refresh-homeboy requires --target-dir when the runner has no workspace_root",
                    None,
                    None,
                )
            })?;
            let source = options
                .source
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(DEFAULT_HOMEBOY_REMOTE);
            let git_ref = options
                .git_ref
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(DEFAULT_HOMEBOY_REF);
            let target_dir = options
                .target_dir
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| default_target_dir(workspace_root, git_ref));
            let binary_path = format!(
                "{}/target/release/homeboy",
                target_dir.trim_end_matches('/')
            );
            let script = materialize_script(source, git_ref, &target_dir, &binary_path);
            Ok(HomeboyBinaryRefreshPlan {
                runner_id: runner_id.clone(),
                mode: "materialize".to_string(),
                source: Some(source.to_string()),
                git_ref: Some(git_ref.to_string()),
                target_dir: Some(target_dir),
                binary_path,
                script,
                reconnect: options.reconnect,
                followup_commands: refresh_followups(&runner_id, options.reconnect),
            })
        }
    }
}

pub fn refresh_homeboy_binary(
    options: HomeboyBinaryRefreshOptions,
) -> Result<(HomeboyBinaryRefreshOutput, i32)> {
    let promotion_lease = crate::core::runtime_promotion::acquire(
        "runner binary promotion",
        options.runner_id.clone(),
    )?;
    let plan = plan_homeboy_binary_refresh(&options)?;
    // A refresh owns the lease it observed before changing the runner binary.
    // If its local session disappears during that transition, reconnect can
    // explicitly reconcile only that exact proven-dead lease.
    let refresh_owned_lease = options
        .reconnect
        .then(|| {
            super::status(&plan.runner_id)
                .ok()
                .and_then(|report| report.session)
                .and_then(refresh_owned_lease)
        })
        .flatten();
    if options.dry_run {
        return Ok((
            HomeboyBinaryRefreshOutput {
                variant: "refresh_homeboy",
                command: "runner.refresh_homeboy",
                runner_id: plan.runner_id.clone(),
                dry_run: true,
                identity: None,
                updated_fields: Vec::new(),
                daemon_refreshed: false,
                interrupted_job_ids: Vec::new(),
                selected_binary_path: plan.binary_path.clone(),
                reconnect_required: !plan.reconnect,
                followup_commands: plan.followup_commands.clone(),
                failure: None,
                plan,
            },
            0,
        ));
    }

    let required_commands = match &options.mode {
        HomeboyBinaryRefreshMode::Materialize => {
            vec!["bash".to_string(), "git".to_string(), "cargo".to_string()]
        }
        HomeboyBinaryRefreshMode::Select { .. } => vec!["bash".to_string()],
    };

    promotion_lease.assert_generation()?;
    let (exec_output, exit_code) = exec(
        &plan.runner_id,
        RunnerExecOptions::raw_command(vec![
            "bash".to_string(),
            "-lc".to_string(),
            plan.script.clone(),
        ])
        .with_capability_preflight(RunnerCapabilityPreflight {
            command: "runner.refresh-homeboy".to_string(),
            required_commands,
            ..Default::default()
        }),
    )?;
    if exit_code != 0 {
        return Ok((
            HomeboyBinaryRefreshOutput {
                variant: "refresh_homeboy",
                command: "runner.refresh_homeboy",
                runner_id: plan.runner_id.clone(),
                dry_run: false,
                identity: None,
                updated_fields: Vec::new(),
                daemon_refreshed: false,
                interrupted_job_ids: Vec::new(),
                selected_binary_path: plan.binary_path.clone(),
                reconnect_required: !plan.reconnect,
                followup_commands: plan.followup_commands.clone(),
                failure: Some(refresh_failure(&plan, exec_output, exit_code)),
                plan,
            },
            exit_code,
        ));
    }

    promotion_lease.assert_generation()?;
    let identity = parse_identity(&exec_output.stdout)?;
    let patch = refreshed_runner_patch(&plan.runner_id, &plan.binary_path)?;
    let updated_fields = match merge(Some(&plan.runner_id), &patch.to_string(), &[])? {
        MergeOutput::Single(result) => result.updated_fields,
        MergeOutput::Bulk(_) => Vec::new(),
    };

    let mut daemon_refreshed = false;
    let interrupted_job_ids;
    if options.reconnect {
        promotion_lease.assert_generation()?;
        let active_jobs = active_jobs_before_daemon_replacement(&plan.runner_id)?;
        interrupted_job_ids = protect_active_jobs_before_reconnect(
            &plan.runner_id,
            active_jobs.iter().map(|job| job.job_id.as_str()),
            options.force,
        )?;
        let _ = disconnect(&plan.runner_id);
        let (_report, connect_exit_code) = connect_with_orphan_adoption(
            &plan.runner_id,
            refresh_owned_lease.as_deref(),
            false,
            None,
            None,
        )?;
        daemon_refreshed = connect_exit_code == 0;
    } else {
        interrupted_job_ids = Vec::new();
    }

    Ok((
        HomeboyBinaryRefreshOutput {
            variant: "refresh_homeboy",
            command: "runner.refresh_homeboy",
            runner_id: plan.runner_id.clone(),
            dry_run: false,
            plan: plan.clone(),
            identity: Some(identity),
            updated_fields,
            daemon_refreshed,
            interrupted_job_ids,
            selected_binary_path: plan.binary_path.clone(),
            reconnect_required: !daemon_refreshed,
            followup_commands: plan.followup_commands,
            failure: None,
        },
        0,
    ))
}

fn refresh_owned_lease(session: super::RunnerSession) -> Option<String> {
    (session.mode == super::RunnerTunnelMode::DirectSsh
        && session.role == super::RunnerSessionRole::Controller)
        .then_some(session.remote_daemon_lease_id)
        .flatten()
}

fn refresh_failure(
    plan: &HomeboyBinaryRefreshPlan,
    execution: RunnerExecOutput,
    exit_code: i32,
) -> HomeboyBinaryRefreshFailure {
    HomeboyBinaryRefreshFailure {
        exit_code,
        failed_command: execution.argv.clone(),
        source: plan.source.clone(),
        git_ref: plan.git_ref.clone(),
        source_sha: source_sha_from_output(&execution.stdout),
        build_path: plan.binary_path.clone(),
        stdout: execution.stdout.clone(),
        stderr: execution.stderr.clone(),
        capture: execution.capture.clone(),
        execution_record: execution.execution_record.clone(),
        job_id: execution.job_id.clone(),
        mirror_run_id: execution.mirror_run_id.clone(),
    }
}

pub fn plan_runner_dev_sync(options: &RunnerDevSyncOptions) -> Result<RunnerDevSyncPlan> {
    let runner = load(&options.runner_id)?;
    let workspace_root = runner.workspace_root.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "workspace_root",
            "runner dev-sync requires the runner to configure workspace_root",
            Some(options.runner_id.clone()),
            Some(vec![
                "Set runner.workspace_root before selecting a managed dev binary slot.".to_string(),
            ]),
        )
    })?;
    Ok(RunnerDevSyncPlan {
        runner_id: runner.id.clone(),
        workspace_root: workspace_root.to_string(),
        local_binary: options.homeboy_binary.clone(),
        remote_binary: options
            .homeboy_binary
            .as_ref()
            .and_then(|path| sha256_file(Path::new(path)).ok())
            .map(|sha| dev_binary_path(workspace_root, &sha)),
        extensions: plan_extension_overlays(workspace_root, &options.extensions)?,
        reconnect: options.reconnect,
        followup_commands: dev_sync_followups(&runner.id, options),
    })
}

pub fn runner_dev_sync(options: RunnerDevSyncOptions) -> Result<(RunnerDevSyncOutput, i32)> {
    if options.homeboy_binary.is_some() && options.homeboy_source.is_some() {
        return Err(Error::validation_invalid_argument(
            "homeboy_binary",
            "Pass either --homeboy-binary or --homeboy-source, not both",
            None,
            None,
        ));
    }
    if !options.extensions.is_empty() {
        validate_extension_specs(&options.extensions)?;
    }

    let mut plan = plan_runner_dev_sync(&options)?;
    let sync_homeboy_binary = should_sync_homeboy_binary(&options);
    if options.dry_run {
        return Ok((
            RunnerDevSyncOutput {
                variant: "dev_sync",
                command: "runner.dev_sync",
                runner_id: plan.runner_id.clone(),
                dry_run: true,
                plan,
                binary: None,
                extensions: Vec::new(),
                extensions_deferred: Vec::new(),
                updated_fields: Vec::new(),
                daemon_refreshed: false,
                reconnect_required: sync_homeboy_binary && !options.reconnect,
                next_actions: dev_sync_next_actions(&options.runner_id, &options),
            },
            0,
        ));
    }

    let mut refresh_output = None;
    let mut binary = None;
    if sync_homeboy_binary {
        let source_path = options.homeboy_source.as_deref().map(expand_path);
        let local_binary = match options.homeboy_binary.as_deref() {
            Some(path) => expand_path(path),
            None => build_local_homeboy_binary(source_path.as_deref())?,
        };
        let runner = load(&options.runner_id)?;
        validate_dev_sync_binary_for_runner(&runner, &local_binary)?;
        let sha256 = sha256_file(&local_binary)?;
        let hash = sha256[..16].to_string();
        let remote_binary = dev_binary_path(&plan.workspace_root, &sha256);
        plan.local_binary = Some(local_binary.display().to_string());
        plan.remote_binary = Some(remote_binary.clone());

        let transfer = RunnerFileTransfer::for_runner(&runner, None)?;
        let remote_parent = Path::new(&remote_binary)
            .parent()
            .and_then(Path::to_str)
            .ok_or_else(|| {
                Error::internal_json("invalid remote dev binary path".to_string(), None)
            })?;
        transfer.ensure_directory(remote_parent)?;
        transfer.upload_file(&local_binary.display().to_string(), &remote_binary)?;

        let chmod_script = format!("chmod 0755 {}", quote_path(&remote_binary));
        let (_chmod, chmod_exit) = exec(
            &options.runner_id,
            RunnerExecOptions::diagnostic_raw_shell(chmod_script),
        )?;
        if chmod_exit != 0 {
            return Ok((
                dev_sync_failure_output(options, plan, None, Vec::new()),
                chmod_exit,
            ));
        }

        let (refreshed, exit_code) = refresh_homeboy_binary(HomeboyBinaryRefreshOptions {
            runner_id: options.runner_id.clone(),
            mode: HomeboyBinaryRefreshMode::Select {
                binary_path: remote_binary.clone(),
            },
            source: None,
            git_ref: None,
            target_dir: None,
            reconnect: options.reconnect,
            force: false,
            dry_run: false,
        })?;
        if exit_code != 0 {
            return Ok((
                dev_sync_failure_output(options, plan, None, Vec::new()),
                exit_code,
            ));
        }

        let source_revision = source_path.as_deref().and_then(git_revision);
        let dirty = source_path.as_deref().is_some_and(git_dirty);
        binary = Some(RunnerDevSyncBinaryProvenance {
            sha256: sha256.clone(),
            hash,
            local_binary: local_binary.display().to_string(),
            remote_binary: remote_binary.clone(),
            source_path: source_path.map(|path| path.display().to_string()),
            source_revision,
            dirty,
        });
        refresh_output = Some(refreshed);
    }
    let extensions = sync_extension_overlays(&options.runner_id, &plan)?;

    let mut runner = load(&options.runner_id)?;
    let dev_sync = updated_dev_sync_resource(
        runner.resources.get("dev_sync").cloned(),
        binary.clone(),
        &extensions,
    )?;
    runner.resources.insert("dev_sync".to_string(), dev_sync);
    let patch = serde_json::json!({ "resources": runner.resources });
    let mut updated_fields = refresh_output
        .as_ref()
        .map(|output| output.updated_fields.clone())
        .unwrap_or_default();
    let replace_fields = vec!["resources".to_string()];
    if let MergeOutput::Single(result) = merge(
        Some(&options.runner_id),
        &patch.to_string(),
        &replace_fields,
    )? {
        updated_fields.extend(result.updated_fields);
    }

    Ok((
        RunnerDevSyncOutput {
            variant: "dev_sync",
            command: "runner.dev_sync",
            runner_id: options.runner_id.clone(),
            dry_run: false,
            plan,
            binary,
            extensions,
            extensions_deferred: Vec::new(),
            updated_fields,
            daemon_refreshed: refresh_output
                .as_ref()
                .is_some_and(|output| output.daemon_refreshed),
            reconnect_required: sync_homeboy_binary && !options.reconnect,
            next_actions: dev_sync_next_actions(&options.runner_id, &options),
        },
        0,
    ))
}

fn dev_sync_failure_output(
    options: RunnerDevSyncOptions,
    plan: RunnerDevSyncPlan,
    binary: Option<RunnerDevSyncBinaryProvenance>,
    extensions: Vec<RunnerDevSyncExtensionProvenance>,
) -> RunnerDevSyncOutput {
    let reconnect_required = should_sync_homeboy_binary(&options) && !options.reconnect;
    let next_actions = dev_sync_next_actions(&options.runner_id, &options);
    RunnerDevSyncOutput {
        variant: "dev_sync",
        command: "runner.dev_sync",
        runner_id: options.runner_id.clone(),
        dry_run: false,
        plan,
        binary,
        extensions,
        extensions_deferred: options.extensions,
        updated_fields: Vec::new(),
        daemon_refreshed: false,
        reconnect_required,
        next_actions,
    }
}

fn should_sync_homeboy_binary(options: &RunnerDevSyncOptions) -> bool {
    options.homeboy_binary.is_some()
        || options.homeboy_source.is_some()
        || options.extensions.is_empty()
}

fn plan_extension_overlays(
    workspace_root: &str,
    specs: &[String],
) -> Result<Vec<RunnerDevSyncExtensionPlan>> {
    specs
        .iter()
        .map(|spec| {
            let (id, source) = parse_extension_spec(spec)?;
            plan_controller_snapshot_extension(workspace_root, id, &expand_path(source))
        })
        .collect()
}

fn sync_extension_overlays(
    runner_id: &str,
    plan: &RunnerDevSyncPlan,
) -> Result<Vec<RunnerDevSyncExtensionProvenance>> {
    let runner = load(runner_id)?;
    let homeboy_env = installed_homeboy_env(&runner.env, runner.settings.homeboy_path.as_deref());
    let mut overlays = Vec::new();
    for extension in &plan.extensions {
        overlays.push(materialize_runner_extension_with_env(
            &runner,
            "homeboy",
            Some(homeboy_env.clone()),
            &RunnerExtensionMaterializationRequest {
                id: extension.id.clone(),
                revision: extension.content_hash.clone(),
                source: RunnerExtensionMaterializationSource::ControllerSnapshot {
                    local_path: PathBuf::from(&extension.source_path),
                },
            },
        )?);
    }
    Ok(overlays)
}

fn updated_dev_sync_resource(
    existing: Option<Value>,
    binary: Option<RunnerDevSyncBinaryProvenance>,
    synced_extensions: &[RunnerDevSyncExtensionProvenance],
) -> Result<Value> {
    let mut dev_sync = existing.unwrap_or_else(|| serde_json::json!({}));
    if !dev_sync.is_object() {
        dev_sync = serde_json::json!({});
    }

    dev_sync["schema"] = Value::String("homeboy/runner-dev-sync/v1".to_string());
    if let Some(binary) = binary {
        dev_sync["homeboy"] = serde_json::to_value(binary)
            .map_err(|err| Error::internal_json(err.to_string(), None))?;
    } else if dev_sync.get("homeboy").is_none() {
        dev_sync["homeboy"] = Value::Null;
    }

    let mut extensions = dev_sync
        .get("extensions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let synced_extension_ids = synced_extensions
        .iter()
        .map(|extension| extension.id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    extensions.retain(|entry| {
        entry
            .get("id")
            .and_then(Value::as_str)
            .is_none_or(|id| !synced_extension_ids.contains(id))
    });
    for extension in synced_extensions {
        extensions.push(
            serde_json::to_value(extension)
                .map_err(|err| Error::internal_json(err.to_string(), None))?,
        );
    }
    dev_sync["extensions"] = Value::Array(dedup_extension_metadata_by_id(extensions));
    dev_sync["extensions_deferred"] = Value::Array(Vec::new());
    Ok(dev_sync)
}

fn dedup_extension_metadata_by_id(extensions: Vec<Value>) -> Vec<Value> {
    let mut deduped = Vec::new();
    for extension in extensions {
        let Some(extension_id) = extension.get("id").and_then(Value::as_str) else {
            deduped.push(extension);
            continue;
        };
        if let Some(index) = deduped
            .iter()
            .position(|entry: &Value| entry.get("id").and_then(Value::as_str) == Some(extension_id))
        {
            deduped.remove(index);
        }
        deduped.push(extension);
    }
    deduped
}

fn installed_homeboy_env(
    env: &std::collections::HashMap<String, String>,
    configured_homeboy_path: Option<&str>,
) -> std::collections::HashMap<String, String> {
    let mut env = env.clone();
    env.remove("HOMEBOY_COMMAND");
    let Some(configured_homeboy_path) = configured_homeboy_path else {
        return env;
    };
    let configured_homeboy_path = Path::new(configured_homeboy_path);
    if !configured_homeboy_path.is_absolute() {
        return env;
    }
    let Some(parent) = configured_homeboy_path.parent().and_then(Path::to_str) else {
        return env;
    };
    if let Some(path) = env.get_mut("PATH") {
        let filtered = path
            .split(':')
            .filter(|part| *part != parent)
            .collect::<Vec<_>>()
            .join(":");
        *path = filtered;
    }
    env
}

fn materialize_script(source: &str, git_ref: &str, target_dir: &str, binary_path: &str) -> String {
    format!(
        "set -e\nsource={}\nref={}\ndir={}\nbinary={}\nmkdir -p \"$(dirname \"$dir\")\"\nif [ ! -d \"$dir/.git\" ]; then\n  git clone \"$source\" \"$dir\"\nfi\ncurrent_remote=$(git -C \"$dir\" config --get remote.origin.url 2>/dev/null || true)\nif [ \"$current_remote\" != \"$source\" ]; then\n  git -C \"$dir\" remote set-url origin \"$source\" 2>/dev/null || git -C \"$dir\" remote add origin \"$source\"\nfi\ngit -C \"$dir\" fetch --prune origin\ntarget=$(git -C \"$dir\" rev-parse --verify --quiet \"origin/$ref\" || git -C \"$dir\" rev-parse --verify --quiet \"$ref\")\nif [ -z \"$target\" ]; then\n  echo \"Homeboy ref not found: $ref\" >&2\n  exit 1\nfi\ngit -C \"$dir\" checkout --quiet --force --detach \"$target\"\ngit -C \"$dir\" reset --hard \"$target\"\necho \"HOMEBOY_REFRESH_SOURCE_SHA=$target\"\ncargo build --release --bin homeboy --manifest-path \"$dir/Cargo.toml\"\n\"$binary\" self identity\n",
        quote_path(source),
        quote_path(git_ref),
        quote_path(target_dir),
        quote_path(binary_path),
    )
}

fn source_sha_from_output(stdout: &str) -> Option<String> {
    stdout.lines().find_map(|line| {
        line.strip_prefix("HOMEBOY_REFRESH_SOURCE_SHA=")
            .filter(|sha| !sha.is_empty())
            .map(str::to_string)
    })
}

fn identity_probe_script(binary_path: &str) -> String {
    format!(
        "set -e\nbinary={}\n\"$binary\" self identity\n",
        quote_path(binary_path)
    )
}

fn parse_identity(stdout: &str) -> Result<Value> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Err(Error::internal_json(
            "refresh-homeboy produced no identity output".to_string(),
            None,
        ));
    }
    let json_start = trimmed.rfind("\n{").map(|index| index + 1).unwrap_or(0);
    let payload = &trimmed[json_start..];
    serde_json::from_str(payload).map_err(|err| {
        Error::internal_json(
            err.to_string(),
            Some("parse runner Homeboy identity output".to_string()),
        )
    })
}

fn refreshed_runner_env(
    runner_id: &str,
    homeboy_path: &str,
) -> Result<std::collections::HashMap<String, String>> {
    let runner = load(runner_id)?;
    let mut env = runner.env;
    normalize_runner_command_env_for_homeboy_path(&mut env, Some(homeboy_path));
    Ok(env)
}

fn refreshed_runner_patch(runner_id: &str, homeboy_path: &str) -> Result<Value> {
    let env = refreshed_runner_env(runner_id, homeboy_path)?;
    let mut resources = load(runner_id)?.resources;
    resources.remove("dev_sync");
    Ok(serde_json::json!({
        "homeboy_path": homeboy_path,
        "env": env,
        "resources": resources,
    }))
}

fn build_local_homeboy_binary(source_path: Option<&Path>) -> Result<PathBuf> {
    let source_path = match source_path {
        Some(path) => path.to_path_buf(),
        None => {
            std::env::current_dir().map_err(|err| Error::internal_json(err.to_string(), None))?
        }
    };
    let manifest = source_path.join("Cargo.toml");
    if !manifest.is_file() {
        return Err(Error::validation_invalid_argument(
            "homeboy_source",
            "homeboy source path must contain Cargo.toml",
            Some(source_path.display().to_string()),
            None,
        ));
    }
    let status = Command::new("cargo")
        .args(["build", "--release", "--bin", "homeboy", "--manifest-path"])
        .arg(&manifest)
        .status()
        .map_err(|err| {
            Error::internal_json(err.to_string(), Some("build local homeboy".to_string()))
        })?;
    if !status.success() {
        return Err(Error::validation_invalid_argument(
            "homeboy_source",
            format!("cargo build failed with status {status}"),
            Some(source_path.display().to_string()),
            None,
        ));
    }
    Ok(source_path.join("target/release/homeboy"))
}

fn dev_binary_path(workspace_root: &str, sha256: &str) -> String {
    format!(
        "{}/_homeboy_binaries/dev/{}/homeboy",
        workspace_root.trim_end_matches('/'),
        &sha256[..16]
    )
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path).map_err(|err| {
        Error::validation_invalid_argument(
            "homeboy_binary",
            format!("could not read binary: {err}"),
            Some(path.display().to_string()),
            None,
        )
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|err| Error::internal_json(err.to_string(), None))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn validate_dev_sync_binary_for_runner(runner: &super::Runner, binary: &Path) -> Result<()> {
    if runner.kind == RunnerKind::Ssh && is_macho_binary(binary)? {
        return Err(Error::validation_invalid_argument(
            "homeboy_binary",
            "runner dev-sync refuses to upload a Darwin/Mach-O Homeboy binary to an SSH runner",
            Some(binary.display().to_string()),
            Some(vec![
                format!(
                    "Build/select Homeboy on the runner with `homeboy runner refresh-homeboy {} --ref main --reconnect`.",
                    shell_arg(&runner.id)
                ),
                format!(
                    "For extension-only sync, run `homeboy runner dev-sync {} --extensions <id>=<path>` without --homeboy-binary or --homeboy-source.",
                    shell_arg(&runner.id)
                ),
            ]),
        ));
    }
    Ok(())
}

fn is_macho_binary(path: &Path) -> Result<bool> {
    let mut file = fs::File::open(path).map_err(|err| {
        Error::validation_invalid_argument(
            "homeboy_binary",
            format!("could not read binary: {err}"),
            Some(path.display().to_string()),
            None,
        )
    })?;
    let mut magic = [0_u8; 4];
    let read = file
        .read(&mut magic)
        .map_err(|err| Error::internal_json(err.to_string(), None))?;
    Ok(read == 4
        && matches!(
            magic,
            [0xfe, 0xed, 0xfa, 0xce]
                | [0xce, 0xfa, 0xed, 0xfe]
                | [0xfe, 0xed, 0xfa, 0xcf]
                | [0xcf, 0xfa, 0xed, 0xfe]
                | [0xca, 0xfe, 0xba, 0xbe]
                | [0xbe, 0xba, 0xfe, 0xca]
        ))
}

fn expand_path(path: &str) -> PathBuf {
    PathBuf::from(shellexpand::tilde(path).to_string())
}

fn git_revision(path: &Path) -> Option<String> {
    run_git(path, &["rev-parse", "HEAD"], "git rev-parse")
        .ok()
        .map(|stdout| stdout.trim().to_string())
}

fn git_dirty(path: &Path) -> bool {
    run_git_output(path, &["status", "--porcelain"], "git status --porcelain")
        .ok()
        .is_some_and(|output| !output.stdout.is_empty())
}

fn validate_extension_specs(specs: &[String]) -> Result<()> {
    for spec in specs {
        parse_extension_spec(spec)?;
    }
    Ok(())
}

fn parse_extension_spec(spec: &str) -> Result<(&str, &str)> {
    let Some((id, path)) = spec.split_once('=') else {
        return Err(Error::validation_invalid_argument(
            "extensions",
            "extension dev-sync specs must use id=path",
            Some(spec.to_string()),
            None,
        ));
    };
    let id = id.trim();
    let path = path.trim();
    if id.is_empty() || path.is_empty() {
        return Err(Error::validation_invalid_argument(
            "extensions",
            "extension dev-sync specs must include a non-empty id and path",
            Some(spec.to_string()),
            None,
        ));
    }
    Ok((id, path))
}

fn dev_sync_followups(runner_id: &str, options: &RunnerDevSyncOptions) -> Vec<String> {
    dev_sync_next_actions(runner_id, options)
}

fn dev_sync_next_actions(runner_id: &str, options: &RunnerDevSyncOptions) -> Vec<String> {
    if !should_sync_homeboy_binary(options) {
        return Vec::new();
    }

    let mut actions = refresh_followups(runner_id, options.reconnect);
    actions.push(format!(
        "homeboy runner refresh-homeboy {} --ref v{} --reconnect",
        shell_arg(runner_id),
        env!("CARGO_PKG_VERSION")
    ));
    actions
}

fn default_target_dir(workspace_root: &str, git_ref: &str) -> String {
    format!(
        "{}/_homeboy_binaries/homeboy-{}",
        workspace_root.trim_end_matches('/'),
        sanitize_ref(git_ref)
    )
}

fn refresh_followups(runner_id: &str, reconnect: bool) -> Vec<String> {
    if reconnect {
        vec![format!("homeboy runner status {}", shell_arg(runner_id))]
    } else {
        vec![
            format!("homeboy runner disconnect {}", shell_arg(runner_id)),
            format!("homeboy runner connect {}", shell_arg(runner_id)),
            format!("homeboy runner status {}", shell_arg(runner_id)),
        ]
    }
}

fn active_job_reconnect_error(runner_id: &str, job_ids: &[String]) -> Error {
    let follow_commands = job_ids
        .iter()
        .map(|job_id| {
            format!(
                "homeboy runner job logs {} {} --follow",
                shell_arg(runner_id),
                shell_arg(job_id)
            )
        })
        .collect::<Vec<_>>();
    let mut error = Error::validation_invalid_argument(
        "reconnect",
        format!(
            "runner `{runner_id}` has active daemon jobs: {}. Wait for them to reach a terminal state before reconnecting, or rerun with --force to interrupt them",
            job_ids.join(", ")
        ),
        Some(runner_id.to_string()),
        Some(follow_commands),
    );
    error.details["active_job_ids"] = serde_json::json!(job_ids);
    error.details["force_command"] = serde_json::json!(format!(
        "homeboy runner refresh-homeboy {} --force --reconnect",
        shell_arg(runner_id)
    ));
    error
}

fn protect_active_jobs_before_reconnect<'a>(
    runner_id: &str,
    active_job_ids: impl IntoIterator<Item = &'a str>,
    force: bool,
) -> Result<Vec<String>> {
    let job_ids = active_job_ids
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    if !job_ids.is_empty() && !force {
        return Err(active_job_reconnect_error(runner_id, &job_ids));
    }
    Ok(job_ids)
}

fn non_empty<'a>(name: &str, value: &'a str) -> Result<&'a str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(Error::validation_invalid_argument(
            name,
            format!("{name} must not be empty"),
            None,
            None,
        ));
    }
    Ok(trimmed)
}

fn sanitize_ref(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect();
    let sanitized = sanitized.trim_matches('-');
    if sanitized.is_empty() {
        "main".to_string()
    } else {
        sanitized.to_string()
    }
}

fn shell_arg(value: &str) -> String {
    quote_arg(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::runner::{RunnerSession, RunnerSessionRole, RunnerTunnelMode};
    use crate::test_support;

    #[test]
    fn routine_reconnect_refuses_to_interrupt_a_detached_lab_cook() {
        let error = protect_active_jobs_before_reconnect(
            "homeboy-lab",
            ["0b77251a-b6a7-42a6-91a3-e49ff5f57c16"],
            false,
        )
        .expect_err("routine reconnect must preserve the active cook");

        assert_eq!(
            error.details["active_job_ids"],
            serde_json::json!(["0b77251a-b6a7-42a6-91a3-e49ff5f57c16"])
        );
        assert!(error.message.contains("homeboy-lab"));
        assert!(error.message.contains("--force"));
        assert!(error.details["tried"][0]
            .as_str()
            .is_some_and(|command| command.contains("runner job logs homeboy-lab")));
    }

    #[test]
    fn forced_reconnect_reports_the_jobs_it_will_interrupt() {
        let interrupted = protect_active_jobs_before_reconnect(
            "homeboy-lab",
            ["0b77251a-b6a7-42a6-91a3-e49ff5f57c16"],
            true,
        )
        .expect("explicit force permits interruption");

        assert_eq!(
            interrupted,
            vec!["0b77251a-b6a7-42a6-91a3-e49ff5f57c16".to_string()]
        );
    }

    #[test]
    fn refresh_preserves_only_its_direct_controller_lease_for_orphan_recovery() {
        let session = RunnerSession {
            runner_id: "lab".to_string(),
            mode: RunnerTunnelMode::DirectSsh,
            role: RunnerSessionRole::Controller,
            server_id: Some("lab".to_string()),
            controller_id: None,
            broker_url: None,
            remote_daemon_address: Some("127.0.0.1:7421".to_string()),
            local_port: Some(7421),
            local_url: Some("http://127.0.0.1:7421".to_string()),
            tunnel_pid: Some(1),
            remote_daemon_pid: Some(2),
            remote_daemon_lease_id: Some("lease-refresh".to_string()),
            homeboy_version: "test".to_string(),
            homeboy_build_identity: None,
            connected_at: "2026-01-01T00:00:00Z".to_string(),
            worker_identity: None,
            worker_pid: None,
            last_seen_at: None,
        };

        assert_eq!(
            refresh_owned_lease(session),
            Some("lease-refresh".to_string())
        );
    }

    #[test]
    fn materialize_plan_uses_clean_runner_cache() {
        let options = HomeboyBinaryRefreshOptions {
            runner_id: "lab".to_string(),
            mode: HomeboyBinaryRefreshMode::Materialize,
            source: Some("https://example.test/homeboy.git".to_string()),
            git_ref: Some("fix/foo".to_string()),
            target_dir: Some("/runner/ws/homeboy-clean".to_string()),
            reconnect: false,
            force: false,
            dry_run: true,
        };
        let plan = HomeboyBinaryRefreshPlan {
            runner_id: "lab".to_string(),
            mode: "materialize".to_string(),
            source: options.source.clone(),
            git_ref: options.git_ref.clone(),
            target_dir: options.target_dir.clone(),
            binary_path: "/runner/ws/homeboy-clean/target/release/homeboy".to_string(),
            script: materialize_script(
                "https://example.test/homeboy.git",
                "fix/foo",
                "/runner/ws/homeboy-clean",
                "/runner/ws/homeboy-clean/target/release/homeboy",
            ),
            reconnect: false,
            followup_commands: refresh_followups("lab", false),
        };

        assert!(plan.script.contains("git clone \"$source\" \"$dir\""));
        assert!(plan.script.contains("checkout --quiet --force --detach"));
        assert!(plan.script.contains("cargo build --release --bin homeboy"));
        assert_eq!(
            plan.binary_path,
            "/runner/ws/homeboy-clean/target/release/homeboy"
        );
    }

    #[test]
    fn materialize_failure_preserves_compiler_diagnostics_and_active_binary() {
        test_support::with_isolated_home(|_| {
            let fixture = tempfile::tempdir().expect("fixture directory");
            let source = fixture.path().join("source");
            let workspace = fixture.path().join("workspace");
            let bin = fixture.path().join("bin");
            std::fs::create_dir_all(source.join("src")).expect("source directory");
            std::fs::create_dir_all(&workspace).expect("workspace directory");
            std::fs::create_dir_all(&bin).expect("tool directory");
            let cargo = bin.join("cargo");
            std::fs::write(
                &cargo,
                "#!/bin/sh\necho compiler_diagnostic_marker >&2\nexit 101\n",
            )
            .expect("fake cargo");
            let status = Command::new("chmod")
                .args(["0755", cargo.to_str().expect("cargo path")])
                .status()
                .expect("make fake cargo executable");
            assert!(status.success(), "fake cargo is executable");
            std::fs::write(
                source.join("Cargo.toml"),
                "[package]\nname = \"homeboy\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            )
            .expect("manifest");
            std::fs::write(
                source.join("src/main.rs"),
                "fn main() { compiler_diagnostic_marker }\n",
            )
            .expect("invalid source");
            for args in [
                vec!["init", "-b", "main"],
                vec!["add", "."],
                vec![
                    "-c",
                    "user.email=homeboy@example.test",
                    "-c",
                    "user.name=Homeboy Test",
                    "commit",
                    "-m",
                    "fixture",
                ],
            ] {
                let status = Command::new("git")
                    .args(args)
                    .current_dir(&source)
                    .status()
                    .expect("run git");
                assert!(status.success(), "git fixture setup succeeds");
            }
            let source_sha = String::from_utf8(
                Command::new("git")
                    .args(["rev-parse", "HEAD"])
                    .current_dir(&source)
                    .output()
                    .expect("read source SHA")
                    .stdout,
            )
            .expect("source SHA is UTF-8")
            .trim()
            .to_string();
            crate::core::runner::create(
                &format!(
                    r#"{{"id":"lab-local","kind":"local","workspace_root":"{}","homeboy_path":"/active/homeboy","env":{{"PATH":"{}:{}"}}}}"#,
                    workspace.display(),
                    bin.display(),
                    std::env::var("PATH").expect("PATH")
                ),
                false,
            )
            .expect("create runner");

            let (output, exit_code) = refresh_homeboy_binary(HomeboyBinaryRefreshOptions {
                runner_id: "lab-local".to_string(),
                mode: HomeboyBinaryRefreshMode::Materialize,
                source: Some(source.display().to_string()),
                git_ref: Some("main".to_string()),
                target_dir: Some(workspace.join("build").display().to_string()),
                reconnect: false,
                force: false,
                dry_run: false,
            })
            .expect("refresh returns diagnostics for compiler failure");

            assert_eq!(
                exit_code,
                101,
                "stdout: {}\nstderr: {}",
                output
                    .failure
                    .as_ref()
                    .map(|failure| failure.stdout.as_str())
                    .unwrap_or_default(),
                output
                    .failure
                    .as_ref()
                    .map(|failure| failure.stderr.as_str())
                    .unwrap_or_default()
            );
            let failure = output.failure.expect("failure evidence is preserved");
            assert_eq!(failure.exit_code, 101);
            assert_eq!(failure.source_sha.as_deref(), Some(source_sha.as_str()));
            assert!(failure
                .failed_command
                .starts_with(&["bash".to_string(), "-lc".to_string()]));
            assert!(failure
                .build_path
                .ends_with("/build/target/release/homeboy"));
            assert!(failure.stderr.contains("compiler_diagnostic_marker"));
            assert!(failure.capture.is_some());
            assert!(failure.execution_record.is_some());
            assert_eq!(
                crate::core::runner::load("lab-local")
                    .expect("reload runner")
                    .settings
                    .homeboy_path
                    .as_deref(),
                Some("/active/homeboy")
            );
        });
    }

    #[test]
    fn select_plan_only_probes_requested_binary() {
        let script = identity_probe_script("/opt/homeboy/bin/homeboy");

        assert!(script.contains("binary='/opt/homeboy/bin/homeboy'"));
        assert!(script.contains("\"$binary\" self identity"));
        assert!(!script.contains("cargo build"));
    }

    #[test]
    fn default_target_dir_is_ref_scoped() {
        assert_eq!(
            default_target_dir("/runner/ws/", "origin/main"),
            "/runner/ws/_homeboy_binaries/homeboy-origin-main"
        );
    }

    #[test]
    fn parse_identity_reads_final_pretty_json_after_command_output() {
        let identity = parse_identity(
            "HEAD is now at abc123 fix runner\n{\n  \"success\": true,\n  \"data\": {\n    \"version\": \"0.263.0\"\n  }\n}\n",
        )
        .expect("identity parses");

        assert_eq!(identity["data"]["version"], "0.263.0");
    }

    #[test]
    fn refreshed_runner_env_prepends_selected_homeboy_dir_to_path() {
        test_support::with_isolated_home(|_| {
            crate::core::runner::create(
                r#"{
                    "id": "lab-local",
                    "kind": "local",
                    "workspace_root": "/runner/ws",
                    "homeboy_path": "/old/homeboy",
                    "env": {"PATH": "/usr/bin:/bin", "RUST_LOG": "info"}
                }"#,
                false,
            )
            .expect("create runner");

            let env = refreshed_runner_env(
                "lab-local",
                "/runner/ws/_homeboy_binaries/homeboy-main/target/release/homeboy",
            )
            .expect("refresh env");

            assert_eq!(
                env.get("PATH").map(String::as_str),
                Some("/runner/ws/_homeboy_binaries/homeboy-main/target/release:/usr/bin:/bin")
            );
            assert_eq!(env.get("RUST_LOG").map(String::as_str), Some("info"));
            assert_eq!(
                env.get("HOMEBOY_COMMAND").map(String::as_str),
                Some("/runner/ws/_homeboy_binaries/homeboy-main/target/release/homeboy")
            );
        });
    }

    #[test]
    fn dev_binary_path_uses_content_hash_slot() {
        assert_eq!(
            dev_binary_path("/runner/ws/", "0123456789abcdef9999"),
            "/runner/ws/_homeboy_binaries/dev/0123456789abcdef/homeboy"
        );
    }

    #[test]
    fn extension_overlay_plan_uses_content_hash_slot() {
        let dir = tempfile::tempdir().expect("extension source");
        std::fs::write(dir.path().join("rust.json"), r#"{"id":"rust"}"#).expect("manifest");
        std::fs::write(dir.path().join("run.sh"), "echo hi\n").expect("source");

        let plan =
            plan_extension_overlays("/runner/ws/", &[format!("rust={}", dir.path().display())])
                .expect("overlay plan");

        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].id, "rust");
        assert!(plan[0]
            .synced_source_path
            .starts_with("/runner/ws/_lab_workspaces/dev-extensions/rust/"));
        assert!(plan[0].synced_source_path.ends_with('/'));
    }

    #[test]
    fn dev_sync_resource_replaces_existing_extension_overlay_by_id() {
        let existing = serde_json::json!({
            "schema": "homeboy/runner-dev-sync/v1",
            "homeboy": {"hash": "old-binary"},
            "extensions": [
                {"id": "nodejs", "source_path": "/old/nodejs", "content_hash": "old"},
                {"id": "rust", "source_path": "/extensions/rust", "content_hash": "rust-hash"}
            ]
        });
        let extension =
            super::super::extension_materialization::RunnerExtensionMaterializationProvenance {
                id: "nodejs".to_string(),
                source_path: "/new/nodejs".to_string(),
                synced_source_path: "/runner/ws/_lab_workspaces/dev-extensions/nodejs/newhash/"
                    .to_string(),
                content_hash: "new".to_string(),
                source_revision: None,
                dirty: false,
                dirty_fingerprint: None,
                synced_at: "2026-07-07T00:00:00Z".to_string(),
                dev_overlay: true,
                lifecycle: super::super::extension_materialization::dev_extension_lifecycle(
                    "lab",
                    "/runner/ws/_lab_workspaces/dev-extensions/nodejs/newhash/",
                    "nodejs",
                ),
                materialization_source: None,
            };

        let updated = updated_dev_sync_resource(Some(existing), None, &[extension])
            .expect("updates dev-sync resource");
        let extensions = updated["extensions"].as_array().expect("extensions array");

        assert_eq!(updated["homeboy"]["hash"], "old-binary");
        assert_eq!(extensions.len(), 2);
        assert_eq!(extensions[0]["id"], "rust");
        assert_eq!(extensions[1]["id"], "nodejs");
        assert_eq!(extensions[1]["source_path"], "/new/nodejs");
        assert_eq!(extensions[1]["content_hash"], "new");
    }

    #[test]
    fn dev_sync_resource_keeps_last_duplicate_overlay_for_same_id() {
        let existing = serde_json::json!({
            "schema": "homeboy/runner-dev-sync/v1",
            "extensions": [
                {"id": "nodejs", "source_path": "/old/nodejs", "content_hash": "old"},
                {"id": "nodejs", "source_path": "/newer/nodejs", "content_hash": "newer"}
            ]
        });

        let updated = updated_dev_sync_resource(Some(existing), None, &[])
            .expect("normalizes dev-sync resource");
        let extensions = updated["extensions"].as_array().expect("extensions array");

        assert_eq!(extensions.len(), 1);
        assert_eq!(extensions[0]["source_path"], "/newer/nodejs");
        assert_eq!(extensions[0]["content_hash"], "newer");
    }

    #[test]
    fn dev_sync_resource_replacement_persists_reconciled_overlay_records() {
        test_support::with_isolated_home(|_| {
            crate::core::runner::create(
                r#"{
                    "id": "lab-local",
                    "kind": "local",
                    "workspace_root": "/runner/ws",
                    "resources": {
                        "dev_sync": {
                            "schema": "homeboy/runner-dev-sync/v1",
                            "extensions": [
                                {"id": "nodejs", "source_path": "/old/nodejs", "content_hash": "old"},
                                {"id": "nodejs", "source_path": "/newer/nodejs", "content_hash": "newer"}
                            ]
                        }
                    }
                }"#,
                false,
            )
            .expect("create runner");

            let runner = crate::core::runner::load("lab-local").expect("load runner");
            let dev_sync =
                updated_dev_sync_resource(runner.resources.get("dev_sync").cloned(), None, &[])
                    .expect("reconcile dev-sync resource");
            let patch = serde_json::json!({ "resources": { "dev_sync": dev_sync } });

            crate::core::runner::merge(
                Some("lab-local"),
                &patch.to_string(),
                &["resources".to_string()],
            )
            .expect("replace resources");

            let runner = crate::core::runner::load("lab-local").expect("reload runner");
            let extensions = runner.resources["dev_sync"]["extensions"]
                .as_array()
                .expect("extensions array");
            assert_eq!(extensions.len(), 1);
            assert_eq!(extensions[0]["source_path"], "/newer/nodejs");
        });
    }

    #[test]
    fn extension_only_dev_sync_plan_does_not_refresh_homeboy_binary() {
        test_support::with_isolated_home(|_| {
            crate::core::runner::create(
                r#"{
                    "id": "lab-local",
                    "kind": "local",
                    "workspace_root": "/runner/ws",
                    "homeboy_path": "/runner/bin/homeboy"
                }"#,
                false,
            )
            .expect("create runner");
            let dir = tempfile::tempdir().expect("extension source");
            std::fs::write(dir.path().join("nodejs.json"), r#"{"id":"nodejs"}"#).expect("manifest");

            let options = RunnerDevSyncOptions {
                runner_id: "lab-local".to_string(),
                homeboy_source: None,
                homeboy_binary: None,
                extensions: vec![format!("nodejs={}", dir.path().display())],
                reconnect: false,
                dry_run: true,
            };
            let plan = plan_runner_dev_sync(&options).expect("plan dev-sync");

            assert!(!should_sync_homeboy_binary(&options));
            assert_eq!(plan.local_binary, None);
            assert_eq!(plan.remote_binary, None);
            assert!(plan.followup_commands.is_empty());
            assert_eq!(plan.extensions.len(), 1);
            assert_eq!(plan.extensions[0].id, "nodejs");
        });
    }

    #[test]
    fn extension_only_dev_sync_scrubs_dev_binary_env() {
        let mut env = std::collections::HashMap::new();
        env.insert(
            "PATH".to_string(),
            "/runner/ws/_homeboy_binaries/dev/darwin:/usr/local/bin:/usr/bin".to_string(),
        );
        env.insert(
            "HOMEBOY_COMMAND".to_string(),
            "/runner/ws/_homeboy_binaries/dev/darwin/homeboy".to_string(),
        );
        env.insert("KEEP".to_string(), "yes".to_string());

        let scrubbed = installed_homeboy_env(
            &env,
            Some("/runner/ws/_homeboy_binaries/dev/darwin/homeboy"),
        );

        assert_eq!(scrubbed.get("HOMEBOY_COMMAND"), None);
        assert_eq!(
            scrubbed.get("PATH").map(String::as_str),
            Some("/usr/local/bin:/usr/bin")
        );
        assert_eq!(scrubbed.get("KEEP").map(String::as_str), Some("yes"));
    }

    #[test]
    fn dev_sync_without_extensions_still_refreshes_homeboy_binary() {
        let options = RunnerDevSyncOptions {
            runner_id: "lab".to_string(),
            homeboy_source: None,
            homeboy_binary: None,
            extensions: Vec::new(),
            reconnect: false,
            dry_run: true,
        };

        assert!(should_sync_homeboy_binary(&options));
        assert!(!dev_sync_next_actions("lab", &options).is_empty());
    }

    #[test]
    fn ssh_dev_sync_rejects_darwin_binary_before_upload() {
        let dir = tempfile::tempdir().expect("binary dir");
        let binary = dir.path().join("homeboy");
        std::fs::write(&binary, [0xcf, 0xfa, 0xed, 0xfe]).expect("write macho binary");
        let runner = super::super::Runner {
            id: "homeboy-lab".to_string(),
            kind: RunnerKind::Ssh,
            server_id: Some("lab-server".to_string()),
            workspace_root: Some("/home/chubes/Developer".to_string()),
            settings: Default::default(),
            env: Default::default(),
            secret_env: Default::default(),
            resources: Default::default(),
            policy: Default::default(),
        };

        let err = validate_dev_sync_binary_for_runner(&runner, &binary)
            .expect_err("darwin binary rejected");

        assert!(err.message.contains("Darwin/Mach-O"));
        let tried = err.details["tried"].as_array().expect("tried remediation");
        assert!(tried.iter().any(|hint| hint.as_str().is_some_and(|hint| {
            hint.contains("runner refresh-homeboy") && hint.contains("--ref main --reconnect")
        })));
    }

    #[test]
    fn local_dev_sync_allows_darwin_binary() {
        let dir = tempfile::tempdir().expect("binary dir");
        let binary = dir.path().join("homeboy");
        std::fs::write(&binary, [0xcf, 0xfa, 0xed, 0xfe]).expect("write macho binary");
        let runner = super::super::Runner {
            id: "lab-local".to_string(),
            kind: RunnerKind::Local,
            server_id: None,
            workspace_root: Some("/tmp/homeboy".to_string()),
            settings: Default::default(),
            env: Default::default(),
            secret_env: Default::default(),
            resources: Default::default(),
            policy: Default::default(),
        };

        validate_dev_sync_binary_for_runner(&runner, &binary).expect("local runner accepts binary");
    }

    #[test]
    fn extension_overlay_lifecycle_uses_ttl_cleanup_policy() {
        let lifecycle = super::super::extension_materialization::dev_extension_lifecycle(
            "lab",
            "/runner/ws/dev/rust/hash",
            "rust",
        );

        assert_eq!(lifecycle.owner, "runner.dev_sync.extension_overlay");
        assert_eq!(lifecycle.ttl.as_deref(), Some("P7D"));
        assert_eq!(
            lifecycle.cleanup_policy,
            crate::core::resource_lifecycle_index::ResourceCleanupPolicy::DeleteAfterTtl
        );
        assert_eq!(
            lifecycle.status,
            crate::core::resource_lifecycle_index::ResourceLifecycleResourceStatus::Active
        );
    }

    #[test]
    fn refresh_patch_clears_dev_sync_provenance() {
        test_support::with_isolated_home(|_| {
            crate::core::runner::create(
                r#"{
                    "id": "lab-local",
                    "kind": "local",
                    "workspace_root": "/runner/ws",
                    "homeboy_path": "/old/homeboy",
                    "resources": {
                        "dev_sync": {"schema":"homeboy/runner-dev-sync/v1"},
                        "keep": {"enabled": true}
                    }
                }"#,
                false,
            )
            .expect("create runner");

            let patch = refreshed_runner_patch("lab-local", "/runner/ws/homeboy")
                .expect("build refresh patch");

            assert_eq!(patch["homeboy_path"], "/runner/ws/homeboy");
            assert!(patch["resources"].get("dev_sync").is_none());
            assert_eq!(patch["resources"]["keep"]["enabled"], true);
        });
    }
}
