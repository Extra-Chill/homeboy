use crate::cli_surface::{Cli, Commands};
use clap::Parser;
use std::time::{Duration, Instant};

/// `homeboy runner doctor homeboy-lab` (JSON by default) answers with the
/// pressure block of the runner's real Homeboy roots within its deadline.
#[test]
fn runner_doctor_json_returns_the_pressure_block_within_its_deadline() {
    crate::test_support::with_isolated_home(|home| {
        let workspace = home.path().join("lab");
        let data = homeboy::core::paths::homeboy_data().expect("data root");
        let fixtures = [
            workspace.join("_lab_workspaces/run-1/src/lib.rs"),
            homeboy::core::paths::controller_scratch_store()
                .expect("scratch")
                .join("attempt/out.log"),
            data.join("task-worktrees/task-1/README.md"),
            data.join("runtime/tmp/gate-home/cache.bin"),
            data.join("gate-inputs/input.json"),
            homeboy::core::paths::artifact_root()
                .expect("artifacts")
                .join("run-1/report.json"),
        ];
        for file in &fixtures {
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, vec![b'x'; 8192]).unwrap();
        }
        homeboy::runner::runners::create(
            &serde_json::json!({
                "id": "homeboy-lab",
                "kind": "local",
                "workspace_root": workspace,
            })
            .to_string(),
            false,
        )
        .expect("register runner");

        let cli = Cli::try_parse_from(["homeboy", "runner", "doctor", "homeboy-lab"])
            .expect("parse runner doctor");
        let Commands::Runner(args) = cli.command else {
            panic!("expected runner command");
        };
        let started = Instant::now();
        let run = crate::commands::runner::run_command_output(args);
        let elapsed = started.elapsed();

        let payload = run.stdout_result.expect("doctor JSON");
        let pressure = &payload["pressure"];
        assert_eq!(pressure["schema"], "homeboy/runner-pressure/v1");
        let deadline = Duration::from_millis(pressure["deadline_ms"].as_u64().unwrap());
        assert!(
            Duration::from_millis(pressure["elapsed_ms"].as_u64().unwrap())
                <= deadline + Duration::from_secs(1)
        );
        assert!(elapsed < Duration::from_secs(60), "doctor took {elapsed:?}");
        assert!(pressure["disk"]["free_bytes"].as_u64().is_some());
        assert!(pressure["disk"]["used_bytes"].as_u64().is_some());
        let roots = pressure["roots"].as_array().expect("roots");
        let root = |name: &str| {
            roots
                .iter()
                .find(|root| root["name"] == name)
                .unwrap_or_else(|| panic!("missing root {name}: {roots:?}"))
        };
        for name in [
            "controller-scratch",
            "_lab_workspaces",
            "task-worktrees",
            "runtime/tmp",
            "gate-inputs",
            "artifacts",
        ] {
            let usage = root(name);
            assert_eq!(usage["measurement"], "measured", "{name}: {usage}");
            assert!(usage["bytes"].as_u64().unwrap() >= 8192, "{name}: {usage}");
            assert!(usage["entries"].as_u64().unwrap() >= 1, "{name}: {usage}");
        }
        assert!(root("daemon")["measurement"].is_string());
        if cfg!(target_os = "linux") {
            assert!(pressure["memory"]["available_bytes"].as_u64().is_some());
            assert!(pressure["memory"]["swap_used_bytes"].as_u64().is_some());
            assert!(pressure["load_per_cpu"].as_f64().is_some());
        }
    });
}
