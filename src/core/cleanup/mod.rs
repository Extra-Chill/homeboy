//! Project cleanup system for identifying config drift, stale state, and hygiene issues.
//!
//! This extension provides project-level health checks that only Homeboy can see:
//! 1. Config health - broken paths, dead version targets, unused extensions
//! 2. Future: git staleness, orphan detection, unused registrations

pub mod baseline;
pub mod config;

use serde::Serialize;

use crate::component::Component;
use crate::Result;

/// Summary counts for the cleanup report.
#[derive(Debug, Clone, Serialize)]
pub struct CleanupSummary {
    pub config_issues: usize,
}

/// Result of running cleanup checks on a component.
#[derive(Debug, Clone, Serialize)]
pub struct CleanupResult {
    pub component_id: String,
    pub summary: CleanupSummary,
    pub config_issues: Vec<config::ConfigIssue>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<String>,
}

/// Run all cleanup checks on a component.
pub fn cleanup_component(component_id: &str) -> Result<CleanupResult> {
    let comp = crate::component::resolve_effective(Some(component_id), None, None)?;
    let config_issues = config::check_config(&comp);

    let mut hints = Vec::new();
    if config_issues.is_empty() {
        hints.push("No config issues found.".to_string());
    } else {
        hints.push(format!(
            "{} config issue(s) found. Review and fix with `homeboy component set`.",
            config_issues.len()
        ));
    }

    Ok(CleanupResult {
        component_id: component_id.to_string(),
        summary: CleanupSummary {
            config_issues: config_issues.len(),
        },
        config_issues,
        hints,
    })
}

/// Run cleanup checks across ALL registered components.
pub fn cleanup_all() -> Result<Vec<CleanupResult>> {
    let components: Vec<Component> = crate::component::inventory().unwrap_or_default();
    let mut results = Vec::new();

    for comp in &components {
        let config_issues = config::check_config(comp);
        let issue_count = config_issues.len();

        let mut hints = Vec::new();
        if config_issues.is_empty() {
            hints.push("No config issues found.".to_string());
        } else {
            hints.push(format!(
                "{} config issue(s) found. Review and fix with `homeboy component set`.",
                issue_count
            ));
        }

        results.push(CleanupResult {
            component_id: comp.id.clone(),
            summary: CleanupSummary {
                config_issues: issue_count,
            },
            config_issues,
            hints,
        });
    }

    Ok(results)
}
