use clap::{Args, Subcommand};
use homeboy_extension_contract::api::v1::{
    ExtensionApiDeploymentProviderReconcileRequest,
    ExtensionApiDeploymentProviderReconcileResponse,
    EXTENSION_API_DEPLOYMENT_PROVIDER_RECONCILE_REQUEST_SCHEMA, EXTENSION_API_V1,
};

use super::utils::args::MutationArgs;
use super::CmdResult;

/// Canonical control-plane operations that are not owned by an agent-task run.
#[derive(Args)]
pub struct ControlPlaneArgs {
    #[command(subcommand)]
    command: ControlPlaneCommand,
}

#[derive(Subcommand)]
enum ControlPlaneCommand {
    /// Reconcile deployment-provider effect state from authoritative evidence
    #[command(name = "provider-effects")]
    ProviderEffects(ProviderEffectsArgs),
}

#[derive(Args)]
struct ProviderEffectsArgs {
    #[command(subcommand)]
    command: ProviderEffectsCommand,
}

#[derive(Subcommand)]
enum ProviderEffectsCommand {
    /// Terminalize one ambiguous effect without invoking its provider again
    Reconcile(ProviderEffectReconcileArgs),
}

#[derive(Args)]
struct ProviderEffectReconcileArgs {
    /// Canonical control-plane effect ID
    #[arg(long)]
    effect_id: String,
    /// Immutable digest from the original provider-effect request
    #[arg(long)]
    request_digest: String,
    /// Recovery lease fence observed with the ambiguous effect
    #[arg(long)]
    recovery_fence: u64,
    /// Terminal provider result JSON (`{"exit_code":0,"evidence":{}}`)
    #[arg(long, value_name = "JSON")]
    terminal_result: String,
    /// Authoritative provider evidence JSON that proves the terminal result
    #[arg(long, value_name = "JSON")]
    authoritative_evidence: String,
    /// Confirm persistence of the reconciled terminal fact
    #[command(flatten)]
    mutation: MutationArgs,
}

pub fn run(args: ControlPlaneArgs) -> CmdResult<ExtensionApiDeploymentProviderReconcileResponse> {
    match args.command {
        ControlPlaneCommand::ProviderEffects(args) => match args.command {
            ProviderEffectsCommand::Reconcile(args) => reconcile(args),
        },
    }
}

fn reconcile(
    args: ProviderEffectReconcileArgs,
) -> CmdResult<ExtensionApiDeploymentProviderReconcileResponse> {
    if !args.mutation.is_apply() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "apply",
            "provider-effect reconciliation persists an authoritative terminal fact and requires explicit --apply",
            None,
            Some(vec!["Add --apply after reviewing the provider evidence.".to_string()]),
        ));
    }
    let result = parse_json("terminal_result", &args.terminal_result)?;
    let authoritative_evidence =
        parse_json_value("authoritative_evidence", &args.authoritative_evidence)?;
    let request = ExtensionApiDeploymentProviderReconcileRequest {
        schema: EXTENSION_API_DEPLOYMENT_PROVIDER_RECONCILE_REQUEST_SCHEMA.to_string(),
        api_version: EXTENSION_API_V1,
        effect_id: homeboy_control_plane_contract::EffectId(args.effect_id),
        request_digest: args.request_digest,
        recovery_fence: args.recovery_fence,
        result,
        authoritative_evidence,
    };
    let response = homeboy::core::control_plane::reconcile_deployment_provider_effect(&request);
    let exit_code = if response.failure.is_some() { 1 } else { 0 };
    Ok((response, exit_code))
}

fn parse_json<T: serde::de::DeserializeOwned>(
    name: &'static str,
    source: &str,
) -> homeboy::core::Result<T> {
    let raw = homeboy::core::config::read_json_spec_to_string(source)?;
    serde_json::from_str(&raw).map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(name, error.to_string(), None, None)
    })
}

