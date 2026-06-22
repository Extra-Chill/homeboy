//! GitHub Actions artifact ingestor for `homeboy runs import --from-gh-actions`.
//!
//! Pure data plumbing. Lists workflow runs via the existing `gh` CLI surface
//! (same auth pathway as `homeboy git` / `homeboy triage`), downloads matching
//! artifacts, validates they parse as JSON, and writes them into the local
//! observation store as artifacts attached to one synthetic Homeboy run per
//! `(repo, gh_run_id)` pair.
//!
//! # Why "synthetic" Homeboy runs
//!
//! GitHub Actions runs already have stable IDs that survive artifact retention
//! expiry. We mint one Homeboy `RunRecord` per GH run with `kind="gh-actions"`
//! and a deterministic UUID (v5 from `(repo, gh_run_id)`) so re-imports are
//! idempotent — re-running the ingestor on the same set of GH runs is a no-op.
//!
//! # Schema-blind
//!
//! The ingestor does not parse, interpret, or validate artifact contents
//! beyond `serde_json::from_str` succeeding. The downstream `runs query` /
//! `runs drift` primitives project arbitrary JSONPath expressions over the
//! resulting artifact corpus.

use std::collections::HashMap;
use std::fs;
use std::io::{Cursor, Read};
use std::path::PathBuf;

use clap::Args;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use homeboy::core::git::GhClient;
use homeboy::core::observation::{ObservationStore, RunRecord};
use homeboy::core::Error;

use super::{CmdResult, RunsOutput};

/// Synthetic Homeboy run kind used for ingested GitHub Actions runs.
const GH_RUN_KIND: &str = "gh-actions";

/// UUID namespace for deterministic Homeboy run IDs derived from GH run
/// metadata. Random v4 generated once and frozen here so re-imports across
/// hosts produce identical IDs.
const HOMEBOY_RUN_NAMESPACE: &[u8; 16] = &[
    0xc4, 0xa6, 0x7e, 0x37, 0x18, 0x4a, 0x4d, 0x32, 0x9b, 0x2e, 0x73, 0x4f, 0x21, 0x6c, 0x55, 0x80,
];

/// UUID namespace for deterministic artifact IDs.
const HOMEBOY_ARTIFACT_NAMESPACE: &[u8; 16] = &[
    0x88, 0x2d, 0x90, 0xc1, 0x44, 0x71, 0x4e, 0x09, 0xa2, 0x14, 0x6c, 0xb1, 0xe9, 0x77, 0x18, 0x44,
];

