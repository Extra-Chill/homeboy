//! Post-deploy front-end smoke check.
//!
//! After a successful real deploy, fetch a configured URL as a fresh
//! (cookie-less) visitor and assert it returns the expected HTTP status and
//! optional content. A failing smoke check fails the deploy (unless
//! `warn_only`) so a runtime-fataling release is flagged for rollback instead
//! of sitting live.
//!
//! This is deliberately stack-agnostic: core only knows "fetch a URL, assert a
//! status/content substring". The concrete front-end URL (e.g. a site
//! home page) is supplied by the project config, keeping core free of any
//! runtime-specific details. See homeboy#5471.

use std::time::Duration;

use crate::core::http_probe::get_status_and_body;
use crate::core::project::SmokeCheckConfig;

/// Outcome of a post-deploy smoke check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SmokeOutcome {
    /// The check passed (status matched and content, if required, was present).
    Passed { status: u16 },
    /// The check ran but the assertion failed.
    Failed { message: String },
    /// The request itself could not be made (connection error, build error).
    Errored { message: String },
}

impl SmokeOutcome {
    pub(super) fn is_ok(&self) -> bool {
        matches!(self, SmokeOutcome::Passed { .. })
    }

    /// Human-readable failure detail, if the smoke did not pass.
    pub(super) fn failure_detail(&self) -> Option<&str> {
        match self {
            SmokeOutcome::Passed { .. } => None,
            SmokeOutcome::Failed { message } | SmokeOutcome::Errored { message } => Some(message),
        }
    }
}

/// Run the post-deploy smoke check described by `config`.
///
/// Returns `None` when the check is disabled (the common, opt-out-by-default
/// case) so callers can cheaply skip when no smoke is configured.
pub(super) fn run_smoke_check(config: &SmokeCheckConfig) -> Option<SmokeOutcome> {
    if !config.enabled {
        return None;
    }

    Some(evaluate(config, |url, timeout| {
        get_status_and_body(url, timeout).map_err(|e| e.message)
    }))
}

/// Evaluate the smoke assertion against a fetcher. Split out from
/// [`run_smoke_check`] so the assertion logic is unit-testable without real
/// network I/O.
fn evaluate<F>(config: &SmokeCheckConfig, fetch: F) -> SmokeOutcome
where
    F: FnOnce(&str, Duration) -> std::result::Result<(u16, String), String>,
{
    let url = config.url.trim();
    if url.is_empty() {
        return SmokeOutcome::Errored {
            message:
                "smoke_check.enabled is true but smoke_check.url is empty — set the front-end URL to probe"
                    .to_string(),
        };
    }

    let timeout = Duration::from_secs(config.timeout_secs.max(1));

    let (status, body) = match fetch(url, timeout) {
        Ok(pair) => pair,
        Err(message) => return SmokeOutcome::Errored { message },
    };

    if status != config.expected_status {
        return SmokeOutcome::Failed {
            message: format!(
                "post-deploy smoke check: {} returned HTTP {} (expected {})",
                url, status, config.expected_status
            ),
        };
    }

    if let Some(needle) = config
        .expect_content
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if !body.contains(needle) {
            return SmokeOutcome::Failed {
                message: format!(
                    "post-deploy smoke check: {} returned HTTP {} but response body did not contain expected content {:?}",
                    url, status, needle
                ),
            };
        }
    }

    SmokeOutcome::Passed { status }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(enabled: bool) -> SmokeCheckConfig {
        SmokeCheckConfig {
            enabled,
            url: "https://example.test/".to_string(),
            expected_status: 200,
            expect_content: None,
            timeout_secs: 5,
            warn_only: false,
        }
    }

    #[test]
    fn disabled_check_is_skipped() {
        assert!(run_smoke_check(&config(false)).is_none());
    }

    #[test]
    fn passes_when_status_matches() {
        let outcome = evaluate(&config(true), |_, _| {
            Ok((200, "<html>ok</html>".to_string()))
        });
        assert!(outcome.is_ok());
        assert_eq!(outcome, SmokeOutcome::Passed { status: 200 });
    }

    #[test]
    fn fails_on_unexpected_status() {
        let outcome = evaluate(&config(true), |_, _| Ok((500, "Fatal error".to_string())));
        assert!(!outcome.is_ok());
        let detail = outcome.failure_detail().expect("failure detail");
        assert!(detail.contains("HTTP 500"));
        assert!(detail.contains("expected 200"));
    }

    #[test]
    fn fails_when_required_content_missing() {
        let mut cfg = config(true);
        cfg.expect_content = Some("Welcome".to_string());
        let outcome = evaluate(&cfg, |_, _| Ok((200, "<html>different</html>".to_string())));
        assert!(!outcome.is_ok());
        assert!(outcome
            .failure_detail()
            .unwrap()
            .contains("did not contain expected content"));
    }

    #[test]
    fn passes_when_required_content_present() {
        let mut cfg = config(true);
        cfg.expect_content = Some("Welcome".to_string());
        let outcome = evaluate(&cfg, |_, _| {
            Ok((200, "<html>Welcome home</html>".to_string()))
        });
        assert!(outcome.is_ok());
    }

    #[test]
    fn errors_on_empty_url() {
        let mut cfg = config(true);
        cfg.url = "   ".to_string();
        let outcome = evaluate(&cfg, |_, _| panic!("fetch must not run for empty url"));
        assert!(matches!(outcome, SmokeOutcome::Errored { .. }));
        assert!(outcome.failure_detail().unwrap().contains("url is empty"));
    }

    #[test]
    fn errors_surface_fetch_failure() {
        let outcome = evaluate(&config(true), |_, _| {
            Err("HTTP GET https://example.test/ failed: connection refused".to_string())
        });
        assert!(matches!(outcome, SmokeOutcome::Errored { .. }));
        assert!(outcome
            .failure_detail()
            .unwrap()
            .contains("connection refused"));
    }
}
