use serde_json::{json, Value};

use super::{daemon_endpoint_response, error_response, HttpResponse};
use crate::api_jobs::JobStore;
use crate::broker_auth::{BrokerAuthStore, BrokerScope};
use crate::error::{Error, Result};

/// Per-request broker authentication context extracted from the network layer.
///
/// `handle_connection` (the only network entry point) builds the real context
/// from request headers and the bind address. In-process callers (CLI dispatch,
/// tests) use [`BrokerAuthContext::trusted_local`], which is already inside the
/// trust boundary and bypasses bearer enforcement.
#[derive(Debug, Clone, Default)]
pub(in crate::daemon) struct BrokerAuthContext {
    pub token: Option<String>,
    pub loopback_bind: bool,
    pub trusted_local: bool,
}

impl BrokerAuthContext {
    /// Context for in-process dispatch already inside the trust boundary.
    pub(in crate::daemon) fn trusted_local() -> Self {
        Self {
            token: None,
            loopback_bind: true,
            trusted_local: true,
        }
    }

    /// Authorize this request against the on-disk broker auth store for the
    /// given scope and (optionally) the runner id carried in the request body.
    pub(in crate::daemon) fn authorize(
        &self,
        required: BrokerScope,
        runner_id: Option<&str>,
    ) -> Result<Option<crate::broker_auth::BrokerAuthGrant>> {
        if self.trusted_local {
            return Ok(None);
        }
        let store = BrokerAuthStore::load()?;
        Ok(Some(store.authorize(
            self.loopback_bind,
            self.token.as_deref(),
            required,
            runner_id,
        )?))
    }
}

pub(in crate::daemon) fn route(
    method: &str,
    path: &str,
    body: Option<Value>,
    job_store: &JobStore,
    auth: &BrokerAuthContext,
) -> HttpResponse {
    match (method, path) {
        ("POST", "/runner/staging/capabilities") => match staging_capabilities(body, auth) {
            Ok(body) => daemon_endpoint_response("runner.staging.capabilities", body),
            Err(err) => auth_or_bad_request(err),
        },
        ("POST", "/runner/staging/direct") => {
            match super::with_daemon_job_admission(|| stage_direct(body, job_store, auth)) {
                Ok(body) => daemon_endpoint_response("runner.staging.submit", body),
                Err(err) => auth_or_bad_request(err),
            }
        }
        _ => error_response(
            404,
            Error::validation_invalid_argument(
                "path",
                "unknown remote runner broker path",
                Some(path.to_string()),
                Some(vec![
                    "Use POST /runner/staging/capabilities or POST /runner/staging/direct."
                        .to_string(),
                ]),
            ),
        ),
    }
}

fn staging_runner_id(body: &Option<Value>) -> Result<String> {
    body.as_ref()
        .and_then(|body| body.pointer("/envelope/handoff/runner_id"))
        .and_then(Value::as_str)
        .filter(|runner_id| !runner_id.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "runner_id",
                "sealed staging request requires envelope.handoff.runner_id",
                None,
                None,
            )
        })
}

fn staging_capabilities(body: Option<Value>, auth: &BrokerAuthContext) -> Result<Value> {
    let runner_id = body
        .as_ref()
        .and_then(|body| body.get("runner_id"))
        .and_then(Value::as_str)
        .filter(|runner_id| !runner_id.trim().is_empty())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "runner_id",
                "sealed staging capability lookup requires runner_id",
                None,
                None,
            )
        })?;
    auth.authorize(BrokerScope::Submit, Some(runner_id))?;
    Ok(json!({
        "runner_id": runner_id,
        "capabilities": crate::daemon::runner_staging::with_provider(|provider| provider.capabilities(runner_id))?,
    }))
}

fn stage_direct(
    body: Option<Value>,
    job_store: &JobStore,
    auth: &BrokerAuthContext,
) -> Result<Value> {
    let runner_id = staging_runner_id(&body)?;
    auth.authorize(BrokerScope::Submit, Some(&runner_id))?;
    crate::daemon::runner_staging::with_provider(|provider| {
        provider.stage_direct(body.unwrap_or(Value::Null), job_store)
    })
}

/// Map a handler error to an HTTP response. Broker auth rejections become
/// `401 Unauthorized` (so unauthenticated callers see a distinct status), all
/// other errors keep the existing `400 Bad Request` contract.
pub(in crate::daemon) fn auth_or_bad_request(err: Error) -> HttpResponse {
    if err.code == crate::error::ErrorCode::BrokerAuthDenied {
        error_response(401, err)
    } else {
        error_response(400, err)
    }
}
