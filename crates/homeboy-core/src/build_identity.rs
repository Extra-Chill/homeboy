pub use homeboy_product_identity::BuildIdentity;

use crate::error::{Error, ErrorCode, Hint};

pub fn current() -> BuildIdentity {
    homeboy_product_identity::build_identity()
}

/// Field name the runner reports for a provenance reference refusal.
pub const PROVENANCE_TRANSPORT_FIELD: &str = "provenance_transport";

/// First Homeboy release whose child process resolves
/// `homeboy/provenance-reference/v1` references (`observation::resolve_json_value`).
/// Any child at or after this release reads the references a runner stages,
/// whatever build the runner itself is on (#18376).
pub const PROVENANCE_REFERENCE_MIN_VERSION: (u64, u64, u64) = (0, 310, 0);

/// Parse a build identity of the form `[homeboy ]<version>[+<commit>[-dirty]]`
/// (the format `homeboy_product_identity` emits), a bare version, or a
/// `v`-prefixed version. The last whitespace-delimited token is the identity, so
/// a leading product name is tolerated without hard-coding it.
///
/// Returns `None` when the version is not semver, so callers stay on their safe
/// unverifiable path rather than pretend to parse.
pub fn parse(identity: &str) -> Option<BuildIdentity> {
    let display = identity.trim();
    let token = display.split_whitespace().last()?;
    let (version, commit, dirty) = match token.split_once('+') {
        Some((version, remainder)) => {
            let (commit, dirty) = match remainder.strip_suffix("-dirty") {
                Some(commit) => (commit, true),
                None => (remainder, false),
            };
            // A `+` with no commit yields no verifiable commit identity.
            let commit = (!commit.is_empty()).then(|| commit.to_string());
            let dirty = commit.as_ref().map(|_| dirty);
            (version, commit, dirty)
        }
        None => (token, None, None),
    };
    semantic_version(version)?;
    Some(BuildIdentity {
        display: display.to_string(),
        version: version.to_string(),
        git_commit: commit,
        git_dirty: dirty,
    })
}

fn semantic_version(version: &str) -> Option<semver::Version> {
    semver::Version::parse(version.trim_start_matches('v')).ok()
}

/// The `(major, minor, patch)` release of a build identity such as
/// `homeboy 0.470.0+c463cb25` or a bare `0.470.0`. Pre-release and build
/// suffixes do not participate.
pub fn build_version(build: &str) -> Option<(u64, u64, u64)> {
    let identity = parse(build)?;
    let version = semantic_version(&identity.version)?;
    Some((version.major, version.minor, version.patch))
}

/// Release ordering of two build identities, or `None` when either cannot be
/// parsed. Commits and pre-release suffixes do not participate.
pub fn compare(left: &str, right: &str) -> Option<std::cmp::Ordering> {
    Some(build_version(left)?.cmp(&build_version(right)?))
}

/// Whether `candidate` is a strictly newer release than `current`. An
/// unparseable identity is never newer.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    compare(candidate, current) == Some(std::cmp::Ordering::Greater)
}

/// The canonical text of a build identity: trimmed, without the `homeboy `
/// product prefix, so `homeboy 0.1.0+abc` and `0.1.0+abc` compare equal.
pub fn canonical(identity: &str) -> &str {
    let identity = identity.trim();
    identity.strip_prefix("homeboy ").unwrap_or(identity)
}

/// Whether two build identities name the same exact build.
pub fn same_build(left: &str, right: &str) -> bool {
    canonical(left) == canonical(right)
}

/// Whether a child on `build` reads provenance references. An identity whose
/// version cannot be read is treated as unsupported, so the runner never hands
/// references to a child that might not resolve them.
pub fn supports_provenance_references(build: &str) -> bool {
    build_version(build).is_some_and(|version| version >= PROVENANCE_REFERENCE_MIN_VERSION)
}

const CHILD_BUILD_PREFIX: &str = "child build `";
const RUNNER_BUILD_INFIX: &str = "` does not support provenance references from runner build `";

/// The runner daemon and the Homeboy binary it dispatches a job to are on
/// different builds, so oversized provenance cannot be handed to the child by
/// reference (#18206).
///
/// This is the one producer and parser of that refusal: the runner raises it
/// as a typed `runner.version_skew` error, and `agent-task diagnose` recovers
/// it from records written before the error was typed, where it only survived
/// as text behind an `internal.unexpected` continuation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvenanceBuildSkew {
    /// Build identity of the Homeboy binary the job runs (the admitted build).
    pub child_build: String,
    /// Build identity of the runner process that stages provenance.
    pub runner_build: String,
}

impl ProvenanceBuildSkew {
    pub fn new(child_build: impl Into<String>, runner_build: impl Into<String>) -> Self {
        Self {
            child_build: child_build.into(),
            runner_build: runner_build.into(),
        }
    }

