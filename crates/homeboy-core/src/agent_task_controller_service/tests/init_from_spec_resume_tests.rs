use super::super::*;
use super::*;

#[test]
fn init_from_spec_for_resume_rejects_changed_existing_spec() {
    with_isolated_home(|_| {
        let base = repo_loop_reconcile_spec("repo-loop-resume-stale-guard");
        init_from_spec_for_resume(ControllerFromSpecRequest { spec: base.clone() })
            .expect("base spec initialized for resume");

        let mut changed = base;
        changed
            .workflows
            .iter_mut()
            .find(|workflow| workflow.workflow_id == "static_validation")
            .expect("static validation workflow")
            .dependencies = vec!["static_site_candidate".to_string()];

        let error = init_from_spec_for_resume(ControllerFromSpecRequest { spec: changed })
            .expect_err("changed spec is blocked before resume");
        assert_eq!(error.code.as_str(), "validation.invalid_argument");
        assert!(error
            .message
            .contains("refusing to reuse stale persisted controller state"));
        assert!(error
            .message
            .contains("--reconcile-stale to safely reset run-scoped state"));
        let tried = error
            .details
            .get("tried")
            .and_then(Value::as_array)
            .expect("guard diagnostic lists tried details");
        let detail_has = |needle: &str| {
            tried.iter().any(|detail| {
                detail
                    .as_str()
                    .is_some_and(|detail| detail.contains(needle))
            })
        };
        // Diagnostic must name the state path, prior + requested fingerprint,
        // and the safe next action (#6221 acceptance criteria).
        assert!(detail_has("state_path="), "{tried:?}");
        assert!(detail_has("prior_spec_fingerprint="), "{tried:?}");
        assert!(detail_has("requested_spec_fingerprint="), "{tried:?}");
        assert!(
            detail_has("safe_next_action=--reconcile-stale"),
            "{tried:?}"
        );
    });
}

#[test]
fn init_from_spec_for_resume_reconcile_stale_resets_state_without_manual_cleanup() {
    with_isolated_home(|_| {
        let (base, changed) = changed_resume_spec("repo-loop-resume-reconcile-stale");
        init_from_spec_for_resume(ControllerFromSpecRequest { spec: base })
            .expect("base initialized");

        // One flag: no prior manual state cleanup required.
        let report = init_from_spec_for_resume_with_resolution(
            ControllerFromSpecRequest {
                spec: changed.clone(),
            },
            ControllerResumeStateResolution::ReconcileStale,
        )
        .expect("reconcile-stale re-initializes controller");
        let resume_state = report.resume_state.expect("resume_state present");
        assert_eq!(resume_state.action, "replacing");
        assert_eq!(resume_state.resolution, "reconcile-stale");
        assert!(resume_state.existing_controller);
        assert!(!resume_state.fingerprint_match);
        // Run-scoped state is re-derived from the requested spec, not the stale base.
        assert_eq!(
            repo_loop_spec_fingerprint_from_metadata(&report.controller),
            Some(repo_loop_spec_fingerprint(&changed).expect("fingerprint"))
        );
        assert_eq!(report.loop_id, "repo-loop-resume-reconcile-stale");
    });
}

#[test]
fn init_from_spec_for_resume_reports_creating_on_fresh_loop() {
    with_isolated_home(|_| {
        let base = repo_loop_reconcile_spec("repo-loop-resume-fresh");
        let report = init_from_spec_for_resume_with_resolution(
            ControllerFromSpecRequest { spec: base },
            ControllerResumeStateResolution::Guard,
        )
        .expect("fresh loop initializes");
        let resume_state = report.resume_state.expect("resume_state present");
        assert_eq!(resume_state.action, "creating");
        assert!(!resume_state.existing_controller);
        assert!(!resume_state.fingerprint_match);
        assert!(resume_state.previous_spec_fingerprint.is_none());
    });
}

