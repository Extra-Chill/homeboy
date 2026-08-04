//! Controller binary staleness — is the `homeboy` binary executing this command
//! behind the latest published release?
//!
//! Runner-side freshness already has surfaces: `runtime_overlay_freshness`
//! compares a synced build against its source, and the controller↔runner version
//! check (`require_exact_homeboy_version`) compares a runner daemon against the
//! controller. Both take the *controller* as the reference point and none of
//! them ask whether that reference is itself current. A controller two minor
//! releases behind dispatches work whose behavior it may not model correctly —
//! the same class of bug, one level up (#11483).
//!
//! # Cost
//!
//! This module performs **no network I/O**. It reads the latest published
//! version out of the daily update-check cache that
//! [`crate::upgrade::update_check::run_startup_check`] already maintains, so the
//! whole surface costs one small file read per command and at most one network
//! call per day — the call the startup check was already making.
//!
//! Every failure mode degrades to [`ControllerCurrency::Unknown`]: no cache, an
//! unparseable version, an operator-disabled update check, or an offline host
//! all report "not established" rather than guessing "current" or failing a
//! command.
//!
//! # What this deliberately does not compute
//!
//! The issue asks for a commit delta ("215 commits behind"). That requires a
//! source checkout with an up-to-date `origin/main`, which a packaged install
//! does not have and which would cost a `git fetch`. The release delta is
//! derivable from facts already on hand, so that is what is reported; the
//! embedded build commit is carried alongside it so an operator with a checkout
//! can compute the commit delta themselves.

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::upgrade::update_check;
use homeboy_core::build_identity::{self, BuildIdentity};
use homeboy_core::update_check_cache;

/// Minor-release distance at or beyond which staleness is escalated from a note
/// to a warning.
///
/// One minor release behind is ordinary drift on a project that releases
/// continuously. *More than* one minor release behind is the threshold the
/// issue names: at that distance the controller is dispatching work against
/// behavior it may not model correctly.
pub const ESCALATION_MINOR_RELEASES_BEHIND: u64 = 2;

/// The command that resolves controller staleness. Reported as remediation so
/// the operator never has to look it up.
pub const REMEDIATION_COMMAND: &str = "homeboy upgrade";

/// Where the running controller binary sits relative to the latest published
/// release.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControllerCurrency {
    /// Running exactly the latest published release.
    Current,
    /// Running something newer than the latest published release — normal for a
    /// source build on an unreleased branch. Not staleness.
    Ahead,
    /// Behind by patch releases only, within the same minor line.
    BehindPatch,
    /// Behind by one or more minor releases.
    BehindMinor,
    /// Behind by one or more major releases.
    BehindMajor,
    /// Not established: no cached release to compare against, an unparseable
    /// version, or the update check is disabled. Never treated as current.
    Unknown,
}

impl ControllerCurrency {
    /// True only when the controller is provably behind the latest published
    /// release. [`ControllerCurrency::Unknown`] is deliberately not behind — an
    /// unestablished verdict must not manufacture a warning.
    pub fn is_behind(self) -> bool {
        matches!(
            self,
            Self::BehindPatch | Self::BehindMinor | Self::BehindMajor
        )
    }
}

/// Freshness of the controller binary running this command, plus everything an
/// operator needs to act on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControllerStaleness {
    pub status: ControllerCurrency,
    /// `true` only when the controller is provably behind a published release.
    pub stale: bool,
    /// `true` when the drift is past [`ESCALATION_MINOR_RELEASES_BEHIND`] (or
    /// any major release), i.e. worth a warning rather than a note.
    pub escalated: bool,
    /// Version of the binary executing this command.
    pub running_version: String,
    /// Full build identity including the embedded git commit, e.g.
    /// `homeboy 0.327.0+ed33954781a9`.
    pub build_identity: String,
    /// Embedded git commit of the running build, when the build recorded one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_commit: Option<String>,
    /// Latest published release, as of the cached check.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_version: Option<String>,
    /// Minor releases between the running build and the latest published
    /// release. `Some(0)` means same minor line; `None` when the comparison
    /// could not be made or the majors differ (where a minor delta is
    /// meaningless).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minor_releases_behind: Option<u64>,
    /// Unix timestamp of the cached check the comparison used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<u64>,
    /// Age of that cached check in seconds, so a reader can tell a fresh verdict
    /// from one made against a week-old release list on an offline host.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_age_secs: Option<u64>,
    /// One-line human summary.
    pub detail: String,
    /// Command that resolves the drift, present only when there is drift.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

