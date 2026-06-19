use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Command;

use crate::core::component::{self, Component};
use crate::core::error::{Error, Result};
use crate::core::extension;
use crate::core::git;
use crate::core::plan::{HomeboyPlan, PlanKind, PlanStep, PlanStepStatus, PlanValues};
use crate::core::project::{self, Project};
use crate::core::release::version;
use crate::core::server::SshClient;

use super::types::{
    ComponentStatus, DeployConfig, ReleaseState, ReleaseStateBuckets, ReleaseStateStatus,
};
use super::version_overrides::fetch_remote_versions_for_project;

pub(super) fn calculate_directory_size(path: &Path) -> std::io::Result<u64> {
    let mut total_size = 0;

    if path.is_dir() {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let entry_path = entry.path();

            if entry_path.is_dir() {
                total_size += calculate_directory_size(&entry_path)?;
            } else {
                total_size += entry.metadata()?.len();
            }
        }
    } else {
        total_size = path.metadata()?.len();
    }

    Ok(total_size)
}

/// Format bytes into human-readable format.
pub(super) fn format_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit_index = 0;

    while size >= 1024.0 && unit_index < UNITS.len() - 1 {
        size /= 1024.0;
        unit_index += 1;
    }

    if unit_index == 0 {
        format!("{} {}", size as u64, UNITS[unit_index])
    } else {
        format!("{:.1} {}", size, UNITS[unit_index])
    }
}

// =============================================================================
// Helper Functions
// =============================================================================

/// Plan which components to deploy based on config flags.
pub(super) fn plan_components(
    config: &DeployConfig,
    all_components: &[Component],
    skipped_component_ids: &[String],
    project: &Project,
    base_path: &str,
    client: &SshClient,
) -> Result<Vec<Component>> {
    let plan = plan_component_deploys(
        config,
        all_components,
        skipped_component_ids,
        project,
        base_path,
        client,
    );
    validate_deploy_plan(config, &plan)?;

    Ok(plan.ready_components())
}

pub(super) struct DeployComponentPlan {
    pub plan: HomeboyPlan,
    components: HashMap<String, Component>,
}

impl DeployComponentPlan {
    pub(super) fn ready_components(&self) -> Vec<Component> {
        self.plan
            .steps
            .iter()
            .filter(|step| step.status == PlanStepStatus::Ready)
            .filter_map(|step| step.input_as::<String>("component_id"))
            .filter_map(|component_id| self.components.get(&component_id).cloned())
            .collect()
    }
}

pub(super) fn plan_component_deploys(
    config: &DeployConfig,
    all_components: &[Component],
    skipped_component_ids: &[String],
    project: &Project,
    base_path: &str,
    client: &SshClient,
) -> DeployComponentPlan {
    let components = all_components
        .iter()
        .map(|component| (component.id.clone(), component.clone()))
        .collect::<HashMap<_, _>>();
    let mut steps = Vec::new();

    if !config.component_ids.is_empty() {
        for component_id in &config.component_ids {
            if components.contains_key(component_id) {
                steps.push(
                    deploy_step(component_id, PlanStepStatus::Ready, "explicitly_selected").build(),
                );
            } else if skipped_component_ids.contains(component_id) {
                steps.push(
                    deploy_step(component_id, PlanStepStatus::Disabled, "non_deployable")
                        .skip_reason("Non-deployable component (no artifact/deploy strategy)")
                        .build(),
                );
            } else {
                steps.push(
                    deploy_step(component_id, PlanStepStatus::Missing, "missing")
                        .missing(vec![component_id.clone()])
                        .skip_reason("Unknown requested component")
                        .build(),
                );
            }
        }

        return DeployComponentPlan {
            plan: deploy_plan("component_ids", config, steps),
            components,
        };
    } else if config.check {
        steps.extend(
            all_components.iter().map(|component| {
                deploy_step(&component.id, PlanStepStatus::Ready, "check").build()
            }),
        );
    } else if config.all {
        steps.extend(all_components.iter().map(|component| {
            deploy_step(&component.id, PlanStepStatus::Ready, "all_selected").build()
        }));
    } else if config.outdated {
        let remote_versions =
            fetch_remote_versions_for_project(all_components, Some(project), base_path, client);
        steps.extend(plan_outdated_steps(all_components, &remote_versions));
    } else if config.behind_upstream {
        let mut git_probe_cache = GitProbeCache::default();
        for component in all_components {
            let behind_upstream = git_probe_cache.component_is_behind_upstream(component);
            let mut step = deploy_step(
                &component.id,
                if behind_upstream {
                    PlanStepStatus::Ready
                } else {
                    PlanStepStatus::Skipped
                },
                "behind_upstream",
            )
            .output_value("behind_upstream", serde_json::json!(behind_upstream));
            if !behind_upstream {
                step = step.skip_reason("Component is not behind upstream");
            }
            steps.push(step.build());
        }
    } else {
        steps.push(
            PlanStep::builder(
                "deploy.selection",
                "deploy_selection",
                PlanStepStatus::Missing,
            )
            .missing(vec![
                "component IDs".to_string(),
                "--all".to_string(),
                "--outdated".to_string(),
                "--behind-upstream".to_string(),
                "--check".to_string(),
            ])
            .skip_reason("No deploy selection provided")
            .build(),
        );
    }

    DeployComponentPlan {
        plan: deploy_plan(selection_mode(config), config, steps),
        components,
    }
}

