//! `deps stack refresh`: roll a downstream component's declared
//! `dependency_stack` edges forward to each source's newest release.
//!
//! Refresh runs the same evaluation as `deps stack outdated`, then applies
//! each behind edge through the same step `deps stack apply` runs: the edge's
//! `update` command if set, otherwise the provider-backed default update with
//! `--to <newest>`, then install/lockfile refresh unless `--no-install`, then
//! `post_update`. It does not commit, push, or open PRs.

use super::dependency_graph::{
    edge_key, parse_dependency_version, rebuild_command, run_default_update_step,
    run_stack_command, stack_outdated_for_component, update_command, DependencyStackCommandResult,
    DependencyStackEdgeOutdatedState, DependencyStackEdgeOutdatedStatus, DependencyStackPlanStep,
    STACK_OUTDATED_RESOLVE_TIMEOUT,
};
use crate::component::{self, Component, DependencyStackEdge};
use crate::{Error, Result};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DependencyStackEdgeRefreshState {
    /// The edge was behind and its update path ran cleanly (or, under
    /// `--dry-run`, would have run).
    Updated,
    /// The locked version is not older than the source's newest release;
    /// nothing was touched.
    Current,
    /// The edge could not be checked; never reported as current.
    Unresolvable,
    Held,
    /// The edge was behind but a step of its update path failed.
    Failed,
}

/// Breaking verdict for a release range.
///
/// `breaking` is `true`, `false`, or `null`. A `null` verdict means the range
/// could not be inspected and callers must treat it as breaking.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DependencyStackBreakingVerdict {
    pub breaking: Option<bool>,
    pub breaking_reasons: Vec<String>,
}

impl DependencyStackBreakingVerdict {
    fn breaking(reasons: Vec<String>) -> Self {
        Self {
            breaking: Some(true),
            breaking_reasons: reasons,
        }
    }

    fn not_breaking() -> Self {
        Self {
            breaking: Some(false),
            breaking_reasons: Vec::new(),
        }
    }

    fn unknown(reason: String) -> Self {
        Self {
            breaking: None,
            breaking_reasons: vec![reason],
        }
    }

