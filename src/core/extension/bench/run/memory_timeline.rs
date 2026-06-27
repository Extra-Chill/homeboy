//! Memory timeline artifact preservation and attachment.

use std::collections::BTreeMap;
use std::fs;

use crate::core::engine::resource::{self, ExtensionChildResourceSummary};
use crate::core::engine::run_dir::RunDir;
use crate::core::error::{Error, Result};
use crate::core::extension::bench::artifact::BenchArtifact;
use crate::core::extension::bench::parsing::BenchResults;

pub(crate) fn attach_memory_timeline_artifacts(
    results: &mut BenchResults,
    child_resource: Option<&ExtensionChildResourceSummary>,
    run_dir: &RunDir,
    suffix: Option<&str>,
) -> Result<()> {
    let Some(child_resource) = child_resource else {
        return Ok(());
    };
    let Some((
        json_filename,
        csv_filename,
        peak_rss_bytes,
        peak_rss_mb,
        sample_count,
        peak_child_count,
        peak_at_ms,
    )) = preserve_memory_timeline_artifacts(Some(child_resource), run_dir, suffix)?
    else {
        return Ok(());
    };

    let memory_metadata = serde_json::json!({
        "peak_rss_bytes": peak_rss_bytes,
        "peak_rss_mb": peak_rss_mb,
        "peak_at_ms": peak_at_ms,
        "peak_child_count": peak_child_count,
        "sample_count": sample_count,
        "timeline_json": json_filename,
        "timeline_csv": csv_filename,
    });
    results
        .metadata
        .insert("memory_timeline".to_string(), memory_metadata);

    let phase_resources = phase_child_resources(run_dir);
    let phase_memory = if phase_resources.is_empty() {
        None
    } else {
        Some(preserve_phase_memory_timeline_artifacts(
            &phase_resources,
            run_dir,
            suffix,
        )?)
    };
    let phase_metrics = phase_memory
        .as_ref()
        .map(|phase_memory| phase_memory.metrics.clone())
        .unwrap_or_default();
    if let Some(phase_memory) = phase_memory.as_ref() {
        results.metadata.insert(
            "phase_memory".to_string(),
            serde_json::json!({
                "phases": phase_memory.phases,
                "timeline_json": phase_memory.json_filename,
                "timeline_csv": phase_memory.csv_filename,
                "sample_count": phase_memory.sample_count,
            }),
        );
    }
    results
        .metric_groups
        .entry("memory".to_string())
        .or_default()
        .extend([
            ("peak_rss_mb".to_string(), peak_rss_mb),
            ("peak_child_count".to_string(), peak_child_count as f64),
            ("sample_count".to_string(), sample_count as f64),
            ("peak_at_ms".to_string(), peak_at_ms as f64),
        ]);
    results
        .metric_groups
        .entry("memory".to_string())
        .or_default()
        .extend(phase_metrics.clone());

    for scenario in &mut results.scenarios {
        scenario
            .metrics
            .values
            .insert("peak_rss_mb".to_string(), peak_rss_mb);
        scenario
            .metrics
            .values
            .insert("peak_child_count".to_string(), peak_child_count as f64);
        scenario
            .metrics
            .values
            .insert("memory_sample_count".to_string(), sample_count as f64);
        scenario.metrics.values.extend(phase_metrics.clone());
        scenario.artifacts.insert(
            "memory_timeline_json".to_string(),
            BenchArtifact {
                path: Some(json_filename.clone()),
                artifact_type: Some("file".to_string()),
                kind: Some("bench_memory_timeline".to_string()),
                label: Some("Bench memory timeline (JSON)".to_string()),
                ..BenchArtifact::default()
            },
        );
        scenario.artifacts.insert(
            "memory_timeline_csv".to_string(),
            BenchArtifact {
                path: Some(csv_filename.clone()),
                artifact_type: Some("file".to_string()),
                kind: Some("bench_memory_timeline".to_string()),
                label: Some("Bench memory timeline (CSV)".to_string()),
                ..BenchArtifact::default()
            },
        );
        if let Some(phase_memory) = phase_memory.as_ref() {
            scenario.artifacts.insert(
                "phase_memory_timeline_json".to_string(),
                BenchArtifact {
                    path: Some(phase_memory.json_filename.clone()),
                    artifact_type: Some("file".to_string()),
                    kind: Some("bench_memory_timeline".to_string()),
                    label: Some("Bench phase memory timeline (JSON)".to_string()),
                    ..BenchArtifact::default()
                },
            );
            scenario.artifacts.insert(
                "phase_memory_timeline_csv".to_string(),
                BenchArtifact {
                    path: Some(phase_memory.csv_filename.clone()),
                    artifact_type: Some("file".to_string()),
                    kind: Some("bench_memory_timeline".to_string()),
                    label: Some("Bench phase memory timeline (CSV)".to_string()),
                    ..BenchArtifact::default()
                },
            );
        }
    }

    Ok(())
}