fn parse_json_value(name: &'static str, source: &str) -> homeboy::core::Result<serde_json::Value> {
    parse_json(name, source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy::core::observation::{ControlPlaneResourceProjection, ObservationStore, RunRecord};
    use homeboy_control_plane_contract::{
        ControlPlaneAction, ControlPlaneActionFence, ControlPlaneActionIntent,
        ControlPlaneActionPayload, ControlPlaneActionRequest, ControlPlaneActionResource,
        ControlPlaneRef, EffectId, RunId, CONTROL_PLANE_ACTION_FENCE_SCHEMA,
        CONTROL_PLANE_ACTION_INTENT_SCHEMA, CONTROL_PLANE_ACTION_REQUEST_SCHEMA,
    };
    use homeboy_extension_contract::api::v1::EXTENSION_API_DEPLOYMENT_PROVIDER_RECONCILE_RESPONSE_SCHEMA;

    fn args(
        effect_id: &str,
        request_digest: &str,
        recovery_fence: u64,
        apply: bool,
    ) -> ControlPlaneArgs {
        ControlPlaneArgs {
            command: ControlPlaneCommand::ProviderEffects(ProviderEffectsArgs {
                command: ProviderEffectsCommand::Reconcile(ProviderEffectReconcileArgs {
                    effect_id: effect_id.to_string(),
                    request_digest: request_digest.to_string(),
                    recovery_fence,
                    terminal_result: r#"{"exit_code":0,"evidence":{"provider":"verified"}}"#
                        .to_string(),
                    authoritative_evidence: r#"{"provider_job":"verified-42"}"#.to_string(),
                    mutation: MutationArgs::from(apply),
                }),
            }),
        }
    }

    fn request(
        effect_id: &str,
        request_digest: &str,
        recovery_fence: u64,
    ) -> ExtensionApiDeploymentProviderReconcileRequest {
        ExtensionApiDeploymentProviderReconcileRequest {
            schema: EXTENSION_API_DEPLOYMENT_PROVIDER_RECONCILE_REQUEST_SCHEMA.to_string(),
            api_version: EXTENSION_API_V1,
            effect_id: EffectId(effect_id.to_string()),
            request_digest: request_digest.to_string(),
            recovery_fence,
            result: serde_json::from_str(r#"{"exit_code":0,"evidence":{"provider":"verified"}}"#)
                .expect("terminal result"),
            authoritative_evidence: serde_json::json!({"provider_job":"verified-42"}),
        }
    }

    fn ambiguous_effect(effect_id: &str) -> (String, u64) {
        let effect_id = EffectId(effect_id.to_string());
        let run = RunId::new(format!("provider-effect-{}", effect_id.0)).expect("run id");
        let request_digest = "d".repeat(64);
        let store = ObservationStore::open_initialized().expect("store");
        let projection = ControlPlaneResourceProjection {
            resource_type: "deployment_provider_effect".to_string(),
            resource_id: run.to_string(),
            version: request_digest.clone(),
            state: "admitted".to_string(),
            aliases: vec![run.to_string()],
            eligibility: serde_json::json!({}),
            provenance: serde_json::json!({}),
        };
        store
            .upsert_imported_run_with_resource_projection(
                &RunRecord {
                    id: run.to_string(),
                    kind: "deployment-provider-effect".to_string(),
                    component_id: None,
                    started_at: "2026-01-01T00:00:00Z".to_string(),
                    finished_at: None,
                    status: "running".to_string(),
                    command: None,
                    cwd: None,
                    homeboy_version: None,
                    git_sha: None,
                    rig_id: None,
                    metadata_json: serde_json::json!({}),
                },
                &projection,
                true,
            )
            .expect("record effect run");
        let intent = ControlPlaneActionIntent {
            schema: CONTROL_PLANE_ACTION_INTENT_SCHEMA.to_string(),
            effect_id: effect_id.clone(),
            resource: ControlPlaneActionResource {
                resource: ControlPlaneRef::Run(run.clone()),
                run: run.clone(),
                original_alias: None,
            },
            request: ControlPlaneActionRequest {
                schema: CONTROL_PLANE_ACTION_REQUEST_SCHEMA.to_string(),
                effect_id: effect_id.clone(),
                action: ControlPlaneAction::Resume,
                idempotency_key: effect_id.0.clone(),
                actor: "test".to_string(),
                expected_updated_at: None,
                parameters: ControlPlaneActionPayload::empty(),
                confirmed: false,
            },
            request_digest: request_digest.clone(),
            accepted_at: "2026-01-01T00:00:00Z".to_string(),
        };
        let fence = ControlPlaneActionFence {
            schema: CONTROL_PLANE_ACTION_FENCE_SCHEMA.to_string(),
            resource_updated_at: request_digest.clone(),
            eligible: true,
            reason: None,
        };
        store
            .enqueue_control_plane_action_intent(
                &intent,
                &fence,
                "deployment_provider_effect",
                &request_digest,
            )
            .expect("admit effect");
        store
            .lease_control_plane_effect_by_id(
                &effect_id,
                "worker",
                "2026-01-01T00:00:00Z",
                "2026-01-01T00:00:01Z",
            )
            .expect("lease effect");
        store
            .lease_control_plane_effect_by_id(
                &effect_id,
                "recovery-worker",
                "2099-01-01T00:00:00Z",
                "2099-01-01T00:00:01Z",
            )
            .expect("expire effect");
        let status = store
            .control_plane_effect_status(&effect_id)
            .expect("effect status")
            .expect("effect exists");
        assert!(status.recovery_required);
        (status.intent.request_digest, status.lease_fence)
    }

    #[test]
    fn handler_requires_apply_before_reconciling() {
        let error = run(args("missing-effect", "digest", 1, false)).expect_err("apply required");
        assert!(error.message.contains("--apply"));
    }

    #[test]
    fn handler_returns_canonical_success_and_idempotent_repeat() {
        homeboy::test_support::with_isolated_home(|_| {
            let (digest, fence) = ambiguous_effect("cli-reconcile-success");
            let direct = homeboy::core::control_plane::reconcile_deployment_provider_effect(
                &request("cli-reconcile-success", &digest, fence),
            );
            let first = run(args("cli-reconcile-success", &digest, fence, true)).expect("success");
            let second = run(args("cli-reconcile-success", &digest, fence, true)).expect("repeat");
            assert_eq!(first, second);
            assert_eq!(
                serde_json::to_value(&first.0).unwrap(),
                serde_json::to_value(direct).unwrap()
            );
            assert!(first.0.failure.is_none());
            assert_eq!(first.1, 0);
        });
    }

    #[test]
    fn handler_preserves_canonical_conflicts() {
        homeboy::test_support::with_isolated_home(|_| {
            let (digest, fence) = ambiguous_effect("cli-reconcile-conflicts");
            let stale =
                run(args("cli-reconcile-conflicts", &digest, fence + 1, true)).expect("response");
            assert!(stale.0.failure.is_some());
            assert_eq!(stale.1, 1);

            let wrong_digest =
                run(args("cli-reconcile-conflicts", "wrong", fence, true)).expect("response");
            assert!(wrong_digest.0.failure.is_some());

            let missing =
                run(args("cli-reconcile-missing", &digest, fence, true)).expect("response");
            assert!(missing.0.failure.is_some());
            assert_eq!(
                missing.0.schema,
                EXTENSION_API_DEPLOYMENT_PROVIDER_RECONCILE_RESPONSE_SCHEMA
            );
        });
    }

    #[test]
    fn parser_and_help_expose_the_dedicated_provider_effect_command() {
        use clap::{CommandFactory, Parser};

        let parsed = crate::cli_surface::Cli::try_parse_from([
            "homeboy",
            "control-plane",
            "provider-effects",
            "reconcile",
            "--effect-id",
            "effect-1",
            "--request-digest",
            "digest",
            "--recovery-fence",
            "1",
            "--terminal-result",
            r#"{"exit_code":0,"evidence":{}}"#,
            "--authoritative-evidence",
            "{}",
            "--apply",
        ])
        .expect("parse dedicated command");
        assert!(matches!(
            parsed.command,
            crate::cli_surface::Commands::ControlPlane(_)
        ));
        let command = crate::cli_surface::Cli::command();
        let control_plane = command
            .find_subcommand("control-plane")
            .expect("control-plane help");
        assert!(control_plane
            .find_subcommand("provider-effects")
            .expect("provider-effects help")
            .find_subcommand("reconcile")
            .expect("reconcile help")
            .get_about()
            .expect("reconcile about")
            .to_string()
            .contains("ambiguous effect"));
    }
}