    /// Callers must treat a `null` verdict as breaking.
    pub fn treated_as_breaking(&self) -> bool {
        self.breaking != Some(false)
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DependencyStackEdgeRefreshResult {
    pub upstream: String,
    pub downstream: String,
    pub package: String,
    /// Version recorded in the downstream lockfile before the refresh.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// Newest released version resolved from the edge's source.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    pub state: DependencyStackEdgeRefreshState,
    /// Breaking verdict for the applied range; `null` means the range could
    /// not be inspected and callers must treat it as breaking.
    pub breaking: Option<bool>,
    pub breaking_reasons: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unresolvable_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub command_results: Vec<DependencyStackCommandResult>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DependencyStackRefreshResult {
    pub component_id: String,
    pub component_path: String,
    pub dry_run: bool,
    pub edge_count: usize,
    pub updated_count: usize,
    pub current_count: usize,
    pub unresolvable_count: usize,
    pub failed_count: usize,
    /// True when any updated edge is breaking or carries a `null` verdict.
    pub breaking: bool,
    pub edges: Vec<DependencyStackEdgeRefreshResult>,
}

impl DependencyStackRefreshResult {
    /// Gate convention: 0 when nothing was behind or everything behind
    /// updated cleanly, 1 when at least one edge failed to update, 2 when
    /// at least one edge was unresolvable and none failed.
    pub fn exit_code(&self) -> i32 {
        if self.failed_count > 0 {
            1
        } else if self.unresolvable_count > 0 {
            2
        } else {
            0
        }
    }

    /// Human-readable gate cause for a non-zero [`Self::exit_code`].
    pub fn gate_failure_message(&self) -> String {
        let mut parts: Vec<String> = self
            .edges
            .iter()
            .filter(|edge| edge.state == DependencyStackEdgeRefreshState::Failed)
            .map(|edge| {
                format!(
                    "{} failed: {}",
                    edge.package,
                    edge.failure.as_deref().unwrap_or("update failed")
                )
            })
            .collect();
        parts.extend(
            self.edges
                .iter()
                .filter(|edge| {
                    matches!(
                        edge.state,
                        DependencyStackEdgeRefreshState::Unresolvable
                            | DependencyStackEdgeRefreshState::Held
                    )
                })
                .map(|edge| {
                    format!(
                        "{} unresolvable: {}",
                        edge.package,
                        edge.unresolvable_reason
                            .as_deref()
                            .unwrap_or("unresolvable")
                    )
                }),
        );
        format!(
            "{} of {} declared edges did not refresh cleanly: {}",
            self.failed_count + self.unresolvable_count,
            self.edge_count,
            parts.join("; ")
        )
    }
}

/// Roll the component's declared `dependency_stack` edges forward to each
/// source's newest release, from its own checkout, with nothing registered
/// locally.
pub fn stack_refresh(
    component: Option<&str>,
    path: Option<&str>,
    dry_run: bool,
    install: bool,
) -> Result<DependencyStackRefreshResult> {
    let component = component::resolve_effective(component, path, None)?;
    let path = PathBuf::from(shellexpand::tilde(&component.local_path).as_ref());
    if !path.exists() {
        return Err(Error::validation_invalid_argument(
            "component_path",
            format!(
                "Component '{}' path does not exist: {}",
                component.id,
                path.display()
            ),
            Some(component.id.clone()),
            None,
        ));
    }
    stack_refresh_for_component(&component, &path, dry_run, install)
}

/// [`stack_refresh`] against an explicit component and checkout path.
pub fn stack_refresh_for_component(
    component: &Component,
    path: &Path,
    dry_run: bool,
    install: bool,
) -> Result<DependencyStackRefreshResult> {
    let outdated = stack_outdated_for_component(component, path)?;

    let mut declared_edges: BTreeMap<String, DependencyStackEdge> = BTreeMap::new();
    for edge in &component.dependency_stack {
        if edge.downstream != component.id {
            continue;
        }
        declared_edges
            .entry(edge_key(edge))
            .or_insert_with(|| edge.clone());
    }

    let mut edges = Vec::new();
    for (sequence, status) in outdated.edges.iter().enumerate() {
        let edge = declared_edges.get(&outdated_edge_key(status)).cloned();
        edges.push(refresh_edge(
            component,
            path,
            status,
            edge.as_ref(),
            sequence + 1,
            dry_run,
            install,
        ));
    }

    let breaking = edges
        .iter()
        .filter(|edge| edge.state == DependencyStackEdgeRefreshState::Updated)
        .any(|edge| edge.breaking != Some(false));

    Ok(DependencyStackRefreshResult {
        component_id: outdated.component_id.clone(),
        component_path: outdated.component_path.clone(),
        dry_run,
        updated_count: count_state(&edges, DependencyStackEdgeRefreshState::Updated),
        current_count: count_state(&edges, DependencyStackEdgeRefreshState::Current),
        unresolvable_count: count_state(&edges, DependencyStackEdgeRefreshState::Unresolvable)
            + count_state(&edges, DependencyStackEdgeRefreshState::Held),
        failed_count: count_state(&edges, DependencyStackEdgeRefreshState::Failed),
        edge_count: edges.len(),
        breaking,
        edges,
    })
}

fn count_state(
    edges: &[DependencyStackEdgeRefreshResult],
    state: DependencyStackEdgeRefreshState,
) -> usize {
    edges.iter().filter(|edge| edge.state == state).count()
}

fn outdated_edge_key(status: &DependencyStackEdgeOutdatedStatus) -> String {
    format!(
        "{}>{}:{}",
        status.upstream, status.downstream, status.package
    )
}

#[allow(clippy::too_many_arguments)]
fn refresh_edge(
    component: &Component,
    path: &Path,
    status: &DependencyStackEdgeOutdatedStatus,
    edge: Option<&DependencyStackEdge>,
    sequence: usize,
    dry_run: bool,
    install: bool,
) -> DependencyStackEdgeRefreshResult {
    let mut result = DependencyStackEdgeRefreshResult {
        upstream: status.upstream.clone(),
        downstream: status.downstream.clone(),
        package: status.package.clone(),
        from: status.locked_version.clone(),
        to: status.newest_version.clone(),
        state: DependencyStackEdgeRefreshState::Unresolvable,
        breaking: None,
        breaking_reasons: Vec::new(),
        unresolvable_reason: status.unresolvable_reason.clone(),
        failure: None,
        command_results: Vec::new(),
    };

    match status.state {
        DependencyStackEdgeOutdatedState::Current => {
            result.state = DependencyStackEdgeRefreshState::Current;
            // Nothing changes, so the range carries no behavior change.
            result.breaking = Some(false);
            return result;
        }
        DependencyStackEdgeOutdatedState::Held => {
            result.state = DependencyStackEdgeRefreshState::Held;
            return result;
        }
        DependencyStackEdgeOutdatedState::Unresolvable => {
            result
                .breaking_reasons
                .push("edge is unresolvable; release range not evaluated".to_string());
            return result;
        }
        DependencyStackEdgeOutdatedState::Behind => {}
    }

    // A behind edge always carries comparable locked/newest versions and a
    // declared source: that is what the outdated evaluation established.
    let verdict = match (
        status
            .locked_version
            .as_deref()
            .and_then(parse_dependency_version),
        status
            .newest_version
            .as_deref()
            .and_then(parse_dependency_version),
        status.source_repo.as_deref(),
        status.newest_tag.as_deref(),
    ) {
        (Some(locked), Some(newest), Some(repo), Some(newest_tag)) => {
            if let Some(github) = edge
                .and_then(|edge| edge.source.as_ref())
                .and_then(|source| source.github_release.as_deref())
            {
                github_range_breaking_verdict(github, &locked, &newest, newest_tag)
            } else if edge
                .and_then(|edge| edge.source.as_ref())
                .and_then(|source| source.npm.as_ref())
                .is_some()
            {
                if newest.major != locked.major
                    || (newest.major == 0 && newest.minor != locked.minor)
                {
                    DependencyStackBreakingVerdict::breaking(vec![format!(
                        "version boundary changes: {locked} -> {newest}"
                    )])
                } else {
                    DependencyStackBreakingVerdict::unknown(
                        "npm source does not declare an inspectable commit range".into(),
                    )
                }
            } else {
                release_range_breaking_verdict(repo, &locked, &newest, newest_tag)
            }
        }
        _ => DependencyStackBreakingVerdict::unknown(
            "behind edge is missing a comparable version range".to_string(),
        ),
    };
    result.breaking = verdict.breaking;
    result.breaking_reasons = verdict.breaking_reasons;

    let Some(edge) = edge else {
        result.state = DependencyStackEdgeRefreshState::Failed;
        result.failure = Some("behind edge has no declared dependency_stack entry".to_string());
        return result;
    };
    let Some(newest_version) = status.newest_version.as_deref() else {
        result.state = DependencyStackEdgeRefreshState::Failed;
        result.failure = Some("behind edge resolved no newest version".to_string());
        return result;
    };

    if let Some(lock) = &edge.lock {
        let write = super::stack_source::version_file_path(path, &lock.path).and_then(|target| {
            if dry_run {
                return Ok(());
            }
            std::fs::write(target, format!("{newest_version}\n")).map_err(|error| error.to_string())
        });
        match write {
            Ok(()) => result.state = DependencyStackEdgeRefreshState::Updated,
            Err(error) => {
                result.state = DependencyStackEdgeRefreshState::Failed;
                result.failure = Some(error);
            }
        }
        return result;
    }

    let downstream_path = path.display().to_string();
    let step = DependencyStackPlanStep {
        sequence,
        declaring_component_id: component.id.clone(),
        upstream: edge.upstream.clone(),
        downstream: edge.downstream.clone(),
        downstream_path: downstream_path.clone(),
        package: edge.package.clone(),
        update_command: update_command(edge, &downstream_path),
        rebuild: edge.rebuild,
        post_update: edge.post_update.clone(),
        test: edge.test.clone(),
    };

    match run_refresh_step(
        &step,
        newest_version,
        dry_run,
        install,
        &mut result.command_results,
    ) {
        Ok(()) => result.state = DependencyStackEdgeRefreshState::Updated,
        Err(error) => {
            result.state = DependencyStackEdgeRefreshState::Failed;
            result.failure = Some(error.message);
        }
    }
    result
}

/// The same step `deps stack apply` runs for one edge: update (custom command
/// or the provider-backed default with `--to <newest>`, honoring the install
/// flag), the edge-declared rebuild, then `post_update`.
///
/// A failure keeps the results of the phases that already ran in `results`
/// so the caller reports exactly how far the edge got.
fn run_refresh_step(
    step: &DependencyStackPlanStep,
    newest_version: &str,
    dry_run: bool,
    install: bool,
    results: &mut Vec<DependencyStackCommandResult>,
) -> Result<()> {
    let update_result = if step.uses_default_update_command() {
        run_default_update_step(step, Some(newest_version), dry_run, install)?
    } else {
        run_stack_command(
            "update",
            &step.update_command,
            &step.downstream_path,
            dry_run,
        )?
    };
    results.push(update_result);

    if step.rebuild {
        results.push(run_stack_command(
            "rebuild",
            &rebuild_command(step),
            &step.downstream_path,
            dry_run,
        )?);
    }

    for command in &step.post_update {
        results.push(run_stack_command(
            "post_update",
            command,
            &step.downstream_path,
            dry_run,
        )?);
    }

    Ok(())
}

/// Breaking verdict for the release range `locked -> newest` from `repo`.
///
/// A range is breaking when the major version changes, when the major is 0
/// and the minor changes (0.x minors carry behavior changes), or when a
/// commit in the release range carries a conventional breaking marker
/// (`type!:` / `type(scope)!:` subject, or a `BREAKING CHANGE` /
/// `BREAKING-CHANGE` footer), or the target release tag's notes do. If the
/// range can't be inspected, the verdict is `null` with a reason, and callers
/// must treat `null` as breaking.
pub fn release_range_breaking_verdict(
    repo: &str,
    locked: &semver::Version,
    newest: &semver::Version,
    newest_tag: &str,
) -> DependencyStackBreakingVerdict {
    if newest.major != locked.major {
        return DependencyStackBreakingVerdict::breaking(vec![format!(
            "major version changes: {locked} -> {newest}"
        )]);
    }
    if newest.major == 0 && newest.minor != locked.minor {
        return DependencyStackBreakingVerdict::breaking(vec![format!(
            "0.x minor changes: {locked} -> {newest} (0.x minors carry behavior changes)"
        )]);
    }

    // Derive the locked release's tag from the newest tag's shape, so the
    // range inspection follows the same tag namespace the source resolves.
    let Some(tag_base) = newest_tag.strip_suffix(&newest.to_string()) else {
        return DependencyStackBreakingVerdict::unknown(format!(
            "cannot derive the locked release tag: tag '{newest_tag}' does not end with version '{newest}'"
        ));
    };
    let from_tag = format!("{tag_base}{locked}");

    match inspect_release_range(repo, &from_tag, newest_tag) {
        Ok(reasons) if reasons.is_empty() => DependencyStackBreakingVerdict::not_breaking(),
        Ok(reasons) => DependencyStackBreakingVerdict::breaking(reasons),
        Err(reason) => DependencyStackBreakingVerdict::unknown(reason),
    }
}

fn github_range_breaking_verdict(
    repo: &str,
    locked: &semver::Version,
    newest: &semver::Version,
    newest_tag: &str,
) -> DependencyStackBreakingVerdict {
    let base = newest_tag.strip_suffix(&newest.to_string()).unwrap_or("");
    let url = format!("https://api.github.com/repos/{repo}/compare/{base}{locked}...{newest_tag}");
    let value = match super::stack_source::http_json(&url) {
        Ok(value) => value,
        Err(error) => return DependencyStackBreakingVerdict::unknown(error.message),
    };
    let Some(commits) = value["commits"].as_array() else {
        return DependencyStackBreakingVerdict::unknown(
            "invalid GitHub comparison response".into(),
        );
    };
    // GitHub truncates comparison commit lists; never call a partial scan clean.
    if value["total_commits"].as_u64() != Some(commits.len() as u64) {
        return DependencyStackBreakingVerdict::unknown(
            "incomplete GitHub comparison commit range".into(),
        );
    }
    let mut reasons = Vec::new();
    if newest.major != locked.major || (newest.major == 0 && newest.minor != locked.minor) {
        reasons.push(format!("version boundary changes: {locked} -> {newest}"));
    }
    for commit in commits {
        let Some(message) = commit["commit"]["message"].as_str() else {
            return DependencyStackBreakingVerdict::unknown(
                "comparison commit has no message".into(),
            );
        };
        if subject_has_breaking_marker(message.lines().next().unwrap_or_default())
            || breaking_footer_line(message).is_some()
        {
            reasons.push(format!("commit declares breaking change: {message}"));
        }
    }
    if reasons.is_empty() {
        DependencyStackBreakingVerdict::not_breaking()
    } else {
        DependencyStackBreakingVerdict::breaking(reasons)
    }
}

/// Fetch both release tags into a scratch repository and scan the commit
/// range (and the target tag's notes) for conventional breaking markers.
///
/// Any failure to establish the range is an inspection failure, not a clean
/// verdict: the caller reports `null` and treats it as breaking.
fn inspect_release_range(
    repo: &str,
    from_tag: &str,
    to_tag: &str,
) -> std::result::Result<Vec<String>, String> {
    let scratch = tempfile::tempdir()
        .map_err(|error| format!("cannot create scratch repository: {error}"))?;
    let scratch_path = scratch.path();

    crate::git::run_git(
        scratch_path,
        &["init", "--bare", "-q"],
        "init release-range scratch repository",
    )
    .map_err(|error| format!("cannot init scratch repository: {}", error.message))?;

    crate::git::run_git_with_env_timeout(
        scratch_path,
        &[
            "fetch",
            "--no-tags",
            repo,
            &format!("+refs/tags/{from_tag}:refs/tags/{from_tag}"),
            &format!("+refs/tags/{to_tag}:refs/tags/{to_tag}"),
        ],
        "fetch release range tags",
        &[],
        STACK_OUTDATED_RESOLVE_TIMEOUT,
    )
    .map_err(|error| {
        format!(
            "release range {from_tag}..{to_tag} could not be fetched from {repo}: {}",
            error.message
        )
    })?;

    let log = crate::git::run_git(
        scratch_path,
        &[
            "log",
            "--format=%B%x1e",
            &format!("refs/tags/{from_tag}..refs/tags/{to_tag}"),
        ],
        "inspect release range commits",
    )
    .map_err(|error| {
        format!(
            "release range {from_tag}..{to_tag} could not be read: {}",
            error.message
        )
    })?;

    let mut reasons = Vec::new();
    for message in log.split('\u{1e}') {
        let message = message.trim();
        if message.is_empty() {
            continue;
        }
        let subject = message.lines().next().unwrap_or_default();
        if subject_has_breaking_marker(subject) {
            reasons.push(format!(
                "commit in {from_tag}..{to_tag} declares a breaking change: {subject}"
            ));
            continue;
        }
        if let Some(footer) = breaking_footer_line(message) {
            reasons.push(format!(
                "commit in {from_tag}..{to_tag} carries a breaking footer: {footer}"
            ));
        }
    }

    // The target tag's annotated message is the release's notes as the git
    // transport carries them; a lightweight tag reads as its commit, which
    // the range scan above already covered.
    let notes = crate::git::run_git(
        scratch_path,
        &["tag", "-l", "--format=%(contents)", to_tag],
        "read target release tag notes",
    )
    .unwrap_or_default();
    for line in notes.lines() {
        let line = line.trim();
        if subject_has_breaking_marker(line) || is_breaking_footer(line) {
            reasons.push(format!(
                "release notes for {to_tag} declare a breaking change: {line}"
            ));
            break;
        }
    }

    Ok(reasons)
}

/// Conventional-commit breaking subject: `type!: subject` or
/// `type(scope)!: subject`.
fn subject_has_breaking_marker(subject: &str) -> bool {
    let Some((prefix, _)) = subject.split_once(':') else {
        return false;
    };
    let Some(prefix) = prefix.trim_end().strip_suffix('!') else {
        return false;
    };
    let commit_type = match prefix.split_once('(') {
        Some((commit_type, scope)) => {
            if !scope.ends_with(')') {
                return false;
            }
            commit_type
        }
        None => prefix,
    };
    !commit_type.is_empty()
        && commit_type
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
}

/// Conventional-commit breaking footer line: `BREAKING CHANGE:` or
/// `BREAKING-CHANGE:`.
fn is_breaking_footer(line: &str) -> bool {
    let rest = line
        .strip_prefix("BREAKING CHANGE")
        .or_else(|| line.strip_prefix("BREAKING-CHANGE"));
    matches!(rest.map(str::trim_start), Some(rest) if rest.starts_with(':') || rest.starts_with('#'))
}

fn breaking_footer_line(message: &str) -> Option<&str> {
    message
        .lines()
        .map(str::trim)
        .find(|line| is_breaking_footer(line))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subject_markers_follow_the_conventional_breaking_forms() {
        assert!(subject_has_breaking_marker("feat!: drop legacy API"));
        assert!(subject_has_breaking_marker("fix(parser)!: reject nested"));
        assert!(!subject_has_breaking_marker("feat: add API"));
        assert!(!subject_has_breaking_marker("fix(parser): accept nested"));
        assert!(!subject_has_breaking_marker("just a sentence with ! in it"));
        assert!(!subject_has_breaking_marker("!: missing type"));
    }

    #[test]
    fn breaking_footers_match_both_spellings_and_nothing_else() {
        assert!(is_breaking_footer("BREAKING CHANGE: drops the v1 API"));
        assert!(is_breaking_footer("BREAKING-CHANGE: drops the v1 API"));
        assert!(!is_breaking_footer("BREAKING NEWS: nothing"));
        assert!(!is_breaking_footer("this mentions BREAKING CHANGE: inline"));
    }

    #[test]
    fn version_shape_verdicts_do_not_touch_the_repository() {
        let major = release_range_breaking_verdict(
            "file:///nonexistent",
            &semver::Version::parse("1.2.3").unwrap(),
            &semver::Version::parse("2.0.0").unwrap(),
            "component-v2.0.0",
        );
        assert_eq!(major.breaking, Some(true));
        assert!(major.breaking_reasons[0].contains("major version changes"));

        let zero_minor = release_range_breaking_verdict(
            "file:///nonexistent",
            &semver::Version::parse("0.17.0").unwrap(),
            &semver::Version::parse("0.18.0").unwrap(),
            "component-v0.18.0",
        );
        assert_eq!(zero_minor.breaking, Some(true));
        assert!(zero_minor.breaking_reasons[0].contains("0.x minor changes"));
    }
}
