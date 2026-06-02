use std::fs;
use std::path::{Path, PathBuf};

use clap::Args;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use homeboy::core::observation::{
    ArtifactRecord, FindingRecord, ObservationStore, RecordedHomeboyFinding, RunRecord,
    TraceSpanRecord,
};
use homeboy::core::runner::is_reportable_artifact_evidence_path;
use homeboy::core::Error;

use super::common::since_threshold;
use super::{require_run, CmdResult, RunsOutput};

const BUNDLE_FORMAT: &str = "homeboy-observations";
const BUNDLE_VERSION: u32 = 1;

#[derive(Args, Clone)]
pub(super) struct RunsExportArgs {
    /// Export one run by id
    #[arg(long, conflicts_with = "since")]
    pub run: Option<String>,
    /// Export runs started within a duration, e.g. 24h, 7d, 30m
    #[arg(long, conflicts_with = "run")]
    pub since: Option<String>,
    /// Output bundle directory. Zip output is intentionally out of scope for v1.
    #[arg(long, value_name = "DIR")]
    pub output: PathBuf,
}

#[derive(Args, Clone, Default)]
pub(super) struct RunsImportArgs {
    /// Bundle directory produced by `homeboy runs export`. Required when not
    /// using `--from-gh-actions`. Mutually exclusive with `--from-gh-actions`.
    pub input: Option<PathBuf>,

    /// Ingest artifacts directly from GitHub Actions instead of from a
    /// portable bundle directory. When set, `--component`, `--repo`,
    /// `--artifact-glob`, and one of `--workflow` or `--run-id` are required.
    #[arg(long, default_value_t = false)]
    pub from_gh_actions: bool,

