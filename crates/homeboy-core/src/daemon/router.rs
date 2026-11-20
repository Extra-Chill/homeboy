//! The daemon's HTTP router. One trusted entry point, [`dispatch`], serves
//! both the network listener (after broker authentication) and in-process
//! callers.

use super::*;

pub fn route(method: &str, path: &str) -> HttpResponse {
    route_with_body(method, path, None, daemon_job_store())
}

/// In-process daemon dispatch against a caller-owned job store, with an optional
/// request body. This is the same trusted-local router the network entry point
/// uses after authentication; it lets callers in higher crates (e.g. the runner
/// crate that registers the real `RunnerExecDriver`) exercise the `/exec` and
/// file routes end to end without standing up an HTTP listener. In-process
/// dispatch is already inside the trust boundary.
pub fn route_with_body(
    method: &str,
    path: &str,
    body: Option<serde_json::Value>,
    job_store: &JobStore,
) -> HttpResponse {
    dispatch(
        method,
        path,
        body,
        job_store,
        UnsupportedAnalysisJobRunner,
        remote_runner::BrokerAuthContext::trusted_local(),
    )
}

/// The common route shape: an endpoint's JSON body, or its error at `error_status`.
fn endpoint(operation: &str, error_status: u16, result: Result<serde_json::Value>) -> HttpResponse {
    match result {
        Ok(body) => daemon_endpoint_response(operation, body),
        Err(error) => error_response(error_status, error),
    }
}

