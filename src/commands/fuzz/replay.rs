use std::path::{Path, PathBuf};

use homeboy::core::engine::run_dir::RunDir;
use homeboy::core::extension::{self, ExtensionCapability, ExtensionRunner};
use homeboy::core::fuzz::{
    FuzzCampaign, FuzzReplayMetadata, FuzzResultEnvelope, FUZZ_CAMPAIGN_SCHEMA,
    FUZZ_RESULT_ENVELOPE_SCHEMA,
};

use super::super::utils::args::PositionalComponentArgs;
use super::types::{
    FuzzMinimizeArgs, FuzzReplayArgs, FuzzReplayEnv, FuzzReplayExecution, FuzzReplayOutput,
};
use super::workloads::{load_rig, resolve_component_id, resolve_fuzz_context};

pub(super) fn run_replay(args: FuzzReplayArgs) -> homeboy::core::Result<(FuzzReplayOutput, i32)> {
    run_replay_like(ReplayLikeArgs::from_replay(args), ReplayLikeMode::Replay)
}

pub(super) fn run_minimize(
    args: FuzzMinimizeArgs,
) -> homeboy::core::Result<(FuzzReplayOutput, i32)> {
    run_replay_like(
        ReplayLikeArgs::from_minimize(args),
        ReplayLikeMode::Minimize,
    )
}

