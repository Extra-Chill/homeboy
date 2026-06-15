use homeboy::core::api_jobs::{JobEventKind, JobStatus, JobStore, RemoteRunnerJobRequest};
use homeboy::core::http_api::{
    self, AnalysisJobRunOutput, AnalysisJobRunner, HttpApiRequest, HttpEndpoint, HttpMethod,
    JobReadyRunKind,
};
use homeboy::core::observation::{
    NewFindingRecord, NewRunRecord, ObservationStore, RunRecord, RunStatus,
};

use crate::test_support::with_isolated_home;

#[derive(Debug, Clone, Copy)]
struct FakeAnalysisJobRunner;

impl AnalysisJobRunner for FakeAnalysisJobRunner {
    fn run_analysis_job(&self, argv: Vec<String>) -> homeboy::core::Result<AnalysisJobRunOutput> {
        Ok(AnalysisJobRunOutput {
            exit_code: 0,
            output: serde_json::json!({ "argv": argv }),
        })
    }
}

#[test]
fn test_run_analysis_job() {
    let output = FakeAnalysisJobRunner
        .run_analysis_job(vec!["homeboy".to_string(), "lint".to_string()])
        .expect("run analysis job");

    assert_eq!(output.exit_code, 0);
    assert_eq!(output.output["argv"][1], "lint");
}

struct XdgGuard {
    prior: Option<String>,
}

impl XdgGuard {
    fn unset() -> Self {
        let prior = std::env::var("XDG_DATA_HOME").ok();
        std::env::remove_var("XDG_DATA_HOME");
        Self { prior }
    }
}

impl Drop for XdgGuard {
    fn drop(&mut self) {
        match &self.prior {
            Some(value) => std::env::set_var("XDG_DATA_HOME", value),
            None => std::env::remove_var("XDG_DATA_HOME"),
        }
    }
}

#[test]
fn routes_component_endpoints() {
    assert_eq!(
        http_api::route(HttpMethod::Get, "/components").expect("route"),
        HttpEndpoint::Components
    );
    assert_eq!(
        http_api::route(HttpMethod::Get, "/components/homeboy").expect("route"),
        HttpEndpoint::Component {
            id: "homeboy".to_string()
        }
    );
    assert_eq!(
        http_api::route(HttpMethod::Get, "/components/homeboy/status").expect("route"),
        HttpEndpoint::ComponentStatus {
            id: "homeboy".to_string()
        }
    );
    assert_eq!(
        http_api::route(HttpMethod::Get, "/components/homeboy/changes?gitDiffs=1").expect("route"),
        HttpEndpoint::ComponentChanges {
            id: "homeboy".to_string()
        }
    );
}

#[test]
fn routes_rig_and_stack_endpoints() {
    assert_eq!(
        http_api::route(HttpMethod::Get, "/rigs/").expect("route"),
        HttpEndpoint::Rigs
    );
    assert_eq!(
        http_api::route(HttpMethod::Get, "/rigs/studio").expect("route"),
        HttpEndpoint::Rig {
            id: "studio".to_string()
        }
    );
    assert_eq!(
        http_api::route(HttpMethod::Post, "/rigs/studio/check").expect("route"),
        HttpEndpoint::RigCheck {
            id: "studio".to_string()
        }
    );
    assert_eq!(
        http_api::route(HttpMethod::Get, "/stacks").expect("route"),
        HttpEndpoint::Stacks
    );
    assert_eq!(
        http_api::route(HttpMethod::Get, "/stacks/studio").expect("route"),
        HttpEndpoint::Stack {
            id: "studio".to_string()
        }
    );
    assert_eq!(
        http_api::route(HttpMethod::Post, "/stacks/studio/status").expect("route"),
        HttpEndpoint::StackStatus {
            id: "studio".to_string()
        }
    );
}

