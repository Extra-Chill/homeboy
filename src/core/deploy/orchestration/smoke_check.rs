use crate::core::project::Project;

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
