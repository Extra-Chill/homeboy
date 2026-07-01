use std::path::{Path, PathBuf};

use homeboy::core::engine::run_dir::RunDir;
use homeboy::core::extension::{self, ExtensionCapability, ExtensionRunner};
use homeboy::core::fuzz::{
    FuzzCampaign, FuzzReplayMetadata, FuzzResultEnvelope, FUZZ_CAMPAIGN_SCHEMA,
    FUZZ_RESULT_ENVELOPE_SCHEMA,
};
use homeboy::core::observation::{runs_service, ArtifactRecord, ObservationStore};
use homeboy::core::runners::is_retrievable_runner_artifact;
use homeboy::core::{Error, ErrorCode};

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
    let mut artifact_ref = replay_artifact_ref(&args);
    let artifact_file = match replay_artifact_path(&args) {
        Some(path) => Some(path),
        None if artifact_ref.is_none() => {
            match resolve_run_replay_artifact_source(args.run_id.as_deref(), args.dry_run)
                .transpose()?
            {
                Some(ReplayArtifactSource::Local(path)) => Some(path),
                Some(ReplayArtifactSource::Reference(reference)) => {
                    artifact_ref = Some(reference);
                    None
                }
                None => None,
            }
        }
        None => None,
    };
    let positional_case = args.artifact_or_case.as_ref().and_then(|value| {
        if artifact_file.is_some() && !Path::new(value).exists() {
            Some(value.clone())
        } else {
            None
        }
    });
    let requested_case_id = args.case_id.clone().or(positional_case);

    if artifact_ref.is_some() && !args.dry_run {
        return Err(runner_artifact_replay_requires_local_bytes(
            artifact_ref.as_deref().unwrap_or_default(),
        ));
    }

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
        artifact_ref.as_deref(),
    );
    let replay_context = match resolve_replay_context(&args, mode) {
        Ok(context) => context,
        Err(error) if args.dry_run && artifact_ref.is_some() && replay_context_optional(&error) => {
            None
        }
        Err(error) => return Err(error),
    };
    let replay_command = replay_context
        .as_ref()
        .and_then(|context| context.command.clone())
        .map(|command| render_replay_command(&command, &env, args.args.as_slice()));

    if args.dry_run {
        let status = if artifact_file.is_some() || artifact_ref.is_some() {
            "dry_run"
        } else {
            "needs_artifact"
        };

        return Ok((
            FuzzReplayOutput {
                command: mode.command_name().to_string(),
                status: status.to_string(),
                message: replay_message(replay_command.as_ref(), true, mode),
                artifact_file: artifact_file
                    .map(|path| path.to_string_lossy().to_string())
                    .or(artifact_ref.clone()),
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
            artifact_file: artifact_file
                .map(|path| path.to_string_lossy().to_string())
                .or(artifact_ref.clone()),
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
            artifact_file: artifact_file
                .map(|path| path.to_string_lossy().to_string())
                .or(artifact_ref.clone()),
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
            artifact_file: artifact_file
                .map(|path| path.to_string_lossy().to_string())
                .or(artifact_ref.clone()),
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

fn replay_context_optional(error: &Error) -> bool {
    matches!(
        error.code,
        ErrorCode::ComponentNotFound | ErrorCode::ExtensionNotFound
    )
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
            if is_replay_artifact_ref(value) {
                return None;
            }
            let path = PathBuf::from(value);
            (path.exists() || value.contains(std::path::MAIN_SEPARATOR) || value.ends_with(".json"))
                .then_some(path)
        })
    })
}

fn replay_artifact_ref(args: &ReplayLikeArgs) -> Option<String> {
    args.artifact_or_case
        .as_deref()
        .filter(|value| is_replay_artifact_ref(value))
        .map(str::to_string)
}

fn is_replay_artifact_ref(value: &str) -> bool {
    value.starts_with("runner-artifact://") || value.starts_with("homeboy://run/")
}

fn runner_artifact_replay_requires_local_bytes(reference: &str) -> homeboy::core::Error {
    Error::validation_invalid_argument(
        "artifact",
        "fuzz replay requires local artifact bytes unless --dry-run is used",
        Some(reference.to_string()),
        Some(vec![
            format!("Runner artifact ref: {reference}"),
            "Use `homeboy fuzz replay --dry-run <runner-artifact://...>` to inspect replay intent without local bytes.".to_string(),
            "Download the artifact first with `homeboy runs artifact get <run-id> <artifact-id> --output <path>`, then replay that local path.".to_string(),
        ]),
    )
}

enum ReplayArtifactSource {
    Local(PathBuf),
    Reference(String),
}