#[derive(Args, Clone, Debug)]
pub struct GhActionsImportArgs {
    /// Component ID stamped on the synthetic Homeboy run.
    #[arg(long = "component")]
    pub component_id: String,
    /// GitHub repository in `owner/name` form.
    #[arg(long)]
    pub repo: String,
    /// Workflow filename (e.g. `static-site-validation.yml`) or workflow name.
    #[arg(long)]
    pub workflow: Option<String>,
    /// Exact GitHub Actions run id.
    #[arg(long = "run-id")]
    pub run_id: Option<u64>,
    /// Artifact name glob — matched against the GitHub artifact name.
    /// Examples: `'design-distribution-*'`, `'*.json'`, `'ssi-validation-*'`.
    #[arg(long = "artifact-glob")]
    pub artifact_glob: String,
    /// Only import runs started within this duration. Defaults to `30d`.
    #[arg(long, default_value = "30d")]
    pub since: String,
    /// Maximum runs to inspect after listing, per page.
    #[arg(long, default_value_t = 200)]
    pub limit: usize,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct GhActionsImportOutput {
    pub command: &'static str,
    pub component_id: String,
    pub repo: String,
    pub workflow: Option<String>,
    pub run_id: Option<u64>,
    pub artifact_glob: String,
    pub runs_inspected: usize,
    pub runs_imported: usize,
    pub runs_skipped_existing: usize,
    pub artifacts_imported: usize,
    pub artifacts_skipped_existing: usize,
    pub artifacts_skipped_non_json: usize,
    pub etag_cache_hit: bool,
    pub artifacts: Vec<GhActionsImportedArtifact>,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct GhActionsImportedArtifact {
    pub run_id: String,
    pub gh_run_id: u64,
    pub artifact_id: String,
    pub gh_artifact_id: u64,
    pub artifact_name: String,
    pub file_name: String,
    pub path: String,
    pub status: GhActionsImportedArtifactStatus,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GhActionsImportedArtifactStatus {
    Imported,
    Existing,
}

pub fn import_from_gh_actions(args: GhActionsImportArgs) -> CmdResult<RunsOutput> {
    let gh = GhClient::from_repo_arg(&args.repo)?;
    gh.ensure_ready()?;
    let repo = gh.repo_path()?.to_string();

    let store = ObservationStore::open_initialized()?;
    let pattern = compile_glob(&args.artifact_glob)?;

    let (runs, etag_cache_hit) = if let Some(run_id) = args.run_id {
        (vec![get_workflow_run(&gh, run_id)?], false)
    } else {
        let workflow = args.workflow.as_deref().ok_or_else(|| {
            Error::validation_missing_argument(vec!["--workflow or --run-id".to_string()])
        })?;
        list_workflow_runs(&gh, workflow, &args.since)?
    };
    let runs_inspected = runs.len().min(args.limit);
    let runs_to_process: Vec<&GhWorkflowRun> = runs.iter().take(args.limit).collect();

    let mut runs_imported = 0usize;
    let mut runs_skipped_existing = 0usize;
    let mut artifacts_imported = 0usize;
    let mut artifacts_skipped_existing = 0usize;
    let mut artifacts_skipped_non_json = 0usize;
    let mut artifact_rows = Vec::new();

    for gh_run in runs_to_process {
        let homeboy_run_id = deterministic_run_id(&repo, gh_run.id);
        let existed = store.get_run(&homeboy_run_id)?.is_some();
        if !existed {
            let run_record = build_run_record(&homeboy_run_id, &args.component_id, &repo, gh_run);
            store.import_run(&run_record)?;
            runs_imported += 1;
        } else {
            runs_skipped_existing += 1;
        }

        // Always reconcile artifacts, even for runs we've seen before. New
        // artifacts can land late (e.g. retried jobs) and we still want them
        // ingested. Existing artifact rows are detected via deterministic
        // (run_id, gh_artifact_id, file_name) IDs.
        let artifacts = list_run_artifacts(&gh, gh_run.id)?;
        let existing_artifacts: HashMap<String, homeboy::core::observation::ArtifactRecord> = store
            .list_artifacts(&homeboy_run_id)?
            .into_iter()
            .map(|a| (a.id.clone(), a))
            .collect();

        for artifact in artifacts {
            if !pattern.matches(&artifact.name) {
                continue;
            }
            if artifact.expired {
                // Once GH retention expires the artifact, we can't fetch
                // bytes — skip silently. Future re-imports won't recover.
                continue;
            }
            let zip_bytes = match download_artifact_zip(&gh, artifact.id) {
                Ok(bytes) => bytes,
                Err(_) => continue,
            };
            let json_files = unpack_json_files_from_zip(&zip_bytes)?;
            for (file_name, json_bytes) in json_files {
                // Validate JSON parse-ability; skip non-JSON.
                if serde_json::from_slice::<Value>(&json_bytes).is_err() {
                    artifacts_skipped_non_json += 1;
                    continue;
                }
                let artifact_id =
                    deterministic_artifact_id(&homeboy_run_id, artifact.id, &file_name);
                if let Some(existing) = existing_artifacts.get(&artifact_id) {
                    artifacts_skipped_existing += 1;
                    artifact_rows.push(GhActionsImportedArtifact {
                        run_id: homeboy_run_id.clone(),
                        gh_run_id: gh_run.id,
                        artifact_id,
                        gh_artifact_id: artifact.id,
                        artifact_name: artifact.name.clone(),
                        file_name,
                        path: existing.path.clone(),
                        status: GhActionsImportedArtifactStatus::Existing,
                    });
                    continue;
                }

                let stored_path =
                    persist_artifact_file(&homeboy_run_id, &artifact_id, &file_name, &json_bytes)?;
                let sha = format!("{:x}", Sha256::digest(&json_bytes));
                let size = i64::try_from(json_bytes.len()).ok();
                let artifact_record = homeboy::core::observation::ArtifactRecord {
                    id: artifact_id,
                    run_id: homeboy_run_id.clone(),
                    kind: artifact.name.clone(),
                    artifact_type: "file".to_string(),
                    path: stored_path.to_string_lossy().to_string(),
                    url: None,
                    public_url: None,
                    viewer_url: None,
                    viewer_links: Vec::new(),
                    sha256: Some(sha),
                    size_bytes: size,
                    mime: Some("application/json".to_string()),
                    metadata_json: serde_json::json!({
                        "source": "github_actions",
                        "gh_run_id": gh_run.id,
                        "gh_artifact_id": artifact.id,
                        "artifact_name": artifact.name,
                    }),
                    created_at: chrono::Utc::now().to_rfc3339(),
                };
                store.import_artifact(&artifact_record)?;
                artifact_rows.push(GhActionsImportedArtifact {
                    run_id: homeboy_run_id.clone(),
                    gh_run_id: gh_run.id,
                    artifact_id: artifact_record.id.clone(),
                    gh_artifact_id: artifact.id,
                    artifact_name: artifact.name.clone(),
                    file_name,
                    path: artifact_record.path.clone(),
                    status: GhActionsImportedArtifactStatus::Imported,
                });
                artifacts_imported += 1;
            }
        }
    }

    Ok((
        RunsOutput::ImportFromGhActions(GhActionsImportOutput {
            command: "runs.import.gh-actions",
            component_id: args.component_id,
            repo,
            workflow: args.workflow,
            run_id: args.run_id,
            artifact_glob: args.artifact_glob,
            runs_inspected,
            runs_imported,
            runs_skipped_existing,
            artifacts_imported,
            artifacts_skipped_existing,
            artifacts_skipped_non_json,
            etag_cache_hit,
            artifacts: artifact_rows,
        }),
        0,
    ))
}

// ── Run record construction ─────────────────────────────────────────────────

fn build_run_record(
    homeboy_run_id: &str,
    component_id: &str,
    repo: &str,
    gh_run: &GhWorkflowRun,
) -> RunRecord {
    let metadata = serde_json::json!({
        "gh": {
            "repo": repo,
            "run_id": gh_run.id,
            "run_number": gh_run.run_number,
            "workflow_name": gh_run.workflow_name,
            "workflow_id": gh_run.workflow_id,
            "branch": gh_run.head_branch,
            "head_sha": gh_run.head_sha,
            "event": gh_run.event,
            "pull_request_numbers": gh_run.pull_request_numbers.clone(),
            "html_url": gh_run.html_url,
            "conclusion": gh_run.conclusion,
            "status": gh_run.status,
            "run_attempt": gh_run.run_attempt,
        },
        "homeboy_ingest": {
            "kind": "gh-actions",
            "ingested_at": chrono::Utc::now().to_rfc3339(),
        },
    });

    RunRecord {
        id: homeboy_run_id.to_string(),
        kind: GH_RUN_KIND.to_string(),
        component_id: Some(component_id.to_string()),
        started_at: gh_run
            .run_started_at
            .clone()
            .or_else(|| gh_run.created_at.clone())
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339()),
        finished_at: gh_run.updated_at.clone(),
        status: map_gh_conclusion_to_status(gh_run),
        command: Some(format!(
            "homeboy runs import --from-gh-actions --repo {repo} --run-id {}",
            gh_run.id
        )),
        cwd: None,
        homeboy_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        git_sha: gh_run.head_sha.clone(),
        rig_id: None,
        metadata_json: metadata,
    }
}

/// Map a GitHub Actions conclusion to a Homeboy run status. Conservative —
/// unknown conclusions become `error` so we never accidentally label a
/// running/cancelled GH run as `pass`.
fn map_gh_conclusion_to_status(gh_run: &GhWorkflowRun) -> String {
    match gh_run.conclusion.as_deref() {
        Some("success") => "pass".to_string(),
        Some("failure") => "fail".to_string(),
        Some("cancelled" | "skipped" | "neutral") => "skipped".to_string(),
        Some(_) => "error".to_string(),
        None => match gh_run.status.as_deref() {
            Some("completed") => "pass".to_string(),
            _ => "running".to_string(),
        },
    }
}

// ── GitHub API listing (via `gh` CLI) ───────────────────────────────────────

fn get_workflow_run(gh: &GhClient, run_id: u64) -> homeboy::core::Result<GhWorkflowRun> {
    let api_path = format!("repos/{}/actions/runs/{run_id}", gh.repo_path()?);
    gh.api_json::<GhWorkflowRunRaw>(&api_path)
        .map(GhWorkflowRun::from)
}

/// One GitHub Actions workflow run, projected to the fields we persist.
#[derive(Debug, Clone, Deserialize)]
struct GhWorkflowRunRaw {
    id: u64,
    run_number: Option<u64>,
    name: Option<String>,
    workflow_id: Option<u64>,
    head_branch: Option<String>,
    head_sha: Option<String>,
    event: Option<String>,
    status: Option<String>,
    conclusion: Option<String>,
    html_url: Option<String>,
    run_started_at: Option<String>,
    created_at: Option<String>,
    updated_at: Option<String>,
    run_attempt: Option<u64>,
    #[serde(default)]
    pull_requests: Vec<GhPullRequestRef>,
}

#[derive(Debug, Clone, Deserialize)]
struct GhPullRequestRef {
    number: u64,
}

#[derive(Debug, Clone)]
struct GhWorkflowRun {
    id: u64,
    run_number: Option<u64>,
    workflow_name: Option<String>,
    workflow_id: Option<u64>,
    head_branch: Option<String>,
    head_sha: Option<String>,
    event: Option<String>,
    status: Option<String>,
    conclusion: Option<String>,
    html_url: Option<String>,
    run_started_at: Option<String>,
    created_at: Option<String>,
    updated_at: Option<String>,
    run_attempt: Option<u64>,
    pull_request_numbers: Vec<u64>,
}

impl From<GhWorkflowRunRaw> for GhWorkflowRun {
    fn from(raw: GhWorkflowRunRaw) -> Self {
        Self {
            id: raw.id,
            run_number: raw.run_number,
            workflow_name: raw.name,
            workflow_id: raw.workflow_id,
            head_branch: raw.head_branch,
            head_sha: raw.head_sha,
            event: raw.event,
            status: raw.status,
            conclusion: raw.conclusion,
            html_url: raw.html_url,
            run_started_at: raw.run_started_at,
            created_at: raw.created_at,
            updated_at: raw.updated_at,
            run_attempt: raw.run_attempt,
            pull_request_numbers: raw.pull_requests.into_iter().map(|pr| pr.number).collect(),
        }
    }
}

/// List workflow runs via `gh api`, with ETag caching to keep us off
/// GitHub's primary rate limit on re-runs of the ingestor.
fn list_workflow_runs(
    gh: &GhClient,
    workflow: &str,
    since: &str,
) -> homeboy::core::Result<(Vec<GhWorkflowRun>, bool)> {
    let repo = gh.repo_path()?;
    let cache_key = list_runs_cache_key(repo, workflow);
    let etag_path = list_runs_cache_path(&cache_key, "etag")?;
    let body_path = list_runs_cache_path(&cache_key, "json")?;

    // Build the API path. We list with --paginate and let `gh` walk pages
    // (it caps via per_page=100). `created` filter narrows to recent runs.
    let created = since_iso_filter(since)?;
    let api_path =
        format!("repos/{repo}/actions/workflows/{workflow}/runs?per_page=100&created=>{created}");

    let prior_etag = fs::read_to_string(&etag_path).ok();
    let mut etag_cache_hit = false;

    // `gh api -i` includes response headers so we can scrape the ETag. When
    // a prior ETag exists, we send it as If-None-Match; on 304 we reuse the
    // cached body without re-paginating.
    let mut args: Vec<String> = vec![
        "api".into(),
        "-i".into(),
        "--paginate".into(),
        api_path.clone(),
    ];
    if let Some(etag) = prior_etag.as_deref() {
        args.push("-H".into());
        args.push(format!("If-None-Match: {etag}"));
    }

    let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    let output = gh.output(&arg_refs)?;

    if !output.status.success() {
        // 304 from gh api -i comes back with a non-zero status because
        // there's no JSON body. Fall back to cache when both the cache
        // body and stderr suggest "not modified".
        let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();
        if stderr.contains("304") && body_path.exists() {
            etag_cache_hit = true;
            let cached = fs::read(&body_path).map_err(|e| {
                Error::internal_io(
                    e.to_string(),
                    Some(format!("read cache {}", body_path.display())),
                )
            })?;
            let runs = parse_runs_payload(&cached)?;
            let runs = filter_runs_by_since(runs, since)?;
            return Ok((runs, etag_cache_hit));
        }
        return Err(Error::internal_io(
            format!(
                "gh api workflow runs failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            Some(format!("gh api {api_path}")),
        ));
    }

    let raw = String::from_utf8_lossy(&output.stdout).to_string();
    let (etag, body) = split_headers_and_body(&raw);

    // Persist body and ETag for the next invocation.
    if let Some(parent) = body_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(&body_path, body.as_bytes());
    if let Some(value) = etag {
        let _ = fs::write(&etag_path, value);
    }

    let runs = parse_runs_payload(body.as_bytes())?;
    let runs = filter_runs_by_since(runs, since)?;
    Ok((runs, etag_cache_hit))
}

/// `--paginate` returns a stream of one or more headers + JSON arrays. We
/// detect the JSON-array boundary at the first `[` after the last header
/// block, and concatenate any subsequent body fragments.
fn split_headers_and_body(raw: &str) -> (Option<String>, String) {
    let mut etag: Option<String> = None;
    // Pull every ETag header we see (gh repeats headers per page).
    for line in raw.lines() {
        let lower = line.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("etag:") {
            // Use the original-case value from the same line.
            let original = &line[line.find(':').map(|i| i + 1).unwrap_or(0)..];
            etag = Some(original.trim().to_string());
            let _ = rest;
        }
    }

    // Body = everything from the first `[` onward, with header blocks
    // between consecutive arrays stripped. Simplification: gh's --paginate
    // joins arrays without separators, so we can keep just the first
    // bracket region per page and concatenate.
    //
    // The simplest robust pass: walk the input, when we see a header line
    // followed by a blank line, the body fragment starts on the next line.
    // For listing purposes we only need parse_runs_payload to handle one
    // or more concatenated `[ ... ]` arrays.
    let mut body = String::with_capacity(raw.len());
    let mut in_body = false;
    let mut blank_seen = false;
    for line in raw.split_inclusive('\n') {
        if !in_body {
            // Entering body when we see a blank line that follows a header
            // block, or when we encounter the first `[` line directly.
            let trimmed = line.trim_end();
            if trimmed.is_empty() {
                blank_seen = true;
                continue;
            }
            if blank_seen || trimmed.starts_with('[') || trimmed.starts_with('{') {
                in_body = true;
                body.push_str(line);
                continue;
            }
            // Header line, drop.
            continue;
        }

        // Once in body, lines that look like new HTTP responses (e.g.
        // `HTTP/2.0 200`) flip us back to header-skipping mode. gh's
        // --paginate emits a fresh response block per page.
        let trimmed = line.trim_start();
        if trimmed.starts_with("HTTP/") {
            in_body = false;
            blank_seen = false;
            continue;
        }
        body.push_str(line);
    }

    (etag, body)
}

/// Parse one or more concatenated JSON arrays (gh `--paginate` output) into
/// a single flat list of workflow runs. The runs payload from
/// `actions/workflows/.../runs` is `{"total_count":N, "workflow_runs":[...]}`.
fn parse_runs_payload(body: &[u8]) -> homeboy::core::Result<Vec<GhWorkflowRun>> {
    #[derive(Deserialize)]
    struct WorkflowRunsPage {
        #[serde(default)]
        workflow_runs: Vec<GhWorkflowRunRaw>,
    }
    let mut runs = Vec::new();
    let trimmed = std::str::from_utf8(body)
        .map_err(|e| Error::internal_json(format!("workflow runs payload not utf-8: {e}"), None))?
        .trim();
    if trimmed.is_empty() {
        return Ok(runs);
    }
    let de = serde_json::Deserializer::from_str(trimmed);
    for value in de.into_iter::<WorkflowRunsPage>() {
        let page = value.map_err(|e| {
            Error::internal_json(e.to_string(), Some("parse workflow runs page".into()))
        })?;
        runs.extend(page.workflow_runs.into_iter().map(GhWorkflowRun::from));
    }
    Ok(runs)
}

fn parse_run_payload(body: &[u8]) -> homeboy::core::Result<GhWorkflowRun> {
    let raw: GhWorkflowRunRaw = serde_json::from_slice(body)
        .map_err(|e| Error::internal_json(e.to_string(), Some("parse workflow run".into())))?;
    Ok(GhWorkflowRun::from(raw))
}

/// Drop runs whose `created_at` is older than the `--since` window.
fn filter_runs_by_since(
    runs: Vec<GhWorkflowRun>,
    since: &str,
) -> homeboy::core::Result<Vec<GhWorkflowRun>> {
    let threshold = super::common::since_threshold(since)?;
    Ok(runs
        .into_iter()
        .filter(|run| {
            let candidate = run
                .run_started_at
                .as_deref()
                .or(run.created_at.as_deref())
                .unwrap_or("");
            candidate >= threshold.as_str()
        })
        .collect())
}

/// `--created` uses GitHub's date filter, which is always inclusive of the
/// boundary day. Convert our duration-style `--since` into a `YYYY-MM-DD`
/// boundary suitable for the filter.
fn since_iso_filter(since: &str) -> homeboy::core::Result<String> {
    let raw = super::common::since_threshold(since)?;
    Ok(raw[..10].to_string())
}

// ── Artifact listing + download ─────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
struct GhArtifactRaw {
    id: u64,
    name: String,
    #[serde(default)]
    expired: bool,
}

#[derive(Debug, Clone)]
struct GhArtifact {
    id: u64,
    name: String,
    expired: bool,
}

fn list_run_artifacts(gh: &GhClient, gh_run_id: u64) -> homeboy::core::Result<Vec<GhArtifact>> {
    let api_path = format!(
        "repos/{}/actions/runs/{gh_run_id}/artifacts?per_page=100",
        gh.repo_path()?
    );
    let args = vec!["api".to_string(), "--paginate".into(), api_path.clone()];
    let output = gh.output(&args.iter().map(String::as_str).collect::<Vec<_>>())?;
    if !output.status.success() {
        return Err(Error::internal_io(
            format!(
                "gh api artifacts failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            Some(format!("gh api {api_path}")),
        ));
    }

    #[derive(Deserialize)]
    struct ArtifactPage {
        #[serde(default)]
        artifacts: Vec<GhArtifactRaw>,
    }
    let mut out = Vec::new();
    let raw = String::from_utf8_lossy(&output.stdout);
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(out);
    }
    let de = serde_json::Deserializer::from_str(trimmed);
    for value in de.into_iter::<ArtifactPage>() {
        let page = value.map_err(|e| {
            Error::internal_json(e.to_string(), Some("parse artifacts page".into()))
        })?;
        for raw_artifact in page.artifacts {
            out.push(GhArtifact {
                id: raw_artifact.id,
                name: raw_artifact.name,
                expired: raw_artifact.expired,
            });
        }
    }
    Ok(out)
}

fn download_artifact_zip(gh: &GhClient, artifact_id: u64) -> homeboy::core::Result<Vec<u8>> {
    // `gh api repos/.../actions/artifacts/{id}/zip` follows the redirect to
    // the artifact storage URL automatically.
    let api_path = format!(
        "repos/{}/actions/artifacts/{artifact_id}/zip",
        gh.repo_path()?
    );
    gh.api_bytes(&api_path)
}

/// Walk a downloaded artifact zip in memory and yield `(file_name, bytes)`
/// pairs for every entry that ends in `.json`. Non-JSON entries are
/// returned to the caller anyway when `keep_non_json` is true (currently
/// we only return JSON — keeping the API simple).
fn unpack_json_files_from_zip(zip_bytes: &[u8]) -> homeboy::core::Result<Vec<(String, Vec<u8>)>> {
    let cursor = Cursor::new(zip_bytes);
    let mut archive = zip::ZipArchive::new(cursor).map_err(|e| {
        Error::validation_invalid_argument(
            "artifact_zip",
            format!("invalid artifact zip: {e}"),
            None,
            None,
        )
    })?;
    let mut out = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|e| {
            Error::internal_io(
                format!("read artifact zip entry: {e}"),
                Some("zip read".into()),
            )
        })?;
        if !entry.is_file() {
            continue;
        }
        let name = entry.name().to_string();
        if !name.to_ascii_lowercase().ends_with(".json") {
            continue;
        }
        let mut buf = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut buf).map_err(|e| {
            Error::internal_io(format!("read artifact zip body: {e}"), Some(name.clone()))
        })?;
        out.push((name, buf));
    }
    Ok(out)
}

