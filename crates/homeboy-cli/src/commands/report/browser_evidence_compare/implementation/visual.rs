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
    include_local_paths: bool,
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
            include_local_paths,
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
    include_local_paths: bool,
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
    Ok(visual_compare_result_from_value(
        &value,
        artifacts_dir,
        include_local_paths,
    ))
}

/// Redact a provider-emitted artifact path for reviewer-facing reports.
///
/// The visual-compare provider returns absolute local paths. Handing those to
/// a reviewer/agent invites hand-built, wrong-shape tunnel URLs (the failure
/// that produced 404s against the artifact origin). When local paths are
/// suppressed we strip to a path relative to the visual-compare artifacts
/// directory, mirroring `parse::artifact_ref`'s root-relative redaction.
fn redact_visual_artifact_target(
    path: &str,
    artifacts_dir: &Path,
    include_local_paths: bool,
) -> String {
    if include_local_paths {
        return path.to_string();
    }
    Path::new(path)
        .strip_prefix(artifacts_dir)
        .unwrap_or_else(|_| Path::new(path))
        .display()
        .to_string()
}

fn visual_compare_result_from_value(
    value: &Value,
    artifacts_dir: &Path,
    include_local_paths: bool,
) -> VisualCompareResult {
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
                        target: redact_visual_artifact_target(
                            path,
                            artifacts_dir,
                            include_local_paths,
                        ),
                    })
                })
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let artifacts_directory = if include_local_paths {
        artifacts_dir.to_string_lossy().to_string()
    } else {
        artifacts_dir
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| artifacts_dir.to_string_lossy().to_string())
    };
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
        artifacts_directory,
        artifacts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider_value(artifacts_dir: &Path) -> Value {
        serde_json::json!({
            "status": "compared",
            "metrics": { "visual_mismatch_ratio": 0.5153_f64 },
            "artifacts": {
                "candidate": { "path": artifacts_dir.join("candidate.png").display().to_string() },
                "diff": { "path": artifacts_dir.join("diff.png").display().to_string() },
            }
        })
    }

    #[test]
    fn redacts_absolute_provider_paths_to_relative_when_local_paths_suppressed() {
        let artifacts_dir = Path::new("/home/chubes/work/visual-compare/27-university-department");
        let result =
            visual_compare_result_from_value(&provider_value(artifacts_dir), artifacts_dir, false);

        // No absolute path leaks to the reviewer-facing report.
        for artifact in &result.artifacts {
            assert!(
                !artifact.target.starts_with('/'),
                "leaked absolute path: {}",
                artifact.target
            );
        }
        let candidate = result
            .artifacts
            .iter()
            .find(|artifact| artifact.label == "candidate")
            .expect("candidate artifact");
        assert_eq!(candidate.target, "candidate.png");
        // artifacts_directory is reduced to the slug, not an absolute path.
        assert_eq!(result.artifacts_directory, "27-university-department");
    }

    #[test]
    fn keeps_absolute_provider_paths_when_local_paths_included() {
        let artifacts_dir = Path::new("/home/chubes/work/visual-compare/27-university-department");
        let result =
            visual_compare_result_from_value(&provider_value(artifacts_dir), artifacts_dir, true);

        let candidate = result
            .artifacts
            .iter()
            .find(|artifact| artifact.label == "candidate")
            .expect("candidate artifact");
        assert_eq!(
            candidate.target,
            artifacts_dir.join("candidate.png").display().to_string()
        );
        assert_eq!(
            result.artifacts_directory,
            artifacts_dir.display().to_string()
        );
    }

    #[test]
    fn redact_helper_leaves_unrelated_paths_untouched() {
        // A provider path outside artifacts_dir (unexpected) is left as-is
        // rather than mangled, so nothing silently corrupts the reference.
        let artifacts_dir = Path::new("/a/b/c");
        assert_eq!(
            redact_visual_artifact_target("/x/y/z.png", artifacts_dir, false),
            "/x/y/z.png"
        );
    }
}