    /// The skew between `child_build` and `runner_build`, when they differ.
    pub fn between(child_build: &str, runner_build: &str) -> Option<Self> {
        let (child, runner) = (child_build.trim(), runner_build.trim());
        (!child.is_empty() && !runner.is_empty() && child != runner)
            .then(|| Self::new(child, runner))
    }

    /// The skew between `child_build` and `runner_build` only when the child
    /// cannot read the provenance references the runner stages.
    ///
    /// Controller and runner upgrade independently and routinely sit a release
    /// or two apart; the reference format is versioned by its schema, not by
    /// the build, so ordinary release churn in either direction is compatible
    /// and must not fail a Cook (#18376).
    pub fn incompatible(child_build: &str, runner_build: &str) -> Option<Self> {
        Self::between(child_build, runner_build)
            .filter(|skew| !supports_provenance_references(&skew.child_build))
    }

    /// The refusal text. Kept stable so records written by older builds and by
    /// this one parse identically.
    pub fn problem(&self) -> String {
        format!(
            "cannot safely bound child provenance environment: {CHILD_BUILD_PREFIX}{}{RUNNER_BUILD_INFIX}{}`",
            self.child_build, self.runner_build
        )
    }

    /// Recover the skew from any text that carries the runner's refusal.
    pub fn parse(text: &str) -> Option<Self> {
        let start = text.find(CHILD_BUILD_PREFIX)? + CHILD_BUILD_PREFIX.len();
        let rest = &text[start..];
        let infix = rest.find(RUNNER_BUILD_INFIX)?;
        let child = &rest[..infix];
        let rest = &rest[infix + RUNNER_BUILD_INFIX.len()..];
        let runner = &rest[..rest.find('`')?];
        Self::between(child, runner)
    }

    /// Commands that converge the runner, in the order an operator runs them.
    pub fn convergence_commands(&self, runner_id: Option<&str>) -> Vec<String> {
        let runner = runner_id.unwrap_or("<runner>");
        vec![
            format!("homeboy runner refresh-homeboy {runner}"),
            format!("homeboy runner doctor {runner} --repair"),
        ]
    }

    /// The operator-facing next action naming both builds and the remedy.
    pub fn next_action(&self, runner_id: Option<&str>) -> String {
        let commands = self.convergence_commands(runner_id);
        format!(
            "Runner job binary is Homeboy `{}` but the runner daemon is Homeboy `{}`. Converge the runner on this controller's build with `{}`, restart its daemon with `{}`, then retry.",
            self.child_build, self.runner_build, commands[0], commands[1]
        )
    }

