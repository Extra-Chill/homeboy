//! Open-wait reconciliation (W3-9).
//!
//! `Waiting` was a state with no automatic exit: a controller with an open
//! `WaitForEvent`/`WaitForController` has no pending action, so `resume`
//! returns `idle` and exits, and nothing polled, subscribed, or timed out.
//!
//! These tests pin both halves of the fix — what durable evidence resolves a
//! wait, and (more importantly) what does not. A wait that resolves wrongly
//! advances a controller past a gate the world has not passed, which is worse
//! than one that stalls.

use super::super::*;
use super::*;

fn parked_controller(loop_id: &str, wait: AgentTaskLoopWait) -> AgentTaskLoopControllerRecord {
    let mut record = AgentTaskLoopControllerRecord::new(loop_id, "delegate", "v1");
    record.state = AgentTaskLoopControllerState::Waiting;
    record.waits.push(wait);
    controller::write_controller(&record).expect("controller written");
    record
}

fn wait(wait_key: &str, event_type: &str, external_ref: Option<&str>) -> AgentTaskLoopWait {
    AgentTaskLoopWait {
        wait_key: wait_key.to_string(),
        event_type: event_type.to_string(),
        entity_id: None,
        external_ref: external_ref.map(str::to_string),
        timeout_at: None,
        escalation_policy: None,
        status: AgentTaskLoopWaitStatus::Open,
        satisfied_by_event_id: None,
    }
}

fn terminal_child(loop_id: &str, state: AgentTaskLoopControllerState) {
    let mut child = AgentTaskLoopControllerRecord::new(loop_id, "work", "v1");
    child.state = state;
    controller::write_controller(&child).expect("child written");
}

/// Persist a durable agent-task run already in a terminal state.
fn terminal_run(run_id: &str) {
    crate::agent_task_lifecycle::submit_plan(&test_plan(), Some(run_id)).expect("durable run");
    crate::agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
        record.state = crate::agent_task_lifecycle::AgentTaskRunState::Succeeded;
    })
    .expect("terminal run");
}

#[test]
fn a_waiting_controller_resolves_when_its_child_controller_is_terminal() {
    with_isolated_home(|_| {
        terminal_child("wait-child-done", AgentTaskLoopControllerState::Completed);
        parked_controller(
            "wait-parent-done",
            wait(
                "controller:wait-child-done:terminal",
                "controller.terminal",
                Some("wait-child-done"),
            ),
        );

        let report = reconcile_waiting_controllers().expect("sweep");
        assert_eq!(report.changed, 1, "{report:?}");
        let entry = &report.controllers[0];
        assert_eq!(entry.resolved_waits.len(), 1);
        assert_eq!(entry.after_state, AgentTaskLoopControllerState::Running);

        // The transition is durable, so the next `resume` finds work.
        let reloaded = controller::load_controller("wait-parent-done").expect("reload");
        assert_eq!(reloaded.state, AgentTaskLoopControllerState::Running);
        assert_eq!(reloaded.open_wait_count(), 0);
        assert!(reloaded
            .history
            .iter()
            .any(|event| event.event_type == "controller.wait.resolved"));
    });
}

#[test]
fn a_waiting_controller_resolves_when_the_run_it_dispatched_is_terminal() {
    // The exact shape W3-9 names: a controller that dispatched a cook sat in
    // Waiting indefinitely even after that cook terminalized.
    with_isolated_home(|_| {
        terminal_run("wait-reconcile-run-a");
        parked_controller(
            "wait-parent-run",
            wait(
                "cook:wait-reconcile-run-a",
                "agent_task.run_terminal",
                Some("wait-reconcile-run-a"),
            ),
        );

        let report = reconcile_waiting_controllers().expect("sweep");
        assert_eq!(report.changed, 1, "{report:?}");
        let reloaded = controller::load_controller("wait-parent-run").expect("reload");
        assert_eq!(reloaded.state, AgentTaskLoopControllerState::Running);
        assert_eq!(reloaded.waits[0].status, AgentTaskLoopWaitStatus::Satisfied);
        assert!(reloaded.waits[0]
            .satisfied_by_event_id
            .as_deref()
            .unwrap()
            .starts_with("run-terminal:wait-reconcile-run-a:"));
    });
}

#[test]
fn a_still_running_child_controller_leaves_the_wait_open() {
    with_isolated_home(|_| {
        terminal_child("wait-child-busy", AgentTaskLoopControllerState::Running);
        parked_controller(
            "wait-parent-busy",
            wait(
                "controller:wait-child-busy:terminal",
                "controller.terminal",
                Some("wait-child-busy"),
            ),
        );

        let report = reconcile_waiting_controllers().expect("sweep");
        assert_eq!(report.changed, 0, "{report:?}");
        let reloaded = controller::load_controller("wait-parent-busy").expect("reload");
        assert_eq!(reloaded.state, AgentTaskLoopControllerState::Waiting);
        assert_eq!(reloaded.open_wait_count(), 1);
    });
}

