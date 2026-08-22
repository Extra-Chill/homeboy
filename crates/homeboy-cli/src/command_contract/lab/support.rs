//! User-facing summaries of commands that support Lab runners.

use std::collections::BTreeSet;

use crate::command_contract::spec::{CommandLabSupportSummary, COMMAND_SPECS};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabRunnerSupportSummary {
    pub supported_labels: Vec<&'static str>,
    pub unsupported_message: String,
    pub hint: String,
}

pub(crate) fn lab_runner_supported_labels() -> Vec<&'static str> {
    lab_support_summaries()
        .map(|summary| summary.message_label)
        .collect()
}

pub(crate) fn lab_runner_support_summary() -> LabRunnerSupportSummary {
    let supported_labels = lab_runner_supported_labels();
    let hint_labels = lab_runner_supported_hint_labels();

    LabRunnerSupportSummary {
        unsupported_message: format!(
            "--runner is only supported for commands with portable Lab offload support: {}",
            human_join(&supported_labels)
        ),
        hint: format!("Current Lab offload support: {}.", human_join(&hint_labels)),
        supported_labels,
    }
}

fn lab_runner_supported_hint_labels() -> Vec<&'static str> {
    lab_support_summaries()
        .map(|summary| summary.hint_label)
        .collect()
}

fn lab_support_summaries() -> impl Iterator<Item = &'static CommandLabSupportSummary> {
    COMMAND_SPECS
        .iter()
        .flat_map(|spec| spec.lab_support_summary.iter())
}

fn human_join(labels: &[&str]) -> String {
    match labels {
        [] => String::new(),
        [label] => (*label).to_string(),
        [first, second] => format!("{first} and {second}"),
        [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
    }
}