pub(super) fn dispatch<R>(
    method: &str,
    path: &str,
    body: Option<serde_json::Value>,
    job_store: &JobStore,
    analysis_runner: R,
    broker_auth: remote_runner::BrokerAuthContext,
) -> HttpResponse
where
    R: AnalysisJobRunner,
{
    if method == "POST"
        && intake_draining()
        && matches!(
            path.split('?').next().unwrap_or(path),
            // `/controller/jobs` queues for the replacement in admission.
            "/jobs" | "/exec" | "/runner/jobs" | "/admissions" | "/v1/control-plane/runs"
        )
        && !post_admits_owned_work(path, body.as_ref(), job_store)
    {
        return error_response(503, daemon_draining_error());
    }
    // Health must never reconcile or inspect the job ledger on a request worker.
    if method == "GET" && path.split('?').next() == Some("/health") {
        return health_response(job_store);
    }
    // Expired reservations are reclaimed by the background reconciler. A
    // request worker never runs a durable store transaction just to route.
    match (method, path.split('?').next().unwrap_or(path)) {
        ("GET", homeboy_runner_contract::RUNNER_API_DESCRIBE_PATH) => {
            endpoint("runner.describe", 503, describe_runner(job_store))
        }
        ("GET", "/lifecycle/identity") => match daemon_endpoint_identity(path) {
            Ok(body) => HttpResponse {
                status_code: 200,
                body,
                artifact: None,
            },
            Err(err) => error_response(503, err),
        },
        ("POST", "/runner-inventory") => {
            let workspace_root = body
                .as_ref()
                .and_then(|value| value.get("workspace_root"))
                .and_then(serde_json::Value::as_str)
                .map(PathBuf::from);
            endpoint(
                "runner.inventory",
                500,
                crate::runner_inventory::pressure_report(workspace_root, Duration::from_secs(2))
                    .map(|report| json!(report)),
            )
        }
        ("GET", "/version") => HttpResponse {
            status_code: 200,
            body: json!({
                "version": VERSION,
                "build_identity": build_identity::current(),
                "lab_contract_version": homeboy_lab_contract::LAB_CONTRACT_VERSION,
                "lab_handoff_capabilities": homeboy_lab_contract::required_lab_handoff_capabilities(),
                "runner_optional_capabilities": homeboy_lab_contract::runner_optional_capabilities(),
                "runtime_paths": daemon_runtime_paths_body(),
                "lease": heartbeat_lease().ok(),
            }),
            artifact: None,
        },
        ("GET", "/capacity") => daemon_endpoint_response(
            "daemon.capacity",
            json!({
                "capacity": crate::lab_capacity::observe(None),
                "queued_steps": crate::lab_capacity::queued_steps(),
                "step_queue": crate::lab_capacity::step_queue_snapshot(),
                "gate_reserve": job_store.gate_capacity(),
                "queued_work": job_store.active_runner_jobs().into_iter()
                    .filter(|job| job.status == JobStatus::Queued).collect::<Vec<_>>(),
            }),
        ),
        ("GET", "/capabilities") => daemon_endpoint_response(
            "daemon.capabilities",
            json!({
                "schema": "homeboy/daemon-capabilities/v1",
                "capabilities": [
                    crate::workspace_claim::WorkspaceClaimProtocol::current(),
                    crate::workspace_claim::WorkspaceOwnerLeaseProtocol::current(),
                    // The Runner API v1 operations the broker serves negotiate
                    // through this list too (#13881).
                    homeboy_runner_contract::RunnerApiWatchCapability::current(),
                ],
            }),
        ),
        ("GET", "/config/paths") => match config_paths_body() {
            Ok(body) => HttpResponse {
                status_code: 200,
                body,
                artifact: None,
            },
            Err(err) => error_response(500, err),
        },
        ("POST", "/health") | ("POST", "/version") | ("POST", "/config/paths") => {
            method_not_allowed()
        }
        ("POST", "/exec") => endpoint(
            "jobs.exec",
            400,
            with_daemon_job_admission(|| enqueue_exec_job(body, job_store)),
        ),
        ("POST", "/workspace-claims/acquire") => endpoint(
            "workspace_claim.acquire",
            400,
            acquire_workspace_claim(body),
        ),
        ("POST", "/workspace-claims/validate") => endpoint(
            "workspace_claim.validate",
            400,
            validate_workspace_claim(body),
        ),
        ("POST", "/workspace-claims/release") => endpoint(
            "workspace_claim.release",
            400,
            release_workspace_claim(body),
        ),
        ("POST", "/workspace-claims/authority") => endpoint(
            "workspace_claim.authority",
            400,
            workspace_claim_authority_status(body),
        ),
        ("POST", "/workspace-owners/register") => endpoint(
            "workspace_owner.register",
            400,
            register_workspace_owner(body),
        ),
        ("POST", "/workspace-owners/validate") => endpoint(
            "workspace_owner.validate",
            400,
            validate_workspace_owner(body),
        ),
        ("POST", "/workspace-owners/renew") => {
            endpoint("workspace_owner.renew", 400, renew_workspace_owner(body))
        }
        ("POST", "/workspace-owners/release") => endpoint(
            "workspace_owner.release",
            400,
            release_workspace_owner(body),
        ),
        ("POST", "/controller/jobs") => endpoint(
            "controller.jobs.create",
            400,
            with_daemon_job_admission(|| {
                let request = parse_controller_job_request(body)?;
                let admission = admit_controller_job(request, job_store)?;
                Ok(controller_job_admission_response(&admission, job_store))
            }),
        ),
        ("POST", path) if path.starts_with("/controller/jobs/") && path.ends_with("/start") => {
            endpoint(
                "controller.jobs.start",
                400,
                parse_controller_job_path_id(path, "/start")
                    .and_then(|job_id| start_controller_job(job_id, job_store))
                    .map(controller_job_operation_response),
            )
        }
        ("POST", path) if path.starts_with("/controller/jobs/") && path.ends_with("/cancel") => {
            let cancel = |job_id| {
                let reason = parse_controller_job_cancel_reason(body)?;
                cancel_controller_job(job_id, &reason, job_store)
            };
            endpoint(
                "controller.jobs.cancel",
                400,
                parse_controller_job_path_id(path, "/cancel")
                    .and_then(cancel)
                    .map(controller_job_cancel_response),
            )
        }
        ("GET", "/workflows") => {
            daemon_endpoint_response("workflows.list", workflow_job::workflow_list(job_store))
        }
        ("GET", "/workflows/cadences") => endpoint(
            "workflows.cadences.list",
            400,
            workflow_cadence::http_list(job_store),
        ),
        ("POST", "/workflows/cadences") => endpoint(
            "workflows.cadences.register",
            400,
            workflow_cadence::http_register(body),
        ),
        ("POST", path) if path.starts_with("/workflows/cadences/") && path.ends_with("/cancel") => {
            let workflow = path
                .trim_start_matches("/workflows/cadences/")
                .trim_end_matches("/cancel");
            endpoint(
                "workflows.cadences.cancel",
                404,
                workflow_cadence::http_cancel(workflow),
            )
        }
        ("POST", path) if path.starts_with("/workflows/") && path.ends_with("/approvals") => {
            let instance = path
                .trim_start_matches("/workflows/")
                .trim_end_matches("/approvals");
            endpoint(
                "workflows.approve",
                400,
                workflow_job::workflow_approve(job_store, instance, body),
            )
        }
        ("POST", path) if path.starts_with("/workflows/") && path.ends_with("/upgrade") => {
            let target = path
                .trim_start_matches("/workflows/")
                .trim_end_matches("/upgrade");
            endpoint(
                "workflows.upgrade",
                400,
                workflow_job::workflow_upgrade(job_store, target, body),
            )
        }
        ("GET", path) if path.starts_with("/workflows/") => endpoint(
            "workflows.status",
            404,
            workflow_job::workflow_status(job_store, path.trim_start_matches("/workflows/")),
        ),
        ("GET", "/reservation") => endpoint(
            "reservation.status",
            400,
            reservation::current().map(|value| json!(value)),
        ),
        ("POST", "/reservation") => endpoint(
            "reservation.update",
            400,
            with_daemon_job_admission(|| reservation::update(body)),
        ),
        ("POST", "/admissions") => endpoint(
            "admissions.reserve",
            400,
            with_daemon_job_admission(|| reserve_admission(body, job_store)),
        ),
        ("POST", path) if path.starts_with("/admissions/") && path.ends_with("/release") => {
            endpoint(
                "admissions.release",
                400,
                release_admission(path, body, job_store),
            )
        }
        ("POST", path) if path.starts_with("/admissions/") && path.ends_with("/renew") => endpoint(
            "admissions.renew",
            400,
            renew_admission(path, body, job_store),
        ),
        ("POST", "/jobs/reconcile-terminal") => endpoint(
            "jobs.reconcile_terminal",
            400,
            job_store
                .reconcile_terminal_linked_daemon_jobs()
                .map(|reconciled| {
                    json!({
                        "reconciled_job_ids": reconciled,
                        "reconciled_count": reconciled.len(),
                        "policy": RECONCILE_TERMINAL_POLICY,
                    })
                }),
        ),
        ("POST", "/lifecycle/stop") => endpoint(
            "lifecycle.stop",
            400,
            lifecycle_stop_request(body).and_then(|request| {
                validate_lifecycle_stop_request(&request)?;
                Ok(json!({
                    "lease_id": request.lease_id,
                    "force": request.force,
                    "accepted": true,
                }))
            }),
        ),
        ("GET", path)
            if path.starts_with("/controller/jobs/")
                && (path.ends_with("/cancel") || path.ends_with("/start")) =>
        {
            method_not_allowed()
        }
        ("GET", "/exec")
        | ("GET", "/admissions")
        | ("GET", "/workspace-claims/acquire")
        | ("GET", "/workspace-claims/validate")
        | ("GET", "/workspace-claims/release")
        | ("GET", "/workspace-claims/authority")
        | ("GET", "/workspace-owners/register")
        | ("GET", "/workspace-owners/validate")
        | ("GET", "/workspace-owners/renew")
        | ("GET", "/workspace-owners/release")
        | ("GET", "/jobs/reconcile-terminal")
        | ("GET", "/controller/jobs")
        | ("GET", "/lifecycle/stop") => method_not_allowed(),
        ("POST", "/runner/staging/direct") | ("POST", "/runner/staging/capabilities") => {
            remote_runner::route(method, path, body, job_store, &broker_auth)
        }
        ("POST", "/v1/control-plane/provider-effects/reconcile") => {
            match authorize_control_plane_write(body, &broker_auth)
                .and_then(reconcile_provider_effect)
            {
                Ok(response) => {
                    daemon_endpoint_response("control_plane.provider_effects.reconcile", response)
                }
                Err(error) => remote_runner::auth_or_bad_request(error),
            }
        }
        ("POST", path)
            if path == "/v1/control-plane/runs"
                || is_control_plane_reference_registration(path)
                || is_control_plane_event_append(path) =>
        {
            match authorize_control_plane_write(body, &broker_auth) {
                Ok(body) => route_read_only_api(method, path, body, job_store, analysis_runner),
                Err(error) => remote_runner::auth_or_bad_request(error),
            }
        }
        _ => route_read_only_api(method, path, body, job_store, analysis_runner),
    }
}

