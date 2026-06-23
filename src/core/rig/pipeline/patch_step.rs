//! Idempotent local-only patch pipeline step.

use std::path::PathBuf;

use super::super::expand::expand_vars;
use super::super::spec::{PatchOp, RigSpec};
use super::component::resolve_component_path;
use crate::core::error::{Error, Result};

/// Apply or verify an idempotent local-only patch.
///
/// `apply` semantics:
/// - If `marker` already appears in the file → no-op (idempotent).
/// - If `after` is set and not in the file → fail with "anchor missing"
///   (file structure changed; refuse to guess where to insert).
/// - If `after` is set and present → insert `content` on the next line
///   after the first occurrence.
/// - If `after` is `None` → append `content` to the end of the file.
/// - Resulting file must contain `marker` (validated against `content`
///   at apply time so misconfigured specs error early instead of
///   double-applying on every run).
///
/// `verify` semantics: pass iff `marker` is present. Read-only — for
/// `check` pipelines that surface stale or unpatched checkouts.
pub(super) fn run_patch_step(
    rig: &RigSpec,
    component_id: &str,
    file_rel: &str,
    marker: &str,
    after: Option<&str>,
    content: &str,
    op: PatchOp,
) -> Result<()> {
    let (_, component_path) = resolve_component_path(rig, component_id)?;
    let expanded_rel = expand_vars(rig, file_rel);
    let path = if PathBuf::from(&expanded_rel).is_absolute() {
        PathBuf::from(&expanded_rel)
    } else {
        PathBuf::from(&component_path).join(&expanded_rel)
    };

    let body = std::fs::read_to_string(&path).map_err(|e| {
        Error::rig_pipeline_failed(&rig.id, "patch", format!("read {}: {}", path.display(), e))
    })?;

    if body.contains(marker) {
        return Ok(()); // Already applied (apply) / present (verify).
    }

    if op == PatchOp::Verify {
        return Err(Error::rig_pipeline_failed(
            &rig.id,
            "patch",
            format!(
                "marker {:?} not found in {} — patch missing or stale checkout",
                marker,
                path.display()
            ),
        ));
    }

    if !content.contains(marker) {
        return Err(Error::rig_pipeline_failed(
            &rig.id,
            "patch",
            format!(
                "patch content does not contain marker {:?} — applying it would not be detectable next run, so the step would re-apply forever",
                marker
            ),
        ));
    }

    let new_body = match after {
        Some(anchor) => {
            let anchor_idx = body.find(anchor).ok_or_else(|| {
                Error::rig_pipeline_failed(
                    &rig.id,
                    "patch",
                    format!(
                        "anchor {:?} not found in {} — file structure changed, refusing to guess insertion point",
                        anchor,
                        path.display()
                    ),
                )
            })?;
            // Insert at the start of the line *after* the anchor's line.
            let after_anchor = anchor_idx + anchor.len();
            let next_newline = body[after_anchor..]
                .find('\n')
                .map(|n| after_anchor + n + 1)
                .unwrap_or(body.len());
            let mut out = String::with_capacity(body.len() + content.len() + 1);
            out.push_str(&body[..next_newline]);
            out.push_str(content);
            if !content.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&body[next_newline..]);
            out
        }
        None => {
            let mut out = body.clone();
            if !out.ends_with('\n') && !out.is_empty() {
                out.push('\n');
            }
            out.push_str(content);
            if !content.ends_with('\n') {
                out.push('\n');
            }
            out
        }
    };

    std::fs::write(&path, new_body).map_err(|e| {
        Error::rig_pipeline_failed(&rig.id, "patch", format!("write {}: {}", path.display(), e))
    })?;

    Ok(())
}
