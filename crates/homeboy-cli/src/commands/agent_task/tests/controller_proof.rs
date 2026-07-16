//! CLI-level tests for the one-command `agent-task controller proof` workflow
//! (#6222).
//!
//! These cover the preflight-then-dispatch boundary at the command adapter:
//! preflight must FAIL BEFORE DISPATCH (the executor never runs) with a
//! Homeboy-owned fix command on an unmet dependency, and a met-dependency
//! profile must pass preflight and compose the run-scoped identity + complexity
//! policy. The deep composition logic is unit-tested in the core proof module.

use super::support::*;

/// Executor that panics if invoked, proving preflight fails before dispatch.
#[derive(Clone)]
struct NeverDispatchExecutor;

impl AgentTaskExecutorAdapter for NeverDispatchExecutor {
    fn execute(
        &self,
        _request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        panic!("proof preflight must fail before dispatch; executor must not run");
    }
}

fn proof_args(
    profile: &str,
    runner: &str,
    profiles: Option<&str>,
    preflight_only: bool,
) -> AgentTaskControllerProofArgs {
    AgentTaskControllerProofArgs {
        profile: profile.to_string(),
        runner: runner.to_string(),
        profiles: profiles.map(str::to_string),
        seed: Some("fixed-seed".to_string()),
        max_actions: 5,
        preflight_only,
    }
}

#[test]
fn proof_fails_before_dispatch_on_missing_secret_with_fix_command() {
    with_temp_home(|| {
        // Profile declares a required secret env that is not present, and no
        // backend (so the live provider catalog is not consulted).
        let registry = json!({
            "example-proof": {
                "name": "example-proof",
                "spec_source": "@spec.json",
                "required_secret_env": ["HOMEBOY_PROOF_TEST_SECRET_ABSENT"]
            }
        })
        .to_string();

        let (value, exit_code) = controller_proof_with_test_executor(
            proof_args("example-proof", "homeboy-lab", Some(&registry), false),
            NeverDispatchExecutor,
        )
        .expect("proof command runs");

        assert_eq!(exit_code, 1, "unmet dependency must fail the command");
        assert_eq!(value["dispatched"], json!(false));
        assert_eq!(value["preflight"]["preflight_passed"], json!(false));

        let checks = value["preflight"]["checks"]
            .as_array()
            .expect("checks array");
        let secret_check = checks
            .iter()
            .find(|check| check["id"] == json!("proof.secret_readiness"))
            .expect("secret readiness check present");
        assert_eq!(secret_check["status"], json!("error"));
        let fix = secret_check["fix_command"]
            .as_str()
            .expect("failed check carries a Homeboy-owned fix command");
        assert!(fix.contains("agent-task auth status"), "fix command: {fix}");
        assert!(fix.contains("HOMEBOY_PROOF_TEST_SECRET_ABSENT"));
    });
}

#[test]
fn proof_fails_before_dispatch_without_profile_registry() {
    with_temp_home(|| {
        let error = controller_proof_with_test_executor(
            proof_args("example-proof", "homeboy-lab", None, false),
            NeverDispatchExecutor,
        )
        .expect_err("missing registry is a hard error");
        assert!(error.message.contains("no proof profile registry"));
    });
}

#[test]
fn proof_passes_preflight_and_composes_identity_and_policy() {
    with_temp_home(|| {
        // No backend and no required secret: preflight composes identity + run
        // inputs and passes without a live runner. `preflight_only` stops before
        // the live dispatch path so the test stays hermetic.
        let registry = json!({
            "example-proof": {
                "name": "example-proof",
                "spec_source": "@spec.json",
                "complexity_policy": { "max_files": 3 }
            }
        })
        .to_string();

        let (value, exit_code) = controller_proof_with_test_executor(
            proof_args("example-proof", "homeboy-lab", Some(&registry), true),
            NeverDispatchExecutor,
        )
        .expect("proof command runs");

        assert_eq!(exit_code, 0, "met dependencies pass preflight");
        assert_eq!(
            value["dispatched"],
            json!(false),
            "preflight_only stops here"
        );
        assert_eq!(value["preflight"]["preflight_passed"], json!(true));
        assert_eq!(value["profile"], json!("example-proof"));
        assert_eq!(value["runner"], json!("homeboy-lab"));

        // Run-scoped identity is Homeboy-generated and reproducible from the seed.
        let identity = &value["preflight"]["identity"];
        assert!(identity["run_id"]
            .as_str()
            .expect("run_id")
            .starts_with("proof-example-proof-"));
        assert!(identity["loop_id"]
            .as_str()
            .expect("loop_id")
            .starts_with("proof/example-proof/"));

        // Complexity policy is materialized into the run inputs.
        assert_eq!(
            value["preflight"]["run_inputs"]["inputs"]["complexity_policy"]["max_files"],
            json!(3)
        );
    });
}
