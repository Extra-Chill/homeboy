mod compare;
mod dispatch;
mod execution;
mod planning;
mod replay;
mod report;
mod types;
mod types_extra;
mod workloads;

pub use dispatch::run;
pub use types::{
    FuzzArgs, FuzzCampaignContract, FuzzContractOutput, FuzzCoverageCompletenessOutput,
    FuzzCoverageSelectorSummaryOutput, FuzzDiscoverOutput, FuzzDiscoverSummary,
    FuzzExecutionOutput, FuzzGateEvaluation, FuzzListOutput, FuzzOutput, FuzzPlanOutput,
    FuzzReplayEnv, FuzzReplayOutput, FuzzReportOutput, FuzzRunArgs, FuzzRunOutput,
    FuzzRunnerContract, FuzzValidateOutput, FuzzWorkloadOutput,
};

#[cfg(test)]
use dispatch::{run_contract, run_discover};

#[cfg(test)]
mod tests;