// ── Local persistence ───────────────────────────────────────────────────────

fn persist_artifact_file(
    homeboy_run_id: &str,
    artifact_id: &str,
    file_name: &str,
    bytes: &[u8],
) -> homeboy::core::Result<PathBuf> {
    let safe_name = sanitize_file_name(file_name);
    let target_dir = crate::core::paths::homeboy_data()?
        .join("artifacts")
        .join(homeboy_run_id);
    fs::create_dir_all(&target_dir).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("create artifact dir {}", target_dir.display())),
        )
    })?;
    let target = target_dir.join(format!("{artifact_id}-{safe_name}"));
    fs::write(&target, bytes).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("write artifact file {}", target.display())),
        )
    })?;
    Ok(target)
}

fn sanitize_file_name(raw: &str) -> String {
    raw.replace(['/', '\\', '\0'], "_")
}

fn list_runs_cache_key(repo: &str, workflow: &str) -> String {
    let composite = format!("{repo}::{workflow}");
    let digest = Sha256::digest(composite.as_bytes());
    format!("{:x}", digest)
}

fn list_runs_cache_path(key: &str, ext: &str) -> homeboy::core::Result<PathBuf> {
    let base = crate::core::paths::homeboy()?
        .join("cache")
        .join("gh-actions-runs");
    fs::create_dir_all(&base).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("create cache dir {}", base.display())),
        )
    })?;
    Ok(base.join(format!("{key}.{ext}")))
}