impl ControllerStaleness {
    /// The single warning line for a stale controller, or `None` when the
    /// controller is current, ahead, or unestablished.
    pub fn warning_line(&self) -> Option<String> {
        self.stale.then(|| self.detail.clone())
    }
}

/// Staleness of the controller running this command, from the daily
/// update-check cache. No network I/O; never fails.
pub fn current() -> ControllerStaleness {
    let identity = build_identity::current();
    if update_check::is_disabled() {
        return unknown(
            &identity,
            None,
            None,
            0,
            "controller freshness not established: the update check is disabled \
             (HOMEBOY_NO_UPDATE_CHECK or `homeboy config set /update_check false`)",
        );
    }

    let cached = update_check::cached_latest_release();
    assess(
        &identity,
        cached.as_ref().map(|entry| entry.latest_version.as_str()),
        cached.as_ref().map(|entry| entry.checked_at),
        update_check_cache::now_unix(),
    )
}

/// Pure staleness comparison. Separated from cache and clock access so the
/// classification, the escalation threshold, and the message are testable
/// without touching the filesystem or the network.
pub fn assess(
    identity: &BuildIdentity,
    latest: Option<&str>,
    checked_at: Option<u64>,
    now: u64,
) -> ControllerStaleness {
    let Some(latest_raw) = latest.map(normalize_version).filter(|v| !v.is_empty()) else {
        return unknown(
            identity,
            None,
            checked_at,
            now,
            "controller freshness not established: no cached release to compare against \
             (the daily update check has not produced a result yet)",
        );
    };

    let running_raw = normalize_version(&identity.version);
    let parsed = Version::parse(&running_raw)
        .ok()
        .zip(Version::parse(&latest_raw).ok());

    let Some((running, latest_version)) = parsed else {
        return unknown(
            identity,
            Some(latest_raw),
            checked_at,
            now,
            format!(
                "controller freshness not established: could not compare `{}` against the latest published release",
                identity.version
            ),
        );
    };

    let (status, minor_releases_behind) = classify(&running, &latest_version);
    let stale = status.is_behind();
    let escalated = matches!(status, ControllerCurrency::BehindMajor)
        || minor_releases_behind.is_some_and(|behind| behind >= ESCALATION_MINOR_RELEASES_BEHIND);

    let detail = detail_line(
        identity,
        status,
        &latest_raw,
        minor_releases_behind,
        escalated,
    );

    ControllerStaleness {
        status,
        stale,
        escalated,
        running_version: identity.version.clone(),
        build_identity: identity.display.clone(),
        git_commit: identity.git_commit.clone(),
        latest_version: Some(latest_raw),
        minor_releases_behind,
        checked_at,
        cache_age_secs: checked_at.map(|at| age_secs(at, now)),
        detail,
        remediation: stale.then(|| REMEDIATION_COMMAND.to_string()),
    }
}

/// Classify the running build against the latest published release, and count
/// minor releases behind where that count is meaningful.
///
/// A minor delta across differing majors is not a distance an operator can act
/// on, so it is reported as `None` rather than a misleading number; a major
/// behind escalates on its own.
fn classify(running: &Version, latest: &Version) -> (ControllerCurrency, Option<u64>) {
    if latest.major > running.major {
        return (ControllerCurrency::BehindMajor, None);
    }
    if latest.major < running.major {
        return (ControllerCurrency::Ahead, None);
    }
    if latest.minor > running.minor {
        return (
            ControllerCurrency::BehindMinor,
            Some(latest.minor - running.minor),
        );
    }
    if latest.minor < running.minor {
        return (ControllerCurrency::Ahead, None);
    }
    if latest.patch > running.patch {
        return (ControllerCurrency::BehindPatch, Some(0));
    }
    if latest.patch < running.patch {
        return (ControllerCurrency::Ahead, None);
    }
    (ControllerCurrency::Current, Some(0))
}