#[derive(Debug, Clone)]
pub(crate) struct PhaseMemoryArtifacts {
    json_filename: String,
    csv_filename: String,
    phases: BTreeMap<String, serde_json::Value>,
    metrics: BTreeMap<String, f64>,
    sample_count: usize,
}

pub(crate) fn phase_child_resources(run_dir: &RunDir) -> Vec<ExtensionChildResourceSummary> {
    resource::read_extension_child_resources(run_dir)
        .into_iter()
        .filter(|summary| {
            summary
                .phase
                .as_deref()
                .is_some_and(|phase| !phase.is_empty())
        })
        .collect()
}

pub(crate) fn preserve_phase_memory_timeline_artifacts(
    phase_resources: &[ExtensionChildResourceSummary],
    run_dir: &RunDir,
    suffix: Option<&str>,
) -> Result<PhaseMemoryArtifacts> {
    let artifact_stem = match suffix {
        Some(suffix) => format!("bench-memory-timeline-phases-{suffix}"),
        None => "bench-memory-timeline-phases".to_string(),
    };
    let json_filename = format!("{artifact_stem}.json");
    let csv_filename = format!("{artifact_stem}.csv");
    let json_path = run_dir.step_file(&json_filename);
    let csv_path = run_dir.step_file(&csv_filename);

    let mut phases = BTreeMap::new();
    let mut metrics = BTreeMap::new();
    let mut sample_count = 0;
    for resource in phase_resources {
        let Some(phase) = resource.phase.as_deref().filter(|phase| !phase.is_empty()) else {
            continue;
        };
        let peak_rss_bytes = resource.peak.sampled_peak_rss_bytes.unwrap_or(0);
        let peak_rss_mb = bytes_to_mb(peak_rss_bytes);
        sample_count += resource.samples.len();
        phases.insert(
            phase.to_string(),
            serde_json::json!({
                "peak_rss_bytes": peak_rss_bytes,
                "peak_rss_mb": peak_rss_mb,
                "peak_at_ms": resource.sampled_peak_at_ms.unwrap_or(0),
                "peak_child_count": resource.sampled_peak_child_count.unwrap_or(0),
                "sample_count": resource.samples.len(),
                "root_pid": resource.child.root_pid,
                "command_label": resource.child.command_label,
            }),
        );
        metrics.insert(
            format!("peak_{}_rss_mb", metric_phase_slug(phase)),
            peak_rss_mb,
        );
    }

    let json = serde_json::to_vec_pretty(&serde_json::json!({
        "schema": "homeboy/bench-memory-timeline/v1",
        "phases": phases.clone(),
        "sample_count": sample_count,
        "resources": phase_resources,
    }))
    .map_err(|e| {
        Error::internal_json(
            e.to_string(),
            Some("serialize bench phase memory timeline".to_string()),
        )
    })?;
    fs::write(&json_path, json).map_err(|e| {
        Error::internal_io(
            format!(
                "Failed to write bench phase memory timeline {}: {}",
                json_path.display(),
                e
            ),
            Some("bench.memory_timeline".to_string()),
        )
    })?;

    fs::write(&csv_path, phase_memory_timeline_csv(phase_resources)).map_err(|e| {
        Error::internal_io(
            format!(
                "Failed to write bench phase memory timeline CSV {}: {}",
                csv_path.display(),
                e
            ),
            Some("bench.memory_timeline".to_string()),
        )
    })?;

    Ok(PhaseMemoryArtifacts {
        json_filename,
        csv_filename,
        phases,
        metrics,
        sample_count,
    })
}

pub(crate) type MemoryTimelineArtifacts = (String, String, u64, f64, usize, usize, u128);