#[test]
fn an_external_event_type_is_never_resolved_from_local_state() {
    // The refusal that matters most. A PR-checks wait describes something
    // Homeboy does not observe locally; resolving it because some local record
    // is terminal would advance the controller past a gate GitHub has not
    // passed. Both a matching-named run and a matching-named controller exist
    // here specifically so an inference-based resolver would fire.
    with_isolated_home(|_| {
        terminal_child("pr-ownership", AgentTaskLoopControllerState::Completed);
        terminal_run("wait-reconcile-run-b");
        for (key, event_type, reference) in [
            (
                "pr-ownership:1:checks",
                "github.pr.checks_changed",
                "wait-reconcile-run-b",
            ),
            ("pr-ownership:1:merged", "github.pr.merged", "pr-ownership"),
            ("custom:thing", "operator.authored.event", "pr-ownership"),
        ] {
            let loop_id = format!("wait-parent-{}", key.replace([':'], "-"));
            parked_controller(&loop_id, wait(key, event_type, Some(reference)));
        }

        let report = reconcile_waiting_controllers().expect("sweep");
        assert_eq!(report.changed, 0, "{report:?}");
        assert_eq!(report.failed, 0, "{report:?}");
    });
}

#[test]
fn a_wait_without_an_external_ref_is_never_resolved() {
    // Its only remaining identity is an entity id, which names what the wait
    // is about, not what would satisfy it.
    with_isolated_home(|_| {
        terminal_run("wait-reconcile-run-c");
        let mut record = parked_controller(
            "wait-parent-anonymous",
            wait("anonymous", "agent_task.run_terminal", None),
        );
        record.waits[0].entity_id = Some("entity-1".to_string());
        controller::write_controller(&record).expect("written");

        let report = reconcile_waiting_controllers().expect("sweep");
        assert_eq!(report.changed, 0, "{report:?}");
        assert_eq!(
            controller::load_controller("wait-parent-anonymous")
                .unwrap()
                .state,
            AgentTaskLoopControllerState::Waiting
        );
    });
}

#[test]
fn an_unreadable_subject_is_not_evidence_of_terminality() {
    // A missing child record and a missing run are both "cannot tell", which
    // must read as keep-waiting rather than as done.
    with_isolated_home(|_| {
        parked_controller(
            "wait-parent-missing-child",
            wait(
                "controller:nope:terminal",
                "controller.terminal",
                Some("no-such-controller"),
            ),
        );
        parked_controller(
            "wait-parent-missing-run",
            wait("run:nope", "agent_task.run_terminal", Some("no-such-run")),
        );

        let report = reconcile_waiting_controllers().expect("sweep");
        assert_eq!(report.changed, 0, "{report:?}");
        assert_eq!(report.failed, 0, "{report:?}");
    });
}

#[test]
fn a_wait_is_only_expired_by_a_deadline_it_declared() {
    with_isolated_home(|_| {
        // No deadline: never expires, however old it is.
        parked_controller(
            "wait-parent-no-deadline",
            wait("forever", "github.pr.merged", Some("pr#1")),
        );
        // A malformed deadline is not a deadline.
        let mut malformed = parked_controller(
            "wait-parent-bad-deadline",
            wait("malformed", "github.pr.merged", Some("pr#2")),
        );
        malformed.waits[0].timeout_at = Some("not a timestamp".to_string());
        controller::write_controller(&malformed).expect("written");

        let report = reconcile_waiting_controllers().expect("sweep");
        assert_eq!(report.changed, 0, "{report:?}");
        for loop_id in ["wait-parent-no-deadline", "wait-parent-bad-deadline"] {
            assert_eq!(
                controller::load_controller(loop_id).unwrap().state,
                AgentTaskLoopControllerState::Waiting,
                "{loop_id}"
            );
        }
    });
}

#[test]
fn an_expired_wait_unblocks_the_controller() {
    with_isolated_home(|_| {
        let mut record = parked_controller(
            "wait-parent-expired",
            wait("expiring", "github.pr.merged", Some("pr#3")),
        );
        record.waits[0].timeout_at =
            Some((chrono::Utc::now() - chrono::Duration::minutes(5)).to_rfc3339());
        controller::write_controller(&record).expect("written");

        let report = reconcile_waiting_controllers().expect("sweep");
        assert_eq!(report.changed, 1, "{report:?}");
        let reloaded = controller::load_controller("wait-parent-expired").expect("reload");
        assert_eq!(reloaded.waits[0].status, AgentTaskLoopWaitStatus::TimedOut);
        // No escalation policy declared: unblocked, not terminalized.
        assert_eq!(reloaded.state, AgentTaskLoopControllerState::Running);
        assert!(reloaded
            .history
            .iter()
            .any(|event| event.event_type == "controller.wait.timed_out"));
    });
}

