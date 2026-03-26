//! source_test_file — extracted from decompose.rs.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use crate::extension::{self, ParsedItem};
use crate::Result;
use super::super::move_items::{MoveOptions, MoveResult};
use super::DecomposeGroup;
use super::DecomposeAuditImpact;


pub(crate) fn project_audit_impact(groups: &[DecomposeGroup]) -> DecomposeAuditImpact {
    let mut likely_findings = Vec::new();
    let mut recommended_test_files = Vec::new();

    for group in groups {
        if let Some(test_file) = source_to_test_file(&group.suggested_target) {
            recommended_test_files.push(test_file);
        }

        if group.suggested_target.starts_with("src/commands/")
            && group.suggested_target.ends_with(".rs")
        {
            likely_findings.push(format!(
                "{} may trigger command convention checks (run method + command tests)",
                group.suggested_target
            ));
        }
    }

    if !recommended_test_files.is_empty() {
        likely_findings.push(
            "New src/*.rs targets will need matching tests (autofix handles this)".to_string(),
        );
    }

    DecomposeAuditImpact {
        estimated_new_files: groups.len(),
        estimated_new_test_files: recommended_test_files.len(),
        recommended_test_files,
        likely_findings,
    }
}

pub(crate) fn source_to_test_file(target: &str) -> Option<String> {
    if !target.starts_with("src/") || !target.ends_with(".rs") {
        return None;
    }

    let without_src = target.strip_prefix("src/")?;
    let without_ext = without_src.strip_suffix(".rs")?;
    Some(format!("tests/{}_test.rs", without_ext))
}
