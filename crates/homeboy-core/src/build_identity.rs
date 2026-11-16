pub use homeboy_product_identity::BuildIdentity;

use crate::error::{Error, ErrorCode, Hint};

pub fn current() -> BuildIdentity {
    homeboy_product_identity::build_identity()
}

/// Field name the runner reports for a provenance reference refusal.
pub const PROVENANCE_TRANSPORT_FIELD: &str = "provenance_transport";

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
    fn identical_or_unrelated_builds_are_not_skew() {
        assert_eq!(
            ProvenanceBuildSkew::between("homeboy 1+a", " homeboy 1+a "),
            None
        );
        assert_eq!(ProvenanceBuildSkew::parse("job not found"), None);
    }
}