fn detail_line(
    identity: &BuildIdentity,
    status: ControllerCurrency,
    latest: &str,
    minor_releases_behind: Option<u64>,
    escalated: bool,
) -> String {
    match status {
        ControllerCurrency::Current => {
            format!("{} is the latest published release", identity.display)
        }
        ControllerCurrency::Ahead => format!(
            "{} is ahead of the latest published release v{latest} (unreleased build)",
            identity.display
        ),
        ControllerCurrency::BehindPatch => format!(
            "{} is behind the latest published release v{latest} (patch) — run `{REMEDIATION_COMMAND}`",
            identity.display
        ),
        ControllerCurrency::BehindMinor => {
            let releases = minor_releases_behind.unwrap_or(1);
            let severity = if escalated { "STALE: " } else { "" };
            format!(
                "{severity}{} is {releases} minor release(s) behind v{latest} — run `{REMEDIATION_COMMAND}`",
                identity.display
            )
        }
        ControllerCurrency::BehindMajor => format!(
            "STALE: {} is a major release behind v{latest} — run `{REMEDIATION_COMMAND}`",
            identity.display
        ),
        ControllerCurrency::Unknown => format!(
            "controller freshness not established for {}",
            identity.display
        ),
    }
}

fn unknown(
    identity: &BuildIdentity,
    latest_version: Option<String>,
    checked_at: Option<u64>,
    now: u64,
    detail: impl Into<String>,
) -> ControllerStaleness {
    ControllerStaleness {
        status: ControllerCurrency::Unknown,
        stale: false,
        escalated: false,
        running_version: identity.version.clone(),
        build_identity: identity.display.clone(),
        git_commit: identity.git_commit.clone(),
        latest_version,
        minor_releases_behind: None,
        checked_at,
        cache_age_secs: checked_at.map(|at| age_secs(at, now)),
        detail: detail.into(),
        remediation: None,
    }
}

/// Saturating age so a clock that moved backwards reports `0` rather than
/// wrapping into a nonsense age.
fn age_secs(checked_at: u64, now: u64) -> u64 {
    now.saturating_sub(checked_at)
}

