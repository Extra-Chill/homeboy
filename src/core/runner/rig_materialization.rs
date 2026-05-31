use std::collections::HashMap;

use crate::core::rig;
use crate::core::{Error, Result};

use super::{
    exec, sync_workspace, RunnerExecOptions, RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOptions,
};

pub(super) fn sync_lab_offload_rigs(
    runner_id: &str,
    homeboy_path: &str,
    remote_cwd: &str,
    args: &[String],
) -> Result<usize> {
    let rig_ids = lab_offload_rig_ids(args);
    if rig_ids.is_empty() {
        return Ok(0);
    }

    for rig_id in &rig_ids {
        let metadata = rig::read_source_metadata(rig_id).ok_or_else(|| {
            Error::validation_invalid_argument(
                "rig",
                format!(
                    "Lab offload cannot materialize rig `{rig_id}` on the runner because it has no installed source metadata"
                ),
                Some(rig_id.clone()),
                Some(vec![
                    format!("Reinstall rig `{rig_id}` from a rig package before using --runner."),
                    "Run `homeboy rig sources` to inspect installed rig sources.".to_string(),
                ]),
            )
        })?;

        let synced = sync_workspace(
            runner_id,
            RunnerWorkspaceSyncOptions {
                path: metadata.package_path,
                mode: RunnerWorkspaceSyncMode::Snapshot,
                changed_since_base: None,
            },
        )?
        .0;

        let (output, exit_code) = exec(
            runner_id,
            RunnerExecOptions {
                cwd: Some(remote_cwd.to_string()),
                project_id: None,
                allow_diagnostic_ssh: false,
                command: vec![
                    homeboy_path.to_string(),
                    "rig".to_string(),
                    "install".to_string(),
                    synced.remote_path,
                    "--id".to_string(),
                    rig_id.clone(),
                ],
                env: HashMap::new(),
                capture_patch: false,
                raw_exec: false,
                source_snapshot: None,
                capability_preflight: None,
                required_extensions: Vec::new(),
            },
        )?;

        if exit_code != 0 {
            return Err(Error::validation_invalid_argument(
                "rig",
                format!("Lab offload could not install rig `{rig_id}` on runner `{runner_id}`"),
                Some(rig_id.clone()),
                Some(vec![
                    output.stderr.trim().to_string(),
                    "Run the command with --force-hot to execute locally while investigating runner rig setup.".to_string(),
                ]),
            ));
        }
    }

    Ok(rig_ids.len())
}

fn lab_offload_rig_ids(args: &[String]) -> Vec<String> {
    let mut rig_ids = Vec::new();
    let is_bench = args.iter().any(|arg| arg == "bench");
    if !is_bench {
        return rig_ids;
    }

    let mut iter = args.iter().peekable();
    let mut passthrough = false;
    while let Some(arg) = iter.next() {
        if passthrough {
            continue;
        }
        if arg == "--" {
            passthrough = true;
            continue;
        }
        let raw = if arg == "--rig" {
            iter.next().map(String::as_str)
        } else {
            arg.strip_prefix("--rig=")
        };
        if let Some(raw) = raw {
            for rig_id in raw
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                push_unique(&mut rig_ids, rig_id.to_string());
            }
        }
    }

    rig_ids
}

fn push_unique<T: PartialEq>(items: &mut Vec<T>, item: T) {
    if !items.contains(&item) {
        items.push(item);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_unique_bench_rig_ids_for_lab_materialization() {
        let args = vec![
            "homeboy".to_string(),
            "bench".to_string(),
            "--rig".to_string(),
            "baseline,candidate".to_string(),
            "--scenario".to_string(),
            "smoke".to_string(),
            "--rig=candidate".to_string(),
        ];

        assert_eq!(
            lab_offload_rig_ids(&args),
            vec!["baseline".to_string(), "candidate".to_string()]
        );
    }

    #[test]
    fn ignores_non_bench_and_passthrough_rig_args_for_lab_materialization() {
        let non_bench = vec![
            "homeboy".to_string(),
            "test".to_string(),
            "--rig".to_string(),
            "candidate".to_string(),
        ];
        assert!(lab_offload_rig_ids(&non_bench).is_empty());

        let passthrough = vec![
            "homeboy".to_string(),
            "bench".to_string(),
            "--".to_string(),
            "--rig".to_string(),
            "candidate".to_string(),
        ];
        assert!(lab_offload_rig_ids(&passthrough).is_empty());
    }
}