fn plan_outdated_steps(
    all_components: &[Component],
    remote_versions: &HashMap<String, String>,
) -> Vec<PlanStep> {
    all_components
        .iter()
        .map(|component| {
            let local_version = version::get_component_version(component);
            let remote_version = remote_versions.get(&component.id).cloned();
            let needs_update = match (&local_version, &remote_version) {
                (Some(local), Some(remote)) => local != remote,
                _ => true,
            };
            let status = if needs_update {
                PlanStepStatus::Ready
            } else {
                PlanStepStatus::Skipped
            };
            let mut step = deploy_step(&component.id, status, "outdated").output_value(
                "component_status",
                serde_json::json!(if needs_update {
                    "needs_update"
                } else {
                    "up_to_date"
                }),
            );
            if let Some(version) = local_version {
                step = step.output_value("local_version", serde_json::json!(version));
            }
            if let Some(version) = remote_version {
                step = step.output_value("remote_version", serde_json::json!(version));
            }
            if !needs_update {
                step = step.skip_reason("Component is up to date");
            }
            step.build()
        })
        .collect()
}

fn validate_deploy_plan(config: &DeployConfig, plan: &DeployComponentPlan) -> Result<()> {
    if !config.component_ids.is_empty() {
        let non_deployable = plan
            .plan
            .steps
            .iter()
            .filter(|step| step.status == PlanStepStatus::Disabled)
            .filter_map(|step| step.input_as::<String>("component_id"))
            .collect::<Vec<_>>();
        let unknown = plan
            .plan
            .steps
            .iter()
            .filter(|step| step.status == PlanStepStatus::Missing)
            .filter_map(|step| step.input_as::<String>("component_id"))
            .collect::<Vec<_>>();

        if !unknown.is_empty() || !non_deployable.is_empty() {
            let mut details = Vec::new();
            details.extend(unknown);
            if !non_deployable.is_empty() {
                details.push(format!(
                    "Non-deployable components (no artifact/deploy strategy): {}",
                    non_deployable.join(", ")
                ));
            }

            return Err(Error::validation_invalid_argument(
                "componentIds",
                "Invalid component selection",
                None,
                Some(details),
            ));
        }

        if plan.ready_components().is_empty() {
            return Err(empty_selection_error(
                "componentIds",
                "No components selected",
            ));
        }

        return Ok(());
    }

    if config.outdated && plan.ready_components().is_empty() {
        return Err(empty_selection_error(
            "outdated",
            "No outdated components found",
        ));
    }

    if config.behind_upstream && plan.ready_components().is_empty() {
        return Err(Error::validation_invalid_argument(
            "behind_upstream",
            "No components behind upstream found",
            None,
            None,
        ));
    }

    if plan
        .plan
        .steps
        .iter()
        .any(|step| step.kind == "deploy_selection" && step.status == PlanStepStatus::Missing)
    {
        return Err(Error::validation_missing_argument(vec![
            "component IDs, --all, --outdated, --behind-upstream, or --check".to_string(),
        ]));
    }

    Ok(())
}

fn deploy_plan(mode: &str, config: &DeployConfig, steps: Vec<PlanStep>) -> HomeboyPlan {
    HomeboyPlan::builder_for_description(PlanKind::Deploy, mode)
        .mode(mode)
        .inputs(
            PlanValues::new()
                .json("component_ids", &config.component_ids)
                .bool("all", config.all)
                .bool("outdated", config.outdated)
                .bool("behind_upstream", config.behind_upstream)
                .bool("dry_run", config.dry_run)
                .bool("check", config.check),
        )
        .steps(steps)
        .summarize_disabled_as_skipped()
        .build()
}