const RECONCILE_TERMINAL_POLICY: &str = "jobs with durable terminal evidence (terminal linked durable run or recorded terminal result) are reconciled; live and unresolved jobs are preserved";

fn reconcile_provider_effect(body: Option<serde_json::Value>) -> Result<serde_json::Value> {
    let body = body.ok_or_else(|| {
        Error::validation_invalid_argument(
            "body",
            "provider-effect reconciliation requires a JSON request body",
            None,
            None,
        )
    })?;
    let request = serde_json::from_value::<
        homeboy_extension_contract::api::v1::ExtensionApiDeploymentProviderReconcileRequest,
    >(body)
    .map_err(|error| Error::validation_invalid_argument("body", error.to_string(), None, None))?;
    Ok(
        serde_json::to_value(crate::control_plane::reconcile_deployment_provider_effect(
            &request,
        ))
        .unwrap_or(serde_json::Value::Null),
    )
}

/// Health reads only the background freshness snapshot and the lease file.
fn health_response(job_store: &JobStore) -> HttpResponse {
    let lease = heartbeat_lease().ok();
    let mut freshness = job_store
        .daemon_freshness_snapshot
        .lock()
        .expect("health snapshot lock")
        .clone();
    if lease.is_none()
        || freshness.as_ref().is_some_and(|snapshot| {
            snapshot.lease_id.as_deref() != lease.as_ref().map(|lease| lease.lease_id.as_str())
        })
    {
        // The cached lease is no longer proven live. Preserve the snapshot,
        // but fail closed until the background updater observes this revision.
        let stale_reason_code = if lease.is_none() {
            state_path()
                .and_then(|path| validate_lease_file(&path))
                .ok()
                .and_then(|validation| validation.stale_reason_code)
                .unwrap_or(DaemonStaleReasonCode::LeaseMissing)
        } else {
            DaemonStaleReasonCode::LeaseMissing
        };
        if let Some(snapshot) = freshness.as_mut() {
            snapshot.fresh = false;
            snapshot.stale_reason_code = Some(stale_reason_code);
        }
        job_store.health_revision.fetch_add(1, Ordering::Release);
    }
    HttpResponse {
        status_code: 200,
        body: json!({
            "status": "ok",
            "pid": std::process::id(),
            "version": VERSION,
            "build_identity": build_identity::current(),
            "lease": lease,
            "freshness": freshness,
        }),
        artifact: None,
    }
}

