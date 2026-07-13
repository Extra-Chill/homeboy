//! Effective component path provenance for rig-pinned benches with
//! `--path` overrides. See Extra-Chill/homeboy#2362.
//!
//! Without these tests the rig snapshot persisted on bench output would
//! still show the rig-declared checkout even when the workload actually
//! ran against a `--path` override. That makes diagnostics ambiguous —
//! the workload exercised one checkout while the run envelope reported
//! another.

use super::*;

#[test]
fn rig_pinned_bench_with_path_override_records_effective_component_path() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let rig_component_dir = tempfile::TempDir::new().expect("rig component dir");
        let override_component_dir = tempfile::TempDir::new().expect("override component dir");
        write_rig(home, "studio-bfb", "studio", rig_component_dir.path());

        let mut args = run_args(
            None,
            vec!["studio-bfb".to_string()],
            vec!["rig-slow".to_string()],
        );
        args.run.comp.path = Some(override_component_dir.path().to_string_lossy().into_owned());

        let (output, exit_code) =
            run(args, &GlobalArgs {}).expect("rig-pinned bench with --path should run");

        assert_eq!(exit_code, 0);
        match output {
            BenchOutput::Single(result) => {
                let rig_state = result.rig_state.expect("rig snapshot on rig-pinned output");
                assert_eq!(rig_state.rig_id, "studio-bfb");
                let component = rig_state
                    .components
                    .get("studio")
                    .expect("studio component in snapshot");
                assert_eq!(
                    component.path,
                    override_component_dir.path().to_string_lossy()
                );
                assert_eq!(
                    component.declared_path.as_deref(),
                    Some(rig_component_dir.path().to_string_lossy().as_ref()),
                    "rig-declared path should be preserved alongside the effective override"
                );
            }
            _ => panic!("expected single output"),
        }
    });
}

#[test]
fn rig_pinned_bench_without_path_override_keeps_rig_declared_path() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let rig_component_dir = tempfile::TempDir::new().expect("rig component dir");
        write_rig(home, "studio-bfb", "studio", rig_component_dir.path());

        let (output, exit_code) = run(
            run_args(
                None,
                vec!["studio-bfb".to_string()],
                vec!["rig-slow".to_string()],
            ),
            &GlobalArgs {},
        )
        .expect("rig-pinned bench without override should run");

        assert_eq!(exit_code, 0);
        match output {
            BenchOutput::Single(result) => {
                let rig_state = result.rig_state.expect("rig snapshot");
                let component = rig_state
                    .components
                    .get("studio")
                    .expect("studio component in snapshot");
                assert_eq!(component.path, rig_component_dir.path().to_string_lossy());
                assert!(
                    component.declared_path.is_none(),
                    "declared_path is only set when path was actually overridden, got {:?}",
                    component.declared_path
                );
            }
            _ => panic!("expected single output"),
        }
    });
}
