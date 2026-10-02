//! Gate placement belongs to the controller, before selecting private HOME.
//! The runner transport is supplied by the existing Lab execution owner.

use super::*;
use crate::agent_task_promotion::{candidate_fingerprint, AgentTaskPromotionCandidate};

pub const LAB_GATE_RECEIPT_SCHEMA: &str = "homeboy/admitted-lab-gate-receipt/v1";
pub const LAB_GATE_DISPOSITION_SCHEMA: &str = "homeboy/lab-gate-disposition/v1";

/// Seal only the declared extension/shared closure using the gate owner's
/// existing copy, path, overlap and identity checks. No registry is exported.
pub fn seal_extension_resources(
    root: &Path,
    inputs: &[AgentTaskGateExtensionInput],
) -> Result<(
    Vec<AgentTaskGateExtensionInput>,
    Vec<AgentTaskGateExtensionInputProvenance>,
)> {
    let home = root.join("validated-home");
    fs::create_dir_all(&home).map_err(|error| Error::internal_io(error.to_string(), None))?;
    let mut report = AgentTaskGateEnvironment::default();
    materialize_extension_inputs(&mut report, &home, inputs)?;
    let mut sealed = Vec::new();
    for input in &report.extension_inputs {
        let package = root.join("packages").join(&input.id);
        let source = package.join(&input.id);
        copy_extension_input(&home.join(&input.destination), &source)?;
        for asset in &input.shared_assets {
            let destination = homeboy_core::resolve_contained_local_path(
                &package,
                &asset.path,
                "gate.shared_asset",
            )?;
            copy_extension_input(&home.join(&asset.destination), &destination)?;
        }
        fs::write(package.join("homeboy-extension-root.json"), serde_json::json!({"shared_assets": input.shared_assets.iter().map(|asset| &asset.path).collect::<Vec<_>>()}).to_string())
            .map_err(|error| Error::internal_io(error.to_string(), None))?;
        sealed.push(AgentTaskGateExtensionInput {
            id: input.id.clone(),
            source: source.display().to_string(),
            identity: Some(input.identity.clone()),
        });
    }
    fs::remove_dir_all(home).map_err(|error| Error::internal_io(error.to_string(), None))?;
    Ok((sealed, report.extension_inputs))
}