fn deploy_step(
    component_id: &str,
    status: PlanStepStatus,
    selection_reason: &str,
) -> crate::core::plan::PlanStepBuilder {
    PlanStep::builder(format!("deploy.{component_id}"), "deploy_component", status)
        .label(format!("Deploy {component_id}"))
        .scope(vec![component_id.to_string()])
        .input_value("component_id", serde_json::json!(component_id))
        .input_value("selection_reason", serde_json::json!(selection_reason))
}

fn selection_mode(config: &DeployConfig) -> &'static str {
    if !config.component_ids.is_empty() {
        "component_ids"
    } else if config.check {
        "check"
    } else if config.all {
        "all"
    } else if config.outdated {
        "outdated"
    } else if config.behind_upstream {
        "behind_upstream"
    } else {
        "missing_selection"
    }
}

fn empty_selection_error(field: &str, message: &str) -> Error {
    Error::validation_invalid_argument(field, message, None, None)
}

#[cfg(test)]
fn select_behind_upstream_components(all_components: &[Component]) -> Vec<Component> {
    let mut git_probe_cache = GitProbeCache::default();
    all_components
        .iter()
        .filter(|component| git_probe_cache.component_is_behind_upstream(component))
        .cloned()
        .collect()
}

#[derive(Default)]
pub(super) struct GitProbeCache {
    behind_upstream: HashMap<String, bool>,
    default_remote_branch: HashMap<String, Option<String>>,
    fetched_origin: HashSet<String>,
}

impl GitProbeCache {
    fn component_is_behind_upstream(&mut self, component: &Component) -> bool {
        if component.is_file_component() {
            return false;
        }

        let Some(git_root) = component_git_root(component) else {
            return false;
        };

        if let Some(behind_upstream) = self.behind_upstream.get(&git_root) {
            return *behind_upstream;
        }

        let behind_upstream = matches!(git::fetch_and_get_behind_count(&git_root), Ok(Some(_)));
        self.behind_upstream
            .insert(git_root.clone(), behind_upstream);

        behind_upstream
    }

    fn component_is_behind_default_branch(&mut self, component: &Component) -> bool {
        if component.is_file_component() {
            return false;
        }

        let Some(git_root) = component_git_root(component) else {
            return false;
        };

        let path = Path::new(&git_root);
        if git_output(path, &["rev-parse", "--abbrev-ref", "@{upstream}"]).is_some() {
            return false;
        }

        let Some(default_branch) = self.default_remote_branch(&git_root) else {
            return false;
        };

        git_output(
            path,
            &[
                "rev-list",
                "--left-only",
                "--count",
                &format!("{default_branch}...HEAD"),
            ],
        )
        .and_then(|value| value.parse::<u32>().ok())
        .is_some_and(|count| count > 0)
    }

    fn default_remote_branch(&mut self, git_root: &str) -> Option<String> {
        if let Some(default_branch) = self.default_remote_branch.get(git_root) {
            return default_branch.clone();
        }

        self.fetch_origin(git_root);
        let default_branch = default_remote_branch(Path::new(git_root));
        self.default_remote_branch
            .insert(git_root.to_string(), default_branch.clone());

        default_branch
    }

    fn fetch_origin(&mut self, git_root: &str) {
        if self.fetched_origin.insert(git_root.to_string()) {
            fetch_origin(Path::new(git_root));
        }
    }
}

fn component_git_root(component: &Component) -> Option<String> {
    git::get_git_root(&component.local_path).ok()
}

/// Calculate component status based on local and remote versions.
pub(super) fn calculate_component_status(
    component: &Component,
    remote_versions: &HashMap<String, String>,
) -> ComponentStatus {
    let mut git_probe_cache = GitProbeCache::default();
    calculate_component_status_with_git_cache(component, remote_versions, &mut git_probe_cache)
}

pub(super) fn calculate_component_status_with_git_cache(
    component: &Component,
    remote_versions: &HashMap<String, String>,
    git_probe_cache: &mut GitProbeCache,
) -> ComponentStatus {
    let local_version = version::get_component_version(component);
    let remote_version = remote_versions.get(&component.id);

    let version_status = match (local_version, remote_version) {
        (None, None) => ComponentStatus::Unknown,
        (None, Some(_)) => ComponentStatus::NeedsUpdate,
        (Some(_), None) => ComponentStatus::NeedsUpdate,
        (Some(local), Some(remote)) => {
            if local == *remote {
                ComponentStatus::UpToDate
            } else {
                ComponentStatus::NeedsUpdate
            }
        }
    };

    if !matches!(version_status, ComponentStatus::UpToDate) {
        return version_status;
    }

    if git_probe_cache.component_is_behind_upstream(component) {
        return ComponentStatus::BehindUpstream;
    }

    if git_probe_cache.component_is_behind_default_branch(component) {
        return ComponentStatus::SourceStale;
    }

    version_status
}

