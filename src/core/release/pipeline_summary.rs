use super::types::{ReleaseRunSummary, ReleaseStepResult, ReleaseStepStatus};

pub(super) fn derive_overall_status(results: &[ReleaseStepResult]) -> ReleaseStepStatus {
    let has_success = results
        .iter()
        .any(|r| matches!(r.status, ReleaseStepStatus::Success));
    let has_failed = results
        .iter()
        .any(|r| matches!(r.status, ReleaseStepStatus::Failed));
    let has_missing = results
        .iter()
        .any(|r| matches!(r.status, ReleaseStepStatus::Missing));

    if (has_failed || has_missing) && has_success {
        ReleaseStepStatus::PartialSuccess
    } else if has_failed {
        ReleaseStepStatus::Failed
    } else if has_missing {
        ReleaseStepStatus::Missing
    } else {
        ReleaseStepStatus::Success
    }
}

pub(super) fn build_summary(
    component_id: &str,
    results: &[ReleaseStepResult],
    status: &ReleaseStepStatus,
) -> ReleaseRunSummary {
    let succeeded = results
        .iter()
        .filter(|r| matches!(r.status, ReleaseStepStatus::Success))
        .count();
    let failed = results
        .iter()
        .filter(|r| matches!(r.status, ReleaseStepStatus::Failed))
        .count();
    let skipped = results
        .iter()
        .filter(|r| matches!(r.status, ReleaseStepStatus::Skipped))
        .count();
    let missing = results
        .iter()
        .filter(|r| matches!(r.status, ReleaseStepStatus::Missing))
        .count();

    let mut next_actions = match status {
        ReleaseStepStatus::PartialSuccess | ReleaseStepStatus::Failed => vec![
            "Fix the issue and re-run (idempotent - completed steps will succeed again)"
                .to_string(),
        ],
        ReleaseStepStatus::Missing => {
            vec!["Install missing extensions or actions to resolve missing steps".to_string()]
        }
        _ => Vec::new(),
    };

    if has_auth_required_publish_skip(results) {
        next_actions.push(format!(
            "To finish GitHub Release assets without retrying registry publish, run: homeboy release {} --head --skip-publish --from-artifacts <artifact-dir>",
            component_id
        ));
        next_actions.push(format!(
            "After registry authentication is fixed, retry registry publish with: homeboy release {} --head",
            component_id
        ));
    }

    let success_summary = if matches!(status, ReleaseStepStatus::Success) {
        results.iter().filter_map(build_step_summary_line).collect()
    } else {
        Vec::new()
    };

    ReleaseRunSummary {
        total_steps: results.len(),
        succeeded,
        failed,
        skipped,
        missing,
        next_actions,
        success_summary,
    }
}

fn has_auth_required_publish_skip(results: &[ReleaseStepResult]) -> bool {
    results.iter().any(|result| {
        result.step_type.starts_with("publish.")
            && matches!(result.status, ReleaseStepStatus::Skipped)
            && result.warnings.iter().any(|warning| {
                let warning = warning.to_ascii_lowercase();
                warning.contains("requires authentication") || warning.contains("eneedauth")
            })
    })
}

