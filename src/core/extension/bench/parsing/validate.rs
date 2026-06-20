//! Post-deserialization validation and span derivation for `BenchResults`.
//!
//! Once the raw envelope is normalized and deserialized, these helpers enforce
//! the structural contract the deserializer cannot express on its own: unique
//! scenario ids, well-formed variance-aware metric distributions, and scenario
//! span results derived from the shared observation timeline contract.

use std::collections::BTreeMap;

use crate::core::error::{Error, Result};
use crate::core::observation::timeline::{reporting_timeline, summarize_spans};

use super::BenchResults;

/// Derive scenario span results from the shared observation timeline contract.
pub(super) fn evaluate_spans(results: &mut BenchResults) {
    for scenario in &mut results.scenarios {
        if scenario.span_definitions.is_empty() {
            continue;
        }
        let timeline = reporting_timeline(&scenario.timeline);
        scenario.span_results = summarize_spans(&timeline, &scenario.span_definitions);
    }
}

pub(super) fn validate_unique_scenario_ids(results: &BenchResults) -> Result<()> {
    let mut seen: BTreeMap<&str, Option<&str>> = BTreeMap::new();

    for scenario in &results.scenarios {
        if let Some(first_file) = seen.insert(&scenario.id, scenario.file.as_deref()) {
            let first = first_file.unwrap_or("<unknown>");
            let second = scenario.file.as_deref().unwrap_or("<unknown>");
            return Err(Error::validation_invalid_argument(
                "scenarios.id",
                format!(
                    "duplicate bench scenario id `{}` from `{}` and `{}`; scenario ids must be unique, so dispatchers should derive ids from workload paths relative to the bench root or fail discovery before emitting results",
                    scenario.id, first, second
                ),
                Some(scenario.id.clone()),
                Some(vec![first.to_string(), second.to_string()]),
            ));
        }
    }

    Ok(())
}

pub(super) fn validate_variance_policies(results: &BenchResults) -> Result<()> {
    for (name, policy) in &results.metric_policies {
        if !policy.variance_aware {
            continue;
        }
        for scenario in &results.scenarios {
            if scenario.metrics.get(name).is_none() {
                continue;
            }
            let Some(samples) = scenario.metrics.distribution(name) else {
                return Err(Error::validation_invalid_argument(
                    "metrics.distributions",
                    format!(
                        "variance-aware metric `{}` in scenario `{}` must emit metrics.distributions.{}",
                        name, scenario.id, name
                    ),
                    None,
                    None,
                ));
            };
            if samples.iter().any(|value| !value.is_finite()) {
                return Err(Error::validation_invalid_argument(
                    "metrics.distributions",
                    format!(
                        "variance-aware metric `{}` in scenario `{}` contains a non-finite sample",
                        name, scenario.id
                    ),
                    None,
                    None,
                ));
            }
            if let Some(min) = policy.min_iterations_for_variance {
                if samples.len() < min as usize {
                    return Err(Error::validation_invalid_argument(
                        "metrics.distributions",
                        format!(
                            "variance-aware metric `{}` in scenario `{}` has {} samples; minimum is {}",
                            name,
                            scenario.id,
                            samples.len(),
                            min
                        ),
                        None,
                        None,
                    ));
                }
            }
        }
    }
    Ok(())
}
