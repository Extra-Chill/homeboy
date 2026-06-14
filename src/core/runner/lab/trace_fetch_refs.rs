//! Trace compare target materialization refs for Lab git workspace sync.

use std::path::Path;

use crate::core::{Error, Result};

use super::super::origin_refs::{advertised_origin_refs_for_commit, best_advertised_ref};
use super::super::RunnerWorkspaceSyncMode;

pub(super) fn lab_offload_git_fetch_refs(
    args: &[String],
    source_path: &Path,
    sync_mode: RunnerWorkspaceSyncMode,
) -> Result<Vec<String>> {
    if sync_mode != RunnerWorkspaceSyncMode::Git {
        return Ok(Vec::new());
    }

    let mut refs = Vec::new();
    for target in lab_offload_trace_compare_targets(args) {
        if trace_compare_target_is_local_path(&target) || target.starts_with("origin/") {
            continue;
        }
        let git_ref = if target.starts_with("refs/") {
            Some(target.clone())
        } else {
            advertised_origin_ref_for_local_target(source_path, &target)?
        };
        if let Some(git_ref) = git_ref {
            if !refs.contains(&git_ref) {
                refs.push(git_ref);
            }
        }
    }
    Ok(refs)
}

fn lab_offload_trace_compare_targets(args: &[String]) -> Vec<String> {
    let mut targets = Vec::new();
    let mut iter = args.iter().skip(1).peekable();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        let target = if arg == "--baseline-target" || arg == "--candidate" {
            iter.next().cloned()
        } else {
            arg.strip_prefix("--baseline-target=")
                .or_else(|| arg.strip_prefix("--candidate="))
                .map(str::to_string)
        };
        if let Some(target) = target {
            targets.push(target);
        }
    }
    targets
}

fn trace_compare_target_is_local_path(target: &str) -> bool {
    let expanded = shellexpand::tilde(target).to_string();
    Path::new(&expanded).exists()
}

fn advertised_origin_ref_for_local_target(
    source_path: &Path,
    target: &str,
) -> Result<Option<String>> {
    let commit = match super::super::workspace::git_output(
        source_path,
        &["rev-parse", "--verify", &format!("{target}^{{commit}}")],
    ) {
        Ok(commit) => commit,
        Err(_) => return Ok(None),
    };
    let refs = advertised_origin_refs_for_commit(
        source_path,
        &commit,
        "trace_compare_target",
        "Lab offload could not inspect origin refs for trace compare target materialization",
        target.to_string(),
        vec!["Run with --force-hot to execute trace compare locally while investigating remote ref availability.".to_string()],
    )?;
    if refs.is_empty() && is_full_hex_sha(target) {
        return Err(Error::validation_invalid_argument(
            "trace_compare_target",
            "Lab offload could not find an advertised origin ref for the trace compare target commit",
            Some(target.to_string()),
            Some(vec![
                "Push the candidate commit to origin or pass an advertised ref such as refs/pull/<id>/head.".to_string(),
                "Run with --force-hot to execute trace compare locally while investigating remote ref availability.".to_string(),
            ]),
        ));
    }

    Ok(best_advertised_ref(refs))
}

fn is_full_hex_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_compare_targets_are_extracted_before_passthrough_args() {
        let args = vec![
            "homeboy".to_string(),
            "trace".to_string(),
            "compare".to_string(),
            "--baseline-target".to_string(),
            "origin/develop".to_string(),
            "--candidate=32f68bb07ac0efa1d754f78e2adc8de115ddca6f".to_string(),
            "--".to_string(),
            "--candidate".to_string(),
            "ignored".to_string(),
        ];

        assert_eq!(
            lab_offload_trace_compare_targets(&args),
            vec![
                "origin/develop".to_string(),
                "32f68bb07ac0efa1d754f78e2adc8de115ddca6f".to_string(),
            ]
        );
    }
}