fn run_replay_like(
    args: ReplayLikeArgs,
    mode: ReplayLikeMode,
) -> homeboy::core::Result<(FuzzReplayOutput, i32)> {
    let artifact_file = replay_artifact_path(&args);
    let positional_case = args.artifact_or_case.as_ref().and_then(|value| {
        if artifact_file.is_some() && !Path::new(value).exists() {
            Some(value.clone())
        } else {
            None
        }
    });
    let requested_case_id = args.case_id.clone().or(positional_case);

    let resolved = if let Some(path) = artifact_file.as_ref() {
        Some(resolve_replay_artifact(path, requested_case_id.as_deref())?)
    } else {
        None
    };
    let case_id = resolved
        .as_ref()
        .and_then(|resolved| resolved.case_id.clone())
        .or(requested_case_id);
    let replay = resolved
        .as_ref()
        .and_then(|resolved| resolved.replay.clone());
    let env = fuzz_replay_env(
        artifact_file.as_ref(),
        case_id.as_deref(),
        replay.as_ref(),
        args.run_id.as_ref(),
    );
    let replay_context = resolve_replay_context(&args, mode)?;
    let replay_command = replay_context
        .as_ref()
        .and_then(|context| context.command.clone())
        .map(|command| render_replay_command(&command, &env, args.args.as_slice()));

    if args.dry_run {
        let status = if artifact_file.is_some() {
            "dry_run"
        } else {
            "needs_artifact"
        };

        return Ok((
            FuzzReplayOutput {
                command: mode.command_name().to_string(),
                status: status.to_string(),
                message: replay_message(replay_command.as_ref(), true, mode),
                artifact_file: artifact_file.map(|path| path.to_string_lossy().to_string()),
                campaign_id: resolved
                    .as_ref()
                    .and_then(|resolved| resolved.campaign_id.clone()),
                envelope_id: resolved
                    .as_ref()
                    .and_then(|resolved| resolved.envelope_id.clone()),
                case_id,
                run_id: args.run_id,
                replay,
                env,
                replay_command,
                execution: None,
                passthrough_args: args.args,
                next_steps: replay_next_steps(true),
            },
            0,
        ));
    }

    let Some(context) = replay_context else {
        return Ok((FuzzReplayOutput {
            command: mode.command_name().to_string(),
            status: "unsupported".to_string(),
            message: format!("Generic fuzz {} execution requires a component/rig extension context with fuzz.{}; use --dry-run to inspect metadata only.", mode.label(), mode.manifest_key()),
            artifact_file: artifact_file.map(|path| path.to_string_lossy().to_string()),
            campaign_id: resolved.as_ref().and_then(|resolved| resolved.campaign_id.clone()),
            envelope_id: resolved.as_ref().and_then(|resolved| resolved.envelope_id.clone()),
            case_id,
            run_id: args.run_id,
            replay,
            env,
            replay_command: None,
            execution: None,
            passthrough_args: args.args,
            next_steps: replay_next_steps(false),
        }, 1));
    };

    let Some(command) = replay_command.clone() else {
        return Ok((FuzzReplayOutput {
            command: mode.command_name().to_string(),
            status: "unsupported".to_string(),
            message: format!(
                "Extension '{}' does not declare fuzz.{}; {} execution is unsupported for this context.",
                context.execution_context.extension_id,
                mode.manifest_key(),
                mode.label()
            ),
            artifact_file: artifact_file.map(|path| path.to_string_lossy().to_string()),
            campaign_id: resolved.as_ref().and_then(|resolved| resolved.campaign_id.clone()),
            envelope_id: resolved.as_ref().and_then(|resolved| resolved.envelope_id.clone()),
            case_id,
            run_id: args.run_id,
            replay,
            env,
            replay_command: None,
            execution: None,
            passthrough_args: args.args,
            next_steps: replay_next_steps(false),
        }, 1));
    };

    let run_dir = RunDir::create()?;
    let mut runner = ExtensionRunner::for_context(context.execution_context.clone())
        .component(context.component)
        .settings(&args.setting_args.setting)
        .settings_json(&args.setting_args.setting_json)
        .path_override(args.path.clone())
        .with_run_dir(&run_dir)
        .command_override(command.clone())
        .passthrough(false);
    for item in &env {
        runner = runner.env(&item.name, &item.value);
    }
    let output = runner.run()?;
    let exit_code = if output.success { 0 } else { output.exit_code };

    Ok((
        FuzzReplayOutput {
            command: mode.command_name().to_string(),
            status: if output.success { "passed" } else { "failed" }.to_string(),
            message: replay_message(Some(&command), false, mode),
            artifact_file: artifact_file.map(|path| path.to_string_lossy().to_string()),
            campaign_id: resolved
                .as_ref()
                .and_then(|resolved| resolved.campaign_id.clone()),
            envelope_id: resolved
                .as_ref()
                .and_then(|resolved| resolved.envelope_id.clone()),
            case_id,
            run_id: args.run_id,
            replay,
            env,
            replay_command: Some(command),
            execution: Some(FuzzReplayExecution {
                kind: mode.execution_kind().to_string(),
                extension_id: context.execution_context.extension_id,
                exit_code: output.exit_code,
                success: output.success,
                run_dir: run_dir.path().to_string_lossy().to_string(),
                stdout: output.stdout,
                stderr: output.stderr,
            }),
            passthrough_args: args.args,
            next_steps: replay_next_steps(false),
        },
        exit_code,
    ))
}

#[derive(Clone)]
struct ReplayLikeArgs {
    component: Option<String>,
    path: Option<String>,
    rig: Option<String>,
    extension_override: super::super::utils::args::ExtensionOverrideArgs,
    setting_args: super::super::utils::args::SettingArgs,
    artifact_or_case: Option<String>,
    artifact: Option<PathBuf>,
    case_id: Option<String>,
    run_id: Option<String>,
    dry_run: bool,
    args: Vec<String>,
}

impl ReplayLikeArgs {
    fn from_replay(args: FuzzReplayArgs) -> Self {
        Self {
            component: args.component,
            path: args.path,
            rig: args.rig,
            extension_override: args.extension_override,
            setting_args: args.setting_args,
            artifact_or_case: args.artifact_or_case,
            artifact: args.artifact,
            case_id: args.case_id,
            run_id: args.run_id,
            dry_run: args.dry_run,
            args: args.args,
        }
    }

    fn from_minimize(args: FuzzMinimizeArgs) -> Self {
        Self {
            component: args.component,
            path: args.path,
            rig: args.rig,
            extension_override: args.extension_override,
            setting_args: args.setting_args,
            artifact_or_case: args.artifact_or_case,
            artifact: args.artifact,
            case_id: args.case_id,
            run_id: args.run_id,
            dry_run: args.dry_run,
            args: args.args,
        }
    }
}