    /// The typed `runner.version_skew` error for this refusal.
    pub fn error(&self, runner_id: Option<&str>) -> Error {
        let mut error = Error::new(
            ErrorCode::RunnerVersionSkew,
            format!(
                "Invalid argument '{PROVENANCE_TRANSPORT_FIELD}': {}",
                self.problem()
            ),
            serde_json::json!({
                "field": PROVENANCE_TRANSPORT_FIELD,
                "problem": self.problem(),
                "runner_id": runner_id,
                "child_build_identity": self.child_build,
                "runner_build_identity": self.runner_build,
                "convergence_commands": self.convergence_commands(runner_id),
                "next_action": self.next_action(runner_id),
            }),
        );
        error.hints.push(Hint {
            message: self.next_action(runner_id),
        });
        error
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_package_version_does_not_override_product_build_version() {
        assert_eq!(env!("CARGO_PKG_VERSION"), "0.1.0");
        assert_eq!(
            current().version,
            homeboy_product_identity::product_version()
        );
        assert_ne!(current().version, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn provenance_build_skew_round_trips_through_the_reported_text() {
        let legacy = "Invalid argument 'provenance_transport': cannot safely bound child provenance environment: child build `homeboy 0.465.1+3bc87452` does not support provenance references from runner build `homeboy 0.465.6+4b7c87df`";
        let skew = ProvenanceBuildSkew::parse(legacy).expect("legacy text parses");
        assert_eq!(skew.child_build, "homeboy 0.465.1+3bc87452");
        assert_eq!(skew.runner_build, "homeboy 0.465.6+4b7c87df");
        assert_eq!(
            ProvenanceBuildSkew::parse(&skew.error(None).message),
            Some(skew)
        );
    }

    #[test]
    fn provenance_build_skew_error_is_typed_and_names_both_builds_and_the_remedy() {
        let skew = ProvenanceBuildSkew::new("homeboy 0.465.1+a", "homeboy 0.465.6+b");
        let error = skew.error(Some("homeboy-lab"));
        assert_eq!(error.code.as_str(), "runner.version_skew");
        assert_eq!(error.details["child_build_identity"], "homeboy 0.465.1+a");
        assert_eq!(error.details["runner_build_identity"], "homeboy 0.465.6+b");
        let next = error.details["next_action"].as_str().expect("next action");
        assert!(next.contains("homeboy 0.465.1+a") && next.contains("homeboy 0.465.6+b"));
        assert!(next.contains("homeboy runner refresh-homeboy homeboy-lab"));
        assert!(next.contains("homeboy runner doctor homeboy-lab --repair"));
        assert_eq!(error.hints.len(), 1);
    }

    #[test]
    fn one_release_skew_in_either_direction_is_compatible() {
        // #18376: the exact pair that failed agent-task-cdc726f5.
        let older_child = ("homeboy 0.469.6+82c2f73e", "homeboy 0.470.0+c463cb25");
        assert!(ProvenanceBuildSkew::between(older_child.0, older_child.1).is_some());
        assert_eq!(
            ProvenanceBuildSkew::incompatible(older_child.0, older_child.1),
            None
        );
        // And the reverse: child newer than the runner daemon.
        assert_eq!(
            ProvenanceBuildSkew::incompatible("homeboy 0.470.4+b", "homeboy 0.470.3+a"),
            None
        );
        assert_eq!(
            ProvenanceBuildSkew::incompatible("homeboy 0.470.3-dirty", "homeboy 0.470.4"),
            None
        );
    }

    #[test]
    fn child_without_reference_support_is_incompatible() {
        let skew = ProvenanceBuildSkew::incompatible("homeboy 0.309.9+a", "homeboy 0.470.0+b")
            .expect("pre-reference child is incompatible");
        assert_eq!(skew.child_build, "homeboy 0.309.9+a");
        assert!(
            ProvenanceBuildSkew::incompatible("homeboy older-build", "homeboy 1.0.0").is_some()
        );
        assert_eq!(
            ProvenanceBuildSkew::incompatible("homeboy 0.310.0+a", "homeboy 0.470.0+b"),
            None
        );
    }

    #[test]
    fn build_identities_parse_order_and_compare_from_one_parser() {
        // (input, version, commit, dirty, release)
        let cases: &[(
            &str,
            Option<(&str, Option<&str>, Option<bool>, (u64, u64, u64))>,
        )] = &[
            ("0.482.0", Some(("0.482.0", None, None, (0, 482, 0)))),
            (
                "homeboy 0.482.0+4a57291e",
                Some(("0.482.0", Some("4a57291e"), Some(false), (0, 482, 0))),
            ),
            (
                " homeboy 0.482.0+4a57291e-dirty ",
                Some(("0.482.0", Some("4a57291e"), Some(true), (0, 482, 0))),
            ),
            (
                "homeboy 0.482.0+",
                Some(("0.482.0", None, None, (0, 482, 0))),
            ),
            ("v0.482.0", Some(("v0.482.0", None, None, (0, 482, 0)))),
            (
                "homeboy v0.482.1+abc",
                Some(("v0.482.1", Some("abc"), Some(false), (0, 482, 1))),
            ),
            (
                "homeboy 0.470.3-dirty",
                Some(("0.470.3-dirty", None, None, (0, 470, 3))),
            ),
            ("homeboy not-a-version", None),
            ("homeboy older-build", None),
            ("homeboy 1.2", None),
            ("", None),
        ];
        for (input, expected) in cases {
            let parsed = parse(input);
            assert_eq!(
                parsed.as_ref().map(|identity| (
                    identity.version.as_str(),
                    identity.git_commit.as_deref(),
                    identity.git_dirty,
                )),
                expected.map(|(version, commit, dirty, _)| (version, commit, dirty)),
                "parse({input:?})"
            );
            assert_eq!(
                build_version(input),
                expected.map(|(.., release)| release),
                "build_version({input:?})"
            );
        }
        assert_eq!(
            parse("homeboy 0.482.0+4a57291e").map(|identity| identity.display),
            Some("homeboy 0.482.0+4a57291e".to_string())
        );

        assert!(is_newer("homeboy 0.482.1+b", "0.482.0"));
        assert!(is_newer("v1.0.0", "0.99.99"));
        assert!(!is_newer("0.482.0", "homeboy 0.482.0+a"));
        assert!(!is_newer("0.481.9", "0.482.0"));
        assert!(!is_newer("not-a-version", "0.482.0"));
        assert_eq!(
            compare("0.481.0", "0.482.0"),
            Some(std::cmp::Ordering::Less)
        );
        assert_eq!(compare("0.482.0", "garbage"), None);

        assert_eq!(canonical(" homeboy 0.482.0+a "), "0.482.0+a");
        assert!(same_build("homeboy 0.482.0+a", "0.482.0+a"));
        assert!(!same_build("homeboy 0.482.0+a", "homeboy 0.482.0+b"));
        assert!(!same_build("homeboy 0.481.0", "homeboy 0.482.0"));
    }

    #[test]
    fn identical_or_unrelated_builds_are_not_skew() {
        assert_eq!(
            ProvenanceBuildSkew::between("homeboy 1+a", " homeboy 1+a "),
            None
        );
        assert_eq!(ProvenanceBuildSkew::parse("job not found"), None);
    }
}
