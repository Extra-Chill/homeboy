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

struct ServiceStartGate {
    release: std::path::PathBuf,
}

impl Drop for ServiceStartGate {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.release, b"release\n");
    }
}

struct ServiceDaemonGuard {
    child: Option<std::process::Child>,
}

impl Drop for ServiceDaemonGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
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
            serde_json::from_slice::<Value>(&output.stdout).unwrap_or_else(|error| {
                panic!(
                    "CLI JSON for {:?}: {error}; stdout={} stderr={}",
                    args,
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                )
            })["data"]
                .clone()
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

#[cfg(unix)]
#[test]
fn service_runner_exec_waits_for_the_published_lease_and_diagnostics_stay_read_only() {
    use std::os::unix::fs::PermissionsExt;

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
            serde_json::from_slice::<Value>(&output.stdout).unwrap_or_else(|error| {
                panic!(
                    "CLI JSON for {:?}: {error}; stdout={} stderr={}",
                    args,
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                )
            })["data"]
                .clone()
        };

        let runner_id = "fixture-service";
        let binary_path = std::path::Path::new(binary);
        let workspace = root.to_str().expect("workspace path");
        cli(&[
            "server",
            "create",
            runner_id,
            "--host",
            "localhost",
            "--user",
            "fixture",
            "--port",
            "22",
        ]);
        cli(&[
            "runner",
            "add",
            runner_id,
            "--server",
            runner_id,
            "--kind",
            "ssh",
            "--workspace-root",
            workspace,
            "--homeboy-path",
            binary_path.to_str().expect("binary path"),
        ]);
        cli(&["runner", "trust", runner_id, "--allow-raw-exec", "true"]);

        let fake_bin = root.join("fake-bin");
        std::fs::create_dir_all(&fake_bin).expect("fake system bin");
        let restart_waiting = root.join("service-restart-waiting");
        let publish_service = root.join("publish-service");
        let systemctl = fake_bin.join("systemctl");
        std::fs::write(
            &systemctl,
            format!(
                r##"#!/bin/sh
printf '%s\n' "$*" >> "$HOME/systemctl.log"
case "$*" in
  *daemon-reload*|*enable*) exit 0 ;;
  *restart*)
    : > "$HOME/{waiting}"
    while [ ! -e "$HOME/{release}" ]; do sleep 0.02; done
    ;;