#[test]
fn routes_job_ready_analysis_endpoints_without_executing_them() {
    assert_eq!(
        http_api::route(HttpMethod::Post, "/audit").expect("route"),
        HttpEndpoint::JobReadyRun {
            kind: JobReadyRunKind::Audit
        }
    );
    assert_eq!(
        http_api::route(HttpMethod::Post, "/lint").expect("route"),
        HttpEndpoint::JobReadyRun {
            kind: JobReadyRunKind::Lint
        }
    );
    assert_eq!(
        http_api::route(HttpMethod::Post, "/test").expect("route"),
        HttpEndpoint::JobReadyRun {
            kind: JobReadyRunKind::Test
        }
    );
    assert_eq!(
        http_api::route(HttpMethod::Post, "/bench").expect("route"),
        HttpEndpoint::JobReadyRun {
            kind: JobReadyRunKind::Bench
        }
    );
}

#[test]
fn routes_sandbox_tool_endpoints() {
    assert_eq!(
        http_api::route(HttpMethod::Get, "/tools").expect("route"),
        HttpEndpoint::SandboxTools
    );
    assert_eq!(
        http_api::route(HttpMethod::Get, "/tools/homeboy.audit").expect("route"),
        HttpEndpoint::SandboxTool {
            id: "homeboy.audit".to_string()
        }
    );
    assert_eq!(
        http_api::route(HttpMethod::Post, "/tools/homeboy.review/run").expect("route"),
        HttpEndpoint::SandboxToolRun {
            id: "homeboy.review".to_string()
        }
    );
}

#[test]
fn routes_observation_run_readers() {
    assert_eq!(
        http_api::route(HttpMethod::Get, "/runs?kind=bench").expect("route"),
        HttpEndpoint::Runs
    );
    assert_eq!(
        http_api::route(HttpMethod::Get, "/runs/run-123").expect("route"),
        HttpEndpoint::Run {
            id: "run-123".to_string()
        }
    );
    assert_eq!(
        http_api::route(HttpMethod::Get, "/runs/run-123/artifacts").expect("route"),
        HttpEndpoint::RunArtifacts {
            id: "run-123".to_string()
        }
    );
    assert_eq!(
        http_api::route(HttpMethod::Get, "/runs/run-123/findings").expect("route"),
        HttpEndpoint::RunFindings {
            id: "run-123".to_string()
        }
    );
    assert_eq!(
        http_api::route(HttpMethod::Get, "/audit/runs").expect("route"),
        HttpEndpoint::AuditRuns
    );
    assert_eq!(
        http_api::route(HttpMethod::Get, "/bench/runs").expect("route"),
        HttpEndpoint::BenchRuns
    );
}

#[test]
fn routes_job_inspection_endpoints() {
    assert_eq!(
        http_api::route(HttpMethod::Get, "/jobs").expect("route"),
        HttpEndpoint::Jobs
    );
    assert_eq!(
        http_api::route(HttpMethod::Get, "/jobs/abc").expect("route"),
        HttpEndpoint::Job {
            id: "abc".to_string()
        }
    );
    assert_eq!(
        http_api::route(HttpMethod::Get, "/jobs/abc/events").expect("route"),
        HttpEndpoint::JobEvents {
            id: "abc".to_string()
        }
    );
    assert_eq!(
        http_api::route(HttpMethod::Post, "/jobs/abc/cancel").expect("route"),
        HttpEndpoint::JobCancel {
            id: "abc".to_string()
        }
    );
}