    /// Component ID to stamp on imported runs (gh-actions mode).
    #[arg(long = "component")]
    pub component_id: Option<String>,
    /// `owner/repo` form (gh-actions mode).
    #[arg(long)]
    pub repo: Option<String>,
    /// Workflow filename or display name (gh-actions mode).
    #[arg(long)]
    pub workflow: Option<String>,
    /// Exact GitHub Actions run id (gh-actions mode).
    #[arg(long = "run-id")]
    pub run_id: Option<u64>,
    /// Glob filter for artifact names (gh-actions mode). Examples:
    /// `'design-distribution-*'`, `'*.json'`.
    #[arg(long = "artifact-glob")]
    pub artifact_glob: Option<String>,
    /// Restrict the gh-actions ingest window (e.g. 24h, 7d, 30d).
    #[arg(long)]
    pub since: Option<String>,
    /// Maximum runs to inspect per import call (gh-actions mode).
    #[arg(long, default_value_t = 200)]
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObservationBundleManifest {
    pub format: String,
    pub version: u32,
    pub created_at: String,
    pub homeboy_version: String,
    pub run_count: usize,
    pub artifact_count: usize,
    pub trace_span_count: usize,
    #[serde(default)]
    pub finding_count: usize,
    #[serde(default)]
    pub test_failure_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct ObservationBundle {
    manifest: ObservationBundleManifest,
    runs: Vec<RunRecord>,
    artifacts: Vec<ArtifactRecord>,
    trace_spans: Vec<TraceSpanRecord>,
    findings: Vec<RecordedHomeboyFinding>,
    test_failures: Vec<RecordedHomeboyFinding>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ObservationBundleImportSummary {
    pub runs: usize,
    pub artifacts: usize,
    pub artifact_metadata_only: usize,
    pub trace_spans: usize,
    pub findings: usize,
    pub test_failures: usize,
}

#[derive(Serialize)]
pub struct RunsExportOutput {
    pub command: &'static str,
    pub output: String,
    pub manifest: ObservationBundleManifest,
    pub run_count: usize,
    pub artifact_count: usize,
    pub trace_span_count: usize,
    pub finding_count: usize,
    pub test_failure_count: usize,
}

#[derive(Serialize)]
pub struct RunsImportOutput {
    pub command: &'static str,
    pub input: String,
    pub imported: ObservationBundleImportSummary,
}

pub(super) fn export_runs(args: RunsExportArgs) -> CmdResult<RunsOutput> {
    if args
        .output
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
    {
        return Err(Error::validation_invalid_argument(
            "output",
            "zip output is out of scope for observation bundle v1; pass a directory path",
            Some(args.output.to_string_lossy().to_string()),
            None,
        ));
    }

    let store = ObservationStore::open_initialized()?;
    let runs = if let Some(run_id) = args.run.as_deref() {
        vec![require_run(&store, run_id)?]
    } else if let Some(since) = args.since.as_deref() {
        let threshold = since_threshold(since)?;
        store.list_runs_started_since(&threshold)?
    } else {
        return Err(Error::validation_missing_argument(vec![
            "--run <run-id> or --since <duration>".to_string(),
        ]));
    };

    let bundle = build_bundle(&store, runs)?;
    write_bundle_dir(&args.output, &bundle)?;

    Ok((
        RunsOutput::Export(RunsExportOutput {
            command: "runs.export",
            output: args.output.to_string_lossy().to_string(),
            run_count: bundle.runs.len(),
            artifact_count: bundle.artifacts.len(),
            trace_span_count: bundle.trace_spans.len(),
            finding_count: bundle.findings.len(),
            test_failure_count: bundle.test_failures.len(),
            manifest: bundle.manifest,
        }),
        0,
    ))
}

pub(super) fn import_runs(args: RunsImportArgs) -> CmdResult<RunsOutput> {
    if args.from_gh_actions {
        return import_via_gh_actions(args);
    }
    let input = args.input.clone().ok_or_else(|| {
        Error::validation_missing_argument(vec![
            "<input> (bundle directory) or --from-gh-actions ...".to_string(),
        ])
    })?;
    let mut bundle = read_bundle_dir(&input)?;
    let store = ObservationStore::open_initialized()?;
    for index in 0..bundle.runs.len() {
        let original_run_id = bundle.runs[index].id.clone();
        let imported_run_id = import_bundle_run(&store, &mut bundle.runs[index])?;
        if imported_run_id != original_run_id {
            rewrite_bundle_run_references(&mut bundle, &original_run_id, &imported_run_id);
        }
    }
    let mut artifacts = 0usize;
    let mut artifact_metadata_only = 0usize;
    for artifact in &bundle.artifacts {
        let artifact = imported_artifact_record(artifact);
        if artifact.artifact_type == "metadata-only" {
            artifact_metadata_only += 1;
        } else {
            artifacts += 1;
        }
        store.import_artifact(&artifact)?;
    }
    for span in &bundle.trace_spans {
        store.import_trace_span(span)?;
    }
    for finding in bundle.findings.iter().chain(bundle.test_failures.iter()) {
        store.import_finding(&FindingRecord::from(finding.clone()))?;
    }

    Ok((
        RunsOutput::Import(RunsImportOutput {
            command: "runs.import",
            input: input.to_string_lossy().to_string(),
            imported: ObservationBundleImportSummary {
                runs: bundle.runs.len(),
                artifacts,
                artifact_metadata_only,
                trace_spans: bundle.trace_spans.len(),
                findings: bundle.findings.len(),
                test_failures: bundle.test_failures.len(),
            },
        }),
        0,
    ))
}

fn import_bundle_run(
    store: &ObservationStore,
    run: &mut RunRecord,
) -> homeboy::core::Result<String> {
    if let Some(existing) = store.get_run(&run.id)? {
        if existing == *run {
            return Ok(run.id.clone());
        }
        if is_lab_bundle_run(run) {
            run.id = remapped_lab_run_id(run)?;
        }
    }
    store.import_run(run)?;
    Ok(run.id.clone())
}

fn rewrite_bundle_run_references(bundle: &mut ObservationBundle, from: &str, to: &str) {
    for artifact in &mut bundle.artifacts {
        if artifact.run_id == from {
            artifact.id = remapped_child_record_id(&artifact.id, to);
            artifact.run_id = to.to_string();
        }
    }
    for span in &mut bundle.trace_spans {
        if span.run_id == from {
            span.id = remapped_child_record_id(&span.id, to);
            span.run_id = to.to_string();
        }
    }
    for finding in bundle
        .findings
        .iter_mut()
        .chain(bundle.test_failures.iter_mut())
    {
        if finding.run_id == from {
            finding.id = remapped_child_record_id(&finding.id, to);
            finding.run_id = to.to_string();
        }
    }
}

fn remapped_child_record_id(id: &str, run_id: &str) -> String {
    let hash = Sha256::digest(format!("{run_id}\0{id}").as_bytes());
    let hex = format!("{hash:x}");
    format!("{id}-imported-{}", &hex[..16])
}

fn is_lab_bundle_run(run: &RunRecord) -> bool {
    run.kind == "runner-exec" && run.metadata_json.get("lab").is_some()
}

fn remapped_lab_run_id(run: &RunRecord) -> homeboy::core::Result<String> {
    let bytes = serde_json::to_vec(run).map_err(|error| {
        Error::internal_unexpected(format!(
            "Failed to fingerprint imported lab run {}: {}",
            run.id, error
        ))
    })?;
    let hash = Sha256::digest(bytes);
    let hex = format!("{hash:x}");
    Ok(format!("{}-imported-{}", run.id, &hex[..16]))
}

fn imported_artifact_record(artifact: &ArtifactRecord) -> ArtifactRecord {
    if !matches!(artifact.artifact_type.as_str(), "file" | "directory") {
        return artifact.clone();
    }
    let mut imported = artifact.clone();
    imported.artifact_type = "metadata-only".to_string();
    imported.path = portable_artifact_label(&artifact.path, &artifact.id);
    imported
}

fn portable_artifact_label(path: &str, fallback: &str) -> String {
    path.rsplit(['/', '\\'])
        .find(|segment| !segment.is_empty())
        .unwrap_or(fallback)
        .to_string()
}

fn import_via_gh_actions(args: RunsImportArgs) -> CmdResult<RunsOutput> {
    let component_id = require_gh_arg(args.component_id.clone(), "component")?;
    let repo = require_gh_arg(args.repo.clone(), "repo")?;
    let workflow = args.workflow.clone().filter(|v| !v.trim().is_empty());
    if workflow.is_none() && args.run_id.is_none() {
        return Err(Error::validation_missing_argument(vec![
            "--workflow or --run-id".to_string(),
        ]));
    }
    let artifact_glob = require_gh_arg(args.artifact_glob.clone(), "artifact-glob")?;
    let since = args.since.clone().unwrap_or_else(|| "30d".to_string());

    super::gh_actions::import_from_gh_actions(super::gh_actions::GhActionsImportArgs {
        component_id,
        repo,
        workflow,
        run_id: args.run_id,
        artifact_glob,
        since,
        limit: args.limit,
    })
}

fn require_gh_arg(value: Option<String>, name: &str) -> homeboy::core::Result<String> {
    value
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| Error::validation_missing_argument(vec![format!("--{name}")]))
}

fn build_bundle(
    store: &ObservationStore,
    runs: Vec<RunRecord>,
) -> homeboy::core::Result<ObservationBundle> {
    let mut artifacts = Vec::new();
    let mut trace_spans = Vec::new();
    let mut findings = Vec::new();
    for run in &runs {
        artifacts.extend(
            store
                .list_artifacts(&run.id)?
                .into_iter()
                .map(portable_bundle_artifact_record),
        );
        trace_spans.extend(store.list_trace_spans(&run.id)?);
        findings.extend(
            store
                .list_findings_for_run(&run.id)?
                .into_iter()
                .map(RecordedHomeboyFinding::from),
        );
    }
    let test_failures = findings
        .iter()
        .filter(|finding| is_test_failure_finding(finding))
        .cloned()
        .collect::<Vec<_>>();
    let manifest = ObservationBundleManifest {
        format: BUNDLE_FORMAT.to_string(),
        version: BUNDLE_VERSION,
        created_at: chrono::Utc::now().to_rfc3339(),
        homeboy_version: env!("CARGO_PKG_VERSION").to_string(),
        run_count: runs.len(),
        artifact_count: artifacts.len(),
        trace_span_count: trace_spans.len(),
        finding_count: findings.len(),
        test_failure_count: test_failures.len(),
    };
    Ok(ObservationBundle {
        manifest,
        runs,
        artifacts,
        trace_spans,
        findings,
        test_failures,
    })
}

fn portable_bundle_artifact_record(artifact: ArtifactRecord) -> ArtifactRecord {
    if !matches!(artifact.artifact_type.as_str(), "file" | "directory")
        || is_reportable_artifact_evidence_path(&artifact.path)
    {
        return artifact;
    }

    let mut portable = artifact;
    portable.artifact_type = "metadata-only".to_string();
    portable.path = format!(
        "metadata-only:{}",
        portable_artifact_label(&portable.path, &portable.id)
    );
    portable
}

fn write_bundle_dir(path: &Path, bundle: &ObservationBundle) -> homeboy::core::Result<()> {
    if path.exists() && !path.is_dir() {
        return Err(Error::validation_invalid_argument(
            "output",
            "observation bundle output must be a directory",
            Some(path.to_string_lossy().to_string()),
            None,
        ));
    }
    fs::create_dir_all(path).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("create observation bundle dir {}", path.display())),
        )
    })?;
    write_json(path.join("manifest.json"), &bundle.manifest)?;
    write_json(path.join("runs.json"), &bundle.runs)?;
    write_json(path.join("artifacts.json"), &bundle.artifacts)?;
    write_json(path.join("trace_spans.json"), &bundle.trace_spans)?;
    write_json(path.join("findings.json"), &bundle.findings)?;
    write_json(path.join("test_failures.json"), &bundle.test_failures)?;
    Ok(())
}

