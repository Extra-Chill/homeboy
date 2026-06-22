use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DifferentialGateInput {
    pub baseline: DifferentialGateSide,
    pub head: DifferentialGateSide,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DifferentialGateSide {
    pub command: String,
    pub exit_code: i32,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DifferentialGateDecision {
    pub status: String,
    pub passed: bool,
    pub conclusion: String,
    pub baseline: DifferentialGateSideOutput,
    pub head: DifferentialGateSideOutput,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DifferentialGateSideOutput {
    pub command: String,
    pub exit_code: i32,
    pub passed: bool,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub evidence: Vec<String>,
}

impl DifferentialGateDecision {
    pub fn exit_code(&self) -> i32 {
        if self.passed {
            0
        } else if self.head.exit_code != 0 {
            self.head.exit_code
        } else {
            1
        }
    }
}

pub fn classify(input: DifferentialGateInput) -> DifferentialGateDecision {
    let baseline = side_output(input.baseline);
    let head = side_output(input.head);

    let (status, passed, conclusion) = if !baseline.passed {
        (
            "baseline_red",
            true,
            "baseline failed before differential comparison; treat this gate as inconclusive instead of a PR-head regression",
        )
    } else if !head.passed {
        (
            "failed",
            false,
            "candidate failed while baseline passed; treat this as a PR-head regression",
        )
    } else {
        (
            "passed",
            true,
            "baseline and candidate passed; no differential regression detected",
        )
    };

    DifferentialGateDecision {
        status: status.to_string(),
        passed,
        conclusion: conclusion.to_string(),
        baseline,
        head,
    }
}

fn side_output(side: DifferentialGateSide) -> DifferentialGateSideOutput {
    DifferentialGateSideOutput {
        passed: side.exit_code == 0,
        command: side.command,
        exit_code: side.exit_code,
        evidence: side.evidence,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_failure_is_inconclusive_success() {
        let decision = classify(input(1, 0));

        assert_eq!(decision.status, "baseline_red");
        assert!(decision.passed);
        assert_eq!(decision.exit_code(), 0);
        assert!(!decision.baseline.passed);
        assert!(decision.head.passed);
    }

    #[test]
    fn candidate_failure_after_green_baseline_is_regression() {
        let decision = classify(input(0, 1));

        assert_eq!(decision.status, "failed");
        assert!(!decision.passed);
        assert_eq!(decision.exit_code(), 1);
        assert!(decision.baseline.passed);
        assert!(!decision.head.passed);
    }

    #[test]
    fn green_baseline_and_head_pass() {
        let decision = classify(input(0, 0));

        assert_eq!(decision.status, "passed");
        assert!(decision.passed);
        assert_eq!(decision.exit_code(), 0);
    }

    fn input(baseline_exit_code: i32, head_exit_code: i32) -> DifferentialGateInput {
        DifferentialGateInput {
            baseline: DifferentialGateSide {
                command: "cargo fmt --check".to_string(),
                exit_code: baseline_exit_code,
                evidence: vec!["FMT SUMMARY: 7 files need formatting".to_string()],
            },
            head: DifferentialGateSide {
                command: "homeboy test homeboy".to_string(),
                exit_code: head_exit_code,
                evidence: vec!["homeboy-ci-results/test.log".to_string()],
            },
        }
    }
}