pub(super) fn authorize_control_plane_write(
    body: Option<serde_json::Value>,
    broker_auth: &remote_runner::BrokerAuthContext,
) -> Result<Option<serde_json::Value>> {
    let Some(grant) = broker_auth.authorize(crate::broker_auth::BrokerScope::Submit, None)? else {
        return Ok(body);
    };
    if grant.credential_id == "loopback-open" {
        return Err(Error::broker_auth_denied(
            "control-plane writes require a paired credential with submit scope",
            None,
            vec!["Pair a controller credential before mutating durable work.".to_string()],
        ));
    }
    let mut body = body.unwrap_or_else(|| json!({}));
    let object = body.as_object_mut().ok_or_else(|| {
        Error::validation_invalid_argument(
            "body",
            "control-plane submission request body must be a JSON object",
            None,
            None,
        )
    })?;
    object.insert(
        "actor".to_string(),
        serde_json::Value::String(format!("broker:{}", grant.credential_id)),
    );
    Ok(Some(body))
}

pub(super) fn is_control_plane_reference_registration(path: &str) -> bool {
    matches!(
        http_api::route(HttpMethod::Post, path),
        Ok(http_api::HttpEndpoint::ControlPlaneRunReferenceRegister { .. })
    )
}

pub(super) fn is_control_plane_event_append(path: &str) -> bool {
    matches!(
        http_api::route(HttpMethod::Post, path),
        Ok(http_api::HttpEndpoint::ControlPlaneRunEventAppend { .. })
    )
}
