use serde::Serialize;

use super::JobReadyRunKind;
use crate::core::error::{Error, Result};

#[derive(Debug, Clone, Serialize)]
pub struct SandboxToolDescriptor {
    pub id: &'static str,
    pub command: &'static str,
    pub required_capability: &'static str,
    pub risk: &'static str,
    pub runs_as_job: bool,
    pub allowed_arguments: &'static [&'static str],
}

const SANDBOX_TOOLS: &[SandboxToolDescriptor] = &[
    SandboxToolDescriptor {
        id: "homeboy.audit",
        command: "homeboy audit",
        required_capability: "run:audit",
        risk: "bounded_local_run",
        runs_as_job: true,
        allowed_arguments: &[
            "component",
            "path",
            "json_summary",
            "conventions",
            "only",
            "exclude",
            "changed_since",
            "fixability",
        ],
    },
    SandboxToolDescriptor {
        id: "homeboy.lint",
        command: "homeboy lint",
        required_capability: "run:lint",
        risk: "bounded_local_run",
        runs_as_job: true,
        allowed_arguments: &[
            "component",
            "path",
            "json_summary",
            "summary",
            "file",
            "glob",
            "changed_only",
            "changed_since",
            "errors_only",
            "sniffs",
            "exclude_sniffs",
            "category",
        ],
    },
    SandboxToolDescriptor {
        id: "homeboy.test",
        command: "homeboy test",
        required_capability: "run:test",
        risk: "bounded_local_run",
        runs_as_job: true,
        allowed_arguments: &[
            "component",
            "path",
            "json_summary",
            "skip_lint",
            "coverage",
            "coverage_min",
            "analyze",
            "drift",
            "since",
            "changed_since",
            "args",
        ],
    },
    SandboxToolDescriptor {
        id: "homeboy.bench",
        command: "homeboy bench",
        required_capability: "run:bench",
        risk: "bounded_local_run",
        runs_as_job: true,
        allowed_arguments: &[
            "component",
            "path",
            "json_summary",
            "iterations",
            "warmup",
            "runs",
            "concurrency",
            "rig",
            "scenario",
            "profile",
            "regression_threshold",
            "ignore_default_baseline",
            "args",
        ],
    },
    SandboxToolDescriptor {
        id: "homeboy.fuzz",
        command: "homeboy fuzz",
        required_capability: "run:fuzz",
        risk: "bounded_discovery_report",
        runs_as_job: false,
        allowed_arguments: &[
            "subcommand",
            "contract",
            "list",
            "plan",
            "validate",
            "report",
            "replay",
            "component",
            "rig",
            "extension",
            "setting",
            "workload",
            "run_id",
            "request_id",
            "inventory",
            "results_file",
            "artifact",
            "case_id",
            "output_envelope",
            "envelope_id",
        ],
    },
    SandboxToolDescriptor {
        id: "homeboy.build",
        command: "homeboy build",
        required_capability: "run:build",
        risk: "bounded_local_run",
        runs_as_job: true,
        allowed_arguments: &["component", "component_ids", "path", "all"],
    },
    SandboxToolDescriptor {
        id: "homeboy.review",
        command: "homeboy review",
        required_capability: "run:review",
        risk: "bounded_local_run",
        runs_as_job: true,
        allowed_arguments: &[
            "component",
            "path",
            "changed_since",
            "changed_only",
            "summary",
            "ci_profile",
        ],
    },
];

pub fn all() -> &'static [SandboxToolDescriptor] {
    SANDBOX_TOOLS
}

pub fn get(id: &str) -> Result<&'static SandboxToolDescriptor> {
    SANDBOX_TOOLS
        .iter()
        .find(|tool| tool.id == id)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "tool_id",
                format!("sandbox tool is not allowlisted: {id}"),
                Some(id.to_string()),
                Some(
                    SANDBOX_TOOLS
                        .iter()
                        .map(|tool| tool.id.to_string())
                        .collect(),
                ),
            )
        })
}

pub fn kind(id: &str) -> Result<JobReadyRunKind> {
    match id {
        "homeboy.audit" => Ok(JobReadyRunKind::Audit),
        "homeboy.lint" => Ok(JobReadyRunKind::Lint),
        "homeboy.test" => Ok(JobReadyRunKind::Test),
        "homeboy.bench" => Ok(JobReadyRunKind::Bench),
        "homeboy.build" => Ok(JobReadyRunKind::Build),
        "homeboy.review" => Ok(JobReadyRunKind::Review),
        _ => Err(Error::validation_invalid_argument(
            "tool_id",
            format!("sandbox tool is not executable: {id}"),
            Some(id.to_string()),
            None,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{get, kind};

    #[test]
    fn fuzz_descriptor_exposes_safe_non_job_subcommands() {
        let tool = get("homeboy.fuzz").expect("fuzz descriptor");

        assert_eq!(tool.required_capability, "run:fuzz");
        assert_eq!(tool.risk, "bounded_discovery_report");
        assert!(!tool.runs_as_job);
        for expected in ["contract", "list", "plan", "validate", "report", "replay"] {
            assert!(
                tool.allowed_arguments.contains(&expected),
                "missing fuzz argument {expected}"
            );
        }
        assert!(!tool.allowed_arguments.contains(&"run"));
        assert!(kind(tool.id).is_err());
    }
}
