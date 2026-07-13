//! Effective component path provenance for rig-pinned benches with
//! `--path` overrides. See Extra-Chill/homeboy#2362.
//!
//! Without these tests the rig snapshot persisted on bench output would
//! still show the rig-declared checkout even when the workload actually
//! ran against a `--path` override. That makes diagnostics ambiguous —
//! the workload exercised one checkout while the run envelope reported
//! another.

use super::*;

fn write_package_rig(package: &std::path::Path, id: &str) {
    let rig_dir = package.join("rigs").join(id);
    std::fs::create_dir_all(&rig_dir).expect("create rig package");
    std::fs::write(
        rig_dir.join("rig.json"),
        format!(
            r#"{{
                "id": "{id}",
                "components": {{
                    "studio": {{
                        "path": "{}",
                        "extensions": {{ "fixture-bench": {{}} }}
                    }}
                }},
                "bench": {{ "default_component": "studio" }}
            }}"#,
            package.display()
        ),
    )
    .expect("write rig package");
}

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

#[test]
fn rig_pinned_bench_repairs_missing_linked_source_from_path_override() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let source_a = tempfile::TempDir::new().expect("source A");
        let checkout_b = tempfile::TempDir::new().expect("checkout B");
        write_package_rig(source_a.path(), "stale-rig");
        write_package_rig(checkout_b.path(), "stale-rig");
        homeboy::core::rig::install(
            source_a.path().to_str().expect("source A path"),
            None,
            false,
        )
        .expect("install linked rig from source A");
        std::fs::remove_dir_all(source_a.path()).expect("remove source A");

        let mut args = run_args(
            None,
            vec!["stale-rig".to_string()],
            vec!["rig-slow".to_string()],
        );
        args.run.comp.path = Some(checkout_b.path().to_string_lossy().into_owned());

        let (_, exit_code) = run(args, &GlobalArgs {}).expect("bench recovers linked rig source");

        assert_eq!(exit_code, 0);
        let metadata = homeboy::core::rig::read_source_metadata("stale-rig")
            .expect("refreshed source metadata");
        assert_eq!(metadata.package_path, checkout_b.path().to_string_lossy());
    });
}

#[test]
fn missing_linked_source_recovery_preserves_conflicting_user_owned_rig() {
    with_isolated_home(|home| {
        let source_a = tempfile::TempDir::new().expect("source A");
        let checkout_b = tempfile::TempDir::new().expect("checkout B");
        write_package_rig(source_a.path(), "stale-rig");
        write_package_rig(checkout_b.path(), "stale-rig");
        homeboy::core::rig::install(
            source_a.path().to_str().expect("source A path"),
            None,
            false,
        )
        .expect("install linked rig from source A");
        std::fs::remove_dir_all(source_a.path()).expect("remove source A");

        let config_path = home.path().join(".config/homeboy/rigs/stale-rig.json");
        let user_owned = r#"{ "id": "other-rig", "components": {} }"#;
        std::fs::write(&config_path, user_owned).expect("write conflicting user rig");

        let error = homeboy::core::rig::RigSourceContext::load_for_invocation_at(
            "stale-rig",
            Some(checkout_b.path().to_str().expect("checkout B path")),
        )
        .expect_err("conflicting user rig must remain protected");

        assert!(error.message.contains("refusing to replace"));
        assert_eq!(
            std::fs::read_to_string(config_path).expect("read user rig"),
            user_owned
        );
    });
}