fn build_step_summary_line(result: &ReleaseStepResult) -> Option<String> {
    if !matches!(result.status, ReleaseStepStatus::Success) {
        return None;
    }

    let data = result.data.as_ref();

    match result.step_type.as_str() {
        "version" => data
            .and_then(|d| d.get("new_version"))
            .and_then(|v| v.as_str())
            .map(|ver| format!("Version bumped to {}", ver)),
        "git.commit" => {
            let skipped = data
                .and_then(|d| d.get("skipped"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if skipped {
                Some("Working tree was clean".to_string())
            } else {
                Some("Committed release changes".to_string())
            }
        }
        "git.tag" => {
            let tag = data.and_then(|d| d.get("tag")).and_then(|v| v.as_str());
            let skipped = data
                .and_then(|d| d.get("skipped"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            match (tag, skipped) {
                (Some(t), true) => Some(format!("Tag {} already exists", t)),
                (Some(t), false) => Some(format!("Tagged {}", t)),
                (None, _) => Some("Tagged release".to_string()),
            }
        }
        "git.push" => Some("Pushed to origin (with tags)".to_string()),
        "release.prepare" => Some("Prepared release files".to_string()),
        "package" => Some("Created release artifacts".to_string()),
        "cleanup" => None,
        "github.release" => {
            let skipped = data
                .and_then(|d| d.get("skipped"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if skipped {
                None
            } else {
                data.and_then(|d| d.get("url"))
                    .and_then(|v| v.as_str())
                    .map(|url| format!("Created GitHub Release: {}", url))
            }
        }
        "post_release" => {
            let all_succeeded = data
                .and_then(|d| d.get("all_succeeded"))
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            if all_succeeded {
                Some("Post-release commands completed".to_string())
            } else {
                Some("Post-release commands completed (with warnings)".to_string())
            }
        }
        step if step.starts_with("publish.") => {
            let target = step.strip_prefix("publish.").unwrap_or("registry");
            Some(format!("Published to {}", target))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{build_summary, derive_overall_status};
    use crate::core::release::types::{ReleaseStepResult, ReleaseStepStatus};

    fn step(id: &str, status: ReleaseStepStatus) -> ReleaseStepResult {
        ReleaseStepResult {
            id: id.to_string(),
            step_type: id.to_string(),
            status,
            missing: Vec::new(),
            warnings: Vec::new(),
            hints: Vec::new(),
            data: None,
            error: None,
        }
    }

    #[test]
    fn test_derive_overall_status() {
        let results = vec![
            step("version", ReleaseStepStatus::Success),
            step("release.prepare", ReleaseStepStatus::Failed),
        ];

        assert_eq!(
            derive_overall_status(&results),
            ReleaseStepStatus::PartialSuccess
        );
    }

    #[test]
    fn derive_overall_status_marks_missing_steps_non_successful() {
        let results = vec![step("package", ReleaseStepStatus::Missing)];

        assert_eq!(derive_overall_status(&results), ReleaseStepStatus::Missing);
    }

    #[test]
    fn derive_overall_status_marks_success_plus_missing_partial() {
        let results = vec![
            step("version", ReleaseStepStatus::Success),
            step("package", ReleaseStepStatus::Missing),
        ];

        assert_eq!(
            derive_overall_status(&results),
            ReleaseStepStatus::PartialSuccess
        );
    }

    #[test]
    fn test_build_summary() {
        let results = vec![
            step("version", ReleaseStepStatus::Success),
            step("release.prepare", ReleaseStepStatus::Failed),
            step("cleanup", ReleaseStepStatus::Skipped),
        ];

        let status = derive_overall_status(&results);
        let summary = build_summary("fixture", &results, &status);

        assert_eq!(summary.total_steps, 3);
        assert_eq!(summary.succeeded, 1);
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.skipped, 1);
        assert_eq!(summary.next_actions.len(), 1);
    }

    #[test]
    fn summary_adds_recovery_guidance_for_auth_required_publish_skip() {
        let results = vec![
            step("github.release", ReleaseStepStatus::Success),
            ReleaseStepResult {
                id: "publish.nodejs".to_string(),
                step_type: "publish.nodejs".to_string(),
                status: ReleaseStepStatus::Skipped,
                missing: Vec::new(),
                warnings: vec![
                    "Publish to nodejs via nodejs requires authentication: npm authentication required (ENEEDAUTH)"
                        .to_string(),
                ],
                hints: Vec::new(),
                data: None,
                error: None,
            },
        ];
        let status = derive_overall_status(&results);
        let summary = build_summary("wp-codebox", &results, &status);

        assert_eq!(status, ReleaseStepStatus::Success);
        assert!(summary.next_actions.iter().any(|action| action.contains(
            "homeboy release wp-codebox --head --skip-publish --from-artifacts <artifact-dir>"
        )));
        assert!(summary
            .next_actions
            .iter()
            .any(|action| action.contains("homeboy release wp-codebox --head")));
    }
}
