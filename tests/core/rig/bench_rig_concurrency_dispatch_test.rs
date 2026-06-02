use super::*;
use crate::test_support::with_isolated_home;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;

fn write_ordering_bench_extension(home: &TempDir) {
    let extension_dir = home
        .path()
        .join(".config")
        .join("homeboy")
        .join("extensions")
        .join("fixture-bench");
    fs::create_dir_all(&extension_dir).expect("mkdir extension");
    fs::write(
        extension_dir.join("fixture-bench.json"),
        r#"{"name":"Fixture Bench","version":"0.0.0","bench":{"extension_script":"bench-runner.sh"}}"#,
    )
    .expect("write extension manifest");

    let script_path = extension_dir.join("bench-runner.sh");
    fs::write(
        &script_path,
        r#"#!/bin/sh
id="$(basename "$HOMEBOY_COMPONENT_PATH")"
log="$HOMEBOY_BENCH_SHARED_STATE/order.log"
mkdir -p "$(dirname "$log")"
printf '%s start\n' "$id" >> "$log"
sleep 0.3
printf '%s end\n' "$id" >> "$log"
cat > "$HOMEBOY_BENCH_RESULTS_FILE" <<JSON
{"component_id":"$HOMEBOY_COMPONENT_ID","iterations":${HOMEBOY_BENCH_ITERATIONS:-0},"scenarios":[{"id":"ordered","iterations":${HOMEBOY_BENCH_ITERATIONS:-0},"metrics":{"p95_ms":1.0}}],"metric_policies":{"p95_ms":{"direction":"lower_is_better"}}}
JSON
"#,
    )
    .expect("write bench script");

    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(&script_path)
            .expect("script metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("chmod script");
    }
}

fn bench_order_events(shared_state: &TempDir) -> Vec<String> {
    fs::read_to_string(shared_state.path().join("order.log"))
        .expect("order log")
        .lines()
        .map(|line| line.split_whitespace().last().expect("event").to_string())
        .collect()
}

#[test]
fn cross_rig_default_runs_rigs_sequentially() {
    with_isolated_home(|home| {
        write_ordering_bench_extension(home);
        let component_a = tempfile::TempDir::new().expect("component a");
        let component_b = tempfile::TempDir::new().expect("component b");
        let shared_state = tempfile::TempDir::new().expect("shared state");
        write_rig(home, "rig-a", "studio", component_a.path());
        write_rig(home, "rig-b", "studio", component_b.path());

        let mut args = run_args(
            None,
            vec!["rig-a".to_string(), "rig-b".to_string()],
            Vec::new(),
        );
        args.run.shared_state = Some(shared_state.path().to_path_buf());

        let (_output, exit_code) = run(args, &GlobalArgs {}).expect("cross-rig bench should run");

        assert_eq!(exit_code, 0);
        assert_eq!(
            bench_order_events(&shared_state),
            vec!["start", "end", "start", "end"]
        );
    });
}

#[test]
fn cross_rig_concurrency_runs_rigs_in_parallel() {
    with_isolated_home(|home| {
        write_ordering_bench_extension(home);
        let component_a = tempfile::TempDir::new().expect("component a");
        let component_b = tempfile::TempDir::new().expect("component b");
        let shared_state = tempfile::TempDir::new().expect("shared state");
        write_rig(home, "rig-a", "studio", component_a.path());
        write_rig(home, "rig-b", "studio", component_b.path());

        let mut args = run_args(
            None,
            vec!["rig-a".to_string(), "rig-b".to_string()],
            Vec::new(),
        );
        args.run.shared_state = Some(shared_state.path().to_path_buf());
        args.run.rig_concurrency = 2;

        let (output, exit_code) = run(args, &GlobalArgs {}).expect("cross-rig bench should run");

        assert_eq!(exit_code, 0);
        let BenchOutput::Comparison(result) = output else {
            panic!("expected comparison output")
        };
        assert_eq!(result.rigs[0].rig_id, "rig-a");
        assert_eq!(result.rigs[1].rig_id, "rig-b");

        let events = bench_order_events(&shared_state);
        assert_eq!(events.len(), 4, "events: {events:?}");
        assert_eq!(
            events
                .iter()
                .filter(|event| event.as_str() == "start")
                .count(),
            2
        );
        assert_eq!(events[0], "start", "events: {events:?}");
        assert_eq!(events[1], "start", "events: {events:?}");
    });
}
