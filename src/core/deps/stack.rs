use crate::core::component::{self, Component, DependencyStackEdge};
use crate::core::deps::{update, DependencyUpdateOptions};
use crate::core::plan::{HomeboyPlan, PlanKind, PlanStep, PlanValues};
use crate::core::{Error, Result};
use crate::extensions::deps_provider;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DependencyStackStatus {
    pub edge_count: usize,
    pub edges: Vec<DependencyStackEdgeStatus>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DependencyStackEdgeStatus {
    pub declaring_component_id: String,
    pub upstream: String,
    pub downstream: String,
    pub package: String,
    pub update_command: String,
    pub rebuild: bool,
    pub post_update: Vec<String>,
    pub test: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DependencyStackPlan {
    #[serde(flatten)]
    pub plan: HomeboyPlan,
    pub upstream: String,
    pub step_count: usize,
    pub steps: Vec<DependencyStackPlanStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DependencyStackPlanStep {
    pub sequence: usize,
    pub declaring_component_id: String,
    pub upstream: String,
    pub downstream: String,
    pub downstream_path: String,
    pub package: String,
    pub update_command: String,
    pub rebuild: bool,
    pub post_update: Vec<String>,
    pub test: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DependencyStackApplyResult {
    pub upstream: String,
    pub dry_run: bool,
    pub step_count: usize,
    pub steps: Vec<DependencyStackApplyStep>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DependencyStackApplyStep {
    pub sequence: usize,
    pub downstream: String,
    pub command_results: Vec<DependencyStackCommandResult>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DependencyStackCommandResult {
    pub phase: String,
    pub command: String,
    pub skipped: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

pub fn stack_status() -> Result<DependencyStackStatus> {
    let mut edges = Vec::new();
    for (component, edge) in stack_edges_from_components(&component::list()?)? {
        edges.push(edge_status(&component, &edge));
    }

    edges.sort_by(|a, b| {
        a.upstream
            .cmp(&b.upstream)
            .then_with(|| a.downstream.cmp(&b.downstream))
            .then_with(|| a.package.cmp(&b.package))
    });

    Ok(DependencyStackStatus {
        edge_count: edges.len(),
        edges,
    })
}

pub fn stack_plan(upstream: &str) -> Result<DependencyStackPlan> {
    let components = component::list()?;
    stack_plan_from_components(upstream, &components)
}

pub fn stack_apply(
    upstream: &str,
    constraint: Option<&str>,
    dry_run: bool,
    install: bool,
    rebuild: bool,
) -> Result<DependencyStackApplyResult> {
    let plan = stack_plan(upstream)?;
    stack_apply_plan(plan, constraint, dry_run, install, rebuild)
}

pub fn stack_apply_plan(
    plan: DependencyStackPlan,
    constraint: Option<&str>,
    dry_run: bool,
    install: bool,
    rebuild: bool,
) -> Result<DependencyStackApplyResult> {
    let mut steps = Vec::new();

    for step in plan.planned_steps() {
        let mut command_results = Vec::new();
        if step.uses_default_update_command() {
            command_results.push(run_default_update_step(
                &step, constraint, dry_run, install,
            )?);
        } else {
            command_results.push(run_stack_command(
                "update",
                &step.update_command,
                &step.downstream_path,
                dry_run,
            )?);
        }
        if rebuild || step.rebuild {
            command_results.push(run_stack_command(
                "rebuild",
                &rebuild_command(&step),
                &step.downstream_path,
                dry_run,
            )?);
        }
        for command in &step.post_update {
            command_results.push(run_stack_command(
                "post_update",
                command,
                &step.downstream_path,
                dry_run,
            )?);
        }
        for command in &step.test {
            command_results.push(run_stack_command(
                "test",
                command,
                &step.downstream_path,
                dry_run,
            )?);
        }
        steps.push(DependencyStackApplyStep {
            sequence: step.sequence,
            downstream: step.downstream.clone(),
            command_results,
        });
    }

    Ok(DependencyStackApplyResult {
        upstream: plan.upstream,
        dry_run,
        step_count: steps.len(),
        steps,
    })
}

pub fn stack_plan_from_components(
    upstream: &str,
    components: &[Component],
) -> Result<DependencyStackPlan> {
    let mut steps = Vec::new();
    let mut queue = vec![upstream.to_string()];
    let mut visited_edges = BTreeSet::new();
    let stack_edges = stack_edges_from_components(components)?;
    let component_paths: BTreeMap<String, String> = components
        .iter()
        .map(|component| (component.id.clone(), component.local_path.clone()))
        .collect();

    while let Some(current_upstream) = queue.pop() {
        let mut matching_edges = Vec::new();
        for (component, edge) in &stack_edges {
            if edge.upstream == current_upstream {
                matching_edges.push((component, edge));
            }
        }
        matching_edges.sort_by(|(a_component, a_edge), (b_component, b_edge)| {
            a_edge
                .downstream
                .cmp(&b_edge.downstream)
                .then_with(|| a_edge.package.cmp(&b_edge.package))
                .then_with(|| a_component.id.cmp(&b_component.id))
        });

        for (component, edge) in matching_edges {
            let key = format!("{}>{}:{}", edge.upstream, edge.downstream, edge.package);
            if !visited_edges.insert(key) {
                continue;
            }
            let Some(downstream_path) = component_paths.get(&edge.downstream) else {
                return Err(Error::validation_invalid_argument(
                    "dependency_stack.downstream",
                    format!(
                        "Dependency stack edge {} -> {} references an unknown downstream component",
                        edge.upstream, edge.downstream
                    ),
                    Some(edge.downstream.clone()),
                    Some(vec![
                        "Add the downstream component to Homeboy inventory".to_string(),
                        "Or fix dependency_stack[].downstream in homeboy.json".to_string(),
                    ]),
                ));
            };
            steps.push(DependencyStackPlanStep {
                sequence: steps.len() + 1,
                declaring_component_id: component.id.clone(),
                upstream: edge.upstream.clone(),
                downstream: edge.downstream.clone(),
                downstream_path: downstream_path.clone(),
                package: edge.package.clone(),
                update_command: update_command(edge, downstream_path),
                rebuild: edge.rebuild,
                post_update: edge.post_update.clone(),
                test: edge.test.clone(),
            });
            queue.push(edge.downstream.clone());
        }
    }

    Ok(DependencyStackPlan::new(upstream, steps))
}

fn stack_edges_from_components(
    components: &[Component],
) -> Result<Vec<(Component, DependencyStackEdge)>> {
    let mut edges = Vec::new();
    let mut seen = BTreeSet::new();
    let mut snapshots = BTreeMap::new();
    let mut identity_to_component = BTreeMap::new();

    for component in components {
        let path = PathBuf::from(shellexpand::tilde(&component.local_path).as_ref());
        let snapshot = deps_provider::dependency_provider_snapshot(component, &path)?;
        for identity in &snapshot.identities {
            identity_to_component
                .entry(identity.clone())
                .or_insert_with(|| component.id.clone());
        }
        snapshots.insert(component.id.clone(), snapshot);
    }

    for component in components {
        for edge in &component.dependency_stack {
            let key = edge_key(edge);
            if seen.insert(key) {
                edges.push((component.clone(), edge.clone()));
            }
        }
    }

    for component in components {
        let Some(snapshot) = snapshots.get(&component.id) else {
            continue;
        };
        for package in &snapshot.packages {
            if package.constraint.is_none() {
                continue;
            }
            let Some(upstream) = identity_to_component.get(&package.name) else {
                continue;
            };
            if upstream == &component.id {
                continue;
            }
            let edge = DependencyStackEdge {
                upstream: upstream.clone(),
                downstream: component.id.clone(),
                package: package.name.clone(),
                update: None,
                rebuild: false,
                post_update: Vec::new(),
                test: Vec::new(),
            };
            let key = edge_key(&edge);
            if seen.insert(key) {
                edges.push((component.clone(), edge));
            }
        }
    }

    Ok(edges)
}

fn edge_key(edge: &DependencyStackEdge) -> String {
    format!("{}>{}:{}", edge.upstream, edge.downstream, edge.package)
}

impl DependencyStackPlan {
    pub fn new(upstream: impl Into<String>, steps: Vec<DependencyStackPlanStep>) -> Self {
        let upstream = upstream.into();
        let plan = HomeboyPlan::builder_for_component(PlanKind::DependencyStack, upstream.clone())
            .steps(steps.iter().map(stack_step))
            .summarize()
            .build();
        let steps = stack_steps_from_plan(&plan);

        Self {
            step_count: plan
                .summary
                .as_ref()
                .map(|summary| summary.total_steps)
                .unwrap_or_else(|| plan.steps.len()),
            plan,
            upstream,
            steps,
        }
    }

    pub fn planned_steps(&self) -> Vec<DependencyStackPlanStep> {
        stack_steps_from_plan(&self.plan)
    }
}

fn stack_step(step: &DependencyStackPlanStep) -> PlanStep {
    PlanStep::ready(
        format!("deps.stack.{:03}.{}", step.sequence, step.downstream),
        "deps.stack.update_downstream",
    )
    .label(format!(
        "Update {} in {} from {}",
        step.package, step.downstream, step.upstream
    ))
    .scope(vec![step.downstream.clone()])
    .inputs(
        PlanValues::new()
            .string(
                "declaring_component_id",
                step.declaring_component_id.clone(),
            )
            .string("upstream", step.upstream.clone())
            .string("downstream", step.downstream.clone())
            .string("downstream_path", step.downstream_path.clone())
            .string("package", step.package.clone())
            .string("update_command", step.update_command.clone())
            .json("rebuild", step.rebuild)
            .json("post_update", &step.post_update)
            .json("test", &step.test)
            .json("stack_step", step),
    )
    .build()
}

fn stack_steps_from_plan(plan: &HomeboyPlan) -> Vec<DependencyStackPlanStep> {
    plan.steps
        .iter()
        .filter_map(|step| step.input_as("stack_step"))
        .collect()
}

fn edge_status(component: &Component, edge: &DependencyStackEdge) -> DependencyStackEdgeStatus {
    DependencyStackEdgeStatus {
        declaring_component_id: component.id.clone(),
        upstream: edge.upstream.clone(),
        downstream: edge.downstream.clone(),
        package: edge.package.clone(),
        update_command: update_command(edge, &component.local_path),
        rebuild: edge.rebuild,
        post_update: edge.post_update.clone(),
        test: edge.test.clone(),
    }
}

impl DependencyStackPlanStep {
    fn uses_default_update_command(&self) -> bool {
        self.update_command == update_command_for(&self.package, &self.downstream_path)
    }
}

fn update_command(edge: &DependencyStackEdge, downstream_path: &str) -> String {
    edge.update
        .clone()
        .unwrap_or_else(|| update_command_for(&edge.package, downstream_path))
}

fn update_command_for(package: &str, downstream_path: &str) -> String {
    update_command_for_options(package, downstream_path, None, true)
}

fn update_command_for_options(
    package: &str,
    downstream_path: &str,
    constraint: Option<&str>,
    install: bool,
) -> String {
    let mut command = format!(
        "homeboy deps update {} --path {}",
        shell_word(package),
        shell_word(downstream_path)
    );
    if let Some(constraint) = constraint {
        command.push_str(" --to ");
        command.push_str(&shell_word(constraint));
    }
    if !install {
        command.push_str(" --no-install");
    }
    command
}

fn rebuild_command(step: &DependencyStackPlanStep) -> String {
    format!(
        "homeboy build {} --path {}",
        shell_word(&step.downstream),
        shell_word(&step.downstream_path)
    )
}

fn run_default_update_step(
    step: &DependencyStackPlanStep,
    constraint: Option<&str>,
    dry_run: bool,
    install: bool,
) -> Result<DependencyStackCommandResult> {
    let command =
        update_command_for_options(&step.package, &step.downstream_path, constraint, install);
    if dry_run {
        return Ok(DependencyStackCommandResult {
            phase: "update".to_string(),
            command,
            skipped: true,
            status: None,
            stdout: String::new(),
            stderr: String::new(),
        });
    }

    let result = update(
        Some(&step.downstream),
        Some(&step.downstream_path),
        &step.package,
        constraint,
        DependencyUpdateOptions {
            install,
            rebuild: false,
        },
    )?;
    let stdout = serde_json::to_string(&result).map_err(|e| {
        Error::internal_json(
            e.to_string(),
            Some("serialize deps stack update".to_string()),
        )
    })?;

    Ok(DependencyStackCommandResult {
        phase: "update".to_string(),
        command,
        skipped: false,
        status: Some(0),
        stdout,
        stderr: String::new(),
    })
}

fn run_stack_command(
    phase: &str,
    command: &str,
    cwd: &str,
    dry_run: bool,
) -> Result<DependencyStackCommandResult> {
    if dry_run {
        return Ok(DependencyStackCommandResult {
            phase: phase.to_string(),
            command: command.to_string(),
            skipped: true,
            status: None,
            stdout: String::new(),
            stderr: String::new(),
        });
    }

    let output = Command::new("sh")
        .args(["-c", command])
        .current_dir(cwd)
        .output()
        .map_err(|e| Error::internal_io(e.to_string(), Some(format!("run {phase} command"))))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() {
        return Err(Error::validation_invalid_argument(
            "dependency_stack.command",
            format!(
                "Dependency stack {phase} command failed with status {}: {}",
                output.status,
                first_non_empty_line(&stderr)
                    .or_else(|| first_non_empty_line(&stdout))
                    .unwrap_or("no output")
            ),
            Some(command.to_string()),
            Some(vec![format!("Run manually in {cwd}: {command}")]),
        ));
    }

    Ok(DependencyStackCommandResult {
        phase: phase.to_string(),
        command: command.to_string(),
        skipped: false,
        status: output.status.code(),
        stdout,
        stderr,
    })
}

fn shell_word(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | ':' | '@'))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn first_non_empty_line(output: &str) -> Option<&str> {
    output.lines().find(|line| !line.trim().is_empty())
}
