//! "Is my fix released yet?" — commit→release containment, and the inverse
//! installed-versus-latest gap.
//!
//! Homeboy owns releases end to end: it cuts the tag, builds the artifacts, and
//! publishes. Until this module existed it could not answer the one question
//! that fast merging makes constant — *which release first contained this
//! commit, and is the binary in front of me running it?* Answering it meant
//! hand-rolling `git merge-base --is-ancestor <sha> <tag>` once per candidate
//! tag (#11754).
//!
//! # Why "earliest containing tag" is the whole problem
//!
//! Release tags are **not ordered by ancestry**. A patch cut from an older
//! release line can contain a commit that a numerically later tag on another
//! line does not, and `git describe --contains` answers a different question
//! (nearest tag by walk distance, which is not the same as lowest version).
//! "First released in" is therefore the *minimum by semantic version* over the
//! set of tags that contain the commit — never the first line of `git tag
//! --contains`, never the newest tag, never `git describe`.
//!
//! # Cost and offline behaviour
//!
//! Containment is pure git plus release metadata: two `git tag` invocations
//! against whatever tags the checkout has already fetched. No network I/O, no
//! GitHub call, no release API. The **only** network path in this module is
//! [`resolve_issue_commit`], reached exclusively when the operator asks by
//! issue number instead of by sha.
//!
//! # Why "not yet released" and "released but not installed" are distinct
//!
//! They lead to different actions. A merged-but-unreleased fix needs a release
//! cut; a released-but-not-installed fix needs `homeboy upgrade`. Collapsing
//! them into one "you do not have it" verdict is exactly the ambiguity that
//! made the manual query expensive.

use semver::Version;
use serde::{Deserialize, Serialize};
use std::path::Path;

use homeboy_core::build_identity;
use homeboy_core::component::{self, Component};
use homeboy_core::error::{Error, Result};
use homeboy_core::git;

use crate::release::scope::ReleaseScope;

/// Command an operator runs when a fix is merged but no release contains it.
pub const REMEDIATION_RELEASE: &str = "homeboy release";
/// Command an operator runs when a release contains the fix but this binary does not.
pub const REMEDIATION_UPGRADE: &str = "homeboy upgrade";

/// A release tag paired with the exact semantic version it encodes.
///
/// Only exact `X.Y.Z` release tags are represented: a pre-release, a
/// `v1.2`-style short tag, or an arbitrary annotation is not a release this
/// query can order, so it is dropped rather than guessed at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseTag {
    pub tag: String,
    pub version: Version,
}

/// Parse the exact release version a tag encodes inside a component's tag
/// namespace, or `None` when the tag is not an exact release tag there.
///
/// Mirrors the tag naming contract in [`crate::release::component_tag_name`]:
/// a scoped component releases as `<component>-vX.Y.Z`, a root component as
/// `vX.Y.Z`.
pub fn release_version_from_tag(tag: &str, tag_prefix: Option<&str>) -> Option<Version> {
    let tag = tag.trim();
    let tag = match tag_prefix {
        Some(prefix) => tag.strip_prefix(&format!("{prefix}-"))?,
        None => tag,
    };
    let version = tag.strip_prefix('v').unwrap_or(tag);

    if !is_exact_semver_core(version) {
        return None;
    }

    Version::parse(version).ok()
}

/// Exactly three all-numeric dot-separated parts. Deliberately stricter than
/// `Version::parse`, which accepts pre-release and build metadata: those are
/// not releases this query orders.
fn is_exact_semver_core(version: &str) -> bool {
    let parts = version.split('.').collect::<Vec<_>>();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
}

/// Parse tag names into release tags, ascending by version.
///
/// Non-release tags are dropped. Sorting is by parsed semantic version, never
/// lexicographic: `v0.9.0` must sort before `v0.10.0`.
pub fn parse_release_tags(tags: &[String], tag_prefix: Option<&str>) -> Vec<ReleaseTag> {
    let mut parsed = tags
        .iter()
        .filter_map(|tag| {
            release_version_from_tag(tag, tag_prefix).map(|version| ReleaseTag {
                tag: tag.trim().to_string(),
                version,
            })
        })
        .collect::<Vec<_>>();
    parsed.sort_by(|a, b| a.version.cmp(&b.version).then_with(|| a.tag.cmp(&b.tag)));
    parsed.dedup_by(|a, b| a.tag == b.tag);
    parsed
}

/// The earliest release that contains a commit.
///
/// `containing` is the output of `git tag --contains <sha>`. Because tags are
/// not ordered by ancestry, the answer is the **minimum by version** over that
/// set — see the module docs.
pub fn earliest_containing_release(
    containing: &[String],
    tag_prefix: Option<&str>,
) -> Option<ReleaseTag> {
    parse_release_tags(containing, tag_prefix)
        .into_iter()
        .next()
}

/// The newest release in a namespace, by version.
pub fn latest_release(tags: &[String], tag_prefix: Option<&str>) -> Option<ReleaseTag> {
    parse_release_tags(tags, tag_prefix).pop()
}

/// Where a commit sits relative to the releases that exist and to the build
/// running this command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainmentStatus {
    /// Merged, but no release tag contains it. Needs a release, not an upgrade.
    NotYetReleased,
    /// Released, and the installed build provably contains it.
    ReleasedAndInstalled,
    /// Released, but the installed build predates that release. Needs an upgrade.
    ReleasedNotInstalled,
    /// Released, but the installed build could not be compared against this
    /// checkout's tags. Never reported as either of the actionable verdicts.
    ReleasedInstallUnknown,
}

