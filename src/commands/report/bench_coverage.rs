use clap::Args;
use serde::Serialize;

use homeboy::core::component::{self, Component};
use homeboy::core::engine::execution_context::{self, ResolveOptions};
use homeboy::core::engine::run_dir::RunDir;
use homeboy::core::extension::bench::{
    run_bench_list_workflow, BenchListWorkflowArgs, BenchScenario,
};
use homeboy::core::extension::{resolve_extension_for_capability, ExtensionCapability};

use crate::commands::escape_markdown_table_cell;
use crate::commands::utils::args::{ExtensionOverrideArgs, PositionalComponentArgs, SettingArgs};

#[derive(Args, Debug, Clone)]
pub struct BenchCoverageArgs {
    #[command(flatten)]
    pub comp: PositionalComponentArgs,

    #[command(flatten)]
    pub extension_override: ExtensionOverrideArgs,

    #[command(flatten)]
    pub setting_args: SettingArgs,

    /// Inspect every registered component instead of the selected component.
    #[arg(long)]
    pub all: bool,

    /// Output format.
    #[arg(long, value_parser = ["markdown", "json"], default_value = "markdown")]
    pub format: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BenchCoverageReport {
    pub command: String,
    pub hot_commands: Vec<String>,
    pub totals: BenchCoverageTotals,
    pub components: Vec<ComponentBenchCoverage>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BenchCoverageTotals {
    pub components: usize,
    pub components_with_bench: usize,
    pub scenarios: usize,
    pub covered_hot_paths: usize,
    pub missing_hot_paths: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComponentBenchCoverage {
    pub component_id: String,
    pub has_bench_capability: bool,
    pub scenario_count: usize,
    pub scenarios: Vec<BenchCoverageScenario>,
    pub hot_paths: Vec<HotPathCoverage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BenchCoverageScenario {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    pub source: String,
    pub covers: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HotPathCoverage {
    pub command: String,
    pub covered: bool,
    pub scenarios: Vec<String>,
}

const HOT_COMMANDS: &[&str] = &[
    "audit", "bench", "lint", "test", "trace", "refactor", "runner", "offload",
];

pub fn run(args: &BenchCoverageArgs) -> homeboy::core::Result<BenchCoverageReport> {
    let components = if args.all {
        component::inventory()?
    } else {
        vec![args.comp.load()?]
    };

    let mut reports = Vec::new();
    for component in components {
        reports.push(component_report(&component, args)?);
    }

    let totals = totals(&reports);
    Ok(BenchCoverageReport {
        command: "report.bench-coverage".to_string(),
        hot_commands: HOT_COMMANDS
            .iter()
            .map(|command| command.to_string())
            .collect(),
        totals,
        components: reports,
    })
}

fn component_report(
    component: &Component,
    args: &BenchCoverageArgs,
) -> homeboy::core::Result<ComponentBenchCoverage> {
    if !has_bench_capability(component, &args.extension_override.extensions) {
        return Ok(empty_component(component.id.clone(), false, None));
    }

    let mut resolve_options = ResolveOptions::with_capability_and_json(
        &component.id,
        args.comp.path.clone(),
        ExtensionCapability::Bench,
        args.setting_args.setting.clone(),
        args.setting_args.setting_json.clone(),
    );
    resolve_options.extension_overrides = args.extension_override.extensions.clone();

    let ctx = match execution_context::resolve_with_component(
        &resolve_options,
        Some(component.clone()),
    ) {
        Ok(ctx) => ctx,
        Err(error) => {
            return Ok(empty_capability_error(component, error));
        }
    };

    let run_dir = RunDir::create()?;
    let list = run_bench_list_workflow(
        &ctx.component,
        BenchListWorkflowArgs {
            component_label: component.id.clone(),
            component_id: ctx.component_id.clone(),
            path_override: args.comp.path.clone(),
            settings: ctx.resolved_settings().string_overrides(),
            settings_json: ctx.resolved_settings().json_overrides(),
            passthrough_args: Vec::new(),
            scenario_ids: Vec::new(),
            extra_workloads: Vec::new(),
        },
        &run_dir,
    );

    let list = match list {
        Ok(list) => list,
        Err(error) => {
            return Ok(empty_capability_error(component, error));
        }
    };

    Ok(build_component_report(
        component.id.clone(),
        true,
        &list.scenarios,
        None,
    ))
}

fn empty_capability_error(
    component: &Component,
    error: impl std::fmt::Display,
) -> ComponentBenchCoverage {
    empty_component(component.id.clone(), true, Some(error.to_string()))
}

fn has_bench_capability(component: &Component, extension_overrides: &[String]) -> bool {
    if component.has_script(ExtensionCapability::Bench) {
        return true;
    }

    let mut component = component.clone();
    if !extension_overrides.is_empty() {
        let existing = component.extensions.clone().unwrap_or_default();
        component.extensions = Some(
            extension_overrides
                .iter()
                .map(|id| (id.clone(), existing.get(id).cloned().unwrap_or_default()))
                .collect(),
        );
    }

    resolve_extension_for_capability(&component, ExtensionCapability::Bench).is_ok()
}

fn empty_component(
    component_id: String,
    has_bench_capability: bool,
    error: Option<String>,
) -> ComponentBenchCoverage {
    build_component_report(component_id, has_bench_capability, &[], error)
}

fn build_component_report(
    component_id: String,
    has_bench_capability: bool,
    scenarios: &[BenchScenario],
    error: Option<String>,
) -> ComponentBenchCoverage {
    let scenario_rows = scenarios
        .iter()
        .map(|scenario| {
            let covers = HOT_COMMANDS
                .iter()
                .filter(|command| scenario_covers(command, scenario))
                .map(|command| command.to_string())
                .collect::<Vec<_>>();
            BenchCoverageScenario {
                id: scenario.id.clone(),
                file: scenario.file.clone(),
                source: scenario
                    .source
                    .clone()
                    .unwrap_or_else(|| "extension".to_string()),
                covers,
            }
        })
        .collect::<Vec<_>>();

    let hot_paths = HOT_COMMANDS
        .iter()
        .map(|command| {
            let scenarios = scenario_rows
                .iter()
                .filter(|scenario| scenario.covers.iter().any(|covered| covered == command))
                .map(|scenario| scenario.id.clone())
                .collect::<Vec<_>>();
            HotPathCoverage {
                command: command.to_string(),
                covered: !scenarios.is_empty(),
                scenarios,
            }
        })
        .collect::<Vec<_>>();

    ComponentBenchCoverage {
        component_id,
        has_bench_capability,
        scenario_count: scenario_rows.len(),
        scenarios: scenario_rows,
        hot_paths,
        error,
    }
}

fn scenario_covers(command: &str, scenario: &BenchScenario) -> bool {
    let command = command.to_ascii_lowercase();
    let mut identity_haystack = scenario.id.to_ascii_lowercase();
    for tag in &scenario.tags {
        identity_haystack.push(' ');
        identity_haystack.push_str(&tag.to_ascii_lowercase());
    }
    if command == "bench" {
        return identity_haystack.contains("bench");
    }

    let mut haystack = identity_haystack;
    if let Some(file) = &scenario.file {
        haystack.push(' ');
        haystack.push_str(&file.to_ascii_lowercase());
    }
    if let Some(source) = &scenario.source {
        haystack.push(' ');
        haystack.push_str(&source.to_ascii_lowercase());
    }

    match command.as_str() {
        "offload" => haystack.contains("offload") || haystack.contains("lab"),
        _ => haystack.contains(&command),
    }
}

fn totals(components: &[ComponentBenchCoverage]) -> BenchCoverageTotals {
    let components_with_bench = components
        .iter()
        .filter(|component| component.has_bench_capability)
        .count();
    let scenarios = components
        .iter()
        .map(|component| component.scenario_count)
        .sum();
    let covered_hot_paths = components
        .iter()
        .flat_map(|component| &component.hot_paths)
        .filter(|path| path.covered)
        .count();
    let missing_hot_paths = components
        .iter()
        .flat_map(|component| &component.hot_paths)
        .filter(|path| !path.covered)
        .count();

    BenchCoverageTotals {
        components: components.len(),
        components_with_bench,
        scenarios,
        covered_hot_paths,
        missing_hot_paths,
    }
}

pub fn render_markdown(report: &BenchCoverageReport) -> String {
    let mut out = String::new();
    out.push_str("# Bench Coverage\n\n");
    out.push_str(&format!(
        "- **Components:** `{}`\n- **Components with bench:** `{}`\n- **Scenarios:** `{}`\n- **Covered hot paths:** `{}`\n- **Missing hot paths:** `{}`\n\n",
        report.totals.components,
        report.totals.components_with_bench,
        report.totals.scenarios,
        report.totals.covered_hot_paths,
        report.totals.missing_hot_paths,
    ));

    for component in &report.components {
        out.push_str(&format!("## `{}`\n\n", component.component_id));
        if let Some(error) = &component.error {
            out.push_str(&format!("- **Discovery error:** {}\n\n", error));
        }
        out.push_str("| Hot path | Covered | Scenarios |\n");
        out.push_str("| --- | --- | --- |\n");
        for path in &component.hot_paths {
            let scenarios = if path.scenarios.is_empty() {
                "-".to_string()
            } else {
                path.scenarios
                    .iter()
                    .map(|scenario| format!("`{}`", escape_markdown_table_cell(scenario)))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            out.push_str(&format!(
                "| `{}` | {} | {} |\n",
                escape_markdown_table_cell(&path.command),
                if path.covered { "yes" } else { "no" },
                scenarios
            ));
        }
        out.push('\n');

        if !component.scenarios.is_empty() {
            out.push_str("**Discovered scenarios**\n\n");
            for scenario in &component.scenarios {
                let file = scenario.file.as_deref().unwrap_or("-");
                let covers = if scenario.covers.is_empty() {
                    "-".to_string()
                } else {
                    scenario.covers.join(", ")
                };
                out.push_str(&format!(
                    "- `{}` ({}, `{}`): {}\n",
                    scenario.id, scenario.source, file, covers
                ));
            }
            out.push('\n');
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use homeboy::core::extension::bench::{BenchMetrics, BenchScenario};

    use super::*;

    fn scenario(id: &str) -> BenchScenario {
        BenchScenario {
            id: id.to_string(),
            file: Some(format!("tests/bench/{id}.rs")),
            source: Some("in_tree".to_string()),
            default_iterations: None,
            tags: Vec::new(),
            iterations: 0,
            metrics: BenchMetrics::default(),
            metric_groups: BTreeMap::new(),
            timeline: Vec::new(),
            span_definitions: Vec::new(),
            span_results: Vec::new(),
            gates: Vec::new(),
            gate_results: Vec::new(),
            metadata: BTreeMap::new(),
            provenance: Default::default(),
            passed: true,
            memory: None,
            artifacts: BTreeMap::new(),
            diagnostics: Vec::new(),
            runs: None,
            runs_summary: None,
        }
    }

    #[test]
    fn audit_self_covers_audit_and_leaves_other_hot_paths_missing() {
        let report =
            build_component_report("homeboy".to_string(), true, &[scenario("audit-self")], None);

        assert!(report
            .hot_paths
            .iter()
            .any(|path| path.command == "audit" && path.covered));
        assert!(report
            .hot_paths
            .iter()
            .any(|path| path.command == "lint" && !path.covered));
    }

    #[test]
    fn markdown_lists_missing_hot_paths() {
        let report = BenchCoverageReport {
            command: "report.bench-coverage".to_string(),
            hot_commands: HOT_COMMANDS
                .iter()
                .map(|command| command.to_string())
                .collect(),
            totals: BenchCoverageTotals {
                components: 1,
                components_with_bench: 1,
                scenarios: 1,
                covered_hot_paths: 1,
                missing_hot_paths: 7,
            },
            components: vec![build_component_report(
                "homeboy".to_string(),
                true,
                &[scenario("audit-self")],
                None,
            )],
        };

        let markdown = render_markdown(&report);
        assert!(markdown.contains("`audit-self`"));
        assert!(markdown.contains("| `lint` | no | - |"));
    }
}
