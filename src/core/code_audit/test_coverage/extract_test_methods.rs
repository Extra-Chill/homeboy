//! extract_test_methods — extracted from test_coverage.rs.

use std::path::Path;
use regex::Regex;
use super::super::test_mapping::{
    build_source_name_index, partition_fingerprints, source_to_test_path, test_to_source_path,
};
use crate::extension::TestMappingConfig;
use std::collections::{HashMap, HashSet};
use super::super::conventions::AuditFinding;
use super::super::findings::{Finding, Severity};
use super::super::fingerprint::FileFingerprint;


/// Load test methods from disk for a known test file path.
///
/// Uses extension fingerprinting when available, with a lightweight regex fallback
/// so singleton test files still contribute method coverage in scoped audits.
pub(crate) fn load_test_methods_from_disk(
    root: &Path,
    test_path: &str,
    config: &TestMappingConfig,
) -> Option<Vec<String>> {
    let abs = root.join(test_path);
    if !abs.exists() {
        return None;
    }

    if let Some(fp) = super::fingerprint::fingerprint_file(&abs, root) {
        if !fp.methods.is_empty() {
            return Some(fp.methods);
        }
    }

    let content = std::fs::read_to_string(&abs).ok()?;
    Some(extract_test_methods_fallback(
        &content,
        test_path,
        &config.method_prefix,
    ))
}

pub(crate) fn extract_test_methods_fallback(
    content: &str,
    test_path: &str,
    method_prefix: &str,
) -> Vec<String> {
    let ext = Path::new(test_path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    let escaped = regex::escape(method_prefix);
    let pattern = match ext {
        "rs" => format!(r"(?m)^\s*fn\s+({}\w*)\s*\(", escaped),
        "php" => format!(r"(?m)^\s*(?:public\s+)?function\s+({}\w*)\s*\(", escaped),
        "js" | "jsx" | "ts" | "tsx" => {
            format!(r"(?m)^\s*(?:async\s+)?function\s+({}\w*)\s*\(", escaped)
        }
        _ => format!(r"(?m)({}\w*)", escaped),
    };

    let re = match Regex::new(&pattern) {
        Ok(re) => re,
        Err(_) => return Vec::new(),
    };

    re.captures_iter(content)
        .filter_map(|cap| cap.get(1).map(|m| m.as_str().to_string()))
        .collect()
}