fn fetch_origin(path: &Path) {
    let _ = Command::new("git")
        .args(["fetch", "--quiet", "origin"])
        .current_dir(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

fn git_output(path: &Path, args: &[&str]) -> Option<String> {
    Command::new("git")
        .args(args)
        .current_dir(path)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| {
            let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
            (!value.is_empty()).then_some(value)
        })
}

fn default_remote_branch(path: &Path) -> Option<String> {
    git_output(
        path,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    )
    .or_else(|| {
        ["origin/main", "origin/trunk", "origin/master"]
            .iter()
            .find(|branch| git_output(path, &["rev-parse", "--verify", branch]).is_some())
            .map(|branch| (*branch).to_string())
    })
}

/// Calculate release state for a component.
/// Returns commit count since last version tag and uncommitted changes status.
pub fn calculate_release_state(component: &Component) -> Option<ReleaseState> {
    let path = &component.local_path;

    let current_version = version::read_component_version(component)
        .ok()
        .map(|info| info.version);

    let baseline = git::detect_baseline_with_version(path, current_version.as_deref()).ok()?;

    let commits = git::get_commits_since_tag(path, baseline.reference.as_deref())
        .ok()
        .unwrap_or_default();

    // Categorize commits into code vs docs-only
    let counts = git::categorize_commits(path, &commits);

    let uncommitted = git::get_uncommitted_changes(path)
        .ok()
        .map(|u| u.has_changes)
        .unwrap_or(false);

    Some(ReleaseState {
        commits_since_version: counts.total,
        code_commits: counts.code,
        docs_only_commits: counts.docs_only,
        has_uncommitted_changes: uncommitted,
        baseline_ref: baseline.reference,
        baseline_warning: baseline.warning,
    })
}

pub fn classify_release_state(state: Option<&ReleaseState>) -> ReleaseStateStatus {
    state
        .map(ReleaseState::status)
        .unwrap_or(ReleaseStateStatus::Unknown)
}

pub fn bucket_release_states<'a, I>(components: I) -> ReleaseStateBuckets
where
    I: IntoIterator<Item = (&'a str, Option<&'a ReleaseState>)>,
{
    let mut buckets = ReleaseStateBuckets::default();

    for (component_id, state) in components {
        match classify_release_state(state) {
            ReleaseStateStatus::Uncommitted => {
                buckets.has_uncommitted.push(component_id.to_string())
            }
            ReleaseStateStatus::NeedsRelease => {
                buckets.needs_release.push(component_id.to_string())
            }
            ReleaseStateStatus::DocsOnly => buckets.docs_only.push(component_id.to_string()),
            ReleaseStateStatus::Clean => buckets.ready_to_deploy.push(component_id.to_string()),
            ReleaseStateStatus::Unknown => buckets.unknown.push(component_id.to_string()),
        }
    }

    buckets
}

/// A component skipped during loading because a required extension is not installed.
///
/// Carries the human-readable reason so check-mode output can report
/// `skipped: missing extension <id>` per component instead of aborting.
pub(super) struct ExtensionSkippedComponent {
    pub id: String,
    pub reason: String,
}

/// Result of loading project components, including skipped (non-deployable) component IDs.
pub(super) struct LoadedComponents {
    pub deployable: Vec<Component>,
    pub skipped: Vec<String>,
    /// Components skipped because a required extension is missing. Only populated
    /// in check mode; otherwise a missing extension is a hard error.
    pub extension_skipped: Vec<ExtensionSkippedComponent>,
}

/// Load effective project components, resolve artifact paths via extension patterns,
/// and filter non-deployable.
///
/// Validates that any extensions declared in the component's `extensions` field are installed.
/// Returns an actionable error with install instructions when extensions are missing,
/// rather than silently skipping the component.
///
/// In `check` mode (read-only diff), a component requiring an uninstalled extension is
/// *not* a hard error: it is skipped, recorded in `extension_skipped`, and reported so
/// operators can see the project-wide diff without installing every build toolchain.
///
/// Returns both the deployable components and the IDs of skipped (non-deployable) ones,
/// so callers can produce accurate error messages.
pub(super) fn load_project_components(
    project: &Project,
    requested_ids: &[String],
    check: bool,
) -> Result<LoadedComponents> {
    let mut deployable = Vec::new();
    let mut skipped = Vec::new();
    let mut extension_skipped = Vec::new();
    let standalone_snapshot = project::StandaloneComponentConfigSnapshot::load();

    for attachment in project
        .components
        .iter()
        .filter(|attachment| requested_ids.is_empty() || requested_ids.contains(&attachment.id))
    {
        // When specific components are requested, skip extension validation for
        // unrelated components — a missing extension on an unrequested component
        // should not block deploying the ones you asked for.
        let is_requested = requested_ids.is_empty() || requested_ids.contains(&attachment.id);

        let mut loaded = project::resolve_project_component_with_standalone_snapshot(
            project,
            &attachment.id,
            Some(&standalone_snapshot),
        )?;

        // Bundled/retired components are no longer standalone deploy targets.
        // Skip them before extension validation and artifact resolution so they
        // never appear as deploy obligations or `--outdated` drift. Their
        // version tracks the host component (when bundled) or is moot (retired).
        if !loaded.is_active_lifecycle() {
            let reason = loaded
                .lifecycle_suppression_reason()
                .unwrap_or_else(|| "Component is not an active deploy target".to_string());
            log_status!("deploy", "Skipping '{}': {}", loaded.id, reason);
            skipped.push(loaded.id.clone());
            continue;
        }

        // Validate required extensions are installed before attempting artifact resolution.
        // Without this check, missing extensions cause resolve_artifact() to silently
        // return None, and the component gets skipped with a vague "no artifact" message.
        if let Err(err) = extension::validate_required_extensions(&loaded) {
            if check {
                // Read-only diff: a missing extension must not poison the whole pass.
                // Skip-and-warn so operators still see the diff for the components they
                // actually care about (see issue #4587).
                let reason = missing_extension_reason(&err);
                log_status!(
                    "deploy",
                    "Skipping '{}' in check mode: {}",
                    loaded.id,
                    reason
                );
                extension_skipped.push(ExtensionSkippedComponent {
                    id: loaded.id.clone(),
                    reason,
                });
                continue;
            }

            if is_requested {
                return Err(err);
            }

            log_status!(
                "deploy",
                "Skipping '{}': missing required extension (not requested for deploy)",
                loaded.id
            );
            skipped.push(loaded.id.clone());
            continue;
        }

        // Resolve effective artifact (component value OR extension pattern)
        let effective_artifact = component::resolve_artifact(&loaded);

        // Git-deploy and file-deploy components don't need a build artifact
        let is_git_deploy = loaded.deploy_strategy.as_deref() == Some("git");
        let is_file_deploy = loaded.deploy_strategy.as_deref() == Some("file");

        match effective_artifact {
            Some(artifact) if !is_git_deploy && !is_file_deploy => {
                let resolved_artifact =
                    crate::core::paths::resolve_path_string(&loaded.local_path, &artifact);
                loaded.build_artifact = Some(resolved_artifact);
                deployable.push(loaded);
            }
            _ if is_git_deploy => {
                // Git-deploy components are deployable without an artifact
                deployable.push(loaded);
            }
            _ if is_file_deploy => {
                // File-deploy components use local_path as the artifact — no build needed
                deployable.push(loaded);
            }
            Some(_) | None => {
                // Skip - component is intentionally non-deployable
                log_status!(
                    "deploy",
                    "Skipping '{}': no artifact configured (non-deployable component)",
                    loaded.id
                );
                skipped.push(loaded.id.clone());
                continue;
            }
        }
    }

    Ok(LoadedComponents {
        deployable,
        skipped,
        extension_skipped,
    })
}

/// Build a concise, operator-facing reason string from a missing-extension error.
///
/// Prefers the list of missing extension IDs from the error details
/// (`skipped: missing extension <id>`), falling back to the full error message.
fn missing_extension_reason(err: &crate::core::error::Error) -> String {
    if let Some(missing) = err
        .details
        .get("missing_extensions")
        .and_then(|v| v.as_array())
    {
        let ids: Vec<String> = missing
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        if !ids.is_empty() {
            let label = if ids.len() == 1 {
                "extension"
            } else {
                "extensions"
            };
            return format!("missing {} {}", label, ids.join(", "));
        }
    }
    format!("missing required extension ({})", err.message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::component::VersionTarget;
    use crate::core::deploy::types::DeployConfig;
    use crate::core::project::Project;
    use crate::core::server::SshClient;
    use tempfile::TempDir;

    fn run_git(path: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("git command");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_source_repo(path: &Path) {
        run_git(path, &["init", "-q", "-b", "main"]);
        run_git(path, &["config", "user.email", "test@example.com"]);
        run_git(path, &["config", "user.name", "Test"]);
        std::fs::write(path.join("component.txt"), "v1\n").expect("write v1");
        run_git(path, &["add", "component.txt"]);
        run_git(path, &["commit", "-q", "-m", "initial"]);
    }

    fn commit_upstream_change(path: &Path) {
        std::fs::write(path.join("component.txt"), "v2\n").expect("write v2");
        run_git(path, &["add", "component.txt"]);
        run_git(path, &["commit", "-q", "-m", "upstream"]);
    }

    fn clone_repo(source: &Path, target: &Path) {
        let output = std::process::Command::new("git")
            .args([
                "clone",
                "-q",
                source.to_str().expect("source path"),
                target.to_str().expect("target path"),
            ])
            .output()
            .expect("git clone");
        assert!(
            output.status.success(),
            "git clone failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn component(id: &str, path: &Path) -> Component {
        Component::new(
            id.to_string(),
            path.to_string_lossy().to_string(),
            String::new(),
            None,
        )
    }

    fn versioned_component(id: &str, path: &Path, version: &str) -> Component {
        std::fs::write(path.join("VERSION"), format!("{}\n", version)).expect("version file");
        let mut component = component(id, path);
        component.version_targets = Some(vec![VersionTarget {
            file: "VERSION".to_string(),
            pattern: Some(r"^(.+)$".to_string()),
            artifact_path: None,
        }]);
        component
    }

    fn deploy_config() -> DeployConfig {
        DeployConfig {
            component_ids: Vec::new(),
            all: false,
            outdated: false,
            behind_upstream: false,
            dry_run: false,
            check: false,
            force: false,
            skip_build: false,
            keep_deps: false,
            expected_version: None,
            no_pull: false,
            head: false,
            tagged: false,
        }
    }

    fn project() -> Project {
        Project {
            id: "fixture".to_string(),
            ..Project::default()
        }
    }

    fn ssh_client() -> SshClient {
        SshClient {
            host: "localhost".to_string(),
            user: std::env::var("USER").unwrap_or_else(|_| "test".to_string()),
            port: 22,
            identity_file: None,
            auth: None,
            is_local: true,
            env: HashMap::new(),
        }
    }

    fn step<'a>(plan: &'a DeployComponentPlan, component_id: &str) -> &'a PlanStep {
        plan.plan
            .steps
            .iter()
            .find(|step| step.input_as::<String>("component_id").as_deref() == Some(component_id))
            .expect("component step")
    }

    #[test]
    fn plan_component_deploys_marks_explicit_selection_ready() {
        let temp = TempDir::new().expect("temp dir");
        let selected = component("selected", temp.path());
        let config = DeployConfig {
            component_ids: vec!["selected".to_string()],
            ..deploy_config()
        };

        let plan = plan_component_deploys(
            &config,
            std::slice::from_ref(&selected),
            &[],
            &project(),
            "/var/www/example",
            &ssh_client(),
        );

        assert_eq!(plan.plan.kind, PlanKind::Deploy);
        assert_eq!(step(&plan, "selected").status, PlanStepStatus::Ready);
        assert_eq!(plan.ready_components()[0].id, "selected");
    }

    #[test]
    fn plan_component_deploys_marks_missing_requested_component() {
        let config = DeployConfig {
            component_ids: vec!["missing".to_string()],
            ..deploy_config()
        };

        let plan = plan_component_deploys(
            &config,
            &[],
            &[],
            &project(),
            "/var/www/example",
            &ssh_client(),
        );

        let step = step(&plan, "missing");
        assert_eq!(step.status, PlanStepStatus::Missing);
        assert_eq!(step.missing, vec!["missing"]);
        assert!(plan.ready_components().is_empty());
    }

    #[test]
    fn plan_component_deploys_marks_non_deployable_requested_component_disabled() {
        let config = DeployConfig {
            component_ids: vec!["docs".to_string()],
            ..deploy_config()
        };

        let plan = plan_component_deploys(
            &config,
            &[],
            &["docs".to_string()],
            &project(),
            "/var/www/example",
            &ssh_client(),
        );

        let step = step(&plan, "docs");
        assert_eq!(step.status, PlanStepStatus::Disabled);
        assert_eq!(
            step.skip_reason.as_deref(),
            Some("Non-deployable component (no artifact/deploy strategy)")
        );
        assert!(plan.ready_components().is_empty());
    }

    #[test]
    fn plan_outdated_steps_mark_outdated_components_ready_and_current_skipped() {
        let temp = TempDir::new().expect("temp dir");
        let outdated_path = temp.path().join("outdated");
        let current_path = temp.path().join("current");
        std::fs::create_dir(&outdated_path).expect("outdated dir");
        std::fs::create_dir(&current_path).expect("current dir");
        let outdated = versioned_component("outdated", &outdated_path, "1.0.0");
        let current = versioned_component("current", &current_path, "1.0.0");
        let remote_versions = HashMap::from([
            ("outdated".to_string(), "0.9.0".to_string()),
            ("current".to_string(), "1.0.0".to_string()),
        ]);

        let steps = plan_outdated_steps(&[outdated, current], &remote_versions);

        let outdated = steps
            .iter()
            .find(|step| step.input_as::<String>("component_id").as_deref() == Some("outdated"))
            .expect("outdated step");
        let current = steps
            .iter()
            .find(|step| step.input_as::<String>("component_id").as_deref() == Some("current"))
            .expect("current step");
        assert_eq!(outdated.status, PlanStepStatus::Ready);
        assert_eq!(current.status, PlanStepStatus::Skipped);
        assert_eq!(
            current.skip_reason.as_deref(),
            Some("Component is up to date")
        );
    }

    #[test]
    fn plan_component_deploys_marks_behind_upstream_ready_and_current_skipped() {
        let temp = TempDir::new().expect("temp dir");
        let source = temp.path().join("source");
        let stale = temp.path().join("stale");
        let current = temp.path().join("current");
        std::fs::create_dir(&source).expect("source dir");

        init_source_repo(&source);
        clone_repo(&source, &stale);
        commit_upstream_change(&source);
        clone_repo(&source, &current);
        let stale = component("stale", &stale);
        let current = component("current", &current);
        let config = DeployConfig {
            behind_upstream: true,
            ..deploy_config()
        };

        let plan = plan_component_deploys(
            &config,
            &[stale, current],
            &[],
            &project(),
            "/var/www/example",
            &ssh_client(),
        );

        assert_eq!(step(&plan, "stale").status, PlanStepStatus::Ready);
        assert_eq!(step(&plan, "current").status, PlanStepStatus::Skipped);
        assert_eq!(plan.ready_components().len(), 1);
        assert_eq!(plan.ready_components()[0].id, "stale");
    }

    #[test]
    fn plan_component_deploys_marks_empty_selection_missing() {
        let plan = plan_component_deploys(
            &deploy_config(),
            &[],
            &[],
            &project(),
            "/var/www/example",
            &ssh_client(),
        );

        assert_eq!(plan.plan.steps[0].kind, "deploy_selection");
        assert_eq!(plan.plan.steps[0].status, PlanStepStatus::Missing);
        assert!(plan.ready_components().is_empty());
    }

    #[test]
    fn select_behind_upstream_components_finds_stale_checkout() {
        let temp = TempDir::new().expect("temp dir");
        let source = temp.path().join("source");
        let local = temp.path().join("local");
        std::fs::create_dir(&source).expect("source dir");

        init_source_repo(&source);
        clone_repo(&source, &local);
        commit_upstream_change(&source);

        let stale = component("stale", &local);
        let selected = select_behind_upstream_components(std::slice::from_ref(&stale));

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].id, "stale");
    }

    #[test]
    fn component_status_reports_behind_upstream_when_deployed_version_matches() {
        let temp = TempDir::new().expect("temp dir");
        let source = temp.path().join("source");
        let local = temp.path().join("local");
        std::fs::create_dir(&source).expect("source dir");

        init_source_repo(&source);
        clone_repo(&source, &local);
        commit_upstream_change(&source);

        let stale = versioned_component("stale", &local, "1.0.0");
        let remote_versions = HashMap::from([("stale".to_string(), "1.0.0".to_string())]);

        assert!(matches!(
            calculate_component_status(&stale, &remote_versions),
            ComponentStatus::BehindUpstream
        ));
    }

    #[test]
    fn component_status_reports_source_stale_for_detached_checkout_behind_default() {
        let temp = TempDir::new().expect("temp dir");
        let source = temp.path().join("source");
        let local = temp.path().join("local");
        std::fs::create_dir(&source).expect("source dir");

        init_source_repo(&source);
        clone_repo(&source, &local);
        run_git(&local, &["checkout", "--detach", "HEAD"]);
        commit_upstream_change(&source);

        let stale = versioned_component("stale", &local, "1.0.0");
        let remote_versions = HashMap::from([("stale".to_string(), "1.0.0".to_string())]);

        assert!(matches!(
            calculate_component_status(&stale, &remote_versions),
            ComponentStatus::SourceStale
        ));
    }

    #[test]
    fn component_status_reports_source_stale_for_untracked_branch_behind_default() {
        let temp = TempDir::new().expect("temp dir");
        let source = temp.path().join("source");
        let local = temp.path().join("local");
        std::fs::create_dir(&source).expect("source dir");

        init_source_repo(&source);
        clone_repo(&source, &local);
        run_git(&local, &["checkout", "-b", "configured-source"]);
        commit_upstream_change(&source);

        let stale = versioned_component("stale", &local, "1.0.0");
        let remote_versions = HashMap::from([("stale".to_string(), "1.0.0".to_string())]);

        assert!(matches!(
            calculate_component_status(&stale, &remote_versions),
            ComponentStatus::SourceStale
        ));
    }

    #[test]
    fn component_status_preserves_deployed_version_drift_when_checkout_is_behind_upstream() {
        let temp = TempDir::new().expect("temp dir");
        let source = temp.path().join("source");
        let local = temp.path().join("local");
        std::fs::create_dir(&source).expect("source dir");

        init_source_repo(&source);
        clone_repo(&source, &local);
        commit_upstream_change(&source);

        let stale = versioned_component("stale", &local, "1.0.0");
        let remote_versions = HashMap::from([("stale".to_string(), "2.0.0".to_string())]);

        assert!(matches!(
            calculate_component_status(&stale, &remote_versions),
            ComponentStatus::NeedsUpdate
        ));
    }

    #[test]
    fn select_behind_upstream_components_skips_current_checkout() {
        let temp = TempDir::new().expect("temp dir");
        let source = temp.path().join("source");
        let local = temp.path().join("local");
        std::fs::create_dir(&source).expect("source dir");

        init_source_repo(&source);
        clone_repo(&source, &local);

        let current = component("current", &local);
        let selected = select_behind_upstream_components(&[current]);

        assert!(selected.is_empty());
    }

    /// Write a deployable component repo (homeboy.json + a deploy artifact) at
    /// `dir`, optionally declaring a non-active `lifecycle`.
    fn write_component_repo(dir: &Path, id: &str, lifecycle: Option<&str>) {
        let mut config = serde_json::json!({
            "id": id,
            "remote_path": format!("wp-content/plugins/{id}"),
            "build_artifact": "dist/plugin.zip",
        });
        if let Some(lifecycle) = lifecycle {
            config["lifecycle"] = serde_json::json!(lifecycle);
        }
        std::fs::write(dir.join("homeboy.json"), config.to_string()).expect("write homeboy.json");
        std::fs::create_dir_all(dir.join("dist")).expect("dist dir");
        std::fs::write(dir.join("dist/plugin.zip"), b"zip").expect("artifact");
    }

    #[test]
    fn load_project_components_skips_bundled_and_retired_lifecycle() {
        crate::test_support::with_isolated_home(|home| {
            let workspace = home.path().join("workspace");
            let active_dir = workspace.join("active");
            let bundled_dir = workspace.join("bundled");
            let retired_dir = workspace.join("retired");
            std::fs::create_dir_all(&active_dir).expect("active dir");
            std::fs::create_dir_all(&bundled_dir).expect("bundled dir");
            std::fs::create_dir_all(&retired_dir).expect("retired dir");

            write_component_repo(&active_dir, "active", None);
            write_component_repo(&bundled_dir, "bundled", Some("bundled"));
            write_component_repo(&retired_dir, "retired", Some("retired"));

            let attach = |id: &str, dir: &Path| crate::core::project::ProjectComponentAttachment {
                id: id.to_string(),
                local_path: dir.to_string_lossy().to_string(),
                remote_path: None,
            };

            let project = Project {
                id: "site".to_string(),
                components: vec![
                    attach("active", &active_dir),
                    attach("bundled", &bundled_dir),
                    attach("retired", &retired_dir),
                ],
                ..Project::default()
            };

            let loaded =
                load_project_components(&project, &[], false).expect("load components succeeds");

            // Only the active component is a deploy obligation.
            let deployable_ids: Vec<&str> =
                loaded.deployable.iter().map(|c| c.id.as_str()).collect();
            assert_eq!(deployable_ids, vec!["active"]);

            // Bundled/retired components are suppressed (not a hard error, not
            // deployable) — exactly the agents-api scenario from #3489.
            assert!(loaded.skipped.contains(&"bundled".to_string()));
            assert!(loaded.skipped.contains(&"retired".to_string()));
        });
    }
}