#[derive(Clone, Copy)]
enum ReplayLikeMode {
    Replay,
    Minimize,
}

impl ReplayLikeMode {
    fn command_name(self) -> &'static str {
        match self {
            Self::Replay => "fuzz.replay",
            Self::Minimize => "fuzz.minimize",
        }
    }

    fn manifest_key(self) -> &'static str {
        match self {
            Self::Replay => "replay_command",
            Self::Minimize => "minimize_command",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Replay => "replay",
            Self::Minimize => "minimize",
        }
    }

    fn execution_kind(self) -> &'static str {
        match self {
            Self::Replay => "fuzz_replay",
            Self::Minimize => "fuzz_minimize",
        }
    }
}

#[derive(Clone)]
struct ResolvedReplayContext {
    execution_context: homeboy::core::extension::ExtensionExecutionContext,
    component: homeboy::core::component::Component,
    command: Option<String>,
}

fn resolve_replay_context(
    args: &ReplayLikeArgs,
    mode: ReplayLikeMode,
) -> homeboy::core::Result<Option<ResolvedReplayContext>> {
    let comp = replay_component_args(args);
    if comp.id().is_none() && args.rig.is_none() {
        return Ok(None);
    }

    let rig_context = load_rig(args.rig.as_deref(), &args.setting_args)?;
    let effective_id =
        resolve_component_id(&comp, rig_context.as_ref().map(|context| &context.spec))?;
    let ctx = resolve_fuzz_context(
        &effective_id,
        &comp,
        &args.setting_args,
        &args.extension_override,
        ExtensionCapability::Fuzz,
        rig_context.as_ref(),
    )?;
    let execution_context =
        extension::resolve_execution_context(&ctx.component, ExtensionCapability::Fuzz)?;
    let command = extension::load_extension(&execution_context.extension_id)
        .ok()
        .and_then(|manifest| manifest.fuzz)
        .and_then(|fuzz| match mode {
            ReplayLikeMode::Replay => fuzz.replay_command,
            ReplayLikeMode::Minimize => fuzz.minimize_command,
        });

    Ok(Some(ResolvedReplayContext {
        execution_context,
        component: ctx.component,
        command,
    }))
}

fn replay_component_args(args: &ReplayLikeArgs) -> PositionalComponentArgs {
    PositionalComponentArgs {
        component: args.component.clone(),
        path: args.path.clone(),
    }
}

fn replay_message(command: Option<&String>, dry_run: bool, mode: ReplayLikeMode) -> String {
    match (command, dry_run) {
        (Some(_), true) => format!("Generic fuzz {} resolved an extension {} and printed the execution environment without running it.", mode.label(), mode.manifest_key()),
        (Some(_), false) => format!("Generic fuzz {} executed the extension {} with canonical Homeboy fuzz replay environment.", mode.label(), mode.manifest_key()),
        (None, _) => format!("Generic fuzz {} resolved replay metadata and printed the extension-owned execution contract; no {} is available to execute.", mode.label(), mode.manifest_key()),
    }
}

fn replay_next_steps(dry_run: bool) -> Vec<String> {
    if dry_run {
        return vec![
            "Run without --dry-run to execute the resolved extension replay_command.".to_string(),
            "Use `homeboy runs artifacts <run-id>` to locate persisted fuzz evidence when a runner records it.".to_string(),
        ];
    }

    vec![
        "Inspect execution stdout/stderr in this replay result for runner-owned reproduction details.".to_string(),
        "Use `homeboy runs artifacts <run-id>` to locate persisted fuzz evidence when a runner records it.".to_string(),
    ]
}

