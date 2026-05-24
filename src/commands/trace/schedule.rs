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
}

impl TraceRunPlanEntry {
    fn new(index: usize, group: &str, iteration: usize) -> Self {
        Self {
            plan: trace_run_entry_plan(index, group, iteration),
        }
    }

    pub(super) fn index(&self) -> usize {
        plan_usize_input(&self.plan, "index")
    }

    pub(super) fn group(&self) -> &str {
        plan_str_input(&self.plan, "group")
    }

    pub(super) fn iteration(&self) -> usize {
        plan_usize_input(&self.plan, "iteration")
    }
}

pub(crate) fn plan_trace_run_order(
    repeat: usize,
    schedule: TraceSchedule,
    groups: &[&str],
) -> Vec<TraceRunPlanEntry> {
    let mut entries = Vec::new();
    let mut push_entry = |group: &str, iteration: usize| {
        entries.push(TraceRunPlanEntry::new(entries.len() + 1, group, iteration));
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
        .number("index", index as u64)
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

fn plan_usize_input(plan: &HomeboyPlan, key: &str) -> usize {
    plan.inputs
        .get(key)
        .and_then(|value| value.as_u64())
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or_else(|| panic!("trace plan missing numeric {key}"))
}

fn plan_str_input<'a>(plan: &'a HomeboyPlan, key: &str) -> &'a str {
    plan.inputs
        .get(key)
        .and_then(|value| value.as_str())
        .unwrap_or_else(|| panic!("trace plan missing string {key}"))
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
