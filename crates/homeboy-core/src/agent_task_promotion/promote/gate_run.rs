use crate::agent_task_gate::AgentTaskGateReport;
use crate::agent_task_promotion::AgentTaskPromotionStatus;
use crate::gate::HomeboyGateResult;

pub(super) struct PromotionGateRun {
    pub(super) status: AgentTaskPromotionStatus,
    pub(super) deterministic_gates: Vec<AgentTaskGateReport>,
    pub(super) gate_results: Vec<HomeboyGateResult>,
    pub(super) dependencies_materialized: bool,
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
        }
    }
}
