//! Generic JSONPath query over imported run artifact rows.
//!
//! Schema-blind: the engine never assumes anything about the shape of the
//! artifacts. The caller passes one or more JSONPath `--select` expressions
//! and an optional `--group-by` expression; we project, group, and emit
//! rows in JSON / table / CSV.
//!
//! No `--kind <slug>` registry. No domain vocabulary. The same primitive
//! works against `design-distribution.json`, `bench.json`, `finding-packets.json`,
//! or anything else.

use clap::{Args, ValueEnum};
use serde::Serialize;
use serde_json::Value;

use homeboy::core::observation::{ObservationStore, RunListFilter};
use homeboy::core::Error;

use super::common::{
    compile_jsonpath, distribution_share, eval_jsonpath, load_artifact_rows, ArtifactJsonRow,
    SkippedArtifactRow,
};
use super::{CmdResult, RunsOutput};

#[derive(Args, Clone, Debug)]
pub struct RunsQueryArgs {
    /// Component ID (matches the synthetic Homeboy run's component_id).
    #[arg(long = "component")]
    pub component_id: Option<String>,
    /// Run kind (e.g. `gh-actions`). Defaults to all kinds.
    #[arg(long)]
    pub kind: Option<String>,
    /// Restrict to runs started within this duration (e.g. 24h, 7d).
    #[arg(long)]
    pub since: Option<String>,
    /// One or more JSONPath expressions to project. Comma-separated.
    /// Example: `--select '$.theme,$.fonts[*].family'`
    #[arg(long, value_delimiter = ',', required = true)]
    pub select: Vec<String>,
    /// Optional JSONPath expression to group by.
    #[arg(long = "group-by")]
    pub group_by: Option<String>,
    /// When set with `--group-by`, emit `(group, count)` instead of full rows.
    #[arg(long, default_value_t = false)]
    pub count: bool,
    /// Output format.
    #[arg(long, value_enum, default_value_t = QueryFormat::Json)]
    pub format: QueryFormat,
    /// Maximum runs to inspect.
    #[arg(long, default_value_t = 200)]
    pub limit: i64,
}

