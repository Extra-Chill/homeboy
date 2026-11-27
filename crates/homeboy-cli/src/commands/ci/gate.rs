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
    fn classify_maps_baseline_and_head_exits_to_status_and_exit_code() {
        // (baseline exit, head exit, status, passed, process exit code)
        let cases = [
            // A red baseline is inconclusive, never a PR-head regression.
            (1, 0, "baseline_red", true, 0),
            (1, 3, "baseline_red", true, 0),
            (0, 1, "failed", false, 1),
            // A failing head propagates its own exit code.
            (0, 2, "failed", false, 2),
            (0, 0, "passed", true, 0),
        ];

        for (baseline_exit, head_exit, status, passed, exit_code) in cases {
            let decision = classify(input(baseline_exit, head_exit));
            let case = format!("baseline={baseline_exit} head={head_exit}");
            assert_eq!(decision.status, status, "{case}");
            assert_eq!(decision.passed, passed, "{case}");
            assert_eq!(decision.exit_code(), exit_code, "{case}");
            assert_eq!(decision.baseline.passed, baseline_exit == 0, "{case}");
            assert_eq!(decision.head.passed, head_exit == 0, "{case}");
        }
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
                evidence: vec!["/runner-temp/homeboy-ci-results/test.log".to_string()],
            },
        }
    }
}
