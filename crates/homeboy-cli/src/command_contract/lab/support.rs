//! User-facing summaries of commands that support Lab runners.

use super::LabCommandRouteSupport;
use crate::command_contract::spec::COMMAND_SPECS;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabRunnerSupportSummary {
    pub supported_labels: Vec<&'static str>,
    pub unsupported_message: String,
    pub hint: String,
}

pub(crate) fn lab_runner_supported_labels(
    composed_support: &[LabCommandRouteSupport],
) -> Vec<&'static str> {
    let mut labels = COMMAND_SPECS
        .iter()
        .flat_map(|spec| spec.lab_support_summary.iter())
        .map(|summary| summary.message_label)
        .collect::<Vec<_>>();
    labels.extend(composed_support.iter().map(|support| support.message_label));
    labels
}

pub(crate) fn lab_runner_support_summary(
    composed_support: &[LabCommandRouteSupport],
) -> LabRunnerSupportSummary {
    let supported_labels = lab_runner_supported_labels(composed_support);
    let hint_labels = lab_runner_supported_hint_labels(composed_support);

    LabRunnerSupportSummary {
        unsupported_message: format!(
            "--runner is only supported for commands with portable Lab offload support: {}",
            human_join(&supported_labels)
        ),
        hint: format!("Current Lab offload support: {}.", human_join(&hint_labels)),
        supported_labels,
    }
}

fn lab_runner_supported_hint_labels(
    composed_support: &[LabCommandRouteSupport],
) -> Vec<&'static str> {
    let mut labels = COMMAND_SPECS
        .iter()
        .flat_map(|spec| spec.lab_support_summary.iter())
        .map(|summary| summary.hint_label)
        .collect::<Vec<_>>();
    labels.extend(composed_support.iter().map(|support| support.hint_label));
    labels
}

fn human_join(labels: &[&str]) -> String {
    match labels {
        [] => String::new(),
        [label] => (*label).to_string(),
        [first, second] => format!("{first} and {second}"),
        [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
    }
}
