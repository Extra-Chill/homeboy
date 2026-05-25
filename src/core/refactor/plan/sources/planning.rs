use crate::core::refactor::auto::{self, FixApplied, FixResultsSummary};
use serde::Serialize;
use std::collections::BTreeSet;

#[derive(Debug, Clone, Serialize)]
pub struct SourceStageSummary {
    pub stage: String,
    pub collected: bool,
    pub applied: bool,
    pub edit_count: usize,
    pub files_modified: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detected_findings: Option<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub changed_files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix_summary: Option<FixResultsSummary>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SourceOverlap {
    pub file: String,
    pub earlier_stage: String,
    pub later_stage: String,
    pub resolution: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceTotals {
    pub stages_with_edits: usize,
    pub total_edits: usize,
    pub total_files_selected: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CollectedEdit {
    pub source: String,
    pub file: String,
    pub rule_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

#[derive(Default)]
pub(super) struct FixAccumulator {
    fixes: Vec<FixApplied>,
}

impl FixAccumulator {
    pub(super) fn extend(&mut self, items: Vec<FixApplied>) {
        self.fixes.extend(items);
    }

    pub(super) fn summary(&self) -> Option<FixResultsSummary> {
        if self.fixes.is_empty() {
            None
        } else {
            Some(auto::summarize_fix_results(&self.fixes))
        }
    }
}

pub(super) struct PlannedStage {
    pub(super) source: String,
    pub(super) summary: SourceStageSummary,
    pub(super) fix_results: Vec<FixApplied>,
}

pub(super) fn collect_collected_edits(stages: &[PlannedStage]) -> Vec<CollectedEdit> {
    let mut edits = Vec::new();

    for stage in stages {
        for fix in &stage.fix_results {
            edits.push(CollectedEdit {
                source: stage.source.clone(),
                file: fix.file.clone(),
                rule_id: fix.rule.clone(),
                action: fix.action.clone(),
            });
        }
    }

    edits.sort_by(|a, b| {
        a.source
            .cmp(&b.source)
            .then(a.file.cmp(&b.file))
            .then(a.rule_id.cmp(&b.rule_id))
    });

    edits
}

pub(super) fn collect_stage_changed_files(stages: &[SourceStageSummary]) -> Vec<String> {
    let mut final_changed_files = BTreeSet::new();
    for stage in stages {
        for file in &stage.changed_files {
            final_changed_files.insert(file.clone());
        }
    }
    final_changed_files.into_iter().collect()
}

pub(super) fn analyze_stage_overlaps(stages: &[SourceStageSummary]) -> Vec<SourceOverlap> {
    let mut overlaps = Vec::new();

    for (later_index, later_stage) in stages.iter().enumerate() {
        if later_stage.changed_files.is_empty() {
            continue;
        }

        let later_files: BTreeSet<&str> = later_stage
            .changed_files
            .iter()
            .map(String::as_str)
            .collect();

        for earlier_stage in stages.iter().take(later_index) {
            if earlier_stage.changed_files.is_empty() {
                continue;
            }

            for file in earlier_stage.changed_files.iter().map(String::as_str) {
                if later_files.contains(file) {
                    overlaps.push(SourceOverlap {
                        file: file.to_string(),
                        earlier_stage: earlier_stage.stage.clone(),
                        later_stage: later_stage.stage.clone(),
                        resolution: format!(
                            "{} pass ran after {} in pipeline sequence",
                            later_stage.stage, earlier_stage.stage
                        ),
                    });
                }
            }
        }
    }

    overlaps.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then(a.earlier_stage.cmp(&b.earlier_stage))
            .then(a.later_stage.cmp(&b.later_stage))
    });

    overlaps
}

pub(super) fn summarize_source_totals(
    stages: &[SourceStageSummary],
    total_files_selected: usize,
) -> SourceTotals {
    SourceTotals {
        stages_with_edits: stages.iter().filter(|stage| stage.edit_count > 0).count(),
        total_edits: stages.iter().map(|stage| stage.edit_count).sum(),
        total_files_selected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analyze_stage_overlaps_reports_later_stage_precedence() {
        let stages = vec![
            SourceStageSummary {
                stage: "audit".to_string(),
                collected: true,
                applied: true,
                edit_count: 1,
                files_modified: 1,
                detected_findings: Some(1),
                changed_files: vec!["src/lib.rs".to_string()],
                fix_summary: None,
                warnings: Vec::new(),
            },
            SourceStageSummary {
                stage: "lint".to_string(),
                collected: true,
                applied: true,
                edit_count: 1,
                files_modified: 2,
                detected_findings: Some(2),
                changed_files: vec!["src/lib.rs".to_string(), "src/main.rs".to_string()],
                fix_summary: None,
                warnings: Vec::new(),
            },
            SourceStageSummary {
                stage: "test".to_string(),
                collected: true,
                applied: true,
                edit_count: 1,
                files_modified: 1,
                detected_findings: None,
                changed_files: vec!["src/main.rs".to_string()],
                fix_summary: None,
                warnings: Vec::new(),
            },
        ];

        let overlaps = analyze_stage_overlaps(&stages);

        assert_eq!(
            overlaps,
            vec![
                SourceOverlap {
                    file: "src/lib.rs".to_string(),
                    earlier_stage: "audit".to_string(),
                    later_stage: "lint".to_string(),
                    resolution: "lint pass ran after audit in pipeline sequence".to_string(),
                },
                SourceOverlap {
                    file: "src/main.rs".to_string(),
                    earlier_stage: "lint".to_string(),
                    later_stage: "test".to_string(),
                    resolution: "test pass ran after lint in pipeline sequence".to_string(),
                },
            ]
        );
    }

    #[test]
    fn test_analyze_stage_overlaps() {
        assert!(analyze_stage_overlaps(&[]).is_empty());
    }

    #[test]
    fn analyze_stage_overlaps_ignores_disjoint_files() {
        let stages = vec![
            SourceStageSummary {
                stage: "audit".to_string(),
                collected: true,
                applied: true,
                edit_count: 1,
                files_modified: 1,
                detected_findings: Some(1),
                changed_files: vec!["src/lib.rs".to_string()],
                fix_summary: None,
                warnings: Vec::new(),
            },
            SourceStageSummary {
                stage: "lint".to_string(),
                collected: true,
                applied: true,
                edit_count: 1,
                files_modified: 1,
                detected_findings: Some(1),
                changed_files: vec!["src/main.rs".to_string()],
                fix_summary: None,
                warnings: Vec::new(),
            },
        ];

        assert!(analyze_stage_overlaps(&stages).is_empty());
    }

    #[test]
    fn summarize_source_totals_counts_stage_and_fix_totals() {
        let stages = vec![
            SourceStageSummary {
                stage: "audit".to_string(),
                collected: true,
                applied: false,
                edit_count: 2,
                files_modified: 1,
                detected_findings: Some(2),
                changed_files: vec!["src/lib.rs".to_string()],
                fix_summary: None,
                warnings: Vec::new(),
            },
            SourceStageSummary {
                stage: "lint".to_string(),
                collected: true,
                applied: false,
                edit_count: 0,
                files_modified: 0,
                detected_findings: Some(1),
                changed_files: Vec::new(),
                fix_summary: None,
                warnings: Vec::new(),
            },
            SourceStageSummary {
                stage: "test".to_string(),
                collected: true,
                applied: false,
                edit_count: 3,
                files_modified: 2,
                detected_findings: None,
                changed_files: vec!["tests/foo.rs".to_string(), "tests/bar.rs".to_string()],
                fix_summary: None,
                warnings: Vec::new(),
            },
        ];

        let totals = summarize_source_totals(&stages, 3);

        assert_eq!(totals.stages_with_edits, 2);
        assert_eq!(totals.total_edits, 5);
        assert_eq!(totals.total_files_selected, 3);
    }

    #[test]
    fn test_summarize_source_totals() {
        let totals = summarize_source_totals(&[], 0);

        assert_eq!(totals.stages_with_edits, 0);
        assert_eq!(totals.total_edits, 0);
        assert_eq!(totals.total_files_selected, 0);
    }
}