#[test]
fn test_handle_with_jobs() {
    let store = JobStore::default();
    let job = store.create("audit");
    store.start(job.id).expect("job starts");
    store
        .append_event(
            job.id,
            homeboy::core::api_jobs::JobEventKind::Stdout,
            Some("audit output".to_string()),
            None,
        )
        .expect("stdout event");

    let list = http_api::handle_with_jobs(
        HttpApiRequest {
            method: HttpMethod::Get,
            path: "/jobs".to_string(),
            body: None,
        },
        &store,
    )
    .expect("list jobs");
    assert_eq!(list.endpoint, "jobs.list");
    assert_eq!(list.body["jobs"].as_array().unwrap().len(), 1);
    assert_eq!(list.body["jobs"][0]["id"], job.id.to_string());

    let show = http_api::handle_with_jobs(
        HttpApiRequest {
            method: HttpMethod::Get,
            path: format!("/jobs/{}", job.id),
            body: None,
        },
        &store,
    )
    .expect("show job");
    assert_eq!(show.endpoint, "jobs.show");
    assert_eq!(show.body["job"]["operation"], "audit");

    let events = http_api::handle_with_jobs(
        HttpApiRequest {
            method: HttpMethod::Get,
            path: format!("/jobs/{}/events", job.id),
            body: None,
        },
        &store,
    )
    .expect("job events");
    assert_eq!(events.endpoint, "jobs.events");
    assert!(events.body["events"].as_array().unwrap().len() >= 3);

    let cancel = http_api::handle_with_jobs(
        HttpApiRequest {
            method: HttpMethod::Post,
            path: format!("/jobs/{}/cancel", job.id),
            body: None,
        },
        &store,
    )
    .expect("cancel job");
    assert_eq!(cancel.endpoint, "jobs.cancel");
    assert_eq!(cancel.body["job"]["status"], "cancelled");
    assert_eq!(store.get(job.id).expect("job").status, JobStatus::Cancelled);
}

#[test]
fn runs_list_includes_active_runner_jobs() {
    with_isolated_home(|_home| {
        let _xdg = XdgGuard::unset();
        ObservationStore::open_initialized().expect("store");
        let store = JobStore::default();
        let job = store
            .submit_remote_runner_job(RemoteRunnerJobRequest {
                runner_id: "homeboy-lab".to_string(),
                project_id: None,
                operation: "runner.exec".to_string(),
                command: vec![
                    "homeboy".to_string(),
                    "agent-task".to_string(),
                    "cook".to_string(),
                    "--run-id".to_string(),
                    "cook-durable-run".to_string(),
                ],
                cwd: Some("/workspace/homeboy".to_string()),
                env: Default::default(),
                capture_patch: false,
                source_snapshot: None,
                require_paths: Vec::new(),
                metadata: None,
            })
            .expect("submit runner job");
        store.start(job.id).expect("start runner job");

        let response = http_api::handle_with_jobs(
            HttpApiRequest {
                method: HttpMethod::Get,
                path: "/runs?status=running".to_string(),
                body: None,
            },
            &store,
        )
        .expect("runs list");

        let runs = response.body["runs"].as_array().expect("runs");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0]["id"], "cook-durable-run");
        assert_eq!(runs[0]["kind"], "lab-runner-job");
        assert_eq!(runs[0]["status"], "running");
        let note = runs[0]["status_note"].as_str().expect("status note");
        assert!(note.contains("runner=homeboy-lab"));
        assert!(note.contains(&format!("job={}", job.id)));
        assert!(note.contains("durable_run=cook-durable-run"));
        assert!(note.contains("elapsed_ms="));
        assert!(note.contains("active_child_count="));
        assert!(note.contains("active_cell_count="));
        assert_eq!(
            response.body["active_runner_jobs"][0]["runner_id"],
            "homeboy-lab"
        );
        assert_eq!(
            response.body["active_runner_jobs"][0]["durable_run_id"],
            "cook-durable-run"
        );
    });
}

