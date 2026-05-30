use crate::core::code_audit::CodeAuditResult;
use std::path::PathBuf;

/// Name of the env var pointing to previous command output files.
///
/// When set, `--from audit` reads the cached audit result instead of
/// re-running the audit. The action sets this during `run-homeboy-commands.sh`
/// and it persists across steps via `GITHUB_ENV`.
pub(super) const OUTPUT_DIR_ENV: &str = "HOMEBOY_OUTPUT_DIR";

/// Try to load a cached audit result from a previous `homeboy audit` run.
///
/// Checks `HOMEBOY_OUTPUT_DIR/audit.json` for a `CliResponse<CodeAuditResult>`
/// envelope. If found and parseable, returns the `CodeAuditResult` without
/// re-running the audit. This avoids redundant full-codebase scans when the
/// refactor step runs after an audit gate that already produced the results.
///
/// Returns `None` if:
/// - `HOMEBOY_OUTPUT_DIR` is not set
/// - The file doesn't exist
/// - The file can't be parsed (e.g. the audit failed and wrote an error envelope)
pub(super) fn try_load_cached_audit() -> Option<CodeAuditResult> {
    let output_dir = std::env::var(OUTPUT_DIR_ENV).ok()?;
    let audit_file = PathBuf::from(&output_dir).join("audit.json");

    let content = std::fs::read_to_string(&audit_file).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;

    // Only use cached results from successful runs
    if !json.get("success")?.as_bool()? {
        return None;
    }

    // The `--output` envelope wraps the audit in a `data` field
    let data = json.get("data")?;
    let result: CodeAuditResult = serde_json::from_value(data.clone()).ok()?;

    crate::log_status!(
        "refactor",
        "Using cached audit result ({} findings from {})",
        result.findings.len(),
        audit_file.display()
    );

    Some(result)
}

/// Try to load cached lint findings from a previous `homeboy lint` run.
///
/// Checks `HOMEBOY_OUTPUT_DIR/lint.json` for a `CliResponse<LintCommandOutput>`
/// envelope. If found and the run passed (zero findings), returns `Clean`
/// — the fix stage can be skipped entirely.
///
/// If findings exist, returns `HasFindings(count)` so the fix stage knows
/// to invoke fix-only mode without re-running the diagnostic pass.
pub(super) fn try_load_cached_lint() -> Option<CachedLintResult> {
    let output_dir = std::env::var(OUTPUT_DIR_ENV).ok()?;
    let lint_file = PathBuf::from(&output_dir).join("lint.json");

    let content = std::fs::read_to_string(&lint_file).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;

    let success = json.get("success")?.as_bool()?;
    let data = json.get("data")?;
    let passed = data.get("passed")?.as_bool()?;
    let finding_count = data
        .get("findings")
        .and_then(|f| f.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    if !success {
        return None;
    }

    if passed && finding_count == 0 {
        crate::log_status!(
            "refactor",
            "Cached lint result is clean (0 findings from {}) — skipping lint fix stage",
            lint_file.display()
        );
        return Some(CachedLintResult::Clean);
    }

    if finding_count > 0 {
        crate::log_status!(
            "refactor",
            "Cached lint result has {} findings — fix stage will invoke fix-only mode",
            finding_count
        );
        return Some(CachedLintResult::HasFindings(finding_count));
    }

    None
}

/// Try to load cached test results from a previous `homeboy test` run.
///
/// Same pattern as lint: if the test run passed, skip the test fix stage.
/// If tests failed, return None so the fix stage runs to attempt auto-fixes.
pub(super) fn try_load_cached_test() -> Option<CachedTestResult> {
    let output_dir = std::env::var(OUTPUT_DIR_ENV).ok()?;
    let test_file = PathBuf::from(&output_dir).join("test.json");

    let content = std::fs::read_to_string(&test_file).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;

    let success = json.get("success")?.as_bool()?;
    let data = json.get("data")?;
    let passed = data.get("passed")?.as_bool()?;

    if success && passed {
        crate::log_status!(
            "refactor",
            "Cached test result is clean (passed from {}) — skipping test fix stage",
            test_file.display()
        );
        return Some(CachedTestResult::Clean);
    }

    crate::log_status!(
        "refactor",
        "Cached test result has failures — fix stage will re-run tests with auto-fix"
    );
    None
}

pub(super) enum CachedLintResult {
    /// Lint passed with zero findings — nothing to fix.
    Clean,
    /// Lint had findings — the fix stage should invoke fix-only mode.
    HasFindings(usize),
}

pub(super) enum CachedTestResult {
    /// Tests passed — nothing to fix.
    Clean,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn tmp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("homeboy-refactor-sources-{name}-{nanos}"))
    }

    #[test]
    fn try_load_cached_audit_reads_output_dir() {
        let _guard = env_lock().lock().unwrap();
        std::env::remove_var(OUTPUT_DIR_ENV);
        let dir = tmp_dir("cached-audit");
        fs::create_dir_all(&dir).unwrap();
        let audit_result = CodeAuditResult {
            component_id: "test".to_string(),
            source_path: "/tmp/test".to_string(),
            summary: crate::core::code_audit::AuditSummary {
                files_scanned: 10,
                conventions_detected: 2,
                outliers_found: 1,
                alignment_score: None,
                files_skipped: 0,
                warnings: vec![],
            },
            conventions: vec![],
            directory_conventions: vec![],
            findings: vec![],
            duplicate_groups: vec![],
        };

        // Write a CliResponse envelope
        let envelope = serde_json::json!({
            "success": true,
            "data": audit_result,
        });
        fs::write(
            dir.join("audit.json"),
            serde_json::to_string_pretty(&envelope).unwrap(),
        )
        .unwrap();

        // Set the env var and load
        std::env::set_var(OUTPUT_DIR_ENV, dir.to_string_lossy().as_ref());
        let loaded = try_load_cached_audit();
        std::env::remove_var(OUTPUT_DIR_ENV);

        let loaded = loaded.expect("should load cached audit");
        assert_eq!(loaded.component_id, "test");
        assert_eq!(loaded.summary.files_scanned, 10);

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn try_load_cached_audit_skips_failed_envelope() {
        let _guard = env_lock().lock().unwrap();
        std::env::remove_var(OUTPUT_DIR_ENV);
        let dir = tmp_dir("cached-audit-fail");
        fs::create_dir_all(&dir).unwrap();
        let envelope = serde_json::json!({
            "success": false,
            "error": {
                "code": "internal.io_error",
                "message": "something broke",
                "details": {},
            },
        });
        fs::write(
            dir.join("audit.json"),
            serde_json::to_string_pretty(&envelope).unwrap(),
        )
        .unwrap();

        std::env::set_var(OUTPUT_DIR_ENV, dir.to_string_lossy().as_ref());
        let loaded = try_load_cached_audit();
        std::env::remove_var(OUTPUT_DIR_ENV);

        assert!(loaded.is_none(), "should not use failed audit result");

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn try_load_cached_audit_returns_none_when_unset() {
        let _guard = env_lock().lock().unwrap();
        std::env::remove_var(OUTPUT_DIR_ENV);
        assert!(try_load_cached_audit().is_none());
    }
}
