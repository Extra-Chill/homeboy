use serde::Serialize;

use super::JobReadyRunKind;
use crate::error::{Error, Result};

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
        id: "homeboy.lint",
        command: "homeboy review lint",
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
        command: "homeboy review test",
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
    use super::{all, get, kind};

    #[test]
    fn only_job_tools_are_executable_and_fuzz_stays_report_only() {
        for tool in all() {
            assert_eq!(
                kind(tool.id).is_ok(),
                tool.runs_as_job,
                "{} executability must match runs_as_job",
                tool.id
            );
        }

        let fuzz = get("homeboy.fuzz").expect("fuzz descriptor");
        assert!(!fuzz.allowed_arguments.contains(&"run"));
        assert!(get("homeboy.unknown").is_err());
    }
}