#[test]
fn test_handle() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let bench = store
            .start_run(sample_run("bench", "homeboy", "studio"))
            .expect("bench run");
        store
            .finish_run(&bench.id, RunStatus::Pass, None)
            .expect("finish bench");
        let audit = store
            .start_run(sample_run("audit", "homeboy", "studio"))
            .expect("audit run");
        store
            .finish_run(&audit.id, RunStatus::Fail, None)
            .expect("finish audit");
        let artifact_path = home.path().join("bench-results.json");
        std::fs::write(&artifact_path, b"{}").expect("artifact");
        store
            .record_artifact(&bench.id, "bench_results", &artifact_path)
            .expect("record artifact");
        let lint = sample_imported_running_run("lint", "homeboy", "studio");
        store.import_run(&lint).expect("lint run");
        store
            .record_findings(&[
                sample_finding(&lint.id, "lint", "style", "src/main.rs"),
                sample_finding(&lint.id, "audit", "security", "src/lib.rs"),
            ])
            .expect("record findings");

        let response = http_api::handle(HttpApiRequest {
            method: HttpMethod::Get,
            path: "/bench/runs?component=homeboy&rig=studio&limit=1".to_string(),
            body: None,
        })
        .expect("bench runs");
        assert_eq!(response.endpoint, "bench.runs");
        assert_eq!(response.body["runs"].as_array().unwrap().len(), 1);
        assert_eq!(response.body["runs"][0]["id"], bench.id);

        let response = http_api::handle(HttpApiRequest {
            method: HttpMethod::Get,
            path: format!("/runs/{}", bench.id),
            body: None,
        })
        .expect("show run");
        assert_eq!(response.endpoint, "runs.show");
        assert_eq!(response.body["run"]["id"], bench.id);
        assert_eq!(
            response.body["run"]["artifacts"][0]["kind"],
            "bench_results"
        );

        let response = http_api::handle(HttpApiRequest {
            method: HttpMethod::Get,
            path: "/audit/runs?component=homeboy".to_string(),
            body: None,
        })
        .expect("audit runs");
        assert_eq!(response.endpoint, "audit.runs");
        assert_eq!(response.body["runs"].as_array().unwrap().len(), 1);
        assert_eq!(response.body["runs"][0]["id"], audit.id);

        let response = http_api::handle(HttpApiRequest {
            method: HttpMethod::Get,
            path: format!("/runs/{}/findings?tool=lint&limit=1", lint.id),
            body: None,
        })
        .expect("run findings");
        assert_eq!(response.endpoint, "runs.findings");
        assert_eq!(response.body["run_id"], lint.id);
        assert_eq!(response.body["findings"].as_array().unwrap().len(), 1);
        assert_eq!(response.body["findings"][0]["tool"], "lint");
        assert_eq!(response.body["findings"][0]["rule"], "style");

        let response = http_api::handle(HttpApiRequest {
            method: HttpMethod::Get,
            path: format!("/runs/{}", lint.id),
            body: None,
        })
        .expect("show running run");
        assert!(response.body["run"]["status_note"]
            .as_str()
            .expect("status note")
            .contains("owner process is not running"));
    });
}

#[test]
fn artifact_content_serves_encoded_artifact_store_locator() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let run = store
            .start_run(sample_run("bench", "homeboy", "studio"))
            .expect("bench run");
        let locator = "homeboy/workflow-bench/runs/run-1/artifacts/blueprint.after.json";
        let artifact_root = home.path().join(".local/share/homeboy/artifacts");
        let path = artifact_root.join(locator);
        std::fs::create_dir_all(path.parent().expect("artifact parent"))
            .expect("create artifact parent");
        std::fs::write(&path, br#"{"steps":[]}"#).expect("artifact-store file");
        let token = homeboy::core::runner::runner_artifact_store_token("lab", &run.id, locator)
            .rsplit('/')
            .next()
            .expect("artifact token")
            .to_string();

        let response = http_api::handle(HttpApiRequest {
            method: HttpMethod::Get,
            path: format!("/runs/{}/artifacts/{}/content", run.id, token),
            body: None,
        })
        .expect("artifact-store content");

        assert_eq!(response.endpoint, "runs.artifact.content");
        assert_eq!(response.body["run_id"], run.id);
        assert_eq!(response.body["filename"], "blueprint.after.json");
        assert_eq!(response.body["mime"], "application/json");
        assert_eq!(response.body["size_bytes"], 12);
        assert_eq!(
            response.body["content_base64"].as_str(),
            Some("eyJzdGVwcyI6W119")
        );
    });
}

#[test]
fn rejects_mutating_endpoint_shapes() {
    assert!(http_api::route(HttpMethod::Post, "/rigs/studio/up").is_err());
    assert!(http_api::route(HttpMethod::Post, "/stacks/studio/apply").is_err());
    assert!(http_api::route(HttpMethod::Post, "/deploy").is_err());
    assert!(http_api::route(HttpMethod::Post, "/release").is_err());
}

