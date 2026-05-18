use clap::ValueEnum;
use homeboy::core::plan::{HomeboyPlan, PlanKind, PlanStep, PlanValues};
use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum TraceSchedule {
    Grouped,
    Interleaved,
}

impl TraceSchedule {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Grouped => "grouped",
            Self::Interleaved => "interleaved",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TraceRunPlanEntry {
    pub(super) plan: HomeboyPlan,
    pub(super) index: usize,
    pub(super) group: String,
    pub(super) iteration: usize,
}

pub(crate) fn plan_trace_run_order(
    repeat: usize,
    schedule: TraceSchedule,
    groups: &[&str],
) -> Vec<TraceRunPlanEntry> {
    let mut entries = Vec::new();
    let mut push_entry = |group: &str, iteration: usize| {
        entries.push(TraceRunPlanEntry {
            plan: trace_run_entry_plan(entries.len() + 1, group, iteration),
            index: entries.len() + 1,
            group: group.to_string(),
            iteration,
        });
    };
    match schedule {
        TraceSchedule::Grouped => {
            for group in groups {
                for iteration in 1..=repeat {
                    push_entry(group, iteration);
                }
            }
        }
        TraceSchedule::Interleaved => {
            for iteration in 1..=repeat {
                for group in groups {
                    push_entry(group, iteration);
                }
            }
        }
    }
    entries
}

fn trace_run_entry_plan(index: usize, group: &str, iteration: usize) -> HomeboyPlan {
    let inputs = PlanValues::new()
        .string("group", group)
        .number("iteration", iteration as u64);

    HomeboyPlan::builder_for_description(PlanKind::Trace, format!("{group} {iteration}"))
        .mode("run_order")
        .inputs(inputs.clone())
        .steps(vec![PlanStep::ready(
            format!("trace.run.{index}"),
            "trace.run",
        )
        .label(format!("Run trace {group} iteration {iteration}"))
        .scope(vec![group.to_string()])
        .inputs(inputs)
        .build()])
        .summarize()
        .build()
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, ValueEnum)]
pub enum TraceVariantMatrixMode {
    #[default]
    None,
    Single,
    Cumulative,
}

impl TraceVariantMatrixMode {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Single => "single",
            Self::Cumulative => "cumulative",
        }
    }
}