impl ContainmentStatus {
    /// True when some release provably contains the commit.
    pub fn is_released(self) -> bool {
        !matches!(self, Self::NotYetReleased)
    }

    /// The command that closes the gap, or `None` when there is nothing to do
    /// or nothing established.
    pub fn remediation(self) -> Option<&'static str> {
        match self {
            Self::NotYetReleased => Some(REMEDIATION_RELEASE),
            Self::ReleasedNotInstalled => Some(REMEDIATION_UPGRADE),
            Self::ReleasedAndInstalled | Self::ReleasedInstallUnknown => None,
        }
    }
}

/// Everything the containment verdict needs, with no git or network access.
///
/// Separated from the git layer so the ordering rule, the status split, and the
/// message are testable without a repository.
#[derive(Debug, Clone, Copy)]
pub struct ContainmentFacts<'a> {
    /// Every tag present in this checkout (release and otherwise).
    pub known_tags: &'a [String],
    /// The subset that contains the commit — `git tag --contains <sha>`.
    pub containing_tags: &'a [String],
    /// The release tag the installed build corresponds to, when one is known.
    pub installed_tag: Option<&'a str>,
}

/// The containment verdict for one commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainmentAssessment {
    pub status: ContainmentStatus,
    /// Earliest release tag containing the commit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_released_in: Option<String>,
    /// That tag's version, without the tag namespace or `v` prefix.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_released_version: Option<String>,
    /// Release tag the installed build corresponds to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed_tag: Option<String>,
    /// Whether that tag exists in this checkout at all. When false the
    /// installed comparison is not attempted — an absent tag is missing
    /// evidence, not evidence of absence.
    pub installed_tag_known_locally: bool,
    /// Whether the installed build contains the commit. `None` when the
    /// comparison could not be made.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed_contains: Option<bool>,
    /// Number of releases cut at or after the first containing one.
    pub releases_since: usize,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

/// Pure containment assessment. No git, no network, no clock.
pub fn assess(facts: ContainmentFacts<'_>, tag_prefix: Option<&str>) -> ContainmentAssessment {
    let first = earliest_containing_release(facts.containing_tags, tag_prefix);
    let known = parse_release_tags(facts.known_tags, tag_prefix);

    let installed_tag_known_locally = facts
        .installed_tag
        .is_some_and(|tag| known.iter().any(|entry| entry.tag == tag));

    // An installed tag that this checkout does not have cannot be compared:
    // reporting "does not contain" from a missing tag would be a false negative
    // that sends the operator to `homeboy upgrade` for no reason.
    let installed_contains = match facts.installed_tag {
        Some(tag) if installed_tag_known_locally => Some(
            facts
                .containing_tags
                .iter()
                .any(|candidate| candidate.trim() == tag),
        ),
        _ => None,
    };

    let status = match (first.as_ref(), installed_contains) {
        (None, _) => ContainmentStatus::NotYetReleased,
        (Some(_), Some(true)) => ContainmentStatus::ReleasedAndInstalled,
        (Some(_), Some(false)) => ContainmentStatus::ReleasedNotInstalled,
        (Some(_), None) => ContainmentStatus::ReleasedInstallUnknown,
    };

    let releases_since = first
        .as_ref()
        .map(|entry| {
            known
                .iter()
                .filter(|candidate| candidate.version >= entry.version)
                .count()
        })
        .unwrap_or(0);

    let detail = containment_detail(status, first.as_ref(), facts.installed_tag);

    ContainmentAssessment {
        status,
        first_released_in: first.as_ref().map(|entry| entry.tag.clone()),
        first_released_version: first.as_ref().map(|entry| entry.version.to_string()),
        installed_tag: facts.installed_tag.map(str::to_string),
        installed_tag_known_locally,
        installed_contains,
        releases_since,
        detail,
        remediation: status.remediation().map(str::to_string),
    }
}

fn containment_detail(
    status: ContainmentStatus,
    first: Option<&ReleaseTag>,
    installed_tag: Option<&str>,
) -> String {
    let released = first.map(|entry| entry.tag.as_str()).unwrap_or("");
    let installed = installed_tag.unwrap_or("an unknown build");
    match status {
        ContainmentStatus::NotYetReleased => format!(
            "merged but NOT YET RELEASED: no release tag contains this commit — cut a release with `{REMEDIATION_RELEASE}`"
        ),
        ContainmentStatus::ReleasedAndInstalled => format!(
            "first released in {released}; installed {installed} contains this commit"
        ),
        ContainmentStatus::ReleasedNotInstalled => format!(
            "first released in {released}; installed {installed} does NOT contain this commit — run `{REMEDIATION_UPGRADE}`"
        ),
        ContainmentStatus::ReleasedInstallUnknown => format!(
            "first released in {released}; the installed build could not be compared against this checkout's tags"
        ),
    }
}

/// Where the installed build sits relative to the newest release in a checkout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GapStatus {
    /// Installed build is the newest release.
    Current,
    /// Installed build is behind one or more releases.
    Behind,
    /// Installed build is newer than any release tag here — normal for a source
    /// build on an unreleased branch.
    Ahead,
    /// Not established: no installed version, no releases, or an unparseable
    /// version. Never reported as current.
    Unknown,
}

/// The installed-versus-latest verdict for one component checkout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseGapAssessment {
    pub status: GapStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed_tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_version: Option<String>,
    /// Releases published after the installed one.
    pub releases_behind: usize,
    /// Commits on the newest release that the installed release does not have.
    /// `None` when it could not be counted offline.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commits_behind: Option<u64>,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

