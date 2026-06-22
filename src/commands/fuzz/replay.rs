use std::path::{Path, PathBuf};

use homeboy::core::fuzz::{
    FuzzCampaign, FuzzReplayMetadata, FuzzResultEnvelope, FUZZ_CAMPAIGN_SCHEMA,
    FUZZ_RESULT_ENVELOPE_SCHEMA,
};

use super::types::{FuzzReplayArgs, FuzzReplayEnv, FuzzReplayOutput};

pub(super) fn run_replay(args: FuzzReplayArgs) -> homeboy::core::Result<FuzzReplayOutput> {
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

    let status = if artifact_file.is_some() {
        "dry_run"
    } else {
        "needs_artifact"
    };

    Ok(FuzzReplayOutput {
        command: "fuzz.replay".to_string(),
        status: status.to_string(),
        message: "Generic fuzz replay resolves replay metadata and prints the extension-owned execution contract; it does not execute local fuzz code without a component/extension context."
            .to_string(),
        artifact_file: artifact_file.map(|path| path.to_string_lossy().to_string()),
        campaign_id: resolved.as_ref().and_then(|resolved| resolved.campaign_id.clone()),
        envelope_id: resolved.as_ref().and_then(|resolved| resolved.envelope_id.clone()),
        case_id,
        run_id: args.run_id,
        replay,
        env,
        passthrough_args: args.args,
        next_steps: vec![
            "Pass the reported HOMEBOY_FUZZ_REPLAY_* values to the originating extension replay runner."
                .to_string(),
            "Use `homeboy runs artifacts <run-id>` to locate persisted fuzz evidence when a runner records it."
                .to_string(),
        ],
    })
}

#[derive(Clone, Debug)]
struct ResolvedReplayArtifact {
    campaign_id: Option<String>,
    envelope_id: Option<String>,
    case_id: Option<String>,
    replay: Option<FuzzReplayMetadata>,
}

fn replay_artifact_path(args: &FuzzReplayArgs) -> Option<PathBuf> {
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
