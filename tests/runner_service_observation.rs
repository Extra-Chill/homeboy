//! A real runner service owns its job observation across transport replacement.
use serde_json::{json, Value};
use std::process::Command;
use std::time::{Duration, Instant};

struct DaemonGuard {
    binary: &'static str,
    home: std::path::PathBuf,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = Command::new(self.binary)
            .args(["daemon", "stop"])
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HOMEBOY_NO_UPDATE_CHECK", "1")
            .output();
    }
}

#[test]
fn runner_observation_and_watch_survive_client_loss_without_resubmission() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let binary = env!("CARGO_BIN_EXE_homeboy");
        let root = home.path();
        let cli = |args: &[&str]| -> Value {
            let output = Command::new(binary)
                .args(args)
                .env_clear()
                .env("HOME", root)
                .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
                .env("HOMEBOY_NO_UPDATE_CHECK", "1")
                .output()
                .expect("CLI output");
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            serde_json::from_slice::<Value>(&output.stdout).expect("CLI JSON")["data"].clone()
        };
        cli(&[
            "runner",
            "add",
            "fixture-local",
            "--kind",
            "local",
            "--workspace-root",
            root.to_str().unwrap(),
        ]);
        cli(&["daemon", "start"]);
        let _daemon_guard = DaemonGuard {
            binary,
            home: root.to_path_buf(),
        };
        let daemon = cli(&["daemon", "status"]);
        let url = format!(
            "http://{}",
            daemon["daemon"]["address"]
                .as_str()
                .expect("daemon endpoint")
        );
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        let read = |client: &reqwest::blocking::Client, route: &str| -> Value {
            let response: Value = client
                .get(format!("{url}{route}"))
                .send()
                .expect("HTTP read")
                .json()
                .expect("HTTP JSON");
            response["data"]["body"].clone()
        };
        let empty = read(&client, homeboy_runner_contract::RUNNER_API_DESCRIBE_PATH);
        assert_eq!(empty["active_runner_job_count"], 0);
        assert_eq!(empty["freshness"]["active_jobs"], 0);
        assert_eq!(empty["lease_id"], daemon["daemon"]["lease_id"]);

        let marker = root.join("effect");
        let request = json!({"runner_id":"fixture-local", "cwd":root,
            "idempotency_key":"one-fixture-execution", "command":["/bin/sh","-c",
                format!("sleep 8; echo effect >> '{}'", marker.display())]});
        // Lose the submission transport before its admission response. Recovery
        // must resolve the same idempotency key, not start another execution.
        {
            use std::io::Write;
            let body = request.to_string();
            let mut socket =
                std::net::TcpStream::connect(daemon["daemon"]["address"].as_str().unwrap())
                    .expect("submission transport");
            write!(socket, "POST /exec HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).expect("submit before transport loss");
        }
        // Establish that the lost-response request was accepted before retrying;
        // otherwise a successful retry alone would not prove response-loss recovery.
        let deadline = Instant::now() + Duration::from_secs(5);
        let lost_response_job_id = loop {
            let observation = read(&client, homeboy_runner_contract::RUNNER_API_DESCRIBE_PATH);
            let jobs = observation["active_runner_jobs"].as_array().unwrap();
            if let Some(job) = jobs.first() {
                assert_eq!(jobs.len(), 1);
                break job["job_id"]
                    .as_str()
                    .expect("accepted job identity")
                    .to_string();
            }
            assert!(
                Instant::now() < deadline,
                "lost-response submission was not admitted"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        let submit = |client: &reqwest::blocking::Client| -> Value {
            client
                .post(format!("{url}/exec"))
                .json(&request)
                .send()
                .expect("submit")
                .json::<Value>()
                .expect("admission JSON")["data"]["body"]
                .clone()
        };
        let admitted = submit(&client);
        let id = admitted["job"]["id"].as_str().expect("durable job ID");
        assert_eq!(
            id, lost_response_job_id,
            "retry must recover the admission whose response was lost"
        );
        let replay = submit(&client);
        assert_eq!(
            replay["job"]["id"], id,
            "idempotent submit must keep one job"
        );
        let first =
            read(&client, &format!("/jobs/{id}/watch?after_sequence=0"))["response"].clone();
        assert_eq!(
            first["terminal"], false,
            "transport replacement must happen mid-job"
        );
        let mut cursor = first["next_sequence"].as_u64().expect("event cursor");
        drop(client);
        // Replace the transport/client, retaining only job ID and event cursor.
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        let terminal = loop {
            let observation = read(&client, homeboy_runner_contract::RUNNER_API_DESCRIBE_PATH);
            assert_eq!(
                observation["lease_id"], empty["lease_id"],
                "transport replacement must not rotate service ownership"
            );
            assert_eq!(
                observation["active_runner_job_count"],
                observation["freshness"]["active_jobs"]
            );
            assert_eq!(
                observation["active_runner_job_count"].as_u64(),
                Some(observation["active_runner_jobs"].as_array().unwrap().len() as u64)
            );
            let watched = read(
                &client,
                &format!("/jobs/{id}/watch?after_sequence={cursor}"),
            )["response"]
                .clone();
            if let Some(events) = watched["events"].as_array() {
                for event in events {
                    assert!(event["sequence"].as_u64().unwrap() > cursor);
                }
            }
            assert_eq!(watched["job_id"], id);
            cursor = watched["next_sequence"].as_u64().expect("resumed cursor");
            if watched["terminal"] == true {
                break watched;
            }
            assert!(Instant::now() < deadline, "runner did not terminalize job");
            std::thread::sleep(Duration::from_millis(100));
        };
        assert_eq!(terminal["terminal_outcome"], "succeeded");
        assert_eq!(
            std::fs::read_to_string(marker).expect("effect receipt"),
            "effect\n"
        );
        cli(&["daemon", "stop"]);
    });
}
