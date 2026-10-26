//! Formatting-finding extraction and harness-failure classification.

use super::types::FormattingFindings;
use homeboy_extension_contract::GENERIC_INFRASTRUCTURE_FAILURE_MARKERS;
use std::collections::BTreeSet;
use std::path::Path;

pub(crate) fn extract_formatting_findings(
    findings: &[homeboy_finding::HomeboyFinding],
    source_path: &Path,
) -> Option<FormattingFindings> {
    let files: BTreeSet<_> = findings
        .iter()
        .filter(|finding| finding.category.as_deref() == Some("format"))
        .filter_map(|finding| finding.location.file.as_deref())
        .map(|file| {
            Path::new(file)
                .strip_prefix(source_path)
                .unwrap_or(Path::new(file))
                .to_string_lossy()
                .trim_start_matches("./")
                .to_string()
        })
        .collect();
    (!files.is_empty()).then(|| FormattingFindings {
        files: files.into_iter().collect(),
    })
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
    .chain(GENERIC_INFRASTRUCTURE_FAILURE_MARKERS.iter())
    .any(|needle| combined.contains(needle))
}
