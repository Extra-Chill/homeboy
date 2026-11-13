#![cfg(target_os = "linux")]

use std::fs;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Controller(Child);

impl Drop for Controller {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_until(description: &str, timeout: Duration, mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !predicate() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {description}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn execution_supervisor_runs_as_fresh_exec_image() {
    let dir = tempfile::tempdir().unwrap();
    // A declared self-check is run by review test through the supervised,
    // bounded-output execution path. Its shell is the workload root.
    fs::write(
        dir.path().join("homeboy.json"),
        serde_json::json!({
            "id": "supervisor-fixture",
            "scripts": { "test": ["echo $PPID > ppid; sleep 30"] }
        })
        .to_string(),
    )
    .unwrap();
    let log = fs::File::create(dir.path().join("controller.log")).unwrap();
    let mut controller = Controller(
        Command::new(env!("CARGO_BIN_EXE_homeboy"))
            .args([
                "--placement",
                "local",
                "review",
                "test",
                "supervisor-fixture",
                "--path",
            ])
            .arg(dir.path())
            .current_dir(dir.path())
            .env("HOME", dir.path())
            .env("XDG_DATA_HOME", dir.path().join("data"))
            .env("XDG_CONFIG_HOME", dir.path().join("config"))
            .env("HOMEBOY_NO_UPDATE_CHECK", "1")
            .env("HOMEBOY_TEST_TIMEOUT_SECONDS", "60")
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap(),
    );
    let ppid_file = dir.path().join("ppid");
    wait_until("workload startup", Duration::from_secs(20), || {
        if let Some(status) = controller.0.try_wait().unwrap() {
            panic!(
                "controller exited {status}: {}",
                fs::read_to_string(dir.path().join("controller.log")).unwrap()
            );
        }
        fs::read_to_string(&ppid_file).is_ok_and(|value| value.trim().parse::<u32>().is_ok())
    });
    let supervisor: u32 = fs::read_to_string(ppid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let cmdline = fs::read(format!("/proc/{supervisor}/cmdline")).unwrap();
    let args: Vec<_> = cmdline.split(|byte| *byte == 0).collect();
    assert!(
        args.contains(&b"__execution_supervisor".as_slice()),
        "{args:?}; controller log: {}",
        fs::read_to_string(dir.path().join("controller.log")).unwrap()
    );
    let root: u32 = std::str::from_utf8(args[2]).unwrap().parse().unwrap();
    let private_kb: u64 = fs::read_to_string(format!("/proc/{supervisor}/smaps_rollup"))
        .unwrap()
        .lines()
        .filter(|line| line.starts_with("Private_Clean:") || line.starts_with("Private_Dirty:"))
        .map(|line| {
            line.split_whitespace()
                .nth(1)
                .unwrap()
                .parse::<u64>()
                .unwrap()
        })
        .sum();
    assert!(
        private_kb < 32 * 1024,
        "supervisor private memory: {private_kb} KiB"
    );

    let mut sleep_pid = None;
    wait_until("sleep workload", Duration::from_secs(5), || {
        let children =
            fs::read_to_string(format!("/proc/{root}/task/{root}/children")).unwrap_or_default();
        sleep_pid = children
            .split_whitespace()
            .filter_map(|pid| pid.parse::<u32>().ok())
            .find(|pid| {
                fs::read_to_string(format!("/proc/{pid}/comm"))
                    .is_ok_and(|name| name.trim() == "sleep")
            });
        sleep_pid.is_some()
    });
    // The workload can exec before the supervisor's new image has written its
    // identity and installed signal handling. Wait for protocol initialization
    // before testing loss of an established controller.
    wait_until("supervisor initialization", Duration::from_secs(5), || {
        fs::read_to_string(format!("/proc/{supervisor}/status")).is_ok_and(|status| {
            status.lines().any(|line| {
                line.strip_prefix("SigCgt:")
                    .and_then(|mask| u64::from_str_radix(mask.trim(), 16).ok())
                    .is_some_and(|mask| mask & (1 << (libc::SIGTERM - 1)) != 0)
            })
        })
    });
    controller.0.kill().unwrap(); // SIGKILL, not cooperative shutdown.
    controller.0.wait().unwrap();
    wait_until("controller-death teardown", Duration::from_secs(10), || {
        !std::path::Path::new(&format!("/proc/{root}")).exists()
            && !std::path::Path::new(&format!("/proc/{}", sleep_pid.unwrap())).exists()
    });
}
