//! Formatting-finding extraction and harness-failure classification.

use super::types::FormattingFindings;
use crate::core::extension;
use std::collections::BTreeSet;
use std::path::Path;

pub(crate) fn extract_formatting_findings(
    stdout: &str,
    stderr: &str,
    source_path: &Path,
) -> Option<FormattingFindings> {
    let mut files = BTreeSet::new();
    let mut summary = None;

    for line in stdout.lines().chain(stderr.lines()) {
        let trimmed = line.trim();
        if trimmed.contains("FMT SUMMARY:") {
            summary = Some(trimmed.to_string());
        }
        if let Some(path) = trimmed
            .strip_prefix("Diff in ")
            .and_then(|rest| formatting_diff_path(rest, source_path))
        {
            files.insert(path);
        }
    }

    if files.is_empty() && summary.is_none() {
        return None;
    }

    Some(FormattingFindings {
        files: files.into_iter().collect(),
        summary,
        suggested_command: "cargo fmt".to_string(),
    })
}

fn formatting_diff_path(raw: &str, source_path: &Path) -> Option<String> {
    let candidate = raw
        .split_once(" at line ")
        .map(|(path, _)| path)
        .or_else(|| raw.split_once(":").map(|(path, _)| path))
        .unwrap_or(raw)
        .trim()
        .trim_matches('`')
        .trim_matches('"');
    if candidate.is_empty() {
        return None;
    }

    let path = Path::new(candidate);
    let relative = path
        .strip_prefix(source_path)
        .ok()
        .and_then(|path| path.to_str())
        .unwrap_or(candidate)
        .trim_start_matches("./")
        .to_string();

    (!relative.is_empty()).then_some(relative)
}

/// Decide whether a non-zero lint exit with zero findings is a harness/infra
/// failure rather than a genuine lint failure.
///
/// Treated as harness failure when:
/// - the exit code is `>= 2` (linters conventionally use 1 for "findings
///   exist" and `>= 2` for tooling/internal errors), or
/// - the captured output contains a known infra marker (missing harness
///   wrapper, bootstrap fatals, etc.).
///
/// Exit code 1 with no infra markers is intentionally NOT treated as a harness
/// failure here, so that a self-check tool that legitimately exits 1 on real
/// problems still surfaces. The release self-check path additionally treats a
/// clean linter (zero findings) as a harness signal because its findings are
/// always reported explicitly.
pub(crate) fn self_check_output_is_harness_failure(
    exit_code: i32,
    stdout: &str,
    stderr: &str,
) -> bool {
    if !(0..2).contains(&exit_code) {
        return true;
    }

    let combined = format!("{}\n{}", stdout, stderr).to_lowercase();
    // Core matches only ecosystem-agnostic markers: generic shell/harness
    // wiring failures plus the shared neutral infra markers. Ecosystem-specific
    // failure signatures (interpreter crashes, bootstrap script paths, etc.)
    // must be detected by the extension that owns that ecosystem, not here.
    [
        "runner-steps.sh",
        "no such file or directory",
        "command not found",
    ]
    .iter()
    .chain(extension::GENERIC_INFRASTRUCTURE_FAILURE_MARKERS.iter())
    .any(|needle| combined.contains(needle))
}
