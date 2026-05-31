use serde::{Deserialize, Deserializer};

use crate::core::budget::BudgetFinding;
use crate::core::finding::HomeboyFinding;

pub(crate) fn deserialize_budget_findings<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<HomeboyFinding>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = Vec::<serde_json::Value>::deserialize(deserializer)?;
    values
        .into_iter()
        .map(|value| {
            if value.get("tool").is_some() {
                serde_json::from_value::<HomeboyFinding>(value).map_err(serde::de::Error::custom)
            } else {
                serde_json::from_value::<BudgetFinding>(value)
                    .map(|finding| finding.to_homeboy_finding())
                    .map_err(serde::de::Error::custom)
            }
        })
        .collect()
}