pub(crate) fn preserve_memory_timeline_artifacts(
    child_resource: Option<&ExtensionChildResourceSummary>,
    run_dir: &RunDir,
    suffix: Option<&str>,
) -> Result<Option<MemoryTimelineArtifacts>> {
    let Some(child_resource) = child_resource else {
        return Ok(None);
    };
    if child_resource.samples.is_empty() {
        return Ok(None);
    }

    let artifact_stem = match suffix {
        Some(suffix) => format!("bench-memory-timeline-{suffix}"),
        None => "bench-memory-timeline".to_string(),
    };
    let json_filename = format!("{artifact_stem}.json");
    let csv_filename = format!("{artifact_stem}.csv");
    let json_path = run_dir.step_file(&json_filename);
    let csv_path = run_dir.step_file(&csv_filename);

    let peak_rss_bytes = child_resource.peak.sampled_peak_rss_bytes.unwrap_or(0);
    let peak_rss_mb = bytes_to_mb(peak_rss_bytes);
    let sample_count = child_resource.samples.len();
    let peak_child_count = child_resource.sampled_peak_child_count.unwrap_or(0);
    let peak_at_ms = child_resource.sampled_peak_at_ms.unwrap_or(0);

    let json = serde_json::to_vec_pretty(&serde_json::json!({
        "schema": "homeboy/bench-memory-timeline/v1",
        "root_pid": child_resource.child.root_pid,
        "command_label": child_resource.child.command_label,
        "started_at": child_resource.started_at,
        "finished_at": child_resource.finished_at,
        "duration_ms": child_resource.duration_ms,
        "peak_rss_bytes": peak_rss_bytes,
        "peak_rss_mb": peak_rss_mb,
        "peak_at_ms": peak_at_ms,
        "peak_child_count": peak_child_count,
        "sample_count": sample_count,
        "samples": child_resource.samples,
    }))
    .map_err(|e| {
        Error::internal_json(
            e.to_string(),
            Some("serialize bench memory timeline".to_string()),
        )
    })?;
    fs::write(&json_path, json).map_err(|e| {
        Error::internal_io(
            format!(
                "Failed to write bench memory timeline {}: {}",
                json_path.display(),
                e
            ),
            Some("bench.memory_timeline".to_string()),
        )
    })?;

    fs::write(&csv_path, memory_timeline_csv(child_resource)).map_err(|e| {
        Error::internal_io(
            format!(
                "Failed to write bench memory timeline CSV {}: {}",
                csv_path.display(),
                e
            ),
            Some("bench.memory_timeline".to_string()),
        )
    })?;

    Ok(Some((
        json_filename,
        csv_filename,
        peak_rss_bytes,
        peak_rss_mb,
        sample_count,
        peak_child_count,
        peak_at_ms,
    )))
}

pub(crate) fn bytes_to_mb(bytes: u64) -> f64 {
    bytes as f64 / 1024.0 / 1024.0
}

pub(crate) fn memory_timeline_csv(child_resource: &ExtensionChildResourceSummary) -> String {
    let mut csv = String::from(
        "timestamp,elapsed_ms,phase,root_pid,rss_bytes,rss_mb,cpu_percent,child_count,process_count\n",
    );
    for sample in &child_resource.samples {
        csv.push_str(&format!(
            "{},{},{},{},{},{:.6},{:.3},{},{}\n",
            sample.timestamp,
            sample.elapsed_ms,
            csv_field(sample.phase.as_deref().unwrap_or("")),
            sample.root_pid,
            sample.rss_bytes,
            bytes_to_mb(sample.rss_bytes),
            sample.cpu_percent,
            sample.child_count,
            sample.processes.len(),
        ));
    }
    csv
}

pub(crate) fn phase_memory_timeline_csv(
    phase_resources: &[ExtensionChildResourceSummary],
) -> String {
    let mut csv = String::from(
        "timestamp,elapsed_ms,phase,root_pid,rss_bytes,rss_mb,cpu_percent,child_count,process_count\n",
    );
    for resource in phase_resources {
        for sample in &resource.samples {
            let phase = sample
                .phase
                .as_deref()
                .or(resource.phase.as_deref())
                .unwrap_or("");
            csv.push_str(&format!(
                "{},{},{},{},{},{:.6},{:.3},{},{}\n",
                sample.timestamp,
                sample.elapsed_ms,
                csv_field(phase),
                sample.root_pid,
                sample.rss_bytes,
                bytes_to_mb(sample.rss_bytes),
                sample.cpu_percent,
                sample.child_count,
                sample.processes.len(),
            ));
        }
    }
    csv
}

pub(crate) fn metric_phase_slug(phase: &str) -> String {
    let slug = phase
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string();
    if slug.is_empty() {
        "phase".to_string()
    } else {
        slug
    }
}

pub(crate) fn csv_field(value: &str) -> String {
    if value.contains(&[',', '"', '\n', '\r'][..]) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}
