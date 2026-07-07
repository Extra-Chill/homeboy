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

use super::{
    connect, disconnect, exec, load, materialize_runner_extension_with_env, merge,
    normalize_runner_command_env_for_homeboy_path, plan_controller_snapshot_extension,
    RunnerCapabilityPreflight, RunnerExecOptions, RunnerExtensionMaterializationRequest,
    RunnerExtensionMaterializationSource, RunnerFileTransfer,
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
    pub selected_binary_path: String,
    pub reconnect_required: bool,
    pub followup_commands: Vec<String>,
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
    let plan = plan_homeboy_binary_refresh(&options)?;
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
                selected_binary_path: plan.binary_path.clone(),
                reconnect_required: !plan.reconnect,
                followup_commands: plan.followup_commands.clone(),
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

    let (exec_output, exit_code) = exec(
        &plan.runner_id,
        RunnerExecOptions::diagnostic_raw_shell(plan.script.clone()).with_capability_preflight(
            RunnerCapabilityPreflight {
                command: "runner.refresh-homeboy".to_string(),
                required_commands,
                ..Default::default()
            },
        ),
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
                selected_binary_path: plan.binary_path.clone(),
                reconnect_required: !plan.reconnect,
                followup_commands: plan.followup_commands.clone(),
                plan,
            },
            exit_code,
        ));
    }

    let identity = parse_identity(&exec_output.stdout)?;
    let patch = refreshed_runner_patch(&plan.runner_id, &plan.binary_path)?;
    let updated_fields = match merge(Some(&plan.runner_id), &patch.to_string(), &[])? {
        MergeOutput::Single(result) => result.updated_fields,
        MergeOutput::Bulk(_) => Vec::new(),
    };

    let mut daemon_refreshed = false;
    if options.reconnect {
        let _ = disconnect(&plan.runner_id);
        let (_report, connect_exit_code) = connect(&plan.runner_id)?;
        daemon_refreshed = connect_exit_code == 0;
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
            selected_binary_path: plan.binary_path.clone(),
            reconnect_required: !daemon_refreshed,
            followup_commands: plan.followup_commands,
        },
        0,
    ))
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
        let sha256 = sha256_file(&local_binary)?;
        let hash = sha256[..16].to_string();
        let remote_binary = dev_binary_path(&plan.workspace_root, &sha256);
        plan.local_binary = Some(local_binary.display().to_string());
        plan.remote_binary = Some(remote_binary.clone());

        let runner = load(&options.runner_id)?;
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
    runner.resources.insert(
        "dev_sync".to_string(),
        serde_json::json!({
            "schema": "homeboy/runner-dev-sync/v1",
            "homeboy": binary.clone(),
            "extensions": extensions,
            "extensions_deferred": [],
        }),
    );
    let patch = serde_json::json!({ "resources": runner.resources });
    let mut updated_fields = refresh_output
        .as_ref()
        .map(|output| output.updated_fields.clone())
        .unwrap_or_default();
    if let MergeOutput::Single(result) = merge(Some(&options.runner_id), &patch.to_string(), &[])? {
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
        "set -e\nsource={}\nref={}\ndir={}\nbinary={}\nmkdir -p \"$(dirname \"$dir\")\"\nif [ ! -d \"$dir/.git\" ]; then\n  git clone \"$source\" \"$dir\"\nfi\ncurrent_remote=$(git -C \"$dir\" config --get remote.origin.url 2>/dev/null || true)\nif [ \"$current_remote\" != \"$source\" ]; then\n  git -C \"$dir\" remote set-url origin \"$source\" 2>/dev/null || git -C \"$dir\" remote add origin \"$source\"\nfi\ngit -C \"$dir\" fetch --prune origin\ntarget=$(git -C \"$dir\" rev-parse --verify --quiet \"origin/$ref\" || git -C \"$dir\" rev-parse --verify --quiet \"$ref\")\nif [ -z \"$target\" ]; then\n  echo \"Homeboy ref not found: $ref\" >&2\n  exit 1\nfi\ngit -C \"$dir\" checkout --quiet --force --detach \"$target\"\ngit -C \"$dir\" reset --hard \"$target\"\ncargo build --release --bin homeboy --manifest-path \"$dir/Cargo.toml\"\n\"$binary\" self identity\n",
        quote_path(source),
        quote_path(git_ref),
        quote_path(target_dir),
        quote_path(binary_path),
    )
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
    sanitized.trim_matches('-').to_string().if_empty("main")
}

trait IfEmpty {
    fn if_empty(self, fallback: &str) -> String;
}

impl IfEmpty for String {
    fn if_empty(self, fallback: &str) -> String {
        if self.is_empty() {
            fallback.to_string()
        } else {
            self
        }
    }
}

fn shell_arg(value: &str) -> String {
    quote_arg(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support;

    #[test]
    fn materialize_plan_uses_clean_runner_cache() {
        let options = HomeboyBinaryRefreshOptions {
            runner_id: "lab".to_string(),
            mode: HomeboyBinaryRefreshMode::Materialize,
            source: Some("https://example.test/homeboy.git".to_string()),
            git_ref: Some("fix/foo".to_string()),
            target_dir: Some("/runner/ws/homeboy-clean".to_string()),
            reconnect: false,
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
