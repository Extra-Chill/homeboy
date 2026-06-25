mod compare;
mod dispatch;
mod doctor;
mod execution;
mod inspect;
mod planning;
mod replay;
mod report;
mod types;
mod types_extra;
mod workloads;

pub(crate) use compare::compare_envelopes;
pub use dispatch::run;
pub use types::{
    FuzzArgs, FuzzCampaignContract, FuzzCompareHotspotPolicy, FuzzCompareOutput,
    FuzzContractOutput, FuzzCoverageCompletenessOutput, FuzzCoverageSelectorSummaryOutput,
    FuzzDiscoverOutput, FuzzDiscoverSummary, FuzzExecutionOutput, FuzzGateEvaluation,
    FuzzListOutput, FuzzOutput, FuzzPlanOutput, FuzzReplayEnv, FuzzReplayOutput, FuzzReportOutput,
    FuzzRunArgs, FuzzRunOutput, FuzzRunnerContract, FuzzValidateOutput, FuzzWorkloadOutput,
};

#[cfg(test)]
use dispatch::{run_contract, run_discover};

#[cfg(test)]
mod tests;
