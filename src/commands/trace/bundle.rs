use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};

use homeboy::core::extension::trace as extension_trace;

use super::output::{
    fmt_delta_avg_ms, fmt_delta_ms, fmt_ms, render_compare_markdown, TraceAggregateInput,
    TraceOverlayInput,
};

pub(super) struct TraceExperimentBundleRequest<'a> {
    pub(super) name: &'a str,
    pub(super) bundle_root: Option<&'a Path>,
    pub(super) command: String,
    pub(super) before_path: &'a Path,
    pub(super) before_json: &'a str,
    pub(super) before: &'a TraceAggregateInput,
    pub(super) after_path: &'a Path,
    pub(super) after_json: &'a str,
    pub(super) after: &'a TraceAggregateInput,
    pub(super) compare: &'a extension_trace::TraceCompareOutput,
}

#[derive(Serialize)]
struct TraceExperimentManifest {
    command: String,
    timestamp: String,
    experiment: String,
    bundle_path: String,
    compare_path: String,
    report_path: String,
    variants: Vec<TraceExperimentVariantManifest>,
    overlays: Vec<TraceExperimentOverlayManifest>,
}

#[derive(Serialize)]
struct TraceExperimentVariantManifest {
    role: &'static str,
    source_path: String,
    bundle_path: String,
    component: Option<String>,
    scenario_id: Option<String>,
    phase_preset: Option<String>,
    repeat: Option<usize>,
    rig_id: Option<String>,
    components: Vec<TraceExperimentComponentManifest>,
    artifact_paths: Vec<String>,
}

#[derive(Serialize)]
struct TraceExperimentComponentManifest {
    id: String,
    path: Option<String>,
    branch: Option<String>,
    sha: Option<String>,
}

#[derive(Serialize)]
struct TraceExperimentOverlayManifest {
    role: &'static str,
    source_path: String,
    bundle_path: Option<String>,
    sha256: Option<String>,
    component_path: String,
    touched_files: Vec<String>,
    kept: bool,
}

pub(super) fn write_trace_experiment_bundle(
    request: TraceExperimentBundleRequest<'_>,
) -> homeboy::core::Result<PathBuf> {
    let experiment = sanitize_path_component(request.name);
    let bundle_root = request
        .bundle_root
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(".homeboy").join("experiments"));
    let bundle_dir = bundle_root.join(&experiment);
    fs::create_dir_all(&bundle_dir).map_err(|err| {
        homeboy::core::Error::internal_io(
            format!(
                "Failed to create trace experiment bundle {}: {}",
                bundle_dir.display(),
                err
            ),
            Some("trace.experiment.mkdir".to_string()),
        )
    })?;

    let baseline_path = bundle_dir.join("baseline.json");
    let variant_filename = format!("variant-{}.json", experiment);
    let variant_path = bundle_dir.join(&variant_filename);
    let compare_filename = format!("compare-{}.json", experiment);
    let compare_path = bundle_dir.join(&compare_filename);
    let report_path = bundle_dir.join("report.md");
    write_file(
        &baseline_path,
        request.before_json,
        "trace.experiment.baseline",
    )?;
    write_file(
        &variant_path,
        request.after_json,
        "trace.experiment.variant",
    )?;
    write_json_file(&compare_path, request.compare, "trace.experiment.compare")?;
    write_file(
        &report_path,
        &render_experiment_report(request.name, request.before, request.after, request.compare),
        "trace.experiment.report",
    )?;

    let overlay_dir = bundle_dir.join("overlays");
    let mut overlays = Vec::new();
    overlays.extend(copy_overlay_manifests(
        "baseline",
        &request.before.overlays,
        &overlay_dir,
    )?);
    overlays.extend(copy_overlay_manifests(
        "variant",
        &request.after.overlays,
        &overlay_dir,
    )?);

    let manifest = TraceExperimentManifest {
        command: request.command,
        timestamp: chrono::Utc::now().to_rfc3339(),
        experiment: request.name.to_string(),
        bundle_path: bundle_dir.display().to_string(),
        compare_path: compare_filename,
        report_path: "report.md".to_string(),
        variants: vec![
            variant_manifest(
                "baseline",
                request.before_path,
                "baseline.json".to_string(),
                request.before,
            ),
            variant_manifest(
                "variant",
                request.after_path,
                variant_filename,
                request.after,
            ),
        ],
        overlays,
    };
    write_json_file(
        &bundle_dir.join("manifest.json"),
        &manifest,
        "trace.experiment.manifest",
    )?;

    Ok(bundle_dir)
}