fn render_replay_command(command: &str, env: &[FuzzReplayEnv], args: &[String]) -> String {
    let mut rendered = command.to_string();
    for (placeholder, env_name) in [
        ("artifact", "HOMEBOY_FUZZ_REPLAY_ARTIFACT_FILE"),
        ("artifact_file", "HOMEBOY_FUZZ_REPLAY_ARTIFACT_FILE"),
        ("case", "HOMEBOY_FUZZ_REPLAY_CASE_ID"),
        ("case_id", "HOMEBOY_FUZZ_REPLAY_CASE_ID"),
        ("run_id", "HOMEBOY_FUZZ_REPLAY_RUN_ID"),
        ("replay", "HOMEBOY_FUZZ_REPLAY_ID"),
        ("replay_id", "HOMEBOY_FUZZ_REPLAY_ID"),
        ("seed", "HOMEBOY_FUZZ_REPLAY_SEED"),
        ("replay_seed", "HOMEBOY_FUZZ_REPLAY_SEED"),
        ("artifact_id", "HOMEBOY_FUZZ_REPLAY_ARTIFACT_ID"),
        ("case_artifact", "HOMEBOY_FUZZ_REPLAY_ARTIFACT_ID"),
        ("replay_artifact_id", "HOMEBOY_FUZZ_REPLAY_ARTIFACT_ID"),
    ] {
        if let Some(value) = env_value(env, env_name) {
            rendered = rendered.replace(
                &format!("{{{placeholder}}}"),
                &homeboy::core::engine::shell::quote_arg(value),
            );
        }
    }

    if !args.is_empty() {
        rendered.push(' ');
        rendered.push_str(&homeboy::core::engine::shell::quote_args(args));
    }

    rendered
}

fn env_value<'a>(env: &'a [FuzzReplayEnv], name: &str) -> Option<&'a str> {
    env.iter()
        .find(|item| item.name == name)
        .map(|item| item.value.as_str())
}

#[derive(Clone, Debug)]
struct ResolvedReplayArtifact {
    campaign_id: Option<String>,
    envelope_id: Option<String>,
    case_id: Option<String>,
    replay: Option<FuzzReplayMetadata>,
}

fn replay_artifact_path(args: &ReplayLikeArgs) -> Option<PathBuf> {
    args.artifact.clone().or_else(|| {
        args.artifact_or_case.as_ref().and_then(|value| {
            let path = PathBuf::from(value);
            (path.exists() || value.contains(std::path::MAIN_SEPARATOR) || value.ends_with(".json"))
                .then_some(path)
        })
    })
}

fn resolve_replay_artifact(
    path: &Path,
    requested_case_id: Option<&str>,
) -> homeboy::core::Result<ResolvedReplayArtifact> {
    let contents = std::fs::read_to_string(path).map_err(|error| {
        homeboy::core::Error::internal_io(error.to_string(), Some(path.display().to_string()))
    })?;
    let value: serde_json::Value = serde_json::from_str(&contents).map_err(|error| {
        homeboy::core::Error::validation_invalid_json(
            error,
            Some(format!("parse fuzz replay artifact {}", path.display())),
            Some(contents.clone()),
        )
    })?;
    let schema = value
        .get("schema")
        .and_then(|schema| schema.as_str())
        .unwrap_or_default();

    if schema == FUZZ_RESULT_ENVELOPE_SCHEMA {
        let envelope: FuzzResultEnvelope = serde_json::from_value(value).map_err(|error| {
            homeboy::core::Error::validation_invalid_argument(
                "artifact",
                format!("failed to decode fuzz result envelope: {error}"),
                Some(path.display().to_string()),
                None,
            )
        })?;
        let campaign = envelope.campaign.as_ref();
        let (case_id, replay) = resolve_replay_metadata(campaign, requested_case_id)?;
        return Ok(ResolvedReplayArtifact {
            campaign_id: campaign.map(|campaign| campaign.id.clone()),
            envelope_id: Some(envelope.id),
            case_id,
            replay,
        });
    }

    if schema == FUZZ_CAMPAIGN_SCHEMA {
        let campaign: FuzzCampaign = serde_json::from_value(value).map_err(|error| {
            homeboy::core::Error::validation_invalid_argument(
                "artifact",
                format!("failed to decode fuzz campaign: {error}"),
                Some(path.display().to_string()),
                None,
            )
        })?;
        let (case_id, replay) = resolve_replay_metadata(Some(&campaign), requested_case_id)?;
        return Ok(ResolvedReplayArtifact {
            campaign_id: Some(campaign.id),
            envelope_id: None,
            case_id,
            replay,
        });
    }

    Err(homeboy::core::Error::validation_invalid_argument(
        "artifact",
        format!(
            "fuzz replay artifact schema must be {FUZZ_CAMPAIGN_SCHEMA} or {FUZZ_RESULT_ENVELOPE_SCHEMA}, got {schema}"
        ),
        Some(path.display().to_string()),
        None,
    ))
}