#[test]
fn init_from_spec_for_resume_reports_resuming_on_matching_spec() {
    with_isolated_home(|_| {
        let base = repo_loop_reconcile_spec("repo-loop-resume-match");
        init_from_spec_for_resume_with_resolution(
            ControllerFromSpecRequest { spec: base.clone() },
            ControllerResumeStateResolution::Guard,
        )
        .expect("base initialized");
        let report = init_from_spec_for_resume_with_resolution(
            ControllerFromSpecRequest { spec: base },
            ControllerResumeStateResolution::Guard,
        )
        .expect("unchanged spec resumes");
        let resume_state = report.resume_state.expect("resume_state present");
        assert_eq!(resume_state.action, "resuming");
        assert!(resume_state.existing_controller);
        assert!(resume_state.fingerprint_match);
    });
}

#[test]
fn init_from_spec_for_resume_replace_resets_stale_state() {
    with_isolated_home(|_| {
        let (base, changed) = changed_resume_spec("repo-loop-resume-replace");
        init_from_spec_for_resume(ControllerFromSpecRequest { spec: base })
            .expect("base initialized");

        let report = init_from_spec_for_resume_with_resolution(
            ControllerFromSpecRequest {
                spec: changed.clone(),
            },
            ControllerResumeStateResolution::Replace,
        )
        .expect("replace re-initializes controller");
        let resume_state = report.resume_state.expect("resume_state present");
        assert_eq!(resume_state.action, "replacing");
        assert_eq!(resume_state.resolution, "replace");
        // Replaced controller carries the new fingerprint and starts fresh.
        assert_eq!(
            repo_loop_spec_fingerprint_from_metadata(&report.controller),
            Some(repo_loop_spec_fingerprint(&changed).expect("fingerprint"))
        );
        assert_eq!(report.loop_id, "repo-loop-resume-replace");
    });
}

#[test]
fn init_from_spec_for_resume_fork_isolates_under_derived_loop_id() {
    with_isolated_home(|_| {
        let (base, changed) = changed_resume_spec("repo-loop-resume-fork");
        init_from_spec_for_resume(ControllerFromSpecRequest { spec: base.clone() })
            .expect("base initialized");

        let report = init_from_spec_for_resume_with_resolution(
            ControllerFromSpecRequest {
                spec: changed.clone(),
            },
            ControllerResumeStateResolution::Fork,
        )
        .expect("fork applies under a derived loop id");
        let resume_state = report.resume_state.expect("resume_state present");
        assert_eq!(resume_state.action, "forking");
        assert_eq!(resume_state.requested_loop_id, "repo-loop-resume-fork");
        assert_ne!(report.loop_id, "repo-loop-resume-fork");
        assert!(report.loop_id.starts_with("repo-loop-resume-fork-fork-"));

        // The original controller still carries the base fingerprint, untouched.
        let original = status("repo-loop-resume-fork").expect("original controller intact");
        assert_eq!(
            repo_loop_spec_fingerprint_from_metadata(&original),
            Some(repo_loop_spec_fingerprint(&base).expect("base fingerprint"))
        );
    });
}

#[test]
fn init_from_spec_for_resume_existing_accepts_stale_state() {
    with_isolated_home(|_| {
        let (base, changed) = changed_resume_spec("repo-loop-resume-existing");
        init_from_spec_for_resume(ControllerFromSpecRequest { spec: base })
            .expect("base initialized");

        let report = init_from_spec_for_resume_with_resolution(
            ControllerFromSpecRequest { spec: changed },
            ControllerResumeStateResolution::ResumeExisting,
        )
        .expect("resume-existing accepts stale state");
        let resume_state = report.resume_state.expect("resume_state present");
        assert_eq!(resume_state.action, "resuming");
        assert_eq!(resume_state.resolution, "resume-existing");
        assert!(resume_state.existing_controller);
        assert!(!resume_state.fingerprint_match);
        assert_eq!(report.loop_id, "repo-loop-resume-existing");
    });
}