fn resolve_run_replay_artifact_source(
    run_id: Option<&str>,
    allow_ref_without_bytes: bool,
) -> Option<homeboy::core::Result<ReplayArtifactSource>> {
    let run_id = run_id.filter(|run_id| !run_id.trim().is_empty())?;
    Some(resolve_run_replay_artifact_source_inner(
        run_id,
        allow_ref_without_bytes,
    ))
}

fn resolve_run_replay_artifact_source_inner(
    run_id: &str,
    allow_ref_without_bytes: bool,
) -> homeboy::core::Result<ReplayArtifactSource> {
    resolve_run_replay_artifact_source_inner_with_downloader(
        run_id,
        allow_ref_without_bytes,
        |artifact| {
            runs_service::download_remote_artifact(artifact.clone(), None)
                .map(|outcome| outcome.output_path)
        },
    )
}

fn resolve_run_replay_artifact_source_inner_with_downloader<F>(
    run_id: &str,
    allow_ref_without_bytes: bool,
    mut download_remote_artifact: F,
) -> homeboy::core::Result<ReplayArtifactSource>
where
    F: FnMut(&ArtifactRecord) -> homeboy::core::Result<PathBuf>,
{
    let store = ObservationStore::open_initialized()?;
    let _run = runs_service::require_run(&store, run_id)?;
    let mut candidates = runs_service::list_artifacts_for_run(&store, run_id)?
        .into_iter()
        .filter(is_run_replay_artifact_candidate)
        .collect::<Vec<_>>();
    candidates.sort_by_key(run_replay_artifact_rank);

    let mut download_errors = Vec::new();
    for artifact in &candidates {
        let path = PathBuf::from(&artifact.path);
        if path.is_file() {
            return Ok(ReplayArtifactSource::Local(path));
        }
    }

    for artifact in &candidates {
        if is_retrievable_runner_artifact(&artifact.path) {
            match download_remote_artifact(artifact) {
                Ok(path) if path.is_file() => return Ok(ReplayArtifactSource::Local(path)),
                Ok(path) => download_errors.push(format!(
                    "artifact {} mirrored to {} but no file was available",
                    artifact.id,
                    path.display()
                )),
                Err(error) => download_errors.push(format!(
                    "artifact {} could not be mirrored from runner: {}",
                    artifact.id, error.message
                )),
            }
        }
    }

    if allow_ref_without_bytes {
        if let Some(artifact) = candidates.first() {
            if is_retrievable_runner_artifact(&artifact.path) {
                return Ok(ReplayArtifactSource::Reference(artifact.path.clone()));
            }
            return Ok(ReplayArtifactSource::Reference(format!(
                "homeboy://run/{run_id}/artifact/{}",
                artifact.id
            )));
        }
    }

    Err(Error::validation_invalid_argument(
        "run-id",
        format!("fuzz replay artifact not found for run: {run_id}"),
        Some(run_id.to_string()),
        Some(
            vec![
                format!("Run `homeboy runs artifacts {run_id}` to inspect recorded artifacts."),
                "Persist a fuzz campaign/result envelope artifact before replaying by run id."
                    .to_string(),
            ]
            .into_iter()
            .chain(download_errors)
            .collect(),
        ),
    ))
}

fn is_run_replay_artifact_candidate(artifact: &ArtifactRecord) -> bool {
    artifact.kind == "fuzz_result_envelope"
        || artifact.kind == "fuzz_results"
        || artifact
            .metadata_json
            .get("schema")
            .and_then(serde_json::Value::as_str)
            == Some(FUZZ_RESULT_ENVELOPE_SCHEMA)
}

fn run_replay_artifact_rank(artifact: &ArtifactRecord) -> u8 {
    if artifact.kind == "fuzz_result_envelope" {
        0
    } else if artifact
        .metadata_json
        .get("schema")
        .and_then(serde_json::Value::as_str)
        == Some(FUZZ_RESULT_ENVELOPE_SCHEMA)
    {
        1
    } else {
        2
    }
}

#[cfg(test)]
mod replay_artifact_source_tests {
    use super::*;
    use homeboy::core::observation::NewRunRecord;
    use homeboy::test_support::with_isolated_home;