esac
"##,
                waiting = "service-restart-waiting",
                release = "publish-service",
            ),
        )
        .expect("write disposable systemctl shim");
        std::fs::set_permissions(&systemctl, std::fs::Permissions::from_mode(0o755))
            .expect("make systemctl shim executable");

        let server_path = root
            .join(".config/homeboy/servers")
            .join(format!("{runner_id}.json"));
        let mut server: Value = serde_json::from_slice(
            &std::fs::read(&server_path).expect("read disposable server config"),
        )
        .expect("server config JSON");
        server["env"] = json!({
            "PATH": format!(
                "{}:{}:/usr/bin:/bin:/usr/sbin:/sbin",
                fake_bin.display(),
                binary_path.parent().expect("binary directory").display()
            ),
        });
        server["runner"]["service_managed"] = Value::Bool(true);
        std::fs::write(&server_path, server.to_string()).expect("configure fake service PATH");

        // This service is a pre-existing, runner-owned unit in the isolated
        // fixture. The shim gates its restart; it never invokes host systemd.
        let gate = ServiceStartGate {
            release: publish_service.clone(),
        };
        let mut install = Command::new(binary)
            .args(["runner", "service", "install", runner_id])
            .env_clear()
            .env("HOME", root)
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HOMEBOY_NO_UPDATE_CHECK", "1")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("start isolated runner service install");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !restart_waiting.exists() && Instant::now() < deadline {
            if let Some(status) = install.try_wait().expect("poll service install") {
                let output = install.wait_with_output().expect("collect install output");
                panic!(
                    "service install exited before restart gate ({status}): {}\n{}\nsystemctl={}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr),
                    std::fs::read_to_string(root.join("systemctl.log")).unwrap_or_default()
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if !restart_waiting.exists() {
            let _ = install.kill();
            let output = install
                .wait_with_output()
                .expect("collect stuck install output");
            panic!(
                "service restart did not reach the gate: {}\n{}\nsystemctl={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
                std::fs::read_to_string(root.join("systemctl.log")).unwrap_or_default()
            );
        }

        let unit_dir = root.join(".config/systemd/user");
        let unit_path = std::fs::read_dir(&unit_dir)
            .expect("service unit directory")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "service")
            })
            .expect("scoped disposable service unit");
        let unit = std::fs::read_to_string(unit_path).expect("read service unit");
        let state_suffix = unit
            .lines()
            .find_map(|line| line.strip_prefix("Environment=HOMEBOY_DAEMON_STATE_DIR=%h"))
            .expect("service-owned daemon state directory");
        let state_dir = root.join(state_suffix.trim_start_matches('/'));
        let startup_token = unit
            .lines()
            .find_map(|line| line.strip_prefix("Environment=HOMEBOY_DAEMON_STARTUP_TOKEN="))
            .expect("service startup token");

        let marker = root.join("service-exec-effect");
        let mut exec = Command::new(binary)
            .args([
                "runner",
                "exec",
                runner_id,
                "--",
                "/bin/sh",
                "-c",
                &format!("sleep 3; printf 'ran\\n' >> '{}';", marker.display()),
            ])
            .env_clear()
            .env("HOME", root)
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HOMEBOY_NO_UPDATE_CHECK", "1")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("start runner exec while service lease is unpublished");

        let service_status = cli(&["runner", "service", "status", runner_id]);
        assert_eq!(service_status["active"], false, "{service_status:#}");
        let status = cli(&["runner", "status", runner_id]);
        assert_ne!(status["state"], "connected", "{status:#}");
        let diagnostic = Command::new(binary)
            .args([
                "runner", "exec", "--ssh", runner_id, "--", "homeboy", "daemon", "status",
            ])
            .env_clear()
            .env("HOME", root)
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HOMEBOY_NO_UPDATE_CHECK", "1")
            .output()
            .expect("diagnostic SSH read during lease publication gap");
        let diagnostic_output = String::from_utf8_lossy(&diagnostic.stdout);
        assert!(diagnostic.status.success(), "{diagnostic_output}");
        assert!(
            diagnostic_output.contains("no daemon lease is recorded"),
            "{diagnostic_output}"
        );
        assert!(exec.try_wait().expect("poll waiting runner exec").is_none());

        let service_process = Command::new(binary)
            .args(["daemon", "serve", "--addr", "127.0.0.1:0"])
            .env_clear()
            .env("HOME", root)
            .env(homeboy_core::paths::DAEMON_STATE_DIR_ENV, &state_dir)
            .env(homeboy_core::paths::DAEMON_STARTUP_TOKEN_ENV, startup_token)
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HOMEBOY_NO_UPDATE_CHECK", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("start runner-owned service daemon process");
        let daemon_guard = ServiceDaemonGuard {
            child: Some(service_process),
        };
        std::fs::write(&publish_service, b"publish\n").expect("release delayed service start");
        let install_output = install
            .wait_with_output()
            .expect("wait for service install");
        assert!(
            install_output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&install_output.stdout),
            format!(
                "{}\nsystemctl={}\ndaemon_log={}",
                String::from_utf8_lossy(&install_output.stderr),
                std::fs::read_to_string(root.join("systemctl.log")).unwrap_or_default(),
                std::fs::read_to_string(root.join("service-daemon.log")).unwrap_or_default()
            )
        );
        assert!(
            state_dir.join("state.json").exists(),
            "service install returned without its durable lease; systemctl={} daemon_log={}",
            std::fs::read_to_string(root.join("systemctl.log")).unwrap_or_default(),
            std::fs::read_to_string(root.join("service-daemon.log")).unwrap_or_default()
        );
        let installed = cli(&["runner", "service", "status", runner_id]);
        assert_eq!(installed["active"], true, "{installed:#}");
        let daemon_address = installed["daemon_address"]
            .as_str()
            .expect("published service endpoint");
        std::net::TcpStream::connect(daemon_address).unwrap_or_else(|error| {
            panic!("service endpoint {daemon_address} is unreachable: {error}")
        });
        let service_lease: Value = serde_json::from_slice(
            &std::fs::read(state_dir.join("state.json")).expect("service lease publication"),
        )
        .expect("service lease JSON");
        assert_eq!(service_lease["lease_id"], installed["daemon_lease_id"]);
        let service_client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("service API client");
        let observation_deadline = Instant::now() + Duration::from_secs(10);
        let (service_job_id, mut event_cursor) = loop {
            let response: Value = service_client
                .get(format!(
                    "http://{daemon_address}{}",
                    homeboy_runner_contract::RUNNER_API_DESCRIBE_PATH
                ))
                .send()
                .expect("read running service job")
                .json()
                .expect("runner observation JSON");
            let body = &response["data"]["body"];
            assert_eq!(body["lease_id"], service_lease["lease_id"]);
            if let Some(job) = body["active_runner_jobs"]
                .as_array()
                .and_then(|jobs| jobs.first())
            {
                break (
                    job["job_id"].as_str().expect("service job ID").to_string(),
                    0_u64,
                );
            }
            assert!(
                Instant::now() < observation_deadline,
                "service runner exec did not publish its active job: {response:#}"
            );
            std::thread::sleep(Duration::from_millis(25));
        };
        assert!(
            exec.try_wait().expect("poll active service exec").is_none(),
            "the admitted child remains active while status and diagnostics read its lease"
        );
        let in_flight_service = cli(&["runner", "service", "status", runner_id]);
        assert_eq!(
            in_flight_service["daemon_lease_id"],
            service_lease["lease_id"]
        );
        let terminal_deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let response: Value = service_client
                .get(format!(
                    "http://{daemon_address}/jobs/{service_job_id}/watch?after_sequence={event_cursor}"
                ))
                .send()
                .expect("watch service job")
                .json()
                .expect("service job watch JSON");
            let body = &response["data"]["body"]["response"];
            event_cursor = body["next_sequence"]
                .as_u64()
                .expect("service event cursor");
            if body["terminal"] == true {
                assert_eq!(body["terminal_outcome"], "succeeded");
                break;
            }
            assert!(
                Instant::now() < terminal_deadline,
                "service job did not finish"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        let exec_output = exec
            .wait_with_output()
            .expect("wait for delayed runner exec");
        assert!(
            exec_output.status.success(),
            "service={installed:#}\n{}\n{}",
            String::from_utf8_lossy(&exec_output.stdout),
            String::from_utf8_lossy(&exec_output.stderr)
        );
        assert_eq!(
            std::fs::read_to_string(&marker).expect("single exec effect"),
            "ran\n"
        );

        let lease = installed["daemon_lease_id"]
            .as_str()
            .expect("service lease");
        let _final_status = cli(&["runner", "status", runner_id]);
        assert_eq!(
            cli(&["runner", "service", "status", runner_id])["daemon_lease_id"],
            lease,
            "status and service diagnostics retain one service generation"
        );
        drop(daemon_guard);
        drop(gate);
    });
}