/// Compare canonical materialization evidence while allowing source-path remaps.
/// Extension/shared cardinalities, identities and private destinations all belong
/// to the existing gate resource owner; no separate closure hash is introduced.
pub fn verify_extension_resource_closure(
    expected: &[AgentTaskGateExtensionInputProvenance],
    actual: &[AgentTaskGateExtensionInputProvenance],
) -> Result<()> {
    if expected.len() != actual.len()
        || expected.iter().zip(actual).any(|(expected, actual)| {
            expected.id != actual.id
                || expected.identity != actual.identity
                || expected.destination != actual.destination
                || expected.shared_assets.len() != actual.shared_assets.len()
                || expected
                    .shared_assets
                    .iter()
                    .zip(&actual.shared_assets)
                    .any(|(expected, actual)| {
                        expected.path != actual.path
                            || expected.identity != actual.identity
                            || expected.destination != actual.destination
                    })
        })
    {
        return Err(Error::invalid_argument(
            "gate.resources",
            "materialized extension/shared closure differs from the sealed original",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabGateRequest {
    #[serde(default)]
    pub preflight_only: bool,
    #[serde(default)]
    pub toolchains: Vec<AgentTaskGateToolchainRequirement>,
    pub runner_id: String,
    pub candidate: AgentTaskPromotionCandidate,
    pub index: usize,
    pub argv: Vec<String>,
    pub label: String,
    pub visibility: AgentTaskGateVisibility,
    pub reveal_policy: AgentTaskGateRevealPolicy,
    pub environment: AgentTaskGateEnvironmentPolicy,
    pub package_artifacts: Vec<AgentTaskGatePackageArtifactRequirement>,
    pub declared_plan: Option<homeboy_engine_primitives::test_execution::TestExecutionPlan>,
    pub timeout_seconds: u64,
    pub no_progress_timeout_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabGateReceipt {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readiness: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub materialization: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_ref: Option<String>,
    pub schema: String,
    pub request_sha256: String,
    pub candidate: AgentTaskPromotionCandidate,
    pub execution_context: serde_json::Value,
    pub report: AgentTaskGateReport,
}

impl LabGateRequest {
    pub fn identity(&self) -> Result<String> {
        serde_json::to_vec(self)
            .map(|bytes| {
                format!(
                    "sha256:{}",
                    homeboy_engine_primitives::content_hash::sha256_hex(&bytes)
                )
            })
            .map_err(|error| {
                Error::internal_json(
                    error.to_string(),
                    Some("encode Lab gate request".to_string()),
                )
            })
    }

    /// Snapshot materialization may change the index projection and absolute
    /// path, but must retain the recorded HEAD, base and exact candidate tree.
    pub fn verify_materialized_candidate(&self, cwd: &Path) -> Result<()> {
        let actual = candidate_fingerprint(&cwd.display().to_string())?;
        match (&self.candidate, actual) {
            (
                AgentTaskPromotionCandidate::Git {
                    fingerprint: expected,
                },
                AgentTaskPromotionCandidate::Git {
                    fingerprint: actual,
                },
            ) if expected.head == actual.head
                && expected.base == actual.base
                && !expected.tree.is_empty()
                && expected.tree == actual.tree =>
            {
                Ok(())
            }
            _ => Err(Error::invalid_argument(
                "gate.candidate",
                "Lab gate workspace differs from the controller candidate",
            )),
        }
    }

    pub fn verify_receipt(&self, receipt: &LabGateReceipt) -> Result<()> {
        if receipt.schema != LAB_GATE_RECEIPT_SCHEMA
            || receipt.request_sha256 != self.identity()?
            || receipt.candidate != self.candidate
            || receipt.report.id != format!("gate-{}", self.index)
        {
            return Err(Error::invalid_argument(
                "gate.receipt",
                "Lab gate receipt is not bound to the admitted candidate and invocation",
            ));
        }
        let context = homeboy_core::runner_job_execution_context::RunnerJobExecutionContext::from_evidence_record(&receipt.execution_context)?;
        if context.runner_id() != self.runner_id
            || receipt
                .execution_context
                .pointer("/context/verification/state")
                .and_then(serde_json::Value::as_str)
                != Some("verified")
        {
            return Err(Error::invalid_argument(
                "gate.receipt",
                "Lab gate receipt has the wrong execution owner",
            ));
        }
        Ok(())
    }
}

pub trait LabGateTransport: Send + Sync {
    fn declared_runner(&self, _command: &str) -> Result<Option<String>> {
        Ok(None)
    }
    fn execute(
        &self,
        cwd: &Path,
        request: &LabGateRequest,
        supervision: Option<&GateSupervision>,
    ) -> Result<LabGateReceipt>;
}

pub(crate) fn declared_runner(command: &str) -> Result<Option<String>> {
    active_transport().declared_runner(command)
}

struct Unavailable;
impl LabGateTransport for Unavailable {
    fn execute(
        &self,
        _: &Path,
        _: &LabGateRequest,
        _: Option<&GateSupervision>,
    ) -> Result<LabGateReceipt> {
        Err(Error::new(
            homeboy_core::ErrorCode::RunnerLabTransportFailure,
            "admitted Lab gate execution transport is unavailable",
            serde_json::json!({"gate_disposition":"unavailable", "stage":"transport_registration"}),
        ))
    }
}

homeboy_engine_primitives::provider_registry_arc! {
    provider: dyn LabGateTransport,
    noop: Unavailable,
    register: pub fn register_lab_gate_transport,
    active: fn active_transport,
}

pub(super) fn dispatch(
    cwd: &Path,
    request: &LabGateRequest,
    supervision: Option<&GateSupervision>,
) -> Result<AgentTaskGateReport> {
    let receipt = match active_transport().execute(cwd, request, supervision) {
        Ok(receipt) => receipt,
        Err(error) => return Ok(non_execution_report(request, &error)),
    };
    request.verify_receipt(&receipt)?;
    // A runner receipt cannot authorize a destination edited during transport.
    if candidate_fingerprint(&cwd.display().to_string())? != request.candidate {
        return Err(Error::invalid_argument(
            "gate.candidate",
            "controller candidate changed during Lab gate execution",
        ));
    }
    let mut report = receipt.report;
    // The step is an in-memory compatibility view and is not on the wire.
    // Rebuild it from the verified terminal result, never its serde skip default.
    report.step = PlanStep::builder(
        report.id.clone(),
        "agent_task.gate",
        match report.status {
            AgentTaskGateStatus::Succeeded => PlanStepStatus::Success,
            AgentTaskGateStatus::Failed | AgentTaskGateStatus::AcceptedInheritedFailure => {
                PlanStepStatus::Failed
            }
            AgentTaskGateStatus::Skipped
            | AgentTaskGateStatus::Deferred
            | AgentTaskGateStatus::Unavailable => PlanStepStatus::Skipped,
        },
    )
    .gate_result(HomeboyGateResult::from(report.clone()))
    .build();
    report.lab_receipt = Some(serde_json::json!({
        "schema": receipt.schema,
        "request_sha256": receipt.request_sha256,
        "candidate": receipt.candidate,
        "execution_context": receipt.execution_context,
        "artifact_ref": receipt.artifact_ref,
        "materialization": receipt.materialization,
    }));
    Ok(report)
}

fn non_execution_report(request: &LabGateRequest, error: &Error) -> AgentTaskGateReport {
    gate_non_execution_report(
        request.index,
        request.argv.clone(),
        request.visibility,
        request.reveal_policy,
        error,
    )
}

pub(crate) fn gate_non_execution_report(
    index: usize,
    argv: Vec<String>,
    visibility: AgentTaskGateVisibility,
    reveal_policy: AgentTaskGateRevealPolicy,
    error: &Error,
) -> AgentTaskGateReport {
    let unavailable = error
        .details
        .get("gate_disposition")
        .and_then(serde_json::Value::as_str)
        == Some("unavailable");
    let mut report = AgentTaskGateReport::new(
        format!("gate-{index}"),
        argv,
        1,
        "",
        "Lab gate did not produce verified execution evidence",
        None,
        visibility,
        reveal_policy,
        AgentTaskGateEnvironment::default(),
    );
    if unavailable {
        report.status = AgentTaskGateStatus::Unavailable;
    }
    if error
        .details
        .get("gate_disposition")
        .and_then(serde_json::Value::as_str)
        == Some("cancelled")
    {
        report.termination = AgentTaskGateTermination::Cancelled;
    }
    report.lab_receipt = Some(
        serde_json::json!({"schema":LAB_GATE_DISPOSITION_SCHEMA, "disposition": error.details.get("gate_disposition").and_then(serde_json::Value::as_str).unwrap_or("failed"), "executed":false, "error_code":error.code.as_str(), "stage":error.details.get("stage"), "job_id":error.details.get("job_id"), "artifact_ref":error.details.get("artifact_ref")}),
    );
    report.step = PlanStep::builder(report.id.clone(), "agent_task.gate", PlanStepStatus::Failed)
        .gate_result(HomeboyGateResult::new(
            report.id.clone(),
            report.id.clone(),
            HomeboyGateKind::Command,
            if unavailable {
                HomeboyGateStatus::Blocked
            } else {
                HomeboyGateStatus::Failed
            },
        ))
        .build();
    report
}

pub(super) fn preflight(
    cwd: &Path,
    runner_id: &str,
    environment: &AgentTaskGateEnvironmentPolicy,
    toolchains: &[AgentTaskGateToolchainRequirement],
    packages: &[AgentTaskGatePackageArtifactRequirement],
    timeout: Duration,
) -> Result<()> {
    let request = LabGateRequest {
        preflight_only: true,
        toolchains: toolchains.to_vec(),
        runner_id: runner_id.to_string(),
        candidate: candidate_fingerprint(&cwd.display().to_string())?,
        index: 0,
        argv: vec![],
        label: String::new(),
        visibility: AgentTaskGateVisibility::Visible,
        reveal_policy: AgentTaskGateRevealPolicy::FullEvidence,
        environment: environment.clone(),
        package_artifacts: packages.to_vec(),
        declared_plan: None,
        timeout_seconds: timeout.as_secs().max(1),
        no_progress_timeout_seconds: timeout.as_secs().max(1),
    };
    let receipt = active_transport().execute(cwd, &request, None)?;
    request.verify_receipt(&receipt)?;
    if receipt
        .readiness
        .as_ref()
        .and_then(|value| value.get("status"))
        .and_then(serde_json::Value::as_str)
        != Some("ready")
        || receipt.report.status != AgentTaskGateStatus::Skipped
    {
        return Err(Error::new(
            homeboy_core::ErrorCode::RunnerLabTransportFailure,
            "runner did not return a ready non-execution readiness receipt",
            serde_json::json!({"gate_disposition":"unavailable", "stage":"readiness", "artifact_ref":receipt.artifact_ref}),
        ));
    }
    Ok(())
}

/// Called by the admitted runner command while its verified execution context
/// still resolves against the runner-owned job store, before private HOME.
pub fn execute_admitted(request: LabGateRequest, cwd: &Path) -> Result<LabGateReceipt> {
    execute_admitted_with_resources(request, cwd, None)
}

pub fn execute_admitted_with_resources(
    request: LabGateRequest,
    cwd: &Path,
    resources: Option<Vec<AgentTaskGateExtensionInput>>,
) -> Result<LabGateReceipt> {
    let context = homeboy_core::runner_job_execution_context::RunnerJobExecutionContext::from_direct_daemon_child_environment(&request.runner_id)?
        .ok_or_else(|| Error::invalid_argument("gate.admission", "Lab gate requires a live authenticated runner reservation"))?;
    context.verify_integrity()?;
    execute_with_context_and_resources(request, cwd, &context, resources)
}

#[cfg(test)]
fn execute_with_context(
    request: LabGateRequest,
    cwd: &Path,
    context: &homeboy_core::runner_job_execution_context::RunnerJobExecutionContext,
) -> Result<LabGateReceipt> {
    execute_with_context_and_resources(request, cwd, context, None)
}

fn execute_with_context_and_resources(
    request: LabGateRequest,
    cwd: &Path,
    context: &homeboy_core::runner_job_execution_context::RunnerJobExecutionContext,
    resources: Option<Vec<AgentTaskGateExtensionInput>>,
) -> Result<LabGateReceipt> {
    context.verify_integrity()?;
    if context.is_local() || context.runner_id() != request.runner_id {
        return Err(Error::invalid_argument(
            "gate.admission",
            "Lab gate requires the selected authenticated runner owner",
        ));
    }
    request.verify_materialized_candidate(cwd)?;
    let mut environment = request.environment.clone();
    environment.lab_runner = None;
    // Runner credentials and its operator registry are not gate inputs. Lab
    // gates receive declared variables/preserve mappings even when the
    // historical local gate policy inherits the controller environment.
    environment.mode = AgentTaskGateEnvironmentMode::Replace;
    if !environment.variables.contains_key("PATH") && !environment.preserve.contains_key("PATH") {
        let runtime = Path::new(context.runtime_id());
        let runtime_parent = runtime.is_absolute().then(|| runtime.parent()).flatten();
        let path = runtime_parent
            .map(|parent| format!("{}:/usr/local/bin:/usr/bin:/bin", parent.display()))
            .unwrap_or_else(|| "/usr/local/bin:/usr/bin:/bin".to_string());
        environment.variables.insert("PATH".to_string(), path);
    }
    if let Some(resources) = resources {
        if resources.len() != environment.extension_inputs.len()
            || resources.iter().zip(&environment.extension_inputs).any(
                |(materialized, declared)| {
                    materialized.id != declared.id
                        || materialized.identity.is_none()
                        || declared.identity.as_ref().is_some_and(|identity| {
                            materialized.identity.as_ref() != Some(identity)
                        })
                },
            )
        {
            return Err(Error::invalid_argument(
                "gate.resources",
                "materialized gate extensions differ from the declared closure",
            ));
        }
        environment.extension_inputs = resources;
    }
    // The existing subprocess projection is emitted only after authenticating
    // the accepted runner context. Nested Homeboy gates execute in place rather
    // than attempting admission through their deliberately private registry.
    environment.variables.insert(homeboy_core::observation::LAB_OFFLOAD_METADATA_ENV.to_string(),
        serde_json::json!({"schema": "homeboy/lab-offload-subprocess/v1", "runner_id": context.runner_id(), "runner_job_execution_context": context.evidence_record()?}).to_string());
    if !environment.isolate_home || !environment.isolate_xdg {
        return Err(Error::invalid_argument(
            "gate.environment",
            "Lab gates require private HOME and XDG directories",
        ));
    }
    let supervision = GateSupervision {
        timeout: Duration::from_secs(request.timeout_seconds.max(1)),
        no_progress_timeout: Duration::from_secs(request.no_progress_timeout_seconds.max(1)),
        heartbeat_interval: Duration::from_secs(5),
        on_spawn: Arc::new(|_, _| Ok(())),
        on_heartbeat: Arc::new(|_| Ok(())),
        is_cancelled: Arc::new(|| false),
    };
    let run_dir = homeboy_core::engine::run_dir::RunDir::create()?;
    let invocation =
        homeboy_core::engine::invocation::InvocationGuard::acquire(&run_dir, &Default::default())?;
    let result = if request.preflight_only {
        preflight_gate_toolchains_local(
            cwd,
            &environment,
            &request.toolchains,
            &request.package_artifacts,
            Some(&invocation.context().tmp_dir),
            supervision.timeout,
            true,
        )
        .map(|()| {
            AgentTaskGateReport::skipped(
                format!("gate-{}", request.index),
                vec![],
                request.visibility,
                request.reveal_policy,
                "readiness-only",
            )
        })
    } else {
        run_gate_argv_local(
            cwd,
            request.index,
            request.argv.clone(),
            &request.label,
            request.visibility,
            request.reveal_policy,
            GateExecution {
                runtime_tmpdir: Some(&invocation.context().tmp_dir),
                timeout: Some(supervision.timeout),
                supervision: Some(&supervision),
                baseline_timeout_diagnostic: false,
            },
            &environment,
            &request.package_artifacts,
            request.declared_plan.as_ref(),
        )
    };
    run_dir.finish(
        matches!(&result, Ok(report) if report.status == AgentTaskGateStatus::Succeeded
        || (request.preflight_only && report.status == AgentTaskGateStatus::Skipped)),
    );
    let (mut report, readiness) = match result {
        Ok(report) => (report, request.preflight_only.then(|| serde_json::json!({"schema":"homeboy/lab-gate-readiness/v1", "status":"ready", "executed_gate":false, "toolchains":request.toolchains}))),
        Err(error) => {
            let mut setup = error.clone();
            setup.details["gate_disposition"] = serde_json::json!("unavailable");
            let mut report = non_execution_report(&request, &setup);
            report.lab_receipt.as_mut().expect("non-execution receipt")["diagnostic"] = homeboy_core::redaction::redact_json(&serde_json::json!({"code":error.code.as_str(), "message":error.message, "details":error.details}));
            (report, Some(serde_json::json!({"schema":"homeboy/lab-gate-readiness/v1", "status":"unavailable", "executed_gate":false})))
        }
    };
    if let Some(plan) = &request.declared_plan {
        report.invocation = Some(AgentTaskGateInvocation::DeclaredTest { plan: plan.clone() });
    }
    report.environment.lab_runner = Some(request.runner_id.clone());
    request.verify_materialized_candidate(cwd)?;
    Ok(LabGateReceipt {
        readiness,
        materialization: None,
        artifact_ref: None,
        schema: LAB_GATE_RECEIPT_SCHEMA.to_string(),
        request_sha256: request.identity()?,
        candidate: request.candidate,
        execution_context: context.evidence_record()?,
        report,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_closure_comparison_checks_terminal_shared_evidence_and_destinations() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::create_dir_all(source.join("fixture")).unwrap();
        fs::create_dir_all(source.join("shared")).unwrap();
        fs::write(source.join("fixture/fixture.json"), "{}").unwrap();
        fs::write(source.join("shared/declared.txt"), "declared").unwrap();
        fs::write(
            source.join("homeboy-extension-root.json"),
            r#"{"shared_assets":["shared"]}"#,
        )
        .unwrap();
        let (_, expected) = seal_extension_resources(
            &root.path().join("sealed"),
            &[AgentTaskGateExtensionInput {
                id: "fixture".to_string(),
                source: source.join("fixture").display().to_string(),
                identity: None,
            }],
        )
        .unwrap();
        assert_eq!(expected[0].shared_assets.len(), 1);
        let mut transported = expected.clone();
        transported[0].source = "/runner/package/fixture".to_string();
        transported[0].shared_assets[0].source = "/runner/package/shared".to_string();
        verify_extension_resource_closure(&expected, &transported).unwrap();
        let mut altered = transported.clone();
        altered[0].shared_assets[0].identity = "altered-terminal-shared-tree".to_string();
        assert_eq!(
            altered[0].identity, expected[0].identity,
            "test keeps the extension-level claim unchanged"
        );
        assert!(verify_extension_resource_closure(&expected, &altered).is_err());
        altered = transported.clone();
        altered[0].shared_assets.clear();
        assert!(verify_extension_resource_closure(&expected, &altered).is_err());
        altered = transported.clone();
        altered[0].shared_assets[0].destination = ".config/homeboy/undeclared".to_string();
        assert!(verify_extension_resource_closure(&expected, &altered).is_err());
        altered = transported.clone();
        altered.push(transported[0].clone());
        assert!(verify_extension_resource_closure(&expected, &altered).is_err());
        fs::write(source.join("shared/declared.txt"), "changed on controller").unwrap();
        let (_, changed) = seal_extension_resources(
            &root.path().join("revalidated"),
            &[AgentTaskGateExtensionInput {
                id: "fixture".to_string(),
                source: source.join("fixture").display().to_string(),
                identity: None,
            }],
        )
        .unwrap();
        assert_eq!(
            fs::read(source.join("fixture/fixture.json")).unwrap(),
            b"{}"
        );
        assert_ne!(
            expected[0].shared_assets[0].identity,
            changed[0].shared_assets[0].identity
        );
        assert!(verify_extension_resource_closure(&expected, &changed).is_err());
    }

    fn repository(root: &Path) {
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.name", "Fixture"],
            vec!["config", "user.email", "fixture@example.invalid"],
        ] {
            assert!(Command::new("git")
                .args(args)
                .current_dir(root)
                .status()
                .unwrap()
                .success());
        }
        fs::write(root.join("candidate.txt"), "candidate\n").unwrap();
        assert!(Command::new("git")
            .args(["add", "."])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["commit", "-qm", "candidate"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
    }

    fn request(root: &Path) -> LabGateRequest {
        LabGateRequest {
            preflight_only: false,
            toolchains: vec![],
            runner_id: "lab".to_string(),
            candidate: candidate_fingerprint(&root.display().to_string()).unwrap(),
            index: 1,
            argv: vec![
                "sh".to_string(),
                "-c".to_string(),
                "test -f candidate.txt && test ! -e \"$HOME/operator-marker\" && test -z \"${UNDECLARED_OPERATOR_CREDENTIAL:-}\" && printf executed"
                    .to_string(),
            ],
            label: "candidate gate".to_string(),
            visibility: AgentTaskGateVisibility::Visible,
            reveal_policy: AgentTaskGateRevealPolicy::FullEvidence,
            environment: AgentTaskGateEnvironmentPolicy {
                hydrate_rust_cache: false,
                mode: AgentTaskGateEnvironmentMode::Inherit,
                ..Default::default()
            },
            package_artifacts: vec![],
            declared_plan: None,
            timeout_seconds: 30,
            no_progress_timeout_seconds: 30,
        }
    }

    #[test]
    fn terminal_receipt_binds_execution_and_rejects_changed_candidate_and_invocation() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let _credential = homeboy_core::test_support::EnvVarGuard::set(
                "UNDECLARED_OPERATOR_CREDENTIAL",
                "fixture-secret",
            );
            let temp = tempfile::tempdir().unwrap();
            repository(temp.path());
            let request = request(temp.path());
            let context = homeboy_core::runner_job_execution_context::RunnerJobExecutionContext::direct_daemon(Some("controller-run"), "lab", "job", "homeboy", "reservation").unwrap();
            let receipt = execute_with_context(request.clone(), temp.path(), &context).unwrap();
            assert_eq!(receipt.report.status, AgentTaskGateStatus::Succeeded);
            assert_eq!(receipt.report.stdout, "executed");
            assert_eq!(
                receipt.report.environment.mode,
                AgentTaskGateEnvironmentMode::Replace
            );
            assert!(receipt
                .report
                .environment
                .sanitized
                .iter()
                .any(|value| value.name == "HOME"));
            request.verify_receipt(&receipt).unwrap();
            let mut altered = request.clone();
            altered.argv.push("altered".to_string());
            assert!(altered.verify_receipt(&receipt).is_err());
            let mut wrong_owner = receipt.clone();
            wrong_owner.execution_context =
                homeboy_core::runner_job_execution_context::RunnerJobExecutionContext::local(
                    "homeboy",
                )
                .evidence_record()
                .unwrap();
            assert!(request.verify_receipt(&wrong_owner).is_err());
            fs::write(temp.path().join("candidate.txt"), "altered candidate\n").unwrap();
            assert!(request.verify_materialized_candidate(temp.path()).is_err());
        });
    }

    #[test]
    fn unverified_wire_context_cannot_execute_a_lab_gate() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().unwrap();
            repository(temp.path());
            let context = homeboy_core::runner_job_execution_context::RunnerJobExecutionContext::direct_daemon(Some("controller-run"), "lab", "job", "homeboy", "reservation").unwrap();
            let assertion = serde_json::from_value(serde_json::to_value(context).unwrap()).unwrap();
            assert!(execute_with_context(request(temp.path()), temp.path(), &assertion).is_err());
        });
    }
}
