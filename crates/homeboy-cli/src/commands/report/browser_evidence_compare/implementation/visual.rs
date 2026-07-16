use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::super::types::{
    ArtifactRef, BrowserEvidenceVariant, BrowserEvidenceVariantComparison, VisualCompareResult,
};
use super::super::VisualCompareOptions;

pub(in crate::commands::report::browser_evidence_compare) fn attach_visual_comparisons(
    variants: &mut [BrowserEvidenceVariantComparison],
    local_variants: &[BrowserEvidenceVariantComparison],
    options: &VisualCompareOptions,
    baseline_label: &str,
    candidate_label: &str,
) -> homeboy::core::Result<()> {
    for variant in variants {
        let Some(local_variant) = local_variants
            .iter()
            .find(|candidate| candidate.variant == variant.variant)
        else {
            continue;
        };
        let Some(source_screenshot) = screenshot_path(&local_variant.artifacts.baseline) else {
            variant
                .notes
                .push("visual compare skipped: baseline screenshot artifact missing".to_string());
            continue;
        };
        let Some(candidate_screenshot) = screenshot_path(&local_variant.artifacts.candidate) else {
            variant
                .notes
                .push("visual compare skipped: candidate screenshot artifact missing".to_string());
            continue;
        };
        let slug = visual_variant_slug(&variant.variant);
        let artifacts_dir = options.artifacts_dir.join(&slug);
        let result = run_visual_compare_provider(
            options,
            &artifacts_dir,
            &source_screenshot,
            &candidate_screenshot,
            baseline_label,
            candidate_label,
        )?;
        variant.visual_compare = Some(result);
    }
    Ok(())
}

fn screenshot_path(artifacts: &[ArtifactRef]) -> Option<String> {
    let source_parent = artifacts.iter().find_map(|artifact| {
        (artifact.label == "source")
            .then(|| PathBuf::from(&artifact.target))
            .filter(|path| path.is_absolute())
            .and_then(|path| path.parent().map(Path::to_path_buf))
    });
    artifacts
        .iter()
        .find(|artifact| artifact.label.to_ascii_lowercase().contains("screenshot"))
        .map(|artifact| {
            let path = PathBuf::from(&artifact.target);
            if path.is_absolute() {
                path
            } else if let Some(parent) = &source_parent {
                parent.join(path)
            } else {
                path
            }
            .to_string_lossy()
            .to_string()
        })
}

fn visual_variant_slug(variant: &BrowserEvidenceVariant) -> String {
    let mut raw = format!("{}-{}", variant.scenario, variant.profile);
    for (key, value) in &variant.matrix {
        raw.push('-');
        raw.push_str(key);
        raw.push('-');
        raw.push_str(value);
    }
    let slug = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        "browser-variant".to_string()
    } else {
        slug
    }
}

fn run_visual_compare_provider(
    options: &VisualCompareOptions,
    artifacts_dir: &Path,
    source_screenshot: &str,
    candidate_screenshot: &str,
    baseline_label: &str,
    candidate_label: &str,
) -> homeboy::core::Result<VisualCompareResult> {
    let value = homeboy::core::browser_visual_compare::run_visual_compare_provider(
        &homeboy::core::browser_visual_compare::VisualCompareProviderRequest {
            artifacts_dir,
            source_screenshot,
            candidate_screenshot,
            baseline_label,
            candidate_label,
            threshold: options.threshold,
            provider_command: &options.provider_command,
            provider_args: &options.provider_args,
        },
    )?;
    Ok(visual_compare_result_from_value(&value, artifacts_dir))
}

fn visual_compare_result_from_value(value: &Value, artifacts_dir: &Path) -> VisualCompareResult {
    let metrics = value.get("metrics").and_then(Value::as_object);
    let artifacts = value
        .get("artifacts")
        .and_then(Value::as_object)
        .map(|artifacts| {
            artifacts
                .iter()
                .filter_map(|(label, value)| {
                    let path = value
                        .get("path")
                        .and_then(Value::as_str)
                        .or_else(|| value.as_str())?;
                    Some(ArtifactRef {
                        label: label.clone(),
                        target: path.to_string(),
                    })
                })
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    VisualCompareResult {
        status: value
            .get("status")
            .and_then(Value::as_str)
            .map(str::to_string),
        mismatch_ratio: metrics
            .and_then(|metrics| metrics.get("visual_mismatch_ratio"))
            .and_then(Value::as_f64),
        mismatch_pixels: metrics
            .and_then(|metrics| metrics.get("visual_mismatch_pixels"))
            .and_then(Value::as_u64),
        total_pixels: metrics
            .and_then(|metrics| metrics.get("visual_total_pixels"))
            .and_then(Value::as_u64),
        dimension_mismatch: metrics
            .and_then(|metrics| metrics.get("visual_dimension_mismatch"))
            .and_then(Value::as_bool),
        artifacts_directory: artifacts_dir.to_string_lossy().to_string(),
        artifacts,
    }
}