/// Pure gap assessment. `commits_behind` is filled in by the git layer.
pub fn assess_gap(
    known_tags: &[String],
    tag_prefix: Option<&str>,
    installed_version: Option<&str>,
    installed_tag: Option<&str>,
) -> ReleaseGapAssessment {
    let releases = parse_release_tags(known_tags, tag_prefix);
    let latest = releases.last().cloned();
    let installed = installed_version
        .map(|value| value.trim().trim_start_matches('v'))
        .and_then(|value| Version::parse(value).ok());

    let releases_behind = installed
        .as_ref()
        .map(|version| {
            releases
                .iter()
                .filter(|entry| entry.version > *version)
                .count()
        })
        .unwrap_or(0);

    let status = match (installed.as_ref(), latest.as_ref()) {
        (Some(installed), Some(latest)) if *installed < latest.version => GapStatus::Behind,
        (Some(installed), Some(latest)) if *installed > latest.version => GapStatus::Ahead,
        (Some(_), Some(_)) => GapStatus::Current,
        _ => GapStatus::Unknown,
    };

    let detail = gap_detail(
        status,
        installed_version,
        latest.as_ref(),
        releases_behind,
        None,
    );

    ReleaseGapAssessment {
        status,
        installed_version: installed_version.map(str::to_string),
        installed_tag: installed_tag.map(str::to_string),
        latest_tag: latest.as_ref().map(|entry| entry.tag.clone()),
        latest_version: latest.as_ref().map(|entry| entry.version.to_string()),
        releases_behind,
        commits_behind: None,
        detail,
        remediation: matches!(status, GapStatus::Behind).then(|| REMEDIATION_UPGRADE.to_string()),
    }
}

fn gap_detail(
    status: GapStatus,
    installed_version: Option<&str>,
    latest: Option<&ReleaseTag>,
    releases_behind: usize,
    commits_behind: Option<u64>,
) -> String {
    let installed = installed_version.unwrap_or("unknown");
    let latest_tag = latest.map(|entry| entry.tag.as_str()).unwrap_or("unknown");
    match status {
        GapStatus::Current => format!("installed {installed} is the latest release ({latest_tag})"),
        GapStatus::Ahead => format!(
            "installed {installed} is ahead of the latest release {latest_tag} (unreleased build)"
        ),
        GapStatus::Behind => {
            let commits = match commits_behind {
                Some(count) => format!(", {count} commits behind"),
                None => String::new(),
            };
            format!(
                "installed {installed}, latest {latest_tag} — {releases_behind} release(s) behind{commits} — run `{REMEDIATION_UPGRADE}`"
            )
        }
        GapStatus::Unknown => format!(
            "release gap not established for installed {installed} against latest {latest_tag}"
        ),
    }
}

// ---------------------------------------------------------------------------
// Git-backed layer
// ---------------------------------------------------------------------------

/// Which commit a containment query is about, and how it was named.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedCommit {
    /// Full resolved commit sha.
    pub commit: String,
    /// Abbreviated sha, as an operator would paste it.
    pub short_commit: String,
    /// Commit subject, when the checkout has the commit's metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// Issue number the commit was resolved from, when `--issue` was used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_from_issue: Option<u64>,
    /// Pull request the issue resolved through.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_via_pull_request: Option<u64>,
}

/// What to ask about, and against which checkout.
#[derive(Debug, Clone, Default)]
pub struct ContainsQuery {
    pub component_id: Option<String>,
    pub path: Option<String>,
    /// Commit-ish to resolve. Mutually exclusive with `issue`.
    pub commit: Option<String>,
    /// Issue number to resolve through the pull request that closed it.
    pub issue: Option<u64>,
    /// Version to treat as installed. Defaults to the running binary's version.
    pub installed_version: Option<String>,
}

/// The full `homeboy release contains` answer.
#[derive(Debug, Clone, Serialize)]
pub struct ReleaseContainsReport {
    pub command: String,
    pub component_id: String,
    pub resolved: ResolvedCommit,
    pub containment: ContainmentAssessment,
    /// Human-readable lines, in the order an operator should read them.
    pub summary: Vec<String>,
}

/// The full `homeboy release gap` answer.
#[derive(Debug, Clone, Serialize)]
pub struct ReleaseGapReport {
    pub command: String,
    pub component_id: String,
    pub gap: ReleaseGapAssessment,
    pub summary: Vec<String>,
}

