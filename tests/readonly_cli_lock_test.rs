use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[test]
fn read_only_cli_commands_complete_while_runtime_promotion_is_held() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let _promotion = homeboy::core::runtime_promotion::acquire("test promotion", "test")
            .expect("hold runtime promotion lease");
        let home = home.path();

        for args in [
            vec!["--version"],
            vec!["--help"],
            vec!["self", "identity"],
            vec!["self", "status"],
            vec!["status"],
        ] {
            let output = run_with_timeout(&args, home, Duration::from_secs(10));
            assert!(
                output.status.success(),
                "{} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                !output.stdout.is_empty(),
                "{} produced no diagnostic output",
                args.join(" ")
            );
        }

        let output = run_with_timeout(
            &["upgrade", "--method", "binary"],
            home,
            Duration::from_secs(10),
        );
        assert!(
            !output.status.success(),
            "a concurrent mutation must not run"
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("runtime_promotion.contended"),
            "mutation exclusion must report typed promotion contention: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    });
}

fn run_with_timeout(args: &[&str], home: &std::path::Path, timeout: Duration) -> Output {
    let child = Command::new(homeboy_bin())
        .args(args)
        .env("HOME", home)
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .current_dir(home)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start Homeboy child");
    wait_for_output(child, timeout)
}

fn wait_for_output(mut child: Child, timeout: Duration) -> Output {
    let started = Instant::now();
    loop {
        if child.try_wait().expect("inspect Homeboy child").is_some() {
            return child
                .wait_with_output()
                .expect("collect Homeboy child output");
        }
        if started.elapsed() >= timeout {
            child.kill().expect("terminate blocked Homeboy child");
            let output = child
                .wait_with_output()
                .expect("collect timed-out Homeboy child");
            panic!("Homeboy child exceeded {timeout:?}: {output:?}");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn homeboy_bin() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_BIN_EXE_homeboy").expect("CARGO_BIN_EXE_homeboy"))
}