fn variant_manifest(
    role: &'static str,
    source_path: &Path,
    bundle_path: String,
    input: &TraceAggregateInput,
) -> TraceExperimentVariantManifest {
    TraceExperimentVariantManifest {
        role,
        source_path: source_path.display().to_string(),
        bundle_path,
        component: input.component.clone(),
        scenario_id: input.scenario_id.clone(),
        phase_preset: input.phase_preset.clone(),
        repeat: input.repeat,
        rig_id: rig_id(input),
        components: rig_components(input),
        artifact_paths: input
            .runs
            .iter()
            .filter(|run| !run.artifact_path.is_empty())
            .map(|run| run.artifact_path.clone())
            .collect(),
    }
}

fn copy_overlay_manifests(
    role: &'static str,
    overlays: &[TraceOverlayInput],
    overlay_dir: &Path,
) -> homeboy::core::Result<Vec<TraceExperimentOverlayManifest>> {
    overlays
        .iter()
        .enumerate()
        .map(|(index, overlay)| overlay_manifest(role, overlay, overlay_dir, index))
        .collect()
}

fn overlay_manifest(
    role: &'static str,
    overlay: &TraceOverlayInput,
    overlay_dir: &Path,
    index: usize,
) -> homeboy::core::Result<TraceExperimentOverlayManifest> {
    let source = Path::new(&overlay.path);
    let (bundle_path, sha256) = if source.is_file() {
        let target = copy_overlay_file(role, source, overlay_dir, index)?;
        (
            Some(target.display().to_string()),
            Some(sha256_file(source)?),
        )
    } else {
        (None, None)
    };
    Ok(TraceExperimentOverlayManifest {
        role,
        source_path: overlay.path.clone(),
        bundle_path,
        sha256,
        component_path: overlay.component_path.clone(),
        touched_files: overlay.touched_files.clone(),
        kept: overlay.kept,
    })
}

fn copy_overlay_file(
    role: &str,
    source: &Path,
    overlay_dir: &Path,
    index: usize,
) -> homeboy::core::Result<PathBuf> {
    fs::create_dir_all(overlay_dir).map_err(|err| {
        homeboy::core::Error::internal_io(
            format!(
                "Failed to create overlay bundle dir {}: {}",
                overlay_dir.display(),
                err
            ),
            Some("trace.experiment.overlay.mkdir".to_string()),
        )
    })?;
    let filename = source
        .file_name()
        .and_then(|name| name.to_str())
        .map(sanitize_path_component)
        .unwrap_or_else(|| format!("overlay-{}.patch", index + 1));
    let target = overlay_dir.join(format!("{}-{}", role, filename));
    let bytes = fs::read(source).map_err(|err| {
        homeboy::core::Error::internal_io(
            format!(
                "Failed to read trace overlay {} for bundling: {}",
                source.display(),
                err
            ),
            Some("trace.experiment.overlay.read".to_string()),
        )
    })?;
    fs::write(&target, bytes).map_err(|err| {
        homeboy::core::Error::internal_io(
            format!(
                "Failed to write bundled trace overlay {}: {}",
                target.display(),
                err
            ),
            Some("trace.experiment.overlay.write".to_string()),
        )
    })?;
    Ok(target)
}

fn render_experiment_report(
    name: &str,
    before: &TraceAggregateInput,
    after: &TraceAggregateInput,
    compare: &extension_trace::TraceCompareOutput,
) -> String {
    let mut out = render_compare_markdown(compare);
    out.push_str(&format!(
        "\n## Experiment Bundle\n\n- **Name:** `{}`\n",
        name
    ));
    push_span_deltas(&mut out, "Top Median Improvements", compare, true, true);
    push_span_deltas(&mut out, "Top Median Regressions", compare, false, true);
    push_span_deltas(&mut out, "Top Average Improvements", compare, true, false);
    push_span_deltas(&mut out, "Top Average Regressions", compare, false, false);
    push_failures_and_outliers(&mut out, "Baseline", before);
    push_failures_and_outliers(&mut out, "Variant", after);
    push_run_artifacts(&mut out, "Baseline", before);
    push_run_artifacts(&mut out, "Variant", after);
    out
}

fn push_span_deltas(
    out: &mut String,
    heading: &str,
    compare: &extension_trace::TraceCompareOutput,
    improvement: bool,
    median: bool,
) {
    let mut spans = compare
        .spans
        .iter()
        .filter(|span| {
            let delta = if median {
                span.median_delta_ms.map(|value| value as f64)
            } else {
                span.avg_delta_ms
            };
            delta.is_some_and(|value| {
                if improvement {
                    value < 0.0
                } else {
                    value > 0.0
                }
            })
        })
        .collect::<Vec<_>>();
    spans.sort_by(|left, right| {
        span_delta_abs(right, median)
            .partial_cmp(&span_delta_abs(left, median))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.id.cmp(&right.id))
    });
    if spans.is_empty() {
        return;
    }
    out.push_str(&format!("\n## {}\n\n", heading));
    for span in spans.into_iter().take(5) {
        let delta = if median {
            fmt_delta_ms(span.median_delta_ms)
        } else {
            fmt_delta_avg_ms(span.avg_delta_ms)
        };
        out.push_str(&format!("- `{}`: {}\n", span.id, delta));
    }
}