#[test]
fn sandbox_tools_declare_capabilities_and_allowed_arguments() {
    let response = http_api::handle_with_jobs(
        HttpApiRequest {
            method: HttpMethod::Get,
            path: "/tools".to_string(),
            body: None,
        },
        &JobStore::default(),
    )
    .expect("list tools");

    assert_eq!(response.endpoint, "tools.list");
    let tools = response.body["tools"].as_array().expect("tools");
    assert!(tools.iter().any(|tool| {
        tool["id"] == "homeboy.review"
            && tool["required_capability"] == "run:review"
            && tool["risk"] == "bounded_local_run"
    }));
    assert!(tools.iter().all(|tool| {
        !tool["required_capability"]
            .as_str()
            .unwrap_or_default()
            .starts_with("operator:")
    }));
}

#[test]
fn sandbox_tool_run_enqueues_allowlisted_job() {
    let store = JobStore::default();
    let response = http_api::handle_with_jobs(
        HttpApiRequest {
            method: HttpMethod::Post,
            path: "/tools/homeboy.build/run".to_string(),
            body: Some(serde_json::json!({
                "component": "missing-component",
                "path": "/tmp/homeboy-missing-component"
            })),
        },
        &store,
    )
    .expect("build tool job enqueued");

    assert_eq!(response.endpoint, "tools.run");
    assert_eq!(response.body["command"], "api.tools.run.enqueue");
    assert_eq!(response.body["tool"]["id"], "homeboy.build");
    assert_eq!(response.body["request"]["kind"], "build");
    let args = response.body["request"]["args"].as_array().expect("args");
    assert!(args.iter().any(|arg| arg == "build"));
    assert!(args.iter().any(|arg| arg == "--path"));
    assert_eq!(store.list().len(), 1);
}

#[test]
fn sandbox_tool_run_rejects_unallowlisted_tool_and_arguments() {
    let unknown = http_api::handle_with_jobs(
        HttpApiRequest {
            method: HttpMethod::Post,
            path: "/tools/homeboy.deploy/run".to_string(),
            body: Some(serde_json::json!({})),
        },
        &JobStore::default(),
    )
    .expect_err("deploy tool is not allowlisted");
    assert!(unknown.to_string().contains("not allowlisted"));

    let mutating = http_api::handle_with_jobs(
        HttpApiRequest {
            method: HttpMethod::Post,
            path: "/tools/homeboy.review/run".to_string(),
            body: Some(serde_json::json!({ "report": "pr-comment" })),
        },
        &JobStore::default(),
    )
    .expect_err("review report output is rejected");
    assert!(mutating.to_string().contains("JSON output"));
}

#[test]
fn job_ready_endpoint_enqueues_daemon_job() {
    let store = JobStore::default();
    let response = http_api::handle_with_jobs(
        HttpApiRequest {
            method: HttpMethod::Post,
            path: "/audit".to_string(),
            body: Some(serde_json::json!({
                "component": "missing-component",
                "path": "/tmp/homeboy-missing-component",
                "changed_since": "origin/main",
                "json_summary": true
            })),
        },
        &store,
    )
    .expect("audit job enqueued");

    assert_eq!(response.endpoint, "jobs.required");
    assert_eq!(response.body["command"], "api.audit.enqueue");
    let job_id = response.body["job"]["id"].as_str().expect("job id");
    assert_eq!(response.body["poll"]["job"], format!("/jobs/{job_id}"));
    assert_eq!(store.list().len(), 1);
}

#[test]
fn job_ready_endpoint_rejects_mutating_body_fields() {
    let err = http_api::handle_with_jobs(
        HttpApiRequest {
            method: HttpMethod::Post,
            path: "/lint".to_string(),
            body: Some(serde_json::json!({ "fix": true })),
        },
        &JobStore::default(),
    )
    .expect_err("mutating lint fix is rejected");

    let rendered = err.to_string();
    assert!(rendered.contains("--fix"), "{rendered}");
}

