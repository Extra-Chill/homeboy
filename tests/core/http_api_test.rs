use homeboy::http_api::{self, HttpApiRequest, HttpEndpoint, HttpMethod, JobReadyRunKind};

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
fn rejects_mutating_endpoint_shapes() {
    assert!(http_api::route(HttpMethod::Post, "/rigs/studio/up").is_err());
    assert!(http_api::route(HttpMethod::Post, "/stacks/studio/apply").is_err());
    assert!(http_api::route(HttpMethod::Post, "/deploy").is_err());
    assert!(http_api::route(HttpMethod::Post, "/release").is_err());
}

#[test]
fn job_ready_endpoint_reports_job_model_blocker() {
    let err = http_api::handle(HttpApiRequest {
        method: HttpMethod::Post,
        path: "/audit".to_string(),
        body: None,
    })
    .expect_err("job model blocker");

    let rendered = err.to_string();
    assert!(rendered.contains("job model"), "{rendered}");
    assert!(rendered.contains("#1764"), "{rendered}");
}