#[derive(ValueEnum, Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum QueryFormat {
    Json,
    Table,
    Csv,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct RunsQueryOutput {
    pub command: &'static str,
    pub filters: RunsQueryFilters,
    pub select: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_by: Option<String>,
    pub matched_artifact_count: usize,
    pub skipped_artifact_count: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skipped_artifacts: Vec<SkippedArtifactRow>,
    /// Flat row list (when `--group-by` is absent or `--count` is false and
    /// the caller wants the raw projection). Each entry has one value per
    /// `--select` expression.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rows: Vec<QueryRow>,
    /// Group counts (when `--group-by --count` is set).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<QueryGroup>,
    /// Tabular text (only when `--format=table` is requested).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table: Option<String>,
    /// CSV text (only when `--format=csv` is requested).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub csv: Option<String>,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct RunsQueryFilters {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    pub limit: i64,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct QueryRow {
    pub run_id: String,
    pub artifact_kind: String,
    /// One projected value per `--select` expression. Each entry is the JSON
    /// matched at that path (as-is) or `null` when nothing matched.
    pub values: Vec<Value>,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct QueryGroup {
    pub group: String,
    pub count: usize,
}

pub fn runs_query(args: RunsQueryArgs) -> CmdResult<RunsOutput> {
    if args.select.iter().any(|s| s.trim().is_empty()) {
        return Err(Error::validation_invalid_argument(
            "select",
            "--select expressions must not be empty",
            None,
            None,
        ));
    }
    let select_paths = args
        .select
        .iter()
        .map(|expr| compile_jsonpath(expr).map(|p| (expr.clone(), p)))
        .collect::<homeboy::core::Result<Vec<_>>>()?;
    let group_path = args
        .group_by
        .as_deref()
        .map(|expr| compile_jsonpath(expr).map(|p| (expr.to_string(), p)))
        .transpose()?;

    let store = ObservationStore::open_initialized()?;
    let filter = RunListFilter {
        kind: args.kind.clone(),
        component_id: args.component_id.clone(),
        status: None,
        rig_id: None,
        limit: Some(args.limit.clamp(1, 5000)),
    };
    let loaded = load_artifact_rows(&store, filter, args.since.as_deref())?;
    let rows = loaded.rows;

    let projected: Vec<QueryRow> = rows
        .iter()
        .map(|row| project_row(row, &select_paths))
        .collect();

    let mut output = RunsQueryOutput {
        command: "runs.query",
        filters: RunsQueryFilters {
            component_id: args.component_id.clone(),
            kind: args.kind.clone(),
            since: args.since.clone(),
            limit: args.limit,
        },
        select: args.select.clone(),
        group_by: args.group_by.clone(),
        matched_artifact_count: rows.len(),
        skipped_artifact_count: loaded.skipped.len(),
        skipped_artifacts: loaded.skipped,
        rows: Vec::new(),
        groups: Vec::new(),
        table: None,
        csv: None,
    };

    if let Some((_, group_path)) = group_path {
        let groups = group_rows(&rows, &group_path);
        if args.count {
            output.groups = groups;
        } else {
            // Group + select-without-count is still a meaningful projection:
            // we emit the projected rows but tag each with its group label.
            // For v1 the simplest behavior is "ignore group_by unless --count
            // is set" — keep the row list as the primary surface.
            output.rows = projected.clone();
            output.groups = groups;
        }
    } else {
        output.rows = projected.clone();
    }

    match args.format {
        QueryFormat::Json => {}
        QueryFormat::Table => {
            output.table = Some(render_table(&output, &args.select));
        }
        QueryFormat::Csv => {
            output.csv = Some(render_csv(&output, &args.select));
        }
    }

    Ok((RunsOutput::Query(output), 0))
}

fn project_row(
    row: &ArtifactJsonRow,
    select_paths: &[(String, serde_json_path::JsonPath)],
) -> QueryRow {
    let values = select_paths
        .iter()
        .map(|(_, path)| {
            let matches = eval_jsonpath(path, &row.json);
            match matches.len() {
                0 => Value::Null,
                1 => matches.into_iter().next().unwrap(),
                _ => Value::Array(matches),
            }
        })
        .collect();
    QueryRow {
        run_id: row.run.id.clone(),
        artifact_kind: row.artifact_kind.clone(),
        values,
    }
}

fn group_rows(rows: &[ArtifactJsonRow], group_path: &serde_json_path::JsonPath) -> Vec<QueryGroup> {
    // Delegate the tally + sort to the shared distribution helper so query
    // and drift surfaces never drift from each other on grouping semantics.
    distribution_share(rows, group_path)
        .values
        .into_iter()
        .map(|(group, count, _share)| QueryGroup { group, count })
        .collect()
}

// ── Table rendering ─────────────────────────────────────────────────────────

fn render_table(output: &RunsQueryOutput, select: &[String]) -> String {
    if !output.groups.is_empty() && output.rows.is_empty() {
        return render_groups_table(&output.groups);
    }
    render_rows_table(&output.rows, select)
}

fn render_groups_table(groups: &[QueryGroup]) -> String {
    let mut lines = vec!["group | count".to_string(), "---   | ----".to_string()];
    for group in groups {
        lines.push(format!("{} | {}", group.group, group.count));
    }
    lines.join("\n")
}

fn render_rows_table(rows: &[QueryRow], select: &[String]) -> String {
    let mut header = vec!["run_id".to_string(), "artifact".to_string()];
    header.extend(select.iter().cloned());
    let mut lines = vec![header.join(" | ")];
    lines.push(vec!["---".to_string(); header.len()].join(" | "));
    for row in rows {
        let mut cells = vec![row.run_id.clone(), row.artifact_kind.clone()];
        for value in &row.values {
            cells.push(json_to_cell(value));
        }
        lines.push(cells.join(" | "));
    }
    lines.join("\n")
}

fn render_csv(output: &RunsQueryOutput, select: &[String]) -> String {
    if !output.groups.is_empty() && output.rows.is_empty() {
        let mut lines = vec!["group,count".to_string()];
        for group in &output.groups {
            lines.push(format!("{},{}", csv_escape(&group.group), group.count));
        }
        return lines.join("\n");
    }
    let mut header = vec!["run_id".to_string(), "artifact".to_string()];
    header.extend(select.iter().cloned());
    let mut lines = vec![header
        .iter()
        .map(|h| csv_escape(h))
        .collect::<Vec<_>>()
        .join(",")];
    for row in &output.rows {
        let mut cells = vec![csv_escape(&row.run_id), csv_escape(&row.artifact_kind)];
        for value in &row.values {
            cells.push(csv_escape(&json_to_cell(value)));
        }
        lines.push(cells.join(","));
    }
    lines.join("\n")
}

fn json_to_cell(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

fn csv_escape(raw: &str) -> String {
    if raw.contains(',') || raw.contains('"') || raw.contains('\n') {
        let escaped = raw.replace('"', "\"\"");
        format!("\"{escaped}\"")
    } else {
        raw.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_row(run_id: &str, kind: &str, json: Value) -> ArtifactJsonRow {
        ArtifactJsonRow {
            run: homeboy::core::observation::RunRecord {
                id: run_id.into(),
                kind: "gh-actions".into(),
                component_id: Some("homeboy".into()),
                started_at: "2026-05-04T00:00:00Z".into(),
                finished_at: None,
                status: "pass".into(),
                command: None,
                cwd: None,
                homeboy_version: None,
                git_sha: None,
                rig_id: None,
                metadata_json: Value::Null,
            },
            artifact_kind: kind.into(),
            artifact_path: "/dev/null".into(),
            json,
        }
    }

    #[test]
    fn project_row_returns_one_value_per_select() {
        let row = sample_row(
            "r1",
            "design-distribution",
            serde_json::json!({ "theme": "dark", "fonts": ["serif", "mono"] }),
        );
        let select = vec![
            ("$.theme".to_string(), compile_jsonpath("$.theme").unwrap()),
            (
                "$.fonts[*]".to_string(),
                compile_jsonpath("$.fonts[*]").unwrap(),
            ),
        ];
        let row = project_row(&row, &select);
        assert_eq!(row.values[0], Value::String("dark".into()));
        assert_eq!(row.values[1], serde_json::json!(["serif", "mono"]));
    }

    #[test]
    fn group_rows_counts_scalar_values_by_jsonpath() {
        let rows = vec![
            sample_row("a", "k", serde_json::json!({ "theme": "dark" })),
            sample_row("b", "k", serde_json::json!({ "theme": "dark" })),
            sample_row("c", "k", serde_json::json!({ "theme": "light" })),
        ];
        let path = compile_jsonpath("$.theme").unwrap();
        let groups = group_rows(&rows, &path);
        assert_eq!(groups[0].group, "dark");
        assert_eq!(groups[0].count, 2);
        assert_eq!(groups[1].group, "light");
        assert_eq!(groups[1].count, 1);
    }

    #[test]
    fn render_table_emits_header_and_pipe_separated_rows() {
        let output = RunsQueryOutput {
            command: "runs.query",
            filters: RunsQueryFilters {
                component_id: None,
                kind: None,
                since: None,
                limit: 200,
            },
            select: vec!["$.theme".into()],
            group_by: None,
            matched_artifact_count: 1,
            skipped_artifact_count: 0,
            skipped_artifacts: vec![],
            rows: vec![QueryRow {
                run_id: "r1".into(),
                artifact_kind: "design-distribution".into(),
                values: vec![Value::String("dark".into())],
            }],
            groups: vec![],
            table: None,
            csv: None,
        };
        let rendered = render_table(&output, &output.select);
        assert!(rendered.contains("run_id | artifact | $.theme"));
        assert!(rendered.contains("r1 | design-distribution | dark"));
    }

    #[test]
    fn render_csv_quotes_commas() {
        let row = QueryRow {
            run_id: "r,1".into(),
            artifact_kind: "design".into(),
            values: vec![Value::String("hello, world".into())],
        };
        let output = RunsQueryOutput {
            command: "runs.query",
            filters: RunsQueryFilters {
                component_id: None,
                kind: None,
                since: None,
                limit: 200,
            },
            select: vec!["$.greeting".into()],
            group_by: None,
            matched_artifact_count: 1,
            skipped_artifact_count: 0,
            skipped_artifacts: vec![],
            rows: vec![row],
            groups: vec![],
            table: None,
            csv: None,
        };
        let rendered = render_csv(&output, &output.select);
        assert!(rendered.contains("\"r,1\""));
        assert!(rendered.contains("\"hello, world\""));
    }
}
