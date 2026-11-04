//! Approval boundaries use the notification outbox, not status projections.
use super::workflow_job::WorkflowJobRequest;
use crate::notification_payload::{
    NotifyAction, NotifyEventKind, NotifyLink, NotifyPayload, NotifySubject,
};
use crate::notify::NotifyEvent;
use crate::notify_outbox::{self, NotifyOnceMarker, NotifyOutboxDisposition};
use crate::workflow::definition::StepKind;
use crate::workflow::template::{references, Reference};
use crate::workflow::{InstanceState, StepPhase};
use sha2::{Digest, Sha256};
use std::time::Duration;

pub(super) fn awaiting_approvals(request: &WorkflowJobRequest, state: &InstanceState) {
    let Some(route) = request.notification_route.as_ref() else {
        return;
    };
    for (id, step) in &state.steps {
        if step.phase != StepPhase::AwaitingApproval {
            continue;
        }
        let subject_id = format!("{}/{}/{}", state.instance, id, state.iteration);
        let Ok(root) = crate::paths::homeboy() else {
            return;
        };
        let directory = root.join("workflow-notifications");
        if std::fs::create_dir_all(&directory).is_err() {
            return;
        }
        let key = format!("{:x}", Sha256::digest(subject_id.as_bytes()));
        let confirmed = directory.join(format!("{key}.json"));
        let claim = directory.join(format!("{key}-claim.json"));
        if confirmed.exists() {
            continue;
        }
        // Same provisional O_EXCL claim / confirmed marker protocol as Cook.
        // A process that dies before dispatch leaves a reclaimable lease.
        if std::fs::metadata(&claim)
            .and_then(|m| m.modified())
            .and_then(|t| t.elapsed().map_err(std::io::Error::other))
            .is_ok_and(|age| age >= Duration::from_secs(300))
        {
            let _ = std::fs::remove_file(&claim);
        }
        let Ok(_claim) = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&claim)
        else {
            continue;
        };
        if confirmed.exists() {
            let _ = std::fs::remove_file(&claim);
            continue;
        }
        let mut payload = NotifyPayload::new(
            NotifyEventKind::NeedsAttention,
            NotifySubject::new("workflow_step", id)
                .with_parent(&state.instance)
                .with_phase("awaiting_approval"),
        )
        .with_fact("Instance", &state.instance)
        .with_fact("Step", id)
        .with_fact("Iteration", state.iteration.to_string())
        .with_fact(
            "Approval prompt",
            step.prompt.as_deref().unwrap_or_default(),
        )
        .with_action(NotifyAction::new(
            "approve",
            format!("homeboy workflow approve {} {id}", state.instance),
        ));
        // Link complete artifacts instead of embedding potentially very large
        // upstream outputs in a bounded transport environment.
        if let Some(definition) = request.definition.steps.iter().find(|s| &s.id == id) {
            if let StepKind::Approval(approval) = definition.kind() {
                let mut linked = std::collections::BTreeSet::new();
                for reference in references(&approval.prompt).unwrap_or_default() {
                    let upstream = match reference {
                        Reference::StepOutput { step, .. } | Reference::StepRunId(step) => step,
                        _ => continue,
                    };
                    if !linked.insert(upstream.clone()) {
                        continue;
                    }
                    if let Some(key) = state.steps.get(&upstream).and_then(|s| s.key.as_deref()) {
                        if let Ok(path) = super::workflow_job::output_path(&state.instance, key) {
                            payload = payload.with_link(NotifyLink::new(
                                format!("{upstream} output"),
                                format!("file://{}", path.display()),
                            ));
                        }
                    }
                }
            }
        }
        payload.title = Some(format!("Workflow approval required: {id}"));
        let event = NotifyEvent::lifecycle(payload.kind, &subject_id, "awaiting_approval")
            .with_title(payload.title.clone().unwrap_or_default())
            .with_payload(payload)
            .with_route(Some(route));
        let delivery = notify_outbox::dispatch_with_outbox(
            &event,
            Some(NotifyOnceMarker::new(
                "workflow_step",
                &subject_id,
                "workflow-controller",
            )),
        );
        match delivery.disposition {
            NotifyOutboxDisposition::Delivered | NotifyOutboxDisposition::Queued { .. } => {
                // Atomic rename consumes the provisional marker. Queued events
                // belong to the outbox now, even across daemon recovery.
                let _ = std::fs::rename(&claim, &confirmed);
            }
            NotifyOutboxDisposition::Dropped | NotifyOutboxDisposition::Rejected { .. } => {
                let _ = std::fs::remove_file(&claim);
            }
        }
    }
}