/// Answer "which release first contained this commit, and does this binary
/// have it?".
///
/// Pure git plus release metadata against already-fetched tags. Network I/O
/// happens only when `query.issue` is set.
pub fn contains(query: &ContainsQuery) -> Result<ReleaseContainsReport> {
    if query.commit.is_some() && query.issue.is_some() {
        return Err(Error::validation_invalid_argument(
            "issue",
            "pass either a commit or --issue <n>, not both",
            None,
            Some(vec![
                "homeboy release contains <sha>".to_string(),
                "homeboy release contains --issue <n>".to_string(),
            ]),
        ));
    }

    let component =
        component::resolve_effective(query.component_id.as_deref(), query.path.as_deref(), None)?;
    let scope = ReleaseScope::resolve(&component, &component.id)?;
    let tag_prefix = scope.tag_prefix().map(str::to_string);

    let (commitish, resolved_from_issue, resolved_via_pull_request) =
        match (&query.commit, query.issue) {
            (Some(commit), _) => (commit.clone(), None, None),
            (None, Some(issue)) => {
                let resolution = resolve_issue_commit(&component, issue)?;
                (
                    resolution.commit,
                    Some(issue),
                    Some(resolution.pull_request),
                )
            }
            (None, None) => {
                return Err(Error::validation_missing_argument(vec![
                    "<commit>, or --issue <n>".to_string(),
                ]));
            }
        };

    let resolved = resolve_commit(
        &scope.git_root,
        &commitish,
        resolved_from_issue,
        resolved_via_pull_request,
    )?;

    let known_tags = list_tags(&scope.git_root)?;
    let containing_tags = tags_containing(&scope.git_root, &resolved.commit)?;
    let installed_version = query
        .installed_version
        .clone()
        .unwrap_or_else(|| build_identity::current().version);
    let installed_tag = scope.tag_name(&installed_version);

    let containment = assess(
        ContainmentFacts {
            known_tags: &known_tags,
            containing_tags: &containing_tags,
            installed_tag: Some(installed_tag.as_str()),
        },
        tag_prefix.as_deref(),
    );

    let summary = contains_summary(&resolved, &containment, &installed_version);

    Ok(ReleaseContainsReport {
        command: "release.contains".to_string(),
        component_id: component.id.clone(),
        resolved,
        containment,
        summary,
    })
}

/// Answer "how far behind the newest release is the installed build?".
pub fn gap(
    component_id: Option<&str>,
    path: Option<&str>,
    installed_override: Option<&str>,
) -> Result<ReleaseGapReport> {
    let component = component::resolve_effective(component_id, path, None)?;
    let scope = ReleaseScope::resolve(&component, &component.id)?;
    let tag_prefix = scope.tag_prefix().map(str::to_string);

    let known_tags = list_tags(&scope.git_root)?;
    let installed_version = installed_override
        .map(str::to_string)
        .unwrap_or_else(|| build_identity::current().version);
    let installed_tag = scope.tag_name(&installed_version);

    let mut assessment = assess_gap(
        &known_tags,
        tag_prefix.as_deref(),
        Some(installed_version.as_str()),
        Some(installed_tag.as_str()),
    );

    // Only count commits when the installed release is genuinely behind and
    // both endpoints exist locally; otherwise the range is meaningless and a
    // missing tag would turn into a silently wrong count.
    let latest = latest_release(&known_tags, tag_prefix.as_deref());
    if matches!(assessment.status, GapStatus::Behind) {
        if let Some(latest) = latest.as_ref() {
            assessment.commits_behind =
                count_commits_between(&scope.git_root, &installed_tag, &latest.tag);
            assessment.detail = gap_detail(
                assessment.status,
                Some(installed_version.as_str()),
                Some(latest),
                assessment.releases_behind,
                assessment.commits_behind,
            );
        }
    }

    let summary = gap_summary(&assessment);

    Ok(ReleaseGapReport {
        command: "release.gap".to_string(),
        component_id: component.id.clone(),
        gap: assessment,
        summary,
    })
}

/// Human-readable lines mirroring the shape the issue asked for.
fn contains_summary(
    resolved: &ResolvedCommit,
    containment: &ContainmentAssessment,
    installed_version: &str,
) -> Vec<String> {
    let mut lines = Vec::new();
    if let (Some(issue), Some(pr)) = (
        resolved.resolved_from_issue,
        resolved.resolved_via_pull_request,
    ) {
        let subject = resolved.subject.as_deref().unwrap_or("");
        lines.push(format!(
            "resolved: #{issue} -> PR #{pr} -> {} ({subject})",
            resolved.short_commit
        ));
    }

    match containment.first_released_in.as_deref() {
        Some(tag) => lines.push(format!("first released in: {tag}")),
        None => lines.push("first released in: (not yet released)".to_string()),
    }

    let verdict = match containment.installed_contains {
        Some(true) => "contains this commit",
        Some(false) => "does NOT contain this commit",
        None => "not comparable against this checkout's tags",
    };
    lines.push(format!(
        "installed here:    {installed_version} — {verdict}"
    ));

    lines.push(containment.detail.clone());
    lines
}

fn gap_summary(assessment: &ReleaseGapAssessment) -> Vec<String> {
    let mut lines = vec![format!(
        "installed {}, latest {}",
        assessment.installed_version.as_deref().unwrap_or("unknown"),
        assessment.latest_tag.as_deref().unwrap_or("unknown")
    )];
    if let Some(commits) = assessment.commits_behind {
        lines.push(format!("{commits} commits behind"));
    }
    lines.push(format!("{} release(s) behind", assessment.releases_behind));
    lines.push(assessment.detail.clone());
    lines
}

/// Every tag name present in the checkout.
fn list_tags(git_root: &str) -> Result<Vec<String>> {
    let raw =
        git::output_allow_empty(Path::new(git_root), &["tag", "--list"]).ok_or_else(|| {
            Error::git_command_failed(format!("git tag --list failed in {git_root}"))
                .with_hint("Is this path a git repository?")
        })?;
    Ok(split_tag_lines(&raw))
}

/// Tag names that contain `commit`.
///
/// One `git tag --contains` invocation replaces the per-tag
/// `git merge-base --is-ancestor` loop this command exists to retire.
fn tags_containing(git_root: &str, commit: &str) -> Result<Vec<String>> {
    let raw = git::output_allow_empty(Path::new(git_root), &["tag", "--contains", commit])
        .ok_or_else(|| {
            Error::git_command_failed(format!("git tag --contains {commit} failed in {git_root}"))
                .with_hint("Fetch tags first: git fetch --tags")
        })?;
    Ok(split_tag_lines(&raw))
}

