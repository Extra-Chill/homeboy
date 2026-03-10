use homeboy::refactor::{
    analyze_stage_overlaps, normalize_sources, summarize_plan_totals, PlanOverlap, PlanStageSummary,
};

#[test]
fn analyze_stage_overlaps_reports_later_stage_precedence() {
    let stages = vec![
        PlanStageSummary {
            stage: "audit".to_string(),
            planned: true,
            applied: true,
            fixes_proposed: 1,
            files_modified: 1,
            detected_findings: Some(1),
            changed_files: vec!["src/lib.rs".to_string()],
            fix_summary: None,
            warnings: Vec::new(),
        },
        PlanStageSummary {
            stage: "lint".to_string(),
            planned: true,
            applied: true,
            fixes_proposed: 1,
            files_modified: 2,
            detected_findings: Some(2),
            changed_files: vec!["src/lib.rs".to_string(), "src/main.rs".to_string()],
            fix_summary: None,
            warnings: Vec::new(),
        },
        PlanStageSummary {
            stage: "test".to_string(),
            planned: true,
            applied: true,
            fixes_proposed: 1,
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
            PlanOverlap {
                file: "src/lib.rs".to_string(),
                earlier_stage: "audit".to_string(),
                later_stage: "lint".to_string(),
                resolution: "lint pass ran after audit in sandbox sequence".to_string(),
            },
            PlanOverlap {
                file: "src/main.rs".to_string(),
                earlier_stage: "lint".to_string(),
                later_stage: "test".to_string(),
                resolution: "test pass ran after lint in sandbox sequence".to_string(),
            },
        ]
    );
}

#[test]
fn analyze_stage_overlaps_ignores_disjoint_files() {
    let stages = vec![
        PlanStageSummary {
            stage: "audit".to_string(),
            planned: true,
            applied: true,
            fixes_proposed: 1,
            files_modified: 1,
            detected_findings: Some(1),
            changed_files: vec!["src/lib.rs".to_string()],
            fix_summary: None,
            warnings: Vec::new(),
        },
        PlanStageSummary {
            stage: "lint".to_string(),
            planned: true,
            applied: true,
            fixes_proposed: 1,
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
fn summarize_plan_totals_counts_stage_and_fix_totals() {
    let stages = vec![
        PlanStageSummary {
            stage: "audit".to_string(),
            planned: true,
            applied: false,
            fixes_proposed: 2,
            files_modified: 1,
            detected_findings: Some(2),
            changed_files: vec!["src/lib.rs".to_string()],
            fix_summary: None,
            warnings: Vec::new(),
        },
        PlanStageSummary {
            stage: "lint".to_string(),
            planned: true,
            applied: false,
            fixes_proposed: 0,
            files_modified: 0,
            detected_findings: Some(1),
            changed_files: Vec::new(),
            fix_summary: None,
            warnings: Vec::new(),
        },
        PlanStageSummary {
            stage: "test".to_string(),
            planned: true,
            applied: false,
            fixes_proposed: 3,
            files_modified: 2,
            detected_findings: None,
            changed_files: vec!["tests/foo.rs".to_string(), "tests/bar.rs".to_string()],
            fix_summary: None,
            warnings: Vec::new(),
        },
    ];

    let totals = summarize_plan_totals(&stages, 3);

    assert_eq!(totals.stages_with_proposals, 2);
    assert_eq!(totals.total_fixes_proposed, 5);
    assert_eq!(totals.total_files_selected, 3);
}

#[test]
fn normalize_sources_orders_known_sources() {
    let normalized =
        normalize_sources(&["test".to_string(), "audit".to_string(), "lint".to_string()])
            .expect("sources should normalize");

    assert_eq!(normalized, vec!["audit", "lint", "test"]);
}

#[test]
fn normalize_sources_rejects_unknown_sources() {
    let err = normalize_sources(&["weird".to_string()]).expect_err("unknown source should fail");
    assert!(err.to_string().contains("Unknown refactor source"));
}
