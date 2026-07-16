use super::*;

fn dossier() -> AgentTaskReviewDossier {
    AgentTaskReviewDossier {
        schema: AGENT_TASK_REVIEW_DOSSIER_SCHEMA.to_string(),
        summary: "Add dossier".into(),
        what_changed: vec!["Changes output".into()],
        how_to_test: vec![AgentTaskReviewTestStep {
            command: "cargo test dossier".into(),
            expected: "passes".into(),
        }],
        compatibility: "No compatibility impact".into(),
        evidence: Vec::new(),
        ai_assistance: AgentTaskReviewAiAssistance {
            used: true,
            tool: "OpenCode".into(),
            model: "openai/gpt-5.6-terra".into(),
            used_for: "Implementation".into(),
        },
        source_relationships: vec![AgentTaskReviewIssueRelationship {
            kind: AgentTaskReviewIssueRelationshipKind::Closes,
            reference: "#8058".into(),
        }],
        overrides: Vec::new(),
    }
}

#[test]
fn renderer_is_deterministic_and_safe() {
    let body = render_review_dossier(&dossier(), &default_profile());
    assert!(body.starts_with("## Summary"));
    assert!(body.contains("1. Run `cargo test dossier`; expect passes."));
    assert!(!body.contains("Publication intent"));
}

#[test]
fn renderer_uses_github_closing_syntax_and_escapes_structural_markdown() {
    let mut value = dossier();
    value.summary = "# heading > quote - list 1. ordered ``` fence [link](x) <!-- comment".into();
    value
        .source_relationships
        .push(AgentTaskReviewIssueRelationship {
            kind: AgentTaskReviewIssueRelationshipKind::RelatesTo,
            reference: "owner/repo#9".into(),
        });
    let body = render_review_dossier(&value, &default_profile());
    assert!(body.contains("Closes #8058"));
    assert!(body.contains("Relates to owner/repo#9"));
    assert!(!body.contains("Closes: #8058"));
    assert!(body.contains("\\# heading \\> quote - list 1. ordered"));
}

#[test]
fn overrides_apply_and_keep_provenance() {
    let mut value = dossier();
    value.overrides.push(AgentTaskReviewOverride {
        target: AgentTaskReviewOverrideTarget::Summary,
        value: "Override".into(),
        provenance: "operator".into(),
    });
    value.apply_overrides().unwrap();
    assert_eq!(value.summary, "Override");
    assert_eq!(value.overrides[0].provenance, "operator");
}

#[test]
fn rejects_injection_and_bad_issue_refs() {
    let mut value = dossier();
    value.summary = "ok\n## injected".into();
    assert!(value.validate(&default_profile()).is_err());
    let mut value = dossier();
    value.source_relationships[0].reference = "https://evil.test/issues/1".into();
    assert!(value.validate(&default_profile()).is_err());
}

#[test]
fn profile_conflicts_fail_closed() {
    let profile = AgentTaskReviewProfile {
        required_sections: vec![AgentTaskReviewSectionId::Summary],
        hidden_sections: vec![AgentTaskReviewSectionId::Summary],
        ..Default::default()
    };
    assert!(validate_profile(&profile).is_err());
}

#[test]
fn url_policy_rejects_local_urls() {
    let mut value = dossier();
    value.evidence.push(AgentTaskReviewEvidence {
        summary: "local".into(),
        url: Some("https://localhost/a".into()),
    });
    assert!(value.validate(&default_profile()).is_err());
}

#[test]
fn configured_profile_is_loaded_canonically_and_invalid_policy_fails_closed() {
    let directory = tempfile::tempdir().expect("temporary component");
    std::fs::write(
        directory.path().join("homeboy.json"),
        r#"{"id":"review-profile-test","review_profile":{"required_sections":["summary"],"hidden_sections":["summary"]}}"#,
    )
    .expect("portable config");
    assert!(resolve_review_profile(directory.path().to_str().expect("path")).is_err());
}