fn read_bundle_dir(path: &Path) -> homeboy::core::Result<ObservationBundle> {
    if !path.is_dir() {
        return Err(Error::validation_invalid_argument(
            "input",
            "observation bundle input must be a directory",
            Some(path.to_string_lossy().to_string()),
            None,
        ));
    }
    let manifest: ObservationBundleManifest = read_json(path.join("manifest.json"))?;
    if manifest.format != BUNDLE_FORMAT {
        return Err(Error::validation_invalid_argument(
            "manifest.format",
            format!("expected {BUNDLE_FORMAT}, got {}", manifest.format),
            Some(manifest.format),
            None,
        ));
    }
    if manifest.version != BUNDLE_VERSION {
        return Err(Error::validation_invalid_argument(
            "manifest.version",
            format!(
                "expected version {BUNDLE_VERSION}, got {}",
                manifest.version
            ),
            Some(manifest.version.to_string()),
            None,
        ));
    }

    let runs: Vec<RunRecord> = read_json(path.join("runs.json"))?;
    let artifacts: Vec<ArtifactRecord> = read_json(path.join("artifacts.json"))?;
    let trace_spans: Vec<TraceSpanRecord> = read_json(path.join("trace_spans.json"))?;
    let mut findings: Vec<RecordedHomeboyFinding> = read_optional_json(path.join("findings.json"))?;
    let test_failures: Vec<RecordedHomeboyFinding> =
        read_optional_json(path.join("test_failures.json"))?;
    for test_failure in &test_failures {
        if !findings.iter().any(|finding| finding.id == test_failure.id) {
            findings.push(test_failure.clone());
        }
    }
    if manifest.run_count != runs.len()
        || manifest.artifact_count != artifacts.len()
        || manifest.trace_span_count != trace_spans.len()
        || manifest.finding_count != findings.len()
        || manifest.test_failure_count != test_failures.len()
    {
        return Err(Error::validation_invalid_argument(
            "manifest",
            "bundle manifest counts do not match record files",
            Some(path.to_string_lossy().to_string()),
            None,
        ));
    }
    Ok(ObservationBundle {
        manifest,
        runs,
        artifacts,
        trace_spans,
        findings,
        test_failures,
    })
}

