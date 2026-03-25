//! types — extracted from decompose.rs.

    pub strategy: String,
    pub total_items: usize,
    pub groups: Vec<DecomposeGroup>,
    pub projected_audit_impact: DecomposeAuditImpact,
    pub checklist: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecomposeAuditImpact {
    pub estimated_new_files: usize,
    pub estimated_new_test_files: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recommended_test_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub likely_findings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecomposeGroup {
    pub name: String,
    pub suggested_target: String,
    pub item_names: Vec<String>,
}

/// A section header found in source comments (e.g., `// === Models ===`).
#[derive(Debug)]
pub(crate) struct Section {
    name: String,
    start_line: usize,
}