// ── Glob compilation ────────────────────────────────────────────────────────

/// Compile a `--artifact-glob` value into a matcher. Uses the existing `glob`
/// crate's `Pattern` for shell-style matching (`*`, `?`, character classes).
fn compile_glob(raw: &str) -> homeboy::core::Result<glob::Pattern> {
    glob::Pattern::new(raw).map_err(|e| {
        Error::validation_invalid_argument(
            "artifact_glob",
            format!("invalid glob: {e}"),
            Some(raw.to_string()),
            None,
        )
    })
}

// ── Deterministic IDs ───────────────────────────────────────────────────────

fn deterministic_run_id(repo: &str, gh_run_id: u64) -> String {
    let composite = format!("{repo}#{gh_run_id}");
    let namespace = uuid::Uuid::from_bytes(*HOMEBOY_RUN_NAMESPACE);
    uuid::Uuid::new_v5(&namespace, composite.as_bytes()).to_string()
}

fn deterministic_artifact_id(homeboy_run_id: &str, gh_artifact_id: u64, file_name: &str) -> String {
    let composite = format!("{homeboy_run_id}#{gh_artifact_id}#{file_name}");
    let namespace = uuid::Uuid::from_bytes(*HOMEBOY_ARTIFACT_NAMESPACE);
    uuid::Uuid::new_v5(&namespace, composite.as_bytes()).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matches_design_distribution_artifacts() {
        let pattern = compile_glob("design-distribution-*").expect("compile glob");
        assert!(pattern.matches("design-distribution-koji"));
        assert!(pattern.matches("design-distribution-spore-ledger"));
        assert!(!pattern.matches("ssi-validation-koji"));
    }

    #[test]
    fn deterministic_run_id_is_stable() {
        let a = deterministic_run_id("example-org/wc-site-generator", 12345);
        let b = deterministic_run_id("example-org/wc-site-generator", 12345);
        assert_eq!(a, b);
        let c = deterministic_run_id("example-org/wc-site-generator", 12346);
        assert_ne!(a, c);
    }

    #[test]
    fn deterministic_artifact_id_is_stable_per_filename() {
        let run_id = deterministic_run_id("example-org/wc-site-generator", 12345);
        let a = deterministic_artifact_id(&run_id, 9000, "design-distribution.json");
        let b = deterministic_artifact_id(&run_id, 9000, "design-distribution.json");
        assert_eq!(a, b);
        let c = deterministic_artifact_id(&run_id, 9000, "ssi-validation.json");
        assert_ne!(a, c);
    }

    #[test]
    fn map_gh_conclusion_to_status_handles_known_outcomes() {
        let mut run = GhWorkflowRun {
            id: 1,
            run_number: None,
            workflow_name: None,
            workflow_id: None,
            head_branch: None,
            head_sha: None,
            event: None,
            status: Some("completed".into()),
            conclusion: Some("success".into()),
            html_url: None,
            run_started_at: None,
            created_at: None,
            updated_at: None,
            run_attempt: None,
            pull_request_numbers: vec![],
        };
        assert_eq!(map_gh_conclusion_to_status(&run), "pass");
        run.conclusion = Some("failure".into());
        assert_eq!(map_gh_conclusion_to_status(&run), "fail");
        run.conclusion = Some("cancelled".into());
        assert_eq!(map_gh_conclusion_to_status(&run), "skipped");
        run.conclusion = None;
        run.status = Some("in_progress".into());
        assert_eq!(map_gh_conclusion_to_status(&run), "running");
    }

    #[test]
    fn parse_runs_payload_handles_paginated_arrays() {
        let raw = serde_json::to_string(&serde_json::json!({
            "total_count": 1,
            "workflow_runs": [{
                "id": 100,
                "run_number": 7,
                "name": "validate",
                "head_branch": "main",
                "head_sha": "deadbeef",
                "event": "push",
                "status": "completed",
                "conclusion": "success",
                "run_started_at": "2026-05-04T10:00:00Z",
                "created_at": "2026-05-04T09:59:00Z",
                "updated_at": "2026-05-04T10:05:00Z",
                "pull_requests": [{"number": 98}]
            }]
        }))
        .unwrap();
        let runs = parse_runs_payload(raw.as_bytes()).expect("parse");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, 100);
        assert_eq!(runs[0].pull_request_numbers, vec![98]);
    }

    #[test]
    fn parse_run_payload_handles_single_run_response() {
        let raw = serde_json::to_vec(&serde_json::json!({
            "id": 26731420339u64,
            "run_number": 42,
            "name": "Static site validation iterator",
            "workflow_id": 123,
            "head_branch": "main",
            "head_sha": "deadbeef",
            "event": "workflow_dispatch",
            "status": "completed",
            "conclusion": "failure",
            "run_started_at": "2026-05-31T10:00:00Z",
            "created_at": "2026-05-31T09:59:00Z",
            "updated_at": "2026-05-31T10:05:00Z",
            "run_attempt": 1,
            "pull_requests": []
        }))
        .unwrap();

        let run = parse_run_payload(&raw).expect("parse run");
        assert_eq!(run.id, 26731420339);
        assert_eq!(
            run.workflow_name.as_deref(),
            Some("Static site validation iterator")
        );
        assert_eq!(run.workflow_id, Some(123));
        assert_eq!(map_gh_conclusion_to_status(&run), "fail");
    }

    #[test]
    fn split_headers_and_body_extracts_etag_and_skips_headers() {
        let raw = "HTTP/2.0 200 OK\nContent-Type: application/json\nETag: \"abc123\"\n\n[]\n";
        let (etag, body) = split_headers_and_body(raw);
        assert_eq!(etag.as_deref(), Some("\"abc123\""));
        assert_eq!(body.trim(), "[]");
    }
}
