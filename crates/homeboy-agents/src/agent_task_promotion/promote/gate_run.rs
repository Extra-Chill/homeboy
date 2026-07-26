use crate::agent_task_gate::{
    AgentTaskGateCandidateCheckout, AgentTaskGateReport, AgentTaskGateSetupEvidence,
};
use crate::agent_task_promotion::AgentTaskPromotionStatus;
use homeboy_core::gate::HomeboyGateResult;

pub(super) struct PromotionGateRun {
    pub(super) status: AgentTaskPromotionStatus,
    pub(super) deterministic_gates: Vec<AgentTaskGateReport>,
    pub(super) gate_results: Vec<HomeboyGateResult>,
    pub(super) dependencies_materialized: bool,
    pub(super) candidate_setup: Vec<AgentTaskGateSetupEvidence>,
    pub(super) destination_gate_setup: Vec<AgentTaskGateSetupEvidence>,
    pub(super) candidate_checkout: Option<AgentTaskGateCandidateCheckout>,
}

impl PromotionGateRun {
    pub(super) fn without_gates(dry_run: bool) -> Self {
        Self {
            status: if dry_run {
                AgentTaskPromotionStatus::DryRun
            } else {
                AgentTaskPromotionStatus::Applied
            },
            deterministic_gates: Vec::new(),
            gate_results: Vec::new(),
            dependencies_materialized: false,
            candidate_setup: Vec::new(),
            destination_gate_setup: Vec::new(),
            candidate_checkout: None,
        }
    }
}
