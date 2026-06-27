use super::*;

#[test]
fn run_list_uses_rig_default_component_and_workloads() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_rig(home, "studio-bfb", "studio", component_dir.path());

        let (output, exit_code) = run_list(&list_args(None, vec!["studio-bfb".to_string()]))
            .expect("rig bench list should run");

        assert_eq!(exit_code, 0);
        match output {
            BenchOutput::List(result) => {
                assert_eq!(result.component, "studio");
                assert_eq!(result.component_id, "studio");
                assert_eq!(result.count, 2);
                assert_eq!(result.scenarios[0].id, "rig-extra");
            }
            _ => panic!("expected list output"),
        }
    });
}

#[test]
fn run_list_serializes_installed_rig_package_evidence() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        let package_root = install_rig_package(home, "studio-bfb", "studio", component_dir.path());

        let (output, exit_code) = run_list(&list_args(None, vec!["studio-bfb".to_string()]))
            .expect("rig bench list should run");

        assert_eq!(exit_code, 0);
        match output {
            BenchOutput::List(result) => {
                let package_root = package_root.to_string_lossy().to_string();
                assert_eq!(result.scenarios[0].id, "package-extra");
                let evidence = result.rig_package.expect("rig package evidence");
                assert_eq!(evidence.rig_id, "studio-bfb");
                assert_eq!(evidence.package_root, package_root);
                assert_eq!(evidence.source, package_root);
                assert!(!evidence.freshness_verified);
                assert_eq!(
                    evidence.freshness,
                    homeboy::core::extension::bench::parsing::RigPackageFreshness::Unknown
                );
                assert!(evidence
                    .refresh_command
                    .as_deref()
                    .unwrap_or_default()
                    .contains("homeboy rig install"));
            }
            _ => panic!("expected list output"),
        }
    });
}

#[test]
fn run_list_prefers_enclosing_local_rig_package_over_installed_package() {
    let _guard = current_dir_lock().lock().expect("lock current dir");
    let original_dir = std::env::current_dir().expect("current dir");

    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        install_rig_package(home, "studio-bfb", "studio", component_dir.path());

        let local_package = tempfile::TempDir::new().expect("local package");
        write_local_rig_package(
            local_package.path(),
            "studio-bfb",
            "studio",
            component_dir.path(),
            "fresh-checkout-extra",
        );
        std::env::set_current_dir(local_package.path()).expect("enter local package");

        let result = run_list(&list_args(None, vec!["studio-bfb".to_string()]));
        std::env::set_current_dir(&original_dir).expect("restore current dir");
        let (output, exit_code) = result.expect("rig bench list should run");

        assert_eq!(exit_code, 0);
        match output {
            BenchOutput::List(result) => {
                assert_eq!(result.scenarios[0].id, "fresh-checkout-extra");
                let evidence = result.rig_package.expect("rig package evidence");
                assert_eq!(
                    std::path::PathBuf::from(evidence.package_root)
                        .canonicalize()
                        .expect("canonical evidence root"),
                    local_package
                        .path()
                        .canonicalize()
                        .expect("canonical local package")
                );
                assert!(evidence.freshness_verified);
            }
            _ => panic!("expected list output"),
        }
    });

    std::env::set_current_dir(original_dir).expect("restore current dir");
}

#[test]
fn run_list_prefers_rig_workloads_over_component_bench_script() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_rig_with_component_script(home, "studio-bfb", "studio", component_dir.path());

        let (output, exit_code) = run_list(&list_args(None, vec!["studio-bfb".to_string()]))
            .expect("rig bench list should use rig workloads");

        assert_eq!(exit_code, 0);
        match output {
            BenchOutput::List(result) => {
                assert_eq!(result.count, 1);
                assert_eq!(result.scenarios[0].id, "rig-extra");
            }
            _ => panic!("expected list output"),
        }
    });
}

#[test]
fn run_list_preserves_registered_component_path() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_registered_component(home, "studio", component_dir.path());

        let (output, exit_code) =
            run_list(&list_args(Some("studio"), Vec::new())).expect("plain bench list should run");

        assert_eq!(exit_code, 0);
        match output {
            BenchOutput::List(result) => {
                assert_eq!(result.component, "studio");
                assert_eq!(result.component_id, "studio");
                assert_eq!(result.count, 2);
                assert_eq!(result.scenarios[0].id, "in-tree");
            }
            _ => panic!("expected list output"),
        }
    });
}

#[test]
fn run_list_filters_selected_scenario() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_registered_component(home, "studio", component_dir.path());

        let mut args = list_args(Some("studio"), Vec::new());
        args.scenario_ids = vec!["slow".to_string()];
        let (output, exit_code) = run_list(&args).expect("plain bench list should run");

        assert_eq!(exit_code, 0);
        match output {
            BenchOutput::List(result) => {
                assert_eq!(result.count, 1);
                assert_eq!(result.scenarios[0].id, "slow");
            }
            _ => panic!("expected list output"),
        }
    });
}

#[test]
fn run_list_requires_rig_default_component_when_component_omitted() {
    with_isolated_home(|home| {
        let rig_dir = home.path().join(".config").join("homeboy").join("rigs");
        fs::create_dir_all(&rig_dir).expect("mkdir rigs");
        fs::write(rig_dir.join("empty.json"), r#"{ "bench": {} }"#).expect("write rig");

        let err = match run_list(&list_args(None, vec!["empty".to_string()])) {
            Ok(_) => panic!("missing default component should error"),
            Err(err) => err,
        };
        let message = err.to_string();
        assert!(
            message.contains("bench.default_component"),
            "expected default-component error, got: {}",
            message
        );
    });
}