fn split_tag_lines(raw: &str) -> Vec<String> {
    raw.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// Commits reachable from `head` but not from `base`. `None` when either
/// endpoint is missing locally, so an offline checkout reports "not counted"
/// rather than a wrong number.
fn count_commits_between(git_root: &str, base: &str, head: &str) -> Option<u64> {
    let range = format!("{base}..{head}");
    git::output_optional(Path::new(git_root), &["rev-list", "--count", &range])
        .and_then(|value| value.trim().parse::<u64>().ok())
}

/// Resolve a commit-ish against the checkout, carrying its subject when
/// available.
fn resolve_commit(
    git_root: &str,
    commitish: &str,
    resolved_from_issue: Option<u64>,
    resolved_via_pull_request: Option<u64>,
) -> Result<ResolvedCommit> {
    let root = Path::new(git_root);
    let peeled = format!("{commitish}^{{commit}}");
    let commit = git::rev_parse(root, &peeled).ok_or_else(|| {
        Error::validation_invalid_argument(
            "commit",
            format!("commit `{commitish}` is not present in {git_root}"),
            Some(commitish.to_string()),
            Some(vec![
                "Fetch the missing history first: git fetch --tags origin".to_string(),
            ]),
        )
    })?;

    let short_commit = git::output_optional(root, &["rev-parse", "--short", &commit])
        .unwrap_or_else(|| commit.chars().take(9).collect());
    let subject = git::output_optional(root, &["log", "-1", "--format=%s", &commit]);

    Ok(ResolvedCommit {
        commit,
        short_commit,
        subject,
        resolved_from_issue,
        resolved_via_pull_request,
    })
}

/// A pull request that closed an issue, and the commit it merged as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueResolution {
    pub pull_request: u64,
    pub commit: String,
}

/// Resolve an issue number to the commit of the pull request that closed it.
///
/// **The only network path in this module.** Asking by issue number is the
/// ergonomic half of this command — without it the operator has to find the sha
/// first, which is most of the manual work the query replaces.
pub fn resolve_issue_commit(component: &Component, issue: u64) -> Result<IssueResolution> {
    let remote = git::release_download::detect_remote_url(Path::new(&component.local_path))
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "issue",
                format!(
                    "component `{}` has no git remote, so issue #{issue} cannot be resolved",
                    component.id
                ),
                None,
                Some(vec!["Pass the commit sha directly instead".to_string()]),
            )
        })?;
    let repo = git::release_download::parse_github_url(&remote).ok_or_else(|| {
        Error::validation_invalid_argument(
            "issue",
            format!("remote `{remote}` is not a GitHub repository"),
            Some(remote.clone()),
            Some(vec!["Pass the commit sha directly instead".to_string()]),
        )
    })?;

    let client = git::GhClient::for_repo(&repo);
    client.ensure_ready()?;

    let query = concat!(
        "query($owner:String!,$name:String!,$number:Int!){",
        "repository(owner:$owner,name:$name){issue(number:$number){",
        "closedByPullRequestsReferences(first:20,includeClosedPrs:true){",
        "nodes{number merged mergeCommit{oid}}}}}}"
    );
    let args = vec![
        "api".to_string(),
        "graphql".to_string(),
        "-f".to_string(),
        format!("query={query}"),
        "-F".to_string(),
        format!("owner={}", repo.owner),
        "-F".to_string(),
        format!("name={}", repo.repo),
        "-F".to_string(),
        format!("number={issue}"),
    ];

    let raw = client.run(&args)?;
    parse_issue_resolution(&raw, issue)
}