    #[test]
    fn mirrors_runner_fuzz_result_envelope_for_run_id_replay() {
        with_isolated_home(|home| {
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(
                    NewRunRecord::builder("fuzz")
                        .component_id("component-a")
                        .command("homeboy fuzz run component-a")
                        .cwd_path(home.path())
                        .build(),
                )
                .expect("run");
            store
                .import_artifact(&ArtifactRecord {
                    id: "4ff1f923-a7c0-47dc-8ba4-cbdbc92e0d62".to_string(),
                    run_id: run.id.clone(),
                    kind: "fuzz_result_envelope".to_string(),
                    artifact_type: "remote_file".to_string(),
                    path: format!(
                        "runner-artifact://homeboy-lab/{}/4ff1f923-a7c0-47dc-8ba4-cbdbc92e0d62",
                        run.id
                    ),
                    url: None,
                    public_url: None,
                    viewer_url: None,
                    viewer_links: Vec::new(),
                    sha256: None,
                    size_bytes: None,
                    mime: Some("application/json".to_string()),
                    metadata_json: serde_json::json!({
                        "schema": FUZZ_RESULT_ENVELOPE_SCHEMA,
                        "ref": "runner-artifact://homeboy-lab/run/artifact"
                    }),
                    created_at: chrono::Utc::now().to_rfc3339(),
                })
                .expect("artifact");

            let mirrored_path = home.path().join("mirrored-fuzz-result-envelope.json");
            std::fs::write(
                &mirrored_path,
                serde_json::json!({
                    "schema": FUZZ_RESULT_ENVELOPE_SCHEMA,
                    "id": "envelope-1",
                    "status": "passed",
                    "request": {
                        "id": "request-1",
                        "component": "component-a"
                    },
                    "campaign": {
                        "schema": FUZZ_CAMPAIGN_SCHEMA,
                        "version": homeboy::core::fuzz::FUZZ_CONTRACT_VERSION,
                        "id": "campaign-1",
                        "safety_class": "read_only",
                        "cases": [{
                            "schema": homeboy::core::fuzz::FUZZ_CASE_SCHEMA,
                            "id": "case-1",
                            "replay_id": "replay-1"
                        }],
                        "replay": {
                            "schema": homeboy::core::fuzz::FUZZ_REPLAY_SCHEMA,
                            "id": "replay-1",
                            "seed": "1234",
                            "artifact_id": "case-artifact"
                        }
                    }
                })
                .to_string(),
            )
            .expect("write envelope");

            let source = resolve_run_replay_artifact_source_inner_with_downloader(
                &run.id,
                false,
                |artifact| {
                    assert_eq!(artifact.kind, "fuzz_result_envelope");
                    assert!(artifact.path.starts_with("runner-artifact://homeboy-lab/"));
                    Ok(mirrored_path.clone())
                },
            )
            .expect("resolve source");

            let ReplayArtifactSource::Local(path) = source else {
                panic!("expected mirrored local source");
            };
            let resolved = resolve_replay_artifact(&path, Some("case-1")).expect("artifact");
            assert_eq!(resolved.envelope_id.as_deref(), Some("envelope-1"));
            assert_eq!(resolved.campaign_id.as_deref(), Some("campaign-1"));
            assert_eq!(resolved.case_id.as_deref(), Some("case-1"));
            assert_eq!(
                resolved.replay.as_ref().map(|replay| replay.id.as_str()),
                Some("replay-1")
            );
        });
    }

    #[test]
    fn dry_run_can_fall_back_to_runner_artifact_ref_when_mirroring_fails() {
        with_isolated_home(|home| {
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(
                    NewRunRecord::builder("fuzz")
                        .component_id("component-a")
                        .command("homeboy fuzz run component-a")
                        .cwd_path(home.path())
                        .build(),
                )
                .expect("run");
            let reference = format!("runner-artifact://homeboy-lab/{}/artifact-1", run.id);
            store
                .import_artifact(&ArtifactRecord {
                    id: "artifact-1".to_string(),
                    run_id: run.id.clone(),
                    kind: "fuzz_result_envelope".to_string(),
                    artifact_type: "remote_file".to_string(),
                    path: reference.clone(),
                    url: None,
                    public_url: None,
                    viewer_url: None,
                    viewer_links: Vec::new(),
                    sha256: None,
                    size_bytes: None,
                    mime: Some("application/json".to_string()),
                    metadata_json: serde_json::json!({ "schema": FUZZ_RESULT_ENVELOPE_SCHEMA }),
                    created_at: chrono::Utc::now().to_rfc3339(),
                })
                .expect("artifact");

            let source = resolve_run_replay_artifact_source_inner_with_downloader(
                &run.id,
                true,
                |_artifact| {
                    Err(Error::internal_io(
                        "runner unavailable".to_string(),
                        Some("download runner artifact".to_string()),
                    ))
                },
            )
            .expect("resolve source");

            let ReplayArtifactSource::Reference(actual) = source else {
                panic!("expected runner reference fallback");
            };
            assert_eq!(actual, reference);
        });
    }
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
    artifact_ref: Option<&str>,
) -> Vec<FuzzReplayEnv> {
    let mut env = Vec::new();
    if let Some(path) = artifact_file {
        push_replay_env(
            &mut env,
            "HOMEBOY_FUZZ_REPLAY_ARTIFACT_FILE",
            path.to_string_lossy().to_string(),
        );
    } else if let Some(reference) = artifact_ref {
        push_replay_env(
            &mut env,
            "HOMEBOY_FUZZ_REPLAY_ARTIFACT_FILE",
            reference.to_string(),
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