#[test]
fn job_ready_endpoint_rejects_unknown_body_fields() {
    let err = http_api::handle_with_jobs(
        HttpApiRequest {
            method: HttpMethod::Post,
            path: "/bench".to_string(),
            body: Some(serde_json::json!({ "deploy": true })),
        },
        &JobStore::default(),
    )
    .expect_err("unknown field is rejected");

    let rendered = err.to_string();
    assert!(
        rendered.contains("unsupported analysis job body field"),
        "{rendered}"
    );
}

#[test]
fn job_ready_endpoint_preserves_background_result_events() {
    let store = JobStore::default();
    let response = http_api::handle_with_jobs_and_runner(
        HttpApiRequest {
            method: HttpMethod::Post,
            path: "/lint".to_string(),
            body: Some(serde_json::json!({
                "component": "missing-component",
                "path": "/tmp/homeboy-missing-component",
                "json_summary": true
            })),
        },
        &store,
        FakeAnalysisJobRunner,
    )
    .expect("lint job enqueued");
    let job_id = response.body["job"]["id"].as_str().expect("job id");
    let job_id = uuid::Uuid::parse_str(job_id).expect("uuid");

    for _ in 0..100 {
        let status = store.get(job_id).expect("job").status;
        if matches!(
            status,
            JobStatus::Succeeded | JobStatus::Failed | JobStatus::Cancelled
        ) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    let events = store.events(job_id).expect("events");
    assert!(events
        .iter()
        .any(|event| event.kind == JobEventKind::Progress));
    assert!(events
        .iter()
        .any(|event| { event.kind == JobEventKind::Result || event.kind == JobEventKind::Error }));
    assert!(events.iter().any(|event| {
        event.kind == JobEventKind::Result
            && event.data.as_ref().is_some_and(|data| {
                data["output"]["argv"]
                    .as_array()
                    .is_some_and(|argv| argv.iter().any(|arg| arg == "lint"))
            })
    }));
}

fn sample_run(kind: &str, component_id: &str, rig_id: &str) -> NewRunRecord {
    sample_run_with_metadata(
        kind,
        component_id,
        rig_id,
        serde_json::json!({ "source": "http-api-test" }),
    )
}

fn sample_run_with_metadata(
    kind: &str,
    component_id: &str,
    rig_id: &str,
    metadata_json: serde_json::Value,
) -> NewRunRecord {
    NewRunRecord::builder(kind)
        .component_id(component_id)
        .command(format!("homeboy {kind}"))
        .cwd_path(std::path::Path::new("/tmp/homeboy-fixture"))
        .homeboy_version("test-version")
        .git_sha(Some("abc123".to_string()))
        .rig_id(rig_id)
        .metadata(metadata_json)
        .build()
}

fn sample_imported_running_run(kind: &str, component_id: &str, rig_id: &str) -> RunRecord {
    RunRecord {
        id: format!("{kind}-dead-owner-run"),
        kind: kind.to_string(),
        component_id: Some(component_id.to_string()),
        started_at: "2026-05-01T00:00:00Z".to_string(),
        finished_at: None,
        status: "running".to_string(),
        command: Some(format!("homeboy {kind}")),
        cwd: Some("/tmp/homeboy-fixture".to_string()),
        homeboy_version: Some("test-version".to_string()),
        git_sha: Some("abc123".to_string()),
        rig_id: Some(rig_id.to_string()),
        metadata_json: serde_json::json!({ "homeboy_run_owner": { "pid": u32::MAX } }),
    }
}

fn sample_finding(run_id: &str, tool: &str, rule: &str, file: &str) -> NewFindingRecord {
    NewFindingRecord {
        run_id: run_id.to_string(),
        tool: tool.to_string(),
        rule: Some(rule.to_string()),
        file: Some(file.to_string()),
        line: Some(12),
        severity: Some("warning".to_string()),
        fingerprint: Some(format!("{file}::{rule}")),
        message: format!("{rule} finding"),
        fixable: Some(false),
        metadata_json: serde_json::json!({ "source": "http-api-test" }),
    }
}