fn normalize_version(version: &str) -> String {
    version.trim().trim_start_matches('v').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(version: &str, commit: Option<&str>) -> BuildIdentity {
        let display = match commit {
            Some(commit) => format!("homeboy {version}+{commit}"),
            None => format!("homeboy {version}"),
        };
        BuildIdentity {
            version: version.to_string(),
            git_commit: commit.map(str::to_string),
            git_dirty: None,
            display,
        }
    }

    /// The exact scenario in #11483: 0.327.0 running against a published
    /// v0.329.1 is two minor releases behind and must escalate.
    #[test]
    fn reported_issue_scenario_is_stale_and_escalated() {
        let staleness = assess(
            &identity("0.327.0", Some("ed33954781a9")),
            Some("0.329.1"),
            Some(1_000),
            1_100,
        );

        assert_eq!(staleness.status, ControllerCurrency::BehindMinor);
        assert!(staleness.stale);
        assert!(staleness.escalated);
        assert_eq!(staleness.minor_releases_behind, Some(2));
        assert_eq!(staleness.latest_version.as_deref(), Some("0.329.1"));
        assert_eq!(staleness.git_commit.as_deref(), Some("ed33954781a9"));
        assert_eq!(staleness.remediation.as_deref(), Some("homeboy upgrade"));
        assert_eq!(staleness.cache_age_secs, Some(100));

        let warning = staleness.warning_line().expect("stale controller warns");
        assert!(warning.contains("STALE"));
        assert!(warning.contains("2 minor release(s) behind v0.329.1"));
        // The embedded commit rides along so an operator with a checkout can
        // compute the commit delta the release delta cannot express.
        assert!(warning.contains("ed33954781a9"));
        assert!(warning.contains("homeboy upgrade"));
    }

    #[test]
    fn matching_version_is_current_and_silent() {
        let staleness = assess(&identity("0.329.1", None), Some("0.329.1"), Some(10), 20);

        assert_eq!(staleness.status, ControllerCurrency::Current);
        assert!(!staleness.stale);
        assert!(!staleness.escalated);
        assert_eq!(staleness.minor_releases_behind, Some(0));
        assert!(staleness.remediation.is_none());
        assert!(staleness.warning_line().is_none());
    }

    /// A `v` prefix on the published tag is the shape GitHub returns; it must
    /// not defeat the comparison.
    #[test]
    fn v_prefixed_release_tag_compares() {
        let staleness = assess(&identity("0.329.1", None), Some("v0.329.1"), None, 0);

        assert_eq!(staleness.status, ControllerCurrency::Current);
        assert_eq!(staleness.latest_version.as_deref(), Some("0.329.1"));
    }

    #[test]
    fn one_minor_behind_is_stale_but_not_escalated() {
        let staleness = assess(&identity("0.328.0", None), Some("0.329.1"), None, 0);

        assert_eq!(staleness.status, ControllerCurrency::BehindMinor);
        assert!(staleness.stale);
        assert!(!staleness.escalated);
        assert_eq!(staleness.minor_releases_behind, Some(1));
        let warning = staleness.warning_line().expect("stale controller warns");
        assert!(!warning.contains("STALE"));
        assert!(warning.contains("1 minor release(s) behind"));
    }

    #[test]
    fn patch_behind_is_stale_but_not_escalated() {
        let staleness = assess(&identity("0.329.0", None), Some("0.329.1"), None, 0);

        assert_eq!(staleness.status, ControllerCurrency::BehindPatch);
        assert!(staleness.stale);
        assert!(!staleness.escalated);
        assert_eq!(staleness.minor_releases_behind, Some(0));
    }

    #[test]
    fn major_behind_escalates_without_a_minor_count() {
        let staleness = assess(&identity("0.329.1", None), Some("1.0.0"), None, 0);

        assert_eq!(staleness.status, ControllerCurrency::BehindMajor);
        assert!(staleness.stale);
        assert!(staleness.escalated);
        // A minor delta across majors is not a distance an operator can act on.
        assert!(staleness.minor_releases_behind.is_none());
    }

    /// A source build on an unreleased branch is ahead, not stale. Warning on it
    /// would train operators to ignore this surface.
    #[test]
    fn newer_than_published_release_is_ahead_not_stale() {
        let staleness = assess(
            &identity("0.330.0", Some("abc123")),
            Some("0.329.1"),
            None,
            0,
        );

        assert_eq!(staleness.status, ControllerCurrency::Ahead);
        assert!(!staleness.stale);
        assert!(!staleness.escalated);
        assert!(staleness.warning_line().is_none());
        assert!(staleness.detail.contains("ahead"));
    }

    /// Offline, first run, or a wiped cache: no published release to compare
    /// against. Unknown, never "current", never a warning.
    #[test]
    fn missing_cached_release_is_unknown_not_current() {
        let staleness = assess(&identity("0.327.0", None), None, None, 0);

        assert_eq!(staleness.status, ControllerCurrency::Unknown);
        assert!(!staleness.stale);
        assert!(!staleness.escalated);
        assert!(staleness.latest_version.is_none());
        assert!(staleness.warning_line().is_none());
        assert!(staleness.detail.contains("not established"));
    }

    #[test]
    fn empty_cached_release_is_unknown() {
        let staleness = assess(&identity("0.327.0", None), Some("   "), Some(5), 5);

        assert_eq!(staleness.status, ControllerCurrency::Unknown);
        assert!(staleness.latest_version.is_none());
    }

    #[test]
    fn unparseable_version_is_unknown_not_current() {
        let staleness = assess(
            &identity("not-a-version", None),
            Some("0.329.1"),
            Some(7),
            9,
        );

        assert_eq!(staleness.status, ControllerCurrency::Unknown);
        assert!(!staleness.stale);
        assert_eq!(staleness.latest_version.as_deref(), Some("0.329.1"));
        assert_eq!(staleness.checked_at, Some(7));
        assert_eq!(staleness.cache_age_secs, Some(2));
    }

    /// The cache age is how a reader tells a fresh verdict from one made
    /// against a stale release list on an offline host.
    #[test]
    fn cache_age_is_reported_and_survives_backwards_clocks() {
        let fresh = assess(&identity("0.329.1", None), Some("0.329.1"), Some(100), 400);
        assert_eq!(fresh.cache_age_secs, Some(300));

        let skewed = assess(&identity("0.329.1", None), Some("0.329.1"), Some(400), 100);
        assert_eq!(skewed.cache_age_secs, Some(0));
    }

    #[test]
    fn serialized_shape_is_stable() {
        let staleness = assess(
            &identity("0.327.0", Some("ed33954781a9")),
            Some("0.329.1"),
            Some(1_000),
            1_100,
        );
        let json = serde_json::to_value(&staleness).expect("staleness serializes");

        assert_eq!(json["status"], "behind_minor");
        assert_eq!(json["stale"], true);
        assert_eq!(json["escalated"], true);
        assert_eq!(json["running_version"], "0.327.0");
        assert_eq!(json["build_identity"], "homeboy 0.327.0+ed33954781a9");
        assert_eq!(json["git_commit"], "ed33954781a9");
        assert_eq!(json["latest_version"], "0.329.1");
        assert_eq!(json["minor_releases_behind"], 2);
        assert_eq!(json["remediation"], "homeboy upgrade");
    }

    /// A current controller emits no remediation and no optional drift fields,
    /// so a JSON consumer can distinguish "current" from "unknown" by `status`
    /// alone.
    #[test]
    fn current_controller_omits_remediation() {
        let staleness = assess(&identity("0.329.1", None), Some("0.329.1"), None, 0);
        let json = serde_json::to_value(&staleness).expect("staleness serializes");

        assert_eq!(json["status"], "current");
        assert!(json.get("remediation").is_none());
        assert!(json.get("git_commit").is_none());
    }

    #[test]
    fn currency_behind_predicate_excludes_unknown_and_ahead() {
        assert!(ControllerCurrency::BehindPatch.is_behind());
        assert!(ControllerCurrency::BehindMinor.is_behind());
        assert!(ControllerCurrency::BehindMajor.is_behind());
        assert!(!ControllerCurrency::Current.is_behind());
        assert!(!ControllerCurrency::Ahead.is_behind());
        assert!(!ControllerCurrency::Unknown.is_behind());
    }

    /// The escalation threshold is the issue's stated rule: *more than* one
    /// minor version behind.
    #[test]
    fn escalation_threshold_is_more_than_one_minor() {
        assert_eq!(ESCALATION_MINOR_RELEASES_BEHIND, 2);
        assert!(!assess(&identity("0.329.0", None), Some("0.330.0"), None, 0).escalated);
        assert!(assess(&identity("0.328.0", None), Some("0.330.0"), None, 0).escalated);
    }

    /// `current()` must never panic or perform network I/O, whatever the host
    /// state. Its verdict depends on the ambient cache, so only its totality and
    /// its self-consistency are asserted here.
    #[test]
    fn current_is_total_and_self_consistent() {
        let staleness = current();

        assert_eq!(staleness.running_version, build_identity::current().version);
        assert_eq!(staleness.stale, staleness.status.is_behind());
        assert_eq!(staleness.stale, staleness.remediation.is_some());
        assert!(!staleness.detail.is_empty());
    }
}