/// Parse the GraphQL closing-PR payload into the merged PR and its commit.
///
/// Split out from the `gh` invocation so the shape — including the
/// unmerged-PR and no-PR cases — is testable without the network.
pub fn parse_issue_resolution(raw: &str, issue: u64) -> Result<IssueResolution> {
    #[derive(Deserialize)]
    struct Envelope {
        data: Option<Data>,
    }
    #[derive(Deserialize)]
    struct Data {
        repository: Option<Repository>,
    }
    #[derive(Deserialize)]
    struct Repository {
        issue: Option<Issue>,
    }
    #[derive(Deserialize)]
    struct Issue {
        #[serde(rename = "closedByPullRequestsReferences")]
        closed_by: Option<Connection>,
    }
    #[derive(Deserialize)]
    struct Connection {
        #[serde(default)]
        nodes: Vec<Node>,
    }
    #[derive(Deserialize)]
    struct Node {
        number: u64,
        #[serde(default)]
        merged: bool,
        #[serde(default, rename = "mergeCommit")]
        merge_commit: Option<MergeCommit>,
    }
    #[derive(Deserialize)]
    struct MergeCommit {
        oid: String,
    }

    let envelope: Envelope = serde_json::from_str(raw.trim()).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some(format!("parse closing pull requests for issue #{issue}")),
        )
    })?;

    let nodes = envelope
        .data
        .and_then(|data| data.repository)
        .and_then(|repository| repository.issue)
        .and_then(|issue| issue.closed_by)
        .map(|connection| connection.nodes)
        .unwrap_or_default();

    // Prefer the lowest-numbered merged PR with a commit: when an issue is
    // closed by several PRs, the first one to land is the one that introduced
    // the fix being asked about.
    let mut candidates = nodes
        .into_iter()
        .filter(|node| node.merged)
        .filter_map(|node| {
            let number = node.number;
            node.merge_commit.map(|commit| IssueResolution {
                pull_request: number,
                commit: commit.oid,
            })
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|candidate| candidate.pull_request);

    candidates.into_iter().next().ok_or_else(|| {
        Error::validation_invalid_argument(
            "issue",
            format!("no merged pull request closes issue #{issue}"),
            Some(issue.to_string()),
            Some(vec![
                "The fix may not have landed yet — check the issue on GitHub".to_string(),
                "Pass the commit sha directly if you already know it".to_string(),
            ]),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    // -- ordering -----------------------------------------------------------

    /// The rule this command exists for. `git tag --contains` emits tags in
    /// refname order, and refname order is neither ancestry order nor version
    /// order — so "first released in" must be the minimum by parsed version.
    #[test]
    fn earliest_containing_release_is_the_minimum_version_not_the_first_line() {
        let containing = tags(&["v0.334.0", "v0.333.9", "v0.340.0"]);

        let earliest = earliest_containing_release(&containing, None).expect("a containing tag");

        assert_eq!(earliest.tag, "v0.333.9");
    }

    /// Lexicographic ordering puts `v0.10.0` before `v0.9.0`; version ordering
    /// does not. A checkout with a two-digit minor is the common case here.
    #[test]
    fn earliest_containing_release_orders_numerically_not_lexicographically() {
        let containing = tags(&["v0.10.0", "v0.9.0", "v0.100.0"]);

        let earliest = earliest_containing_release(&containing, None).expect("a containing tag");

        assert_eq!(earliest.tag, "v0.9.0");
    }

    /// A patch cut from an older line contains commits a numerically later tag
    /// on another line does not. The containing set is the authority; ordering
    /// only decides which of those is earliest.
    #[test]
    fn earliest_containing_release_prefers_an_older_line_patch() {
        let containing = tags(&["v0.331.4", "v0.334.0", "v0.335.0"]);

        let earliest = earliest_containing_release(&containing, None).expect("a containing tag");

        assert_eq!(earliest.tag, "v0.331.4");
        assert_eq!(earliest.version, Version::new(0, 331, 4));
    }

    #[test]
    fn earliest_containing_release_is_absent_when_nothing_contains_the_commit() {
        assert!(earliest_containing_release(&[], None).is_none());
    }

    /// Non-release tags in the same repository must not become the answer.
    #[test]
    fn parse_release_tags_drops_non_release_tags() {
        let all = tags(&[
            "v0.334.0",
            "nightly",
            "v0.334.0-rc.1",
            "v1.2",
            "release-candidate",
            "v0.333.0",
        ]);

        let parsed = parse_release_tags(&all, None);

        assert_eq!(
            parsed
                .iter()
                .map(|tag| tag.tag.as_str())
                .collect::<Vec<_>>(),
            vec!["v0.333.0", "v0.334.0"]
        );
    }

    /// A scoped component releases as `<component>-vX.Y.Z`; tags from other
    /// namespaces in the same monorepo must not leak into the answer.
    #[test]
    fn parse_release_tags_scopes_to_the_component_namespace() {
        let all = tags(&[
            "wordpress-v1.2.0",
            "wordpress-v1.3.0",
            "theme-v9.9.9",
            "v2.0.0",
        ]);

        let parsed = parse_release_tags(&all, Some("wordpress"));

        assert_eq!(
            parsed
                .iter()
                .map(|tag| tag.tag.as_str())
                .collect::<Vec<_>>(),
            vec!["wordpress-v1.2.0", "wordpress-v1.3.0"]
        );
    }

    #[test]
    fn latest_release_is_the_maximum_version() {
        let all = tags(&["v0.9.0", "v0.100.0", "v0.10.0"]);

        assert_eq!(
            latest_release(&all, None).expect("a release").tag,
            "v0.100.0"
        );
    }

    // -- status split -------------------------------------------------------

    /// The exact scenario in #11754: the fix is in v0.334.0, the binary is
    /// 0.327.0. That is "released but not installed" — `homeboy upgrade`, not a
    /// release.
    #[test]
    fn released_but_not_installed_points_at_upgrade() {
        let known = tags(&["v0.327.0", "v0.333.0", "v0.334.0"]);
        let containing = tags(&["v0.334.0"]);

        let assessment = assess(
            ContainmentFacts {
                known_tags: &known,
                containing_tags: &containing,
                installed_tag: Some("v0.327.0"),
            },
            None,
        );

        assert_eq!(assessment.status, ContainmentStatus::ReleasedNotInstalled);
        assert_eq!(assessment.first_released_in.as_deref(), Some("v0.334.0"));
        assert_eq!(
            assessment.first_released_version.as_deref(),
            Some("0.334.0")
        );
        assert_eq!(assessment.installed_contains, Some(false));
        assert!(assessment.installed_tag_known_locally);
        assert_eq!(assessment.remediation.as_deref(), Some("homeboy upgrade"));
        assert!(assessment.detail.contains("does NOT contain"));
    }

    /// Merged but unreleased is a different action from stale-binary, and must
    /// read as one. Collapsing them is the ambiguity this command removes.
    #[test]
    fn not_yet_released_points_at_a_release_not_an_upgrade() {
        let known = tags(&["v0.333.0", "v0.334.0"]);

        let assessment = assess(
            ContainmentFacts {
                known_tags: &known,
                containing_tags: &[],
                installed_tag: Some("v0.334.0"),
            },
            None,
        );

        assert_eq!(assessment.status, ContainmentStatus::NotYetReleased);
        assert!(assessment.first_released_in.is_none());
        assert_eq!(assessment.remediation.as_deref(), Some("homeboy release"));
        assert!(assessment.detail.contains("NOT YET RELEASED"));
        assert_eq!(assessment.releases_since, 0);
    }

    #[test]
    fn released_and_installed_asks_for_nothing() {
        let known = tags(&["v0.333.0", "v0.334.0"]);
        let containing = tags(&["v0.333.0", "v0.334.0"]);

        let assessment = assess(
            ContainmentFacts {
                known_tags: &known,
                containing_tags: &containing,
                installed_tag: Some("v0.334.0"),
            },
            None,
        );

        assert_eq!(assessment.status, ContainmentStatus::ReleasedAndInstalled);
        assert_eq!(assessment.installed_contains, Some(true));
        assert!(assessment.remediation.is_none());
    }

    /// An installed tag this checkout does not have is missing evidence, not
    /// evidence of absence — reporting "does not contain" would send the
    /// operator to `homeboy upgrade` for no reason.
    #[test]
    fn absent_installed_tag_is_unknown_not_missing() {
        let known = tags(&["v0.333.0", "v0.334.0"]);
        let containing = tags(&["v0.334.0"]);

        let assessment = assess(
            ContainmentFacts {
                known_tags: &known,
                containing_tags: &containing,
                installed_tag: Some("v0.327.0"),
            },
            None,
        );

        assert_eq!(assessment.status, ContainmentStatus::ReleasedInstallUnknown);
        assert!(!assessment.installed_tag_known_locally);
        assert!(assessment.installed_contains.is_none());
        assert!(assessment.remediation.is_none());
    }

    #[test]
    fn no_installed_tag_at_all_is_unknown() {
        let known = tags(&["v0.334.0"]);
        let containing = tags(&["v0.334.0"]);

        let assessment = assess(
            ContainmentFacts {
                known_tags: &known,
                containing_tags: &containing,
                installed_tag: None,
            },
            None,
        );

        assert_eq!(assessment.status, ContainmentStatus::ReleasedInstallUnknown);
        assert!(assessment.installed_contains.is_none());
    }

    #[test]
    fn releases_since_counts_the_first_containing_release_and_later() {
        let known = tags(&["v0.331.0", "v0.332.0", "v0.333.0", "v0.334.0"]);
        let containing = tags(&["v0.332.0", "v0.333.0", "v0.334.0"]);

        let assessment = assess(
            ContainmentFacts {
                known_tags: &known,
                containing_tags: &containing,
                installed_tag: None,
            },
            None,
        );

        assert_eq!(assessment.releases_since, 3);
    }

    #[test]
    fn containment_status_predicates_match_the_action_split() {
        assert!(!ContainmentStatus::NotYetReleased.is_released());
        assert!(ContainmentStatus::ReleasedAndInstalled.is_released());
        assert!(ContainmentStatus::ReleasedNotInstalled.is_released());
        assert!(ContainmentStatus::ReleasedInstallUnknown.is_released());

        assert_eq!(
            ContainmentStatus::NotYetReleased.remediation(),
            Some(REMEDIATION_RELEASE)
        );
        assert_eq!(
            ContainmentStatus::ReleasedNotInstalled.remediation(),
            Some(REMEDIATION_UPGRADE)
        );
        assert!(ContainmentStatus::ReleasedAndInstalled
            .remediation()
            .is_none());
        assert!(ContainmentStatus::ReleasedInstallUnknown
            .remediation()
            .is_none());
    }

    /// A JSON consumer distinguishes the four verdicts by `status` alone.
    #[test]
    fn containment_serialized_shape_is_stable() {
        let known = tags(&["v0.327.0", "v0.334.0"]);
        let containing = tags(&["v0.334.0"]);

        let assessment = assess(
            ContainmentFacts {
                known_tags: &known,
                containing_tags: &containing,
                installed_tag: Some("v0.327.0"),
            },
            None,
        );
        let json = serde_json::to_value(&assessment).expect("assessment serializes");

        assert_eq!(json["status"], "released_not_installed");
        assert_eq!(json["first_released_in"], "v0.334.0");
        assert_eq!(json["installed_contains"], false);
        assert_eq!(json["remediation"], "homeboy upgrade");
    }

    // -- gap ----------------------------------------------------------------

    #[test]
    fn gap_counts_releases_published_after_the_installed_one() {
        let known = tags(&["v0.327.0", "v0.328.0", "v0.331.0", "v0.333.0"]);

        let assessment = assess_gap(&known, None, Some("0.327.0"), Some("v0.327.0"));

        assert_eq!(assessment.status, GapStatus::Behind);
        assert_eq!(assessment.releases_behind, 3);
        assert_eq!(assessment.latest_tag.as_deref(), Some("v0.333.0"));
        assert_eq!(assessment.remediation.as_deref(), Some("homeboy upgrade"));
    }

    #[test]
    fn gap_on_the_newest_release_is_current_and_silent() {
        let known = tags(&["v0.332.0", "v0.333.0"]);

        let assessment = assess_gap(&known, None, Some("0.333.0"), Some("v0.333.0"));

        assert_eq!(assessment.status, GapStatus::Current);
        assert_eq!(assessment.releases_behind, 0);
        assert!(assessment.remediation.is_none());
    }

    /// A source build on an unreleased branch is ahead, not behind. Warning on
    /// it would train operators to ignore this surface.
    #[test]
    fn gap_ahead_of_every_tag_is_not_behind() {
        let known = tags(&["v0.332.0", "v0.333.0"]);

        let assessment = assess_gap(&known, None, Some("0.334.0"), Some("v0.334.0"));

        assert_eq!(assessment.status, GapStatus::Ahead);
        assert_eq!(assessment.releases_behind, 0);
        assert!(assessment.remediation.is_none());
    }

    #[test]
    fn gap_without_tags_is_unknown_not_current() {
        let assessment = assess_gap(&[], None, Some("0.327.0"), Some("v0.327.0"));

        assert_eq!(assessment.status, GapStatus::Unknown);
        assert!(assessment.latest_tag.is_none());
    }

    #[test]
    fn gap_with_an_unparseable_installed_version_is_unknown() {
        let known = tags(&["v0.333.0"]);

        let assessment = assess_gap(&known, None, Some("not-a-version"), None);

        assert_eq!(assessment.status, GapStatus::Unknown);
    }

    /// The `v` prefix is how a release tag spells a version; it must not defeat
    /// the comparison when it rides along on the installed value.
    #[test]
    fn gap_tolerates_a_v_prefixed_installed_version() {
        let known = tags(&["v0.332.0", "v0.333.0"]);

        let assessment = assess_gap(&known, None, Some("v0.333.0"), Some("v0.333.0"));

        assert_eq!(assessment.status, GapStatus::Current);
    }

    #[test]
    fn gap_serialized_shape_is_stable() {
        let known = tags(&["v0.327.0", "v0.333.0"]);

        let assessment = assess_gap(&known, None, Some("0.327.0"), Some("v0.327.0"));
        let json = serde_json::to_value(&assessment).expect("gap serializes");

        assert_eq!(json["status"], "behind");
        assert_eq!(json["installed_version"], "0.327.0");
        assert_eq!(json["latest_tag"], "v0.333.0");
        assert_eq!(json["releases_behind"], 1);
    }

    // -- issue resolution ---------------------------------------------------

    #[test]
    fn issue_resolution_reads_the_merged_closing_pull_request() {
        let raw = r#"{
            "data": {"repository": {"issue": {"closedByPullRequestsReferences": {"nodes": [
                {"number": 11720, "merged": true, "mergeCommit": {"oid": "6043c013d0000000000000000000000000000000"}}
            ]}}}}
        }"#;

        let resolution = parse_issue_resolution(raw, 11702).expect("merged PR resolves");

        assert_eq!(resolution.pull_request, 11720);
        assert_eq!(
            resolution.commit,
            "6043c013d0000000000000000000000000000000"
        );
    }

    /// When several PRs close one issue, the one that landed first is the one
    /// that introduced the fix being asked about.
    #[test]
    fn issue_resolution_prefers_the_first_landed_pull_request() {
        let raw = r#"{
            "data": {"repository": {"issue": {"closedByPullRequestsReferences": {"nodes": [
                {"number": 11800, "merged": true, "mergeCommit": {"oid": "bbb"}},
                {"number": 11720, "merged": true, "mergeCommit": {"oid": "aaa"}}
            ]}}}}
        }"#;

        let resolution = parse_issue_resolution(raw, 11702).expect("merged PR resolves");

        assert_eq!(resolution.pull_request, 11720);
        assert_eq!(resolution.commit, "aaa");
    }

    /// An open PR has no commit on main. Resolving through it would answer a
    /// containment question about a commit that does not exist yet.
    #[test]
    fn issue_resolution_ignores_unmerged_pull_requests() {
        let raw = r#"{
            "data": {"repository": {"issue": {"closedByPullRequestsReferences": {"nodes": [
                {"number": 11720, "merged": false, "mergeCommit": null}
            ]}}}}
        }"#;

        let error = parse_issue_resolution(raw, 11702).expect_err("unmerged PR does not resolve");

        assert_eq!(error.code.as_str(), "validation.invalid_argument");
        assert!(error.message.contains("no merged pull request"));
    }

    #[test]
    fn issue_resolution_reports_an_issue_with_no_closing_pull_request() {
        let raw = r#"{"data": {"repository": {"issue": {"closedByPullRequestsReferences": {"nodes": []}}}}}"#;

        let error = parse_issue_resolution(raw, 11702).expect_err("no PR does not resolve");

        assert!(error.message.contains("#11702"));
    }

    #[test]
    fn issue_resolution_reports_a_missing_issue() {
        let raw = r#"{"data": {"repository": {"issue": null}}}"#;

        let error =
            parse_issue_resolution(raw, 999_999).expect_err("missing issue does not resolve");

        assert!(error.message.contains("#999999"));
    }

    // -- helpers ------------------------------------------------------------

    #[test]
    fn tag_lines_are_split_and_trimmed() {
        assert_eq!(
            split_tag_lines("v1.0.0\n\n  v1.1.0  \n"),
            vec!["v1.0.0".to_string(), "v1.1.0".to_string()]
        );
        assert!(split_tag_lines("").is_empty());
    }

    #[test]
    fn exact_semver_core_rejects_prerelease_and_short_versions() {
        assert!(is_exact_semver_core("1.2.3"));
        assert!(!is_exact_semver_core("1.2"));
        assert!(!is_exact_semver_core("1.2.3.4"));
        assert!(!is_exact_semver_core("1.2.3-rc.1"));
        assert!(!is_exact_semver_core("1.2.x"));
        assert!(!is_exact_semver_core(""));
    }
}
