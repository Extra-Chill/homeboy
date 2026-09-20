use std::collections::HashSet;

use crate::component;
use crate::error::{Error, Result};

use super::component::resolution::is_checkout_less_release_candidate;
use super::{Project, StandaloneComponentConfigSnapshot};

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentLocalPathDiagnosticStatus {
    Missing,
    Stale,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ComponentLocalPathDiagnostic {
    pub component_id: String,
    pub local_path: String,
    pub status: ComponentLocalPathDiagnosticStatus,
    pub message: String,
    pub repair_command: String,
}

pub fn calculate_deploy_readiness(project: &Project) -> (bool, Vec<String>) {
    let mut blockers = Vec::new();
    let standalone_snapshot = StandaloneComponentConfigSnapshot::load();
    let resolved_components = project
        .components
        .iter()
        .filter_map(|attachment| {
            super::resolve_project_component_with_standalone_snapshot(
                project,
                &attachment.id,
                Some(&standalone_snapshot),
            )
            .ok()
        })
        .collect::<Vec<_>>();
    let provider_owned = !resolved_components.is_empty()
        && resolved_components.len() == project.components.len()
        && project
            .components
            .iter()
            .all(|attachment| attachment.selects_deployment_provider());

    match &project.server_id {
        _ if provider_owned => {}
        None => {
            blockers.push(format!(
                "Missing server_id - set with: homeboy project set {} --json '{{\"server_id\": \"<server-id>\"}}'",
                project.id
            ));
        }
        Some(sid) if !crate::server::exists(sid) => {
            blockers.push(format!(
                "Server '{}' not found - create with: homeboy server set {} --json '{{\"host\": \"...\", \"user\": \"...\"}}'",
                sid, sid
            ));
        }
        _ => {}
    }

    if !provider_owned
        && project
            .base_path
            .as_ref()
            .map(|p| p.is_empty())
            .unwrap_or(true)
    {
        blockers.push(format!(
            "Missing base_path - set with: homeboy project set {} --json '{{\"base_path\": \"/path/to/webroot\"}}'",
            project.id
        ));
    }

    if project.components.is_empty() {
        blockers.push(format!(
            "No components linked - add with: homeboy project components add {} <component-id> or attach a repo: homeboy project components attach-path {} <component-id> <path>",
            project.id,
            project.id
        ));
    } else {
        blockers.extend(component_local_path_blockers_with_snapshot(
            project,
            Some(&standalone_snapshot),
        ));

        let has_deployable =
            resolved_components
                .iter()
                .zip(&project.components)
                .any(|(comp, attachment)| {
                    let has_provider = attachment.selects_deployment_provider();
                    let is_git = comp.deploy_strategy.as_deref() == Some("git");
                    // An ambiguous artifact owner is an authoring error, not a
                    // readiness signal, and this closure has no error channel. Treat
                    // it as "not deployable" here; `build`/`deploy` surface the
                    // actionable ambiguity error when the component is used.
                    let has_artifact = component::resolve_artifact(comp).ok().flatten().is_some();
                    has_provider || is_git || has_artifact
                });

        if !has_deployable {
            blockers.push(format!(
                "No deployable components - {} component(s) exist but none have a build artifact or deploy strategy configured",
                project.components.len()
            ));
        }
    }

    (blockers.is_empty(), blockers)
}

pub fn validate_component_local_paths(project: &Project) -> Result<()> {
    let blockers = component_local_path_blockers(project);
    if blockers.is_empty() {
        return Ok(());
    }

    let mut err = Error::validation_invalid_argument(
        "components.local_path",
        "Project has component local_path blockers",
        Some(project.id.clone()),
        Some(blockers.clone()),
    );
    for blocker in blockers {
        err = err.with_hint(blocker);
    }
    Err(err)
}

pub fn validate_deploy_component_local_paths(
    project: &Project,
    component_ids: &[String],
) -> Result<()> {
    if component_ids.is_empty() {
        return validate_component_local_paths(project);
    }

    let scoped_ids = scoped_deploy_component_ids(project, component_ids)?;
    let standalone_snapshot = StandaloneComponentConfigSnapshot::load();

    let mut scoped_blockers = Vec::new();
    let mut hygiene_blockers = Vec::new();
    for attachment in &project.components {
        let Some(blocker) = component_local_path_diagnostic(project, attachment) else {
            continue;
        };
        if is_checkout_less_eligible_missing(&blocker, Some(&standalone_snapshot)) {
            continue;
        }

        if scoped_ids.contains(&attachment.id) {
            scoped_blockers.push(blocker.message);
        } else {
            hygiene_blockers.push(blocker.message);
        }
    }

    for blocker in hygiene_blockers {
        log_status!(
            "deploy",
            "Project component local_path hygiene warning: {}",
            blocker
        );
    }

    if scoped_blockers.is_empty() {
        return Ok(());
    }

    let mut err = Error::validation_invalid_argument(
        "components.local_path",
        "Scoped deploy has component local_path blockers",
        Some(project.id.clone()),
        Some(scoped_blockers.clone()),
    );
    for blocker in scoped_blockers {
        err = err.with_hint(blocker);
    }
    Err(err)
}

fn scoped_deploy_component_ids(
    project: &Project,
    component_ids: &[String],
) -> Result<HashSet<String>> {
    let standalone_snapshot = StandaloneComponentConfigSnapshot::load();
    let mut scoped_ids = HashSet::new();
    let mut pending = component_ids.to_vec();

    while let Some(component_id) = pending.pop() {
        if !scoped_ids.insert(component_id.clone()) {
            continue;
        }

        // A missing checkout is reported as a scoped blocker below. We still keep
        // the ID in scope so the operator sees that precise dependency, but we
        // cannot resolve further dependencies from a path that is not present.
        if !component_local_path_findings(project, &component_id).is_empty() {
            continue;
        }

        let component = super::resolve_project_component_with_standalone_snapshot(
            project,
            &component_id,
            Some(&standalone_snapshot),
        )?;

        pending.extend(component.deploy_together);
        pending.extend(
            component
                .artifact_inputs
                .into_iter()
                .map(|input| input.component),
        );
    }

    Ok(scoped_ids)
}

/// The operator-facing local_path findings for one attached component, as
/// values rather than as an error.
///
/// A missing local checkout is a fact about *this host*, not a malformed
/// project. Read-only callers (`deploy --check`, status probes) need to report
/// it as one scoped finding and keep going, instead of aborting a project-wide
/// pass and hiding every other component (#12214). `validate_component_local_path`
/// remains the fail-closed wrapper that every mutating path uses.
pub fn component_local_path_findings(project: &Project, component_id: &str) -> Vec<String> {
    project
        .components
        .iter()
        .filter(|attachment| attachment.id == component_id)
        .filter_map(|attachment| component_local_path_diagnostic(project, attachment))
        .map(|diagnostic| diagnostic.message)
        .collect()
}

pub fn validate_component_local_path(project: &Project, component_id: &str) -> Result<()> {
    let blockers = component_local_path_findings(project, component_id);

    if blockers.is_empty() {
        return Ok(());
    }

    let mut err = Error::validation_invalid_argument(
        "components.local_path",
        "Project component has a missing local_path",
        Some(project.id.clone()),
        Some(blockers.clone()),
    );
    for blocker in blockers {
        err = err.with_hint(blocker);
    }
    Err(err)
}

pub(crate) fn component_local_path_blockers(project: &Project) -> Vec<String> {
    let standalone_snapshot = StandaloneComponentConfigSnapshot::load();
    component_local_path_blockers_with_snapshot(project, Some(&standalone_snapshot))
}

/// [`component_local_path_blockers`] against an already-loaded standalone
/// snapshot, so a caller that loaded one for other reasons (readiness already
/// does, to resolve every component) does not pay for a second one.
fn component_local_path_blockers_with_snapshot(
    project: &Project,
    standalone_snapshot: Option<&StandaloneComponentConfigSnapshot>,
) -> Vec<String> {
    project
        .components
        .iter()
        .filter_map(|attachment| component_local_path_diagnostic(project, attachment))
        .filter(|diagnostic| !is_checkout_less_eligible_missing(diagnostic, standalone_snapshot))
        .map(|diagnostic| diagnostic.message)
        .collect()
}

/// A `Missing` diagnostic (an absent/empty `local_path`) is not an actual
/// deploy blocker when the component can resolve from a GitHub Release
/// without a checkout (#14782) — project-wide and scoped deploy preflight and
/// `project show` readiness must agree with resolution's own fallback
/// eligibility, reusing the identical predicate resolution's checkout-less
/// fallback already applies so the two paths cannot disagree about the same
/// component (#14795).
///
/// This deliberately leaves [`component_local_path_diagnostic`],
/// [`component_local_path_findings`], and [`validate_component_local_path`]
/// themselves unchanged: they are the raw, eligibility-blind fact "does a
/// local checkout exist for this component", and `resolve_project_component`
/// depends on `validate_component_local_path` failing unconditionally on a
/// missing path to know when to *attempt* the checkout-less fallback in the
/// first place (see `resolution.rs`). Making that check eligibility-aware
/// would make it stop signaling "try the fallback" for exactly the components
/// the fallback exists for. The exemption instead lives one layer up, in the
/// blocker-collecting callers that decide whether a missing checkout should
/// stop a deploy or a readiness check — not in the fact-finding primitive
/// resolution relies on.
///
/// A `Stale` diagnostic (a `local_path` that is set but wrong) always blocks
/// regardless of eligibility — that is an authoring error, not the "no
/// checkout at all" shape the checkout-less fallback exists for.
fn is_checkout_less_eligible_missing(
    diagnostic: &ComponentLocalPathDiagnostic,
    standalone_snapshot: Option<&StandaloneComponentConfigSnapshot>,
) -> bool {
    matches!(
        diagnostic.status,
        ComponentLocalPathDiagnosticStatus::Missing
    ) && is_checkout_less_release_candidate(None, &diagnostic.component_id, standalone_snapshot)
}

pub fn component_local_path_diagnostic(
    project: &Project,
    attachment: &super::ProjectComponentAttachment,
) -> Option<ComponentLocalPathDiagnostic> {
    let trimmed = attachment.local_path.trim();
    if trimmed.is_empty() {
        let repair_command = format!(
            "homeboy project components attach-path {} <local-path>",
            project.id
        );
        return Some(ComponentLocalPathDiagnostic {
            component_id: attachment.id.clone(),
            local_path: attachment.local_path.clone(),
            status: ComponentLocalPathDiagnosticStatus::Missing,
            message: format!(
                "Component '{}' is missing local_path - attach a checkout with: {}",
                attachment.id, repair_command
            ),
            repair_command,
        });
    }

    let expanded = shellexpand::tilde(trimmed);
    let path = std::path::Path::new(expanded.as_ref());
    if path.exists() {
        return None;
    }

    let repair_command = format!(
        "homeboy project components attach-path {} <local-path>",
        project.id
    );
    Some(ComponentLocalPathDiagnostic {
        component_id: attachment.id.clone(),
        local_path: attachment.local_path.clone(),
        status: ComponentLocalPathDiagnosticStatus::Stale,
        message: format!(
            "Component '{}' local_path '{}' does not exist - update it with: {}",
            attachment.id, trimmed, repair_command
        ),
        repair_command,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::ProjectComponentAttachment;
    use tempfile::TempDir;

    fn project_with_component(local_path: String) -> Project {
        Project {
            id: "site".to_string(),
            server_id: Some("server".to_string()),
            base_path: Some("/srv/site".to_string()),
            components: vec![ProjectComponentAttachment {
                id: "plugin".to_string(),
                local_path,
                remote_path: Some("wp-content/plugins/plugin".to_string()),
                ..Default::default()
            }],
            ..Project::default()
        }
    }

    fn repo_with_component(id: &str, extra: serde_json::Value) -> TempDir {
        let dir = TempDir::new().expect("temp dir");
        let mut component = serde_json::json!({
            "id": id,
            "remote_path": format!("wp-content/plugins/{id}"),
            "build_artifact": format!("dist/{id}.zip")
        });
        let component_obj = component.as_object_mut().expect("component object");
        for (key, value) in extra.as_object().expect("extra object") {
            component_obj.insert(key.clone(), value.clone());
        }
        std::fs::write(dir.path().join("homeboy.json"), component.to_string())
            .expect("write homeboy.json");
        dir
    }

    fn project_with_components(components: Vec<(&str, String)>) -> Project {
        Project {
            id: "site".to_string(),
            server_id: Some("server".to_string()),
            base_path: Some("/srv/site".to_string()),
            components: components
                .into_iter()
                .map(|(id, local_path)| ProjectComponentAttachment {
                    id: id.to_string(),
                    local_path,
                    remote_path: Some(format!("wp-content/plugins/{id}")),
                    ..Default::default()
                })
                .collect(),
            ..Project::default()
        }
    }

    #[test]
    fn project_show_readiness_blocks_missing_component_local_path() {
        let project = project_with_component("/tmp/homeboy-missing-component-path".to_string());

        let (ready, blockers) = calculate_deploy_readiness(&project);

        assert!(!ready);
        assert!(blockers.iter().any(|blocker| {
            blocker.contains("Component 'plugin' local_path '/tmp/homeboy-missing-component-path' does not exist")
        }));
    }

    /// Read-only callers need the blocker as a value so a missing checkout can be
    /// reported as one scoped finding instead of aborting a project-wide pass (#12214).
    #[test]
    fn component_local_path_findings_report_missing_checkout_without_erroring() {
        let project = project_with_component("/tmp/homeboy-missing-component-path".to_string());

        let findings = component_local_path_findings(&project, "plugin");

        assert_eq!(findings.len(), 1);
        assert!(findings[0].contains(
            "Component 'plugin' local_path '/tmp/homeboy-missing-component-path' does not exist"
        ));
    }

    /// The findings helper and the fail-closed validator must agree — one is the
    /// value form of the other, not a second implementation that can drift.
    #[test]
    fn component_local_path_findings_are_empty_for_a_present_checkout() {
        let repo = repo_with_component("plugin", serde_json::json!({}));
        let project = project_with_component(repo.path().to_string_lossy().to_string());

        assert!(component_local_path_findings(&project, "plugin").is_empty());
        validate_component_local_path(&project, "plugin").expect("present checkout validates");
    }

    /// An unattached component has nothing to report — absence of an attachment is
    /// a different error, surfaced by resolution.
    #[test]
    fn component_local_path_findings_are_empty_for_an_unattached_component() {
        let project = project_with_component("/tmp/homeboy-missing-component-path".to_string());

        assert!(component_local_path_findings(&project, "not-attached").is_empty());
    }

    #[test]
    fn deploy_readiness_validation_fails_closed_on_missing_component_local_path() {
        let project = project_with_component("/tmp/homeboy-missing-component-path".to_string());

        let err = validate_component_local_paths(&project).expect_err("missing path should block");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("component local_path blockers"));
        assert!(err.hints.iter().any(|hint| hint
            .message
            .contains("local_path '/tmp/homeboy-missing-component-path' does not exist")));
    }

    #[test]
    fn scoped_deploy_validation_ignores_unrelated_missing_component_local_path() {
        let requested = repo_with_component("requested", serde_json::json!({}));
        let project = project_with_components(vec![
            ("requested", requested.path().to_string_lossy().to_string()),
            (
                "stale",
                "/tmp/homeboy-stale-unrelated-component".to_string(),
            ),
        ]);

        validate_deploy_component_local_paths(&project, &["requested".to_string()])
            .expect("unrelated stale local_path should be hygiene-only");
    }

    #[test]
    fn scoped_deploy_validation_ignores_multiple_unrelated_missing_component_local_paths() {
        let requested = repo_with_component("requested", serde_json::json!({}));
        let project = project_with_components(vec![
            ("requested", requested.path().to_string_lossy().to_string()),
            (
                "stale-one",
                "/tmp/homeboy-stale-unrelated-component-one".to_string(),
            ),
            (
                "stale-two",
                "/tmp/homeboy-stale-unrelated-component-two".to_string(),
            ),
        ]);

        validate_deploy_component_local_paths(&project, &["requested".to_string()])
            .expect("multiple unrelated stale local_paths should stay hygiene-only");
    }

    #[test]
    fn scoped_deploy_validation_blocks_missing_direct_dependency_local_path() {
        let requested = repo_with_component(
            "requested",
            serde_json::json!({ "deploy_together": ["required"] }),
        );
        let project = project_with_components(vec![
            ("requested", requested.path().to_string_lossy().to_string()),
            (
                "required",
                "/tmp/homeboy-stale-required-component".to_string(),
            ),
            (
                "stale",
                "/tmp/homeboy-stale-unrelated-component".to_string(),
            ),
        ]);

        let err = validate_deploy_component_local_paths(&project, &["requested".to_string()])
            .expect_err("stale direct dependency should block scoped deploy");

        assert!(err.message.contains("Scoped deploy"));
        assert!(err.hints.iter().any(|hint| hint
            .message
            .contains("Component 'required' local_path '/tmp/homeboy-stale-required-component' does not exist")));
        assert!(!err
            .hints
            .iter()
            .any(|hint| hint.message.contains("homeboy-stale-unrelated-component")));
    }

    #[test]
    fn scoped_deploy_validation_blocks_missing_artifact_input_dependency_local_path() {
        let requested = repo_with_component(
            "requested",
            serde_json::json!({
                "artifact_inputs": [{
                    "component": "producer",
                    "artifact": "dist/producer.zip",
                    "target": "vendor/producer.zip",
                    "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                }]
            }),
        );
        let project = project_with_components(vec![
            ("requested", requested.path().to_string_lossy().to_string()),
            (
                "producer",
                "/tmp/homeboy-stale-producer-component".to_string(),
            ),
            (
                "stale",
                "/tmp/homeboy-stale-unrelated-component".to_string(),
            ),
        ]);

        let err = validate_deploy_component_local_paths(&project, &["requested".to_string()])
            .expect_err("stale artifact producer should block scoped deploy");

        assert!(err.hints.iter().any(|hint| hint
            .message
            .contains("Component 'producer' local_path '/tmp/homeboy-stale-producer-component' does not exist")));
        assert!(!err
            .hints
            .iter()
            .any(|hint| hint.message.contains("homeboy-stale-unrelated-component")));
    }

    #[test]
    fn scoped_deploy_validation_blocks_transitive_dependency_local_path() {
        let requested = repo_with_component(
            "requested",
            serde_json::json!({ "deploy_together": ["required"] }),
        );
        let required = repo_with_component(
            "required",
            serde_json::json!({
                "artifact_inputs": [{
                    "component": "producer",
                    "artifact": "dist/producer.zip",
                    "target": "vendor/producer.zip",
                    "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                }]
            }),
        );
        let project = project_with_components(vec![
            ("requested", requested.path().to_string_lossy().to_string()),
            ("required", required.path().to_string_lossy().to_string()),
            (
                "producer",
                "/tmp/homeboy-stale-transitive-producer-component".to_string(),
            ),
            (
                "stale",
                "/tmp/homeboy-stale-unrelated-component".to_string(),
            ),
        ]);

        let err = validate_deploy_component_local_paths(&project, &["requested".to_string()])
            .expect_err("stale transitive dependency should block scoped deploy");

        assert!(err.hints.iter().any(|hint| hint.message.contains(
            "Component 'producer' local_path '/tmp/homeboy-stale-transitive-producer-component' does not exist"
        )));
        assert!(!err
            .hints
            .iter()
            .any(|hint| hint.message.contains("homeboy-stale-unrelated-component")));
    }

    #[test]
    fn full_project_deploy_validation_remains_strict_for_all_components() {
        let requested = repo_with_component("requested", serde_json::json!({}));
        let project = project_with_components(vec![
            ("requested", requested.path().to_string_lossy().to_string()),
            (
                "stale",
                "/tmp/homeboy-stale-unrelated-component".to_string(),
            ),
        ]);

        let err = validate_deploy_component_local_paths(&project, &[])
            .expect_err("full-project deploy should validate every component");

        assert!(err.message.contains("component local_path blockers"));
        assert!(err.hints.iter().any(|hint| hint.message.contains(
            "Component 'stale' local_path '/tmp/homeboy-stale-unrelated-component' does not exist"
        )));
    }

    #[test]
    fn provider_only_project_is_ready_without_server_deployment_fields() {
        let provider = repo_with_component(
            "provider",
            serde_json::json!({
                "build_artifact": null,
                "deployment_provider": {
                    "extension": "fixture-extension",
                    "provider": "fixture.deploy",
                    "policy": {}
                }
            }),
        );
        let mut project = project_with_components(vec![(
            "provider",
            provider.path().to_string_lossy().to_string(),
        )]);
        project.server_id = None;
        project.base_path = None;
        project.components[0].deployment_provider_input =
            Some(serde_json::json!({ "target": "site" }));

        let (ready, blockers) = calculate_deploy_readiness(&project);

        assert!(ready, "unexpected blockers: {blockers:?}");
        assert!(blockers.is_empty());
    }

    #[test]
    fn generic_project_still_requires_server_deployment_fields() {
        let generic = repo_with_component("generic", serde_json::json!({}));
        let mut project = project_with_components(vec![(
            "generic",
            generic.path().to_string_lossy().to_string(),
        )]);
        project.server_id = None;
        project.base_path = None;

        let (ready, blockers) = calculate_deploy_readiness(&project);

        assert!(!ready);
        assert!(blockers
            .iter()
            .any(|blocker| blocker.contains("Missing server_id")));
        assert!(blockers
            .iter()
            .any(|blocker| blocker.contains("Missing base_path")));
    }

    // =========================================================================
    // A missing/empty local_path is not a blocker when the component can
    // resolve from a GitHub Release without a checkout (#14782, #14795).
    // =========================================================================

    fn write_standalone_component(home: &TempDir, component_id: &str, extra: serde_json::Value) {
        let components_dir = home
            .path()
            .join(".config")
            .join("homeboy")
            .join("components");
        std::fs::create_dir_all(&components_dir).expect("components dir");
        std::fs::write(
            components_dir.join(format!("{component_id}.json")),
            extra.to_string(),
        )
        .expect("write standalone component config");
    }

    #[test]
    fn project_wide_validation_passes_when_every_missing_local_path_is_checkout_less_eligible() {
        crate::test_support::with_isolated_home(|home| {
            write_standalone_component(
                home,
                "chubes-gallery-lightbox",
                serde_json::json!({
                    "remote_url": "https://github.com/Extra-Chill/chubes-gallery-lightbox.git"
                }),
            );
            write_standalone_component(
                home,
                "data-machine",
                serde_json::json!({
                    "remote_url": "https://github.com/Extra-Chill/data-machine.git"
                }),
            );
            let project = project_with_components(vec![
                ("chubes-gallery-lightbox", String::new()),
                ("data-machine", String::new()),
            ]);

            validate_component_local_paths(&project)
                .expect("every missing local_path is checkout-less eligible");
            validate_deploy_component_local_paths(&project, &[])
                .expect("--outdated/--all project-wide deploy must not hard-fail either");

            // `calculate_deploy_readiness` / `project show` reuse the same
            // blocker collection — confirmed directly here rather than through
            // `calculate_deploy_readiness` itself, which also attempts full
            // component resolution (a real GitHub Release lookup) for its
            // separate "has a deployable artifact" signal. That resolution
            // path is exercised in `resolution.rs` and in
            // `homeboy-deploy`'s `--outdated` planning tests, with the GitHub
            // layer mocked; it is orthogonal to the local_path gate this test
            // is about.
            assert!(
                component_local_path_blockers(&project).is_empty(),
                "readiness must not report a local_path blocker for a checkout-less-eligible component"
            );
        });
    }

    #[test]
    fn project_wide_validation_names_only_the_non_github_component_in_a_mixed_project() {
        crate::test_support::with_isolated_home(|home| {
            write_standalone_component(
                home,
                "eligible",
                serde_json::json!({
                    "remote_url": "https://github.com/Extra-Chill/eligible.git"
                }),
            );
            write_standalone_component(
                home,
                "non-github",
                serde_json::json!({
                    "remote_url": "https://gitlab.com/example/non-github.git"
                }),
            );
            let project = project_with_components(vec![
                ("eligible", String::new()),
                ("non-github", String::new()),
            ]);

            let err = validate_component_local_paths(&project)
                .expect_err("a non-GitHub remote_url must still block");

            assert!(err.hints.iter().any(|hint| hint
                .message
                .contains("Component 'non-github' is missing local_path")));
            assert!(!err
                .hints
                .iter()
                .any(|hint| hint.message.contains("Component 'eligible'")));
        });
    }

    #[test]
    fn project_wide_validation_names_the_component_with_no_standalone_entry_at_all() {
        crate::test_support::with_isolated_home(|home| {
            write_standalone_component(
                home,
                "eligible",
                serde_json::json!({
                    "remote_url": "https://github.com/Extra-Chill/eligible.git"
                }),
            );
            let project = project_with_components(vec![
                ("eligible", String::new()),
                ("unregistered", String::new()),
            ]);

            let err = validate_component_local_paths(&project)
                .expect_err("a component with no standalone entry must still block");

            assert!(err.hints.iter().any(|hint| hint
                .message
                .contains("Component 'unregistered' is missing local_path")));
            assert!(!err
                .hints
                .iter()
                .any(|hint| hint.message.contains("Component 'eligible'")));
        });
    }

    #[test]
    fn a_stale_local_path_still_blocks_even_when_checkout_less_eligible() {
        crate::test_support::with_isolated_home(|home| {
            write_standalone_component(
                home,
                "eligible",
                serde_json::json!({
                    "remote_url": "https://github.com/Extra-Chill/eligible.git"
                }),
            );
            let project = project_with_components(vec![(
                "eligible",
                "/tmp/homeboy-14795-stale-but-eligible".to_string(),
            )]);

            let err = validate_component_local_paths(&project).expect_err(
                "a local_path that is set but wrong is an authoring error, not \"no checkout\"",
            );

            assert!(err.hints.iter().any(|hint| hint.message.contains(
                "Component 'eligible' local_path '/tmp/homeboy-14795-stale-but-eligible' does not exist"
            )));
        });
    }

    #[test]
    fn mixed_project_keeps_generic_server_requirements() {
        let provider = repo_with_component(
            "provider",
            serde_json::json!({
                "deployment_provider": {
                    "extension": "fixture-extension",
                    "provider": "fixture.deploy",
                    "policy": {}
                }
            }),
        );
        let generic = repo_with_component("generic", serde_json::json!({}));
        let mut project = project_with_components(vec![
            ("provider", provider.path().to_string_lossy().to_string()),
            ("generic", generic.path().to_string_lossy().to_string()),
        ]);
        project.server_id = None;
        project.base_path = None;

        let (ready, blockers) = calculate_deploy_readiness(&project);

        assert!(!ready);
        assert!(blockers
            .iter()
            .any(|blocker| blocker.contains("Missing server_id")));
        assert!(blockers
            .iter()
            .any(|blocker| blocker.contains("Missing base_path")));
    }
}
