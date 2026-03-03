use clap::Args;
use serde::Serialize;

use homeboy::cleanup::{self, CleanupResult};

use super::CmdResult;

#[derive(Args)]
pub struct CleanupArgs {
    /// Component to check (omit for all components)
    pub component_id: Option<String>,

    /// Show only issues of a specific severity: error, warning, info
    #[arg(long)]
    pub severity: Option<String>,

    /// Show only issues in a specific category: local_path, remote_path, version_targets, extensions
    #[arg(long)]
    pub category: Option<String>,
}

#[derive(Serialize)]
pub struct CleanupOutput {
    pub command: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component_id: Option<String>,
    pub total_issues: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<CleanupResult>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub results: Vec<CleanupResult>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<String>,
}

pub fn run(args: CleanupArgs, _global: &super::GlobalArgs) -> CmdResult<CleanupOutput> {
    let severity_filter = args.severity.as_deref();
    let category_filter = args.category.as_deref();

    if let Some(ref component_id) = args.component_id {
        // Single component mode
        let mut result = cleanup::cleanup_component(component_id)?;

        // Apply filters
        filter_issues(&mut result, severity_filter, category_filter);

        let total_issues = result.summary.config_issues;

        Ok((
            CleanupOutput {
                command: "cleanup",
                component_id: Some(component_id.clone()),
                total_issues,
                result: Some(result),
                results: Vec::new(),
                hints: vec!["Full docs: homeboy docs commands/cleanup".to_string()],
            },
            0,
        ))
    } else {
        // All components mode
        let mut results = cleanup::cleanup_all()?;

        // Apply filters to each result
        for result in &mut results {
            filter_issues(result, severity_filter, category_filter);
        }

        let total_issues: usize = results.iter().map(|r| r.summary.config_issues).sum();

        let mut hints = Vec::new();
        if total_issues == 0 {
            hints.push("All components passed config health checks.".to_string());
        } else {
            hints.push(format!(
                "{} total issue(s) across {} component(s).",
                total_issues,
                results
                    .iter()
                    .filter(|r| r.summary.config_issues > 0)
                    .count()
            ));
        }
        hints.push("Full docs: homeboy docs commands/cleanup".to_string());

        Ok((
            CleanupOutput {
                command: "cleanup",
                component_id: None,
                total_issues,
                result: None,
                results,
                hints,
            },
            0,
        ))
    }
}

/// Filter issues in a CleanupResult by severity and/or category.
fn filter_issues(
    result: &mut CleanupResult,
    severity_filter: Option<&str>,
    category_filter: Option<&str>,
) {
    if severity_filter.is_none() && category_filter.is_none() {
        return;
    }

    result.config_issues.retain(|issue| {
        let severity_match = match severity_filter {
            Some("error") => issue.severity == cleanup::config::IssueSeverity::Error,
            Some("warning") => issue.severity == cleanup::config::IssueSeverity::Warning,
            Some("info") => issue.severity == cleanup::config::IssueSeverity::Info,
            _ => true,
        };

        let category_match = match category_filter {
            Some(cat) => issue.category == cat,
            None => true,
        };

        severity_match && category_match
    });

    // Update summary after filtering
    result.summary.config_issues = result.config_issues.len();
}