fn resolve_replay_metadata(
    campaign: Option<&FuzzCampaign>,
    requested_case_id: Option<&str>,
) -> homeboy::core::Result<(Option<String>, Option<FuzzReplayMetadata>)> {
    let Some(campaign) = campaign else {
        return Ok((requested_case_id.map(str::to_string), None));
    };

    if let Some(case_id) = requested_case_id {
        let case = campaign
            .cases
            .iter()
            .find(|case| case.id == case_id)
            .ok_or_else(|| {
                homeboy::core::Error::validation_invalid_argument(
                    "case-id",
                    format!(
                        "fuzz campaign '{}' does not contain case '{case_id}'",
                        campaign.id
                    ),
                    Some(case_id.to_string()),
                    None,
                )
            })?;
        let replay = case
            .replay_id
            .as_ref()
            .and_then(|replay_id| {
                campaign
                    .replay
                    .as_ref()
                    .filter(|replay| replay.id == *replay_id)
            })
            .cloned()
            .or_else(|| campaign.replay.clone());
        return Ok((Some(case.id.clone()), replay));
    }

    if campaign.cases.len() == 1 {
        let case = &campaign.cases[0];
        let replay = case
            .replay_id
            .as_ref()
            .and_then(|replay_id| {
                campaign
                    .replay
                    .as_ref()
                    .filter(|replay| replay.id == *replay_id)
            })
            .cloned()
            .or_else(|| campaign.replay.clone());
        return Ok((Some(case.id.clone()), replay));
    }

    Ok((None, campaign.replay.clone()))
}

fn fuzz_replay_env(
    artifact_file: Option<&PathBuf>,
    case_id: Option<&str>,
    replay: Option<&FuzzReplayMetadata>,
    run_id: Option<&String>,
) -> Vec<FuzzReplayEnv> {
    let mut env = Vec::new();
    if let Some(path) = artifact_file {
        push_replay_env(
            &mut env,
            "HOMEBOY_FUZZ_REPLAY_ARTIFACT_FILE",
            path.to_string_lossy().to_string(),
        );
    }
    if let Some(case_id) = case_id.filter(|case_id| !case_id.trim().is_empty()) {
        push_replay_env(&mut env, "HOMEBOY_FUZZ_REPLAY_CASE_ID", case_id.to_string());
    }
    if let Some(run_id) = run_id.filter(|run_id| !run_id.trim().is_empty()) {
        push_replay_env(&mut env, "HOMEBOY_FUZZ_RUN_ID", run_id.clone());
        push_replay_env(&mut env, "HOMEBOY_FUZZ_REPLAY_RUN_ID", run_id.clone());
    }
    if let Some(replay) = replay {
        push_replay_env(&mut env, "HOMEBOY_FUZZ_REPLAY_ID", replay.id.clone());
        push_opt_replay_env(&mut env, "HOMEBOY_FUZZ_REPLAY_SEED", replay.seed.as_ref());
        push_opt_replay_env(
            &mut env,
            "HOMEBOY_FUZZ_REPLAY_ARTIFACT_ID",
            replay.artifact_id.as_ref(),
        );
    }
    env
}

fn push_replay_env(env: &mut Vec<FuzzReplayEnv>, name: &str, value: String) {
    env.push(FuzzReplayEnv {
        name: name.to_string(),
        value,
    });
}

fn push_opt_replay_env(env: &mut Vec<FuzzReplayEnv>, name: &str, value: Option<&String>) {
    if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
        push_replay_env(env, name, value.clone());
    }
}