fn is_test_failure_finding(finding: &RecordedHomeboyFinding) -> bool {
    let metadata_json = finding.finding.metadata_json();
    finding.finding.tool == "test"
        && (metadata_json
            .get("record_kind")
            .and_then(|value| value.as_str())
            == Some("failure")
            || metadata_json
                .get("source_sidecar")
                .and_then(|value| value.as_str())
                == Some("test-failures"))
}

fn write_json(path: PathBuf, value: &impl Serialize) -> homeboy::core::Result<()> {
    let json = serde_json::to_string_pretty(value).map_err(|e| {
        Error::internal_json(e.to_string(), Some(format!("serialize {}", path.display())))
    })?;
    fs::write(&path, json).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("write observation bundle file {}", path.display())),
        )
    })
}

fn read_json<T: for<'de> Deserialize<'de>>(path: PathBuf) -> homeboy::core::Result<T> {
    let raw = fs::read_to_string(&path).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("read observation bundle file {}", path.display())),
        )
    })?;
    serde_json::from_str(&raw).map_err(|e| {
        Error::validation_invalid_json(
            e,
            Some(format!("parse observation bundle file {}", path.display())),
            Some(raw),
        )
    })
}

fn read_optional_json<T: for<'de> Deserialize<'de> + Default>(
    path: PathBuf,
) -> homeboy::core::Result<T> {
    if !path.exists() {
        return Ok(T::default());
    }
    read_json(path)
}