fn span_delta_abs(span: &extension_trace::TraceCompareSpanOutput, median: bool) -> f64 {
    if median {
        span.median_delta_ms.map(|value| value as f64)
    } else {
        span.avg_delta_ms
    }
    .unwrap_or(0.0)
    .abs()
}

fn push_failures_and_outliers(out: &mut String, label: &str, input: &TraceAggregateInput) {
    let mut lines = Vec::new();
    for run in &input.runs {
        if run.exit_code != 0 || run.failure.is_some() {
            lines.push(format!(
                "- Run {}: `{}` exit={} artifact=`{}`{}",
                run.index,
                run.status,
                run.exit_code,
                run.artifact_path,
                run.failure
                    .as_ref()
                    .map(|failure| format!(" failure={}", failure))
                    .unwrap_or_default()
            ));
        }
    }
    for span in &input.spans {
        if span.failures > 0 || span.max_artifact_path.is_some() {
            lines.push(format!(
                "- `{}`: failures={} max={} run={} artifact=`{}`",
                span.identity.id,
                span.failures,
                fmt_ms(span.max_ms),
                span.max_run_index
                    .map(|index| index.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                span.max_artifact_path.as_deref().unwrap_or("-")
            ));
        }
    }
    if lines.is_empty() {
        return;
    }
    out.push_str(&format!("\n## {} Failures and Outliers\n\n", label));
    for line in lines {
        out.push_str(&line);
        out.push('\n');
    }
}

fn push_run_artifacts(out: &mut String, label: &str, input: &TraceAggregateInput) {
    let artifact_paths = input
        .runs
        .iter()
        .filter(|run| !run.artifact_path.is_empty())
        .collect::<Vec<_>>();
    if artifact_paths.is_empty() {
        return;
    }
    out.push_str(&format!("\n## {} Artifact Paths\n\n", label));
    for run in artifact_paths {
        out.push_str(&format!("- Run {}: `{}`\n", run.index, run.artifact_path));
    }
}

fn rig_id(input: &TraceAggregateInput) -> Option<String> {
    input
        .rig_state
        .as_ref()
        .and_then(|value| value.get("rig_id"))
        .and_then(serde_json::Value::as_str)
        .map(ToString::to_string)
}

fn rig_components(input: &TraceAggregateInput) -> Vec<TraceExperimentComponentManifest> {
    input
        .rig_state
        .as_ref()
        .and_then(|value| value.get("components"))
        .and_then(serde_json::Value::as_object)
        .map(|components| {
            components
                .iter()
                .map(|(id, component)| TraceExperimentComponentManifest {
                    id: id.clone(),
                    path: component
                        .get("path")
                        .and_then(serde_json::Value::as_str)
                        .map(ToString::to_string),
                    branch: component
                        .get("branch")
                        .and_then(serde_json::Value::as_str)
                        .map(ToString::to_string),
                    sha: component
                        .get("sha")
                        .and_then(serde_json::Value::as_str)
                        .map(ToString::to_string),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn write_file(path: &Path, content: &str, context: &str) -> homeboy::core::Result<()> {
    fs::write(path, content).map_err(|err| {
        homeboy::core::Error::internal_io(
            format!("Failed to write {}: {}", path.display(), err),
            Some(context.to_string()),
        )
    })
}

fn write_json_file<T: Serialize>(
    path: &Path,
    value: &T,
    context: &str,
) -> homeboy::core::Result<()> {
    let content = serde_json::to_string_pretty(value).map_err(|err| {
        homeboy::core::Error::internal_json(err.to_string(), Some(context.to_string()))
    })?;
    write_file(path, &(content + "\n"), context)
}

fn sha256_file(path: &Path) -> homeboy::core::Result<String> {
    let bytes = fs::read(path).map_err(|err| {
        homeboy::core::Error::internal_io(
            format!("Failed to read {} for checksum: {}", path.display(), err),
            Some("trace.experiment.overlay.sha256".to_string()),
        )
    })?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

fn sanitize_path_component(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' => ch,
            _ => '-',
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if sanitized.is_empty() {
        "experiment".to_string()
    } else {
        sanitized
    }
}