#[test]
fn an_expired_wait_escalates_only_when_its_policy_says_so() {
    with_isolated_home(|_| {
        let mut record = parked_controller(
            "wait-parent-escalate",
            wait("escalating", "github.pr.merged", Some("pr#4")),
        );
        record.waits[0].timeout_at =
            Some((chrono::Utc::now() - chrono::Duration::minutes(5)).to_rfc3339());
        record.waits[0].escalation_policy = Some("escalate".to_string());
        controller::write_controller(&record).expect("written");

        reconcile_waiting_controllers().expect("sweep");
        let reloaded = controller::load_controller("wait-parent-escalate").expect("reload");
        assert_eq!(reloaded.state, AgentTaskLoopControllerState::Escalated);
        assert!(reloaded
            .history
            .iter()
            .any(|event| event.event_type == "controller.wait.escalated"));
    });
}

#[test]
fn an_unrelated_escalation_policy_does_not_terminalize_the_controller() {
    // `reinspect_pr` and `wait_for_merge` are both already emitted by the
    // PR-ownership path. Neither means "give up".
    with_isolated_home(|_| {
        let mut record = parked_controller(
            "wait-parent-reinspect",
            wait("reinspect", "github.pr.checks_changed", Some("pr#5")),
        );
        record.waits[0].timeout_at =
            Some((chrono::Utc::now() - chrono::Duration::minutes(5)).to_rfc3339());
        record.waits[0].escalation_policy = Some("reinspect_pr".to_string());
        controller::write_controller(&record).expect("written");

        reconcile_waiting_controllers().expect("sweep");
        assert_eq!(
            controller::load_controller("wait-parent-reinspect")
                .unwrap()
                .state,
            AgentTaskLoopControllerState::Running
        );
    });
}

#[test]
fn one_broken_controller_does_not_stop_the_sweep() {
    with_isolated_home(|_| {
        terminal_child("wait-child-ok", AgentTaskLoopControllerState::Completed);
        parked_controller(
            "wait-parent-ok",
            wait(
                "controller:wait-child-ok:terminal",
                "controller.terminal",
                Some("wait-child-ok"),
            ),
        );
        // A wait naming a run whose status read fails is skipped, not fatal.
        parked_controller(
            "wait-parent-broken",
            wait("run:broken", "agent_task.run_terminal", Some("   ")),
        );

        let report = reconcile_waiting_controllers().expect("sweep");
        assert_eq!(report.changed, 1, "{report:?}");
        assert_eq!(
            controller::load_controller("wait-parent-ok").unwrap().state,
            AgentTaskLoopControllerState::Running
        );
    });
}

#[test]
fn a_controller_with_no_open_waits_is_not_touched() {
    with_isolated_home(|_| {
        let mut record = AgentTaskLoopControllerRecord::new("wait-parent-idle", "delegate", "v1");
        record.state = AgentTaskLoopControllerState::Running;
        controller::write_controller(&record).expect("written");

        let report = reconcile_waiting_controllers().expect("sweep");
        assert!(report.controllers.is_empty(), "{report:?}");
        assert_eq!(report.changed, 0);
    });
}

#[test]
fn resume_resolves_a_satisfied_wait_instead_of_reporting_idle() {
    // The manual path benefits from the same reconciler: `resume` on a parked
    // controller whose child is done now advances rather than stopping at
    // `idle`.
    with_isolated_home(|_| {
        terminal_child("wait-child-resume", AgentTaskLoopControllerState::Completed);
        let mut record = parked_controller(
            "wait-parent-resume",
            wait(
                "controller:wait-child-resume:terminal",
                "controller.terminal",
                Some("wait-child-resume"),
            ),
        );
        record.record_action(
            AgentTaskLoopPolicyAction::Complete {
                reason: Some("child done".to_string()),
            },
            "complete after child",
        );
        controller::write_controller(&record).expect("written");

        let result = resume(
            "wait-parent-resume",
            CapturingExecutor::default(),
            &NoopDispatchHook,
        )
        .expect("resumed");

        assert_ne!(result.value.stopped_reason, "idle", "{:?}", result.value);
        assert_eq!(
            result.value.controller.state,
            AgentTaskLoopControllerState::Completed
        );
    });
}
