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

struct LeasePublicationGuard {
    published: std::path::PathBuf,
    withheld: std::path::PathBuf,
}

impl LeasePublicationGuard {
    fn publish(&self) {
        std::fs::rename(&self.withheld, &self.published).expect("republish service lease");
    }
}

impl Drop for LeasePublicationGuard {
    fn drop(&mut self) {
        if !self.published.exists() && self.withheld.exists() {
            let _ = std::fs::rename(&self.withheld, &self.published);
        }
    }
}

#[test]
fn runner_service_lease_publication_race_retries_denial_and_preserves_job_identity() {
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

        // Withhold the durable service lease while leaving its daemon process
        // alive. This creates the publication gap the controller must treat as
        // unavailable, not as a fresh service generation.
        let lease_path = homeboy_core::paths::daemon_state_file().expect("isolated lease path");
        let unpublished_lease = root.join("state.pending");
        std::fs::rename(&lease_path, &unpublished_lease).expect("withhold service lease");
        let lease_publication = LeasePublicationGuard {
            published: lease_path.clone(),
            withheld: unpublished_lease.clone(),
        };
        let gate = std::sync::Arc::new(std::sync::Barrier::new(5));
        let status_binary = binary;
        let status_home = root.to_path_buf();
        let status_gate = std::sync::Arc::clone(&gate);
        let status_reader = std::thread::spawn(move || {
            status_gate.wait();
            let output = Command::new(status_binary)
                .args(["daemon", "status"])
                .env_clear()
                .env("HOME", &status_home)
                .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
                .env("HOMEBOY_NO_UPDATE_CHECK", "1")
                .output()
                .expect("read status during lease publication gap");
            assert!(output.status.success());
            let status: Value = serde_json::from_slice(&output.stdout).expect("status JSON");
            assert_eq!(status["data"]["fresh"], false, "{status:#}");
            assert_eq!(
                status["data"]["recovery"]["stale_reason_code"], "lease_missing",
                "{status:#}"
            );
        });
        let describe_client = client.clone();
        let describe_url = url.clone();
        let describe_gate = std::sync::Arc::clone(&gate);
        let describe_reader = std::thread::spawn(move || {
            describe_gate.wait();
            for _ in 0..3 {
                let response = describe_client
                    .get(format!(
                        "{describe_url}{}",
                        homeboy_runner_contract::RUNNER_API_DESCRIBE_PATH
                    ))
                    .send()
                    .expect("read runner diagnostic during publication gap");
                assert_eq!(response.status().as_u16(), 503);
                let body: Value = response.json().expect("diagnostic error JSON");
                assert_eq!(body["success"], false, "{body:#}");
                assert!(body["data"]["message"].is_string(), "{body:#}");
            }
        });
        let health_client = client.clone();
        let health_url = url.clone();
        let health_gate = std::sync::Arc::clone(&gate);
        let health_reader = std::thread::spawn(move || {
            health_gate.wait();
            for _ in 0..3 {
                let body: Value = health_client
                    .get(format!("{health_url}/health"))
                    .send()
                    .expect("read health during publication gap")
                    .json()
                    .expect("health JSON");
                assert_eq!(body["data"]["lease"], Value::Null, "{body:#}");
                assert_eq!(body["data"]["freshness"]["fresh"], false, "{body:#}");
            }
        });
        let admission_client = client.clone();
        let admission_url = url.clone();
        let admission_gate = std::sync::Arc::clone(&gate);
        let retry_key = "lease-publication-gap-retry";
        let denied_admission = std::thread::spawn(move || {
            admission_gate.wait();
            let request = json!({
                "runner_id": "fixture-local",
                "command": "publication-gap-regression",
                "expected_daemon_lease_id": "expected-after-publication",
                "idempotency_key": retry_key,
                "admission_lease_protocol": 1,
            });
            let response = admission_client
                .post(format!("{admission_url}/admissions"))
                .json(&request)
                .send()
                .expect("denied zero-admission request");
            assert!(response.status().is_client_error() || response.status().is_server_error());
            response.json::<Value>().expect("denial JSON")
        });
        gate.wait();
        status_reader.join().expect("status reader");
        describe_reader.join().expect("diagnostic reader");
        health_reader.join().expect("health reader");
        let denial = denied_admission.join().expect("admission reader");
        assert!(!denial["success"].as_bool().unwrap_or(true), "{denial:#}");
        assert!(
            denial["data"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("daemon lease is not fresh")),
            "the denial must come from the unpublished daemon lease: {denial:#}"
        );
        assert_eq!(
            std::fs::read(&lease_path)
                .expect_err("lease remains unpublished")
                .kind(),
            std::io::ErrorKind::NotFound
        );

        // Republish the exact same daemon generation. The denied request did
        // not consume its idempotency key or create an admission reservation.
        lease_publication.publish();
        let admission_request = json!({
            "runner_id": "fixture-local",
            "command": "publication-gap-regression",
            "expected_daemon_lease_id": daemon["daemon"]["lease_id"],
            "idempotency_key": retry_key,
            "admission_lease_protocol": 1,
        });
        let submit_admission = || -> Value {
            let response: Value = client
                .post(format!("{url}/admissions"))
                .json(&admission_request)
                .send()
                .expect("retry denied admission with the same identity")
                .json()
                .expect("admission response JSON");
            assert!(
                response["success"].as_bool().unwrap_or(false),
                "{response:#}"
            );
            response["data"]["body"].clone()
        };
        let admission = submit_admission();
        let replayed_admission = submit_admission();
        assert_eq!(admission["job"]["id"], replayed_admission["job"]["id"]);
        assert_eq!(replayed_admission["idempotent_resubmission"], true);
        let admission_id = admission["job"]["id"].as_str().expect("admission ID");
        let released: Value = client
            .post(format!("{url}/admissions/{admission_id}/release"))
            .json(&json!({ "admission_token": admission["admission_token"] }))
            .send()
            .expect("release temporary admission")
            .json()
            .expect("release response JSON");
        assert!(
            released["success"].as_bool().unwrap_or(false),
            "{released:#}"
        );

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
