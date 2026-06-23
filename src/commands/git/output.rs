use serde::{Serialize, Serializer};

use homeboy::core::git::{
    GitOutput, GithubFindOutput, GithubIssueOutput, GithubPrFleetOutput, GithubPrOutput,
    GithubPrReadinessOutput, PrLandOutput, PrMergeabilityReconcileOutput, PrPolicyDecision,
    PrRefreshOutput,
};
use homeboy::core::BulkResult;

pub enum GitCommandOutput {
    Single(GitOutput),
    Bulk(BulkResult<GitOutput>),
    Issue(GithubIssueOutput),
    Pr(GithubPrOutput),
    PrRefresh(PrRefreshOutput),
    PrReadiness(GithubPrReadinessOutput),
    Find(GithubFindOutput),
    ReconcileMergeability(PrMergeabilityReconcileOutput),
    Policy(PrPolicyDecision),
    Fleet(GithubPrFleetOutput),
    Land(PrLandOutput),
}

impl Serialize for GitCommandOutput {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let (variant, payload) = match self {
            GitCommandOutput::Single(output) => ("single", serde_json::to_value(output)),
            GitCommandOutput::Bulk(output) => ("bulk", serde_json::to_value(output)),
            GitCommandOutput::Issue(output) => ("issue", serde_json::to_value(output)),
            GitCommandOutput::Pr(output) => ("pr", serde_json::to_value(output)),
            GitCommandOutput::PrRefresh(output) => ("pr_refresh", serde_json::to_value(output)),
            GitCommandOutput::PrReadiness(output) => ("pr_readiness", serde_json::to_value(output)),
            GitCommandOutput::Find(output) => ("find", serde_json::to_value(output)),
            GitCommandOutput::ReconcileMergeability(output) => {
                ("reconcile_mergeability", serde_json::to_value(output))
            }
            GitCommandOutput::Policy(output) => ("policy", serde_json::to_value(output)),
            GitCommandOutput::Fleet(output) => ("fleet", serde_json::to_value(output)),
            GitCommandOutput::Land(output) => ("land", serde_json::to_value(output)),
        };

        let mut payload = payload.map_err(serde::ser::Error::custom)?;
        let Some(object) = payload.as_object_mut() else {
            return Err(serde::ser::Error::custom(
                "git command output payload must serialize as an object",
            ));
        };
        object.insert(
            "variant".to_string(),
            serde_json::Value::String(variant.into()),
        );
        payload.serialize(serializer)
    }
}
