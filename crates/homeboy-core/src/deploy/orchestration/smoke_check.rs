use crate::project::Project;

use super::super::types::ComponentDeployResult;

/// Run the project's post-deploy smoke check, recording the outcome on the
/// deploy results.
///
/// Returns:
/// - `None` when no smoke check is configured/enabled,
/// - `Some(true)` when the smoke FAILED and should fail the deploy,
/// - `Some(false)` when the smoke passed or only warned.
///
/// Warnings/errors are appended to the first deployed component result so they
/// surface in CLI/JSON output alongside the deploy that triggered them.
pub(super) fn run_post_deploy_smoke(
    project: &Project,
    results: &mut [ComponentDeployResult],
) -> Option<bool> {
    let config = project.smoke_check.as_ref()?;
    let outcome = super::super::smoke::run_smoke_check(config)?;

    if outcome.is_ok() {
        log_status!(
            "deploy",
            "Post-deploy smoke check passed for '{}' ({})",
            project.id,
            config.url
        );
        return Some(false);
    }

    let detail = outcome
        .failure_detail()
        .unwrap_or("post-deploy smoke check failed")
        .to_string();

    if config.warn_only {
        log_status!("deploy", "Warning: {} (warn_only)", detail);
        if let Some(first) = results.iter_mut().find(|r| r.status == "deployed") {
            first.warnings.push(format!("{} (warn_only)", detail));
        }
        return Some(false);
    }

    log_status!(
        "deploy",
        "{} — failing deploy; roll back the release",
        detail
    );
    if let Some(first) = results.iter_mut().find(|r| r.status == "deployed") {
        first.error = Some(detail);
    }
    Some(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::Component;
    use crate::project::SmokeCheckConfig;

    fn deployed_result(id: &str) -> ComponentDeployResult {
        let component = Component::new(
            id.to_string(),
            "/tmp/does-not-matter".to_string(),
            String::new(),
            None,
        );
        ComponentDeployResult::new(&component, "/srv/site").with_status("deployed")
    }

    #[test]
    fn post_deploy_smoke_is_noop_without_config() {
        let project = Project {
            id: "site".to_string(),
            ..Project::default()
        };
        let mut results = vec![deployed_result("plugin")];

        assert_eq!(run_post_deploy_smoke(&project, &mut results), None);
        assert_eq!(results[0].status, "deployed");
    }

    #[test]
    fn post_deploy_smoke_noop_when_disabled() {
        let project = Project {
            id: "site".to_string(),
            smoke_check: Some(SmokeCheckConfig {
                enabled: false,
                url: "https://example.test/".to_string(),
                ..Default::default()
            }),
            ..Project::default()
        };
        let mut results = vec![deployed_result("plugin")];

        assert_eq!(run_post_deploy_smoke(&project, &mut results), None);
    }

    #[test]
    fn post_deploy_smoke_failure_records_error_and_fails() {
        // enabled smoke against an unreachable URL fails the deploy and records
        // the error on the deployed component.
        let project = Project {
            id: "site".to_string(),
            smoke_check: Some(SmokeCheckConfig {
                enabled: true,
                // Reserved TEST-NET address (RFC 5737) so the request fails fast.
                url: "http://192.0.2.1:9/".to_string(),
                timeout_secs: 1,
                ..Default::default()
            }),
            ..Project::default()
        };
        let mut results = vec![deployed_result("plugin")];

        assert_eq!(run_post_deploy_smoke(&project, &mut results), Some(true));
        assert!(
            results[0].error.is_some(),
            "failed smoke must record an error on the deployed component"
        );
    }

    #[test]
    fn post_deploy_smoke_warn_only_does_not_fail() {
        let project = Project {
            id: "site".to_string(),
            smoke_check: Some(SmokeCheckConfig {
                enabled: true,
                url: "http://192.0.2.1:9/".to_string(),
                timeout_secs: 1,
                warn_only: true,
                ..Default::default()
            }),
            ..Project::default()
        };
        let mut results = vec![deployed_result("plugin")];

        assert_eq!(run_post_deploy_smoke(&project, &mut results), Some(false));
        assert_eq!(
            results[0].status, "deployed",
            "warn_only smoke must not fail the deploy"
        );
        assert!(
            results[0].warnings.iter().any(|w| w.contains("warn_only")),
            "warn_only smoke failure should be surfaced as a warning"
        );
    }
}
