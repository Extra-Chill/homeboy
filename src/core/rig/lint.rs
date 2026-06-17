//! Generic rig package lint checks used by `homeboy rig check`.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use super::install::read_source_metadata;
use super::pipeline::{PipelineOutcome, PipelineStepOutcome};
use super::spec::RigSpec;
use crate::core::error::{Error, Result};

const IGNORED_DIRECTORIES: &[&str] = &[
    ".git",
    ".claude",
    ".datamachine",
    ".opencode",
    "node_modules",
    "vendor",
];

pub fn run_package_lint(rig: &RigSpec) -> Result<PipelineOutcome> {
    let Some(root) = package_lint_root(rig) else {
        return Ok(empty_outcome());
    };

    let mut files = Vec::new();
    collect_files(&root, &mut files)?;

    let conflict_failures = conflict_marker_failures(&root, &files)?;
    let json_failures = json_parse_failures(&root, &files)?;
    let template_failures = template_materialization_failures(&root, &files)?;
    let steps = vec![
        aggregate_step(
            "rig-package-lint",
            "rig package has no unresolved conflict markers",
            conflict_failures,
        ),
        aggregate_step(
            "rig-package-lint",
            "rig package JSON specs parse",
            json_failures,
        ),
        aggregate_step(
            "rig-package-lint",
            "rig package template specs materialize",
            template_failures,
        ),
    ];

    Ok(PipelineOutcome {
        name: "check".to_string(),
        passed: steps.iter().filter(|step| step.status == "pass").count(),
        failed: steps.iter().filter(|step| step.status == "fail").count(),
        steps,
    })
}

fn empty_outcome() -> PipelineOutcome {
    PipelineOutcome {
        name: "check".to_string(),
        steps: Vec::new(),
        passed: 0,
        failed: 0,
    }
}

fn package_lint_root(rig: &RigSpec) -> Option<PathBuf> {
    let metadata = read_source_metadata(&rig.id)?;
    metadata
        .discovery_path
        .as_deref()
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(metadata.package_path)))
        .filter(|path| path.is_dir())
}

fn collect_files(root: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    collect_files_inner(root, files, "read rig package directory")
}

fn collect_files_inner(
    directory: &Path,
    files: &mut Vec<PathBuf>,
    context: &'static str,
) -> Result<()> {
    let entries = fs::read_dir(directory)
        .map_err(|error| Error::internal_io(error.to_string(), Some(context.to_string())))?;
    for entry in entries {
        let entry = entry
            .map_err(|error| Error::internal_io(error.to_string(), Some(context.to_string())))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| Error::internal_io(error.to_string(), Some(context.to_string())))?;
        if file_type.is_dir() {
            if !ignored_directory(entry.file_name().as_os_str()) {
                collect_files_inner(&path, files, context)?;
            }
        } else if file_type.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

fn ignored_directory(name: &OsStr) -> bool {
    IGNORED_DIRECTORIES
        .iter()
        .any(|ignored| name == OsStr::new(ignored))
}

fn conflict_marker_failures(root: &Path, files: &[PathBuf]) -> Result<Vec<String>> {
    let mut failures = Vec::new();
    for file in files {
        let content = fs::read_to_string(file).unwrap_or_default();
        for (index, line) in content.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("<<<<<<<")
                || trimmed.starts_with("=======")
                || trimmed.starts_with(">>>>>>>")
            {
                failures.push(format!(
                    "{}:{} unresolved conflict marker: {}",
                    display_relative(root, file),
                    index + 1,
                    line.trim()
                ));
            }
        }
    }
    Ok(failures)
}

fn json_parse_failures(root: &Path, files: &[PathBuf]) -> Result<Vec<String>> {
    let mut failures = Vec::new();
    for file in files
        .iter()
        .filter(|path| path.extension() == Some(OsStr::new("json")))
    {
        let content = fs::read_to_string(file).map_err(|error| {
            Error::internal_io(error.to_string(), Some(format!("read {}", file.display())))
        })?;
        if let Err(error) = serde_json::from_str::<serde_json::Value>(&content) {
            failures.push(format!(
                "{} invalid JSON: {}",
                display_relative(root, file),
                error
            ));
        }
    }
    Ok(failures)
}

fn template_materialization_failures(root: &Path, files: &[PathBuf]) -> Result<Vec<String>> {
    let mut failures = Vec::new();
    for file in files
        .iter()
        .filter(|path| path.file_name() == Some(OsStr::new("rig.json")))
    {
        let content = fs::read_to_string(file).map_err(|error| {
            Error::internal_io(error.to_string(), Some(format!("read {}", file.display())))
        })?;
        if !content.contains("\"extends\"") {
            continue;
        }
        if let Err(error) = super::install::materialize_rig_spec(file, root) {
            failures.push(format!(
                "{} template materialization failed: {}",
                display_relative(root, file),
                error.message
            ));
        }
    }
    Ok(failures)
}

fn aggregate_step(kind: &str, label: &str, failures: Vec<String>) -> PipelineStepOutcome {
    if failures.is_empty() {
        return PipelineStepOutcome {
            kind: kind.to_string(),
            label: label.to_string(),
            status: "pass".to_string(),
            error: None,
        };
    }

    PipelineStepOutcome {
        kind: kind.to_string(),
        label: label.to_string(),
        status: "fail".to_string(),
        error: Some(failures.join("\n")),
    }
}

fn display_relative(root: &Path, file: &Path) -> String {
    file.strip_prefix(root)
        .unwrap_or(file)
        .to_string_lossy()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregate_step_reports_failure_details() {
        let outcome = aggregate_step(
            "rig-package-lint",
            "JSON parses",
            vec!["rig.json invalid".to_string()],
        );
        assert_eq!(outcome.status, "fail");
        assert!(outcome.error.unwrap().contains("rig.json invalid"));
    }

    #[test]
    fn aggregate_step_passes_without_failures() {
        let outcome = aggregate_step("rig-package-lint", "JSON parses", Vec::new());
        assert_eq!(outcome.status, "pass");
        assert_eq!(outcome.error, None);
    }
}
