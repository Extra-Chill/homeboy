//! Capacity inspection walks Pi model routes without running inference.
use super::*;
use crate::commands::agent_task::fake_pi_test::FakePi;
use crate::commands::agent_task_summary::{render_agent_task_summary, AgentTaskSummaryKind};
use homeboy::agents::agent_tasks::provider::AgentTaskExecutorProvider;
use serde_json::json;

fn capacity_args() -> AgentTaskCapacityArgs {
    AgentTaskCapacityArgs {
        backend: None,
        selector: None,
        model: None,
    }
}

fn save_provider_policy(default_backend: Option<&str>, rotation: Option<Value>) {
    let mut config = homeboy::core::defaults::load_config();
    config.agent_task.default_backend = default_backend.map(str::to_string);
    config.agent_task.rotation = rotation;
    homeboy::core::defaults::save_config(&config).expect("save provider policy");
}

fn catalog_with(providers: Vec<AgentTaskExecutorProvider>) -> AgentTaskProviderCatalog {
    AgentTaskProviderCatalog {
        providers,
        ..Default::default()
    }
}

#[test]
fn capacity_walks_the_rotation_dedupes_routes_and_probes_capacity_only() {
    crate::test_support::with_isolated_home(|home| {
        let log = home.path().join("inference.log");
        let fake = FakePi::with_probes(
            &format!("touch '{}'; exit 99", log.display()),
            "printf 'test model\\ntest fallback\\n'",
        );
        save_provider_policy(
            Some("pi"),
            Some(json!({"entries":[
                {"backend":"pi", "model":"test/model"},
                {"backend":"pi", "model":"test/model"},
                {"backend":"pi", "model":"test/fallback"}
            ]})),
        );
        let (payload, status) =
            capacity_with_catalog(capacity_args(), catalog_with(vec![fake.provider()])).unwrap();
        assert_eq!(status, 0);
        assert_eq!(payload["schema"], "homeboy/agent-task-capacity/v1");
        let routes = payload["routes"].as_array().unwrap();
        assert_eq!(routes.len(), 2);
        assert_eq!(routes[0]["models"], json!(["test/model"]));
        assert_eq!(routes[1]["models"], json!(["test/fallback"]));
        for route in routes {
            assert_eq!(route["backend"], "pi");
            assert_eq!(route["capacity"]["state"], "unknown");
        }
        assert!(payload["next_reset"].is_null());
        assert!(
            !log.exists(),
            "capacity inspection must never run inference"
        );
    });
}

// Known pools, exhausted-account reset instants, and failing extension
// capacity invocations were extension readiness JSON features. Pi does not
// publish that protocol; those fixture-only cases are retired.

#[test]
fn capacity_never_reports_a_mode_ignoring_runtime_as_publishing_capacity() {
    crate::test_support::with_isolated_home(|_home| {
        save_provider_policy(Some("ignorant"), None);
        let mut provider: AgentTaskExecutorProvider = serde_json::from_value(json!({
            "id": "ignorant.agent",
            "backend": "ignorant"
        }))
        .expect("provider fixture");
        provider.readiness_invocation = Some(
            serde_json::from_value(json!({
                "argv": ["sh", "-c", "cat >/dev/null; printf '%s' '{\"schema\":\"homeboy/agent-task-provider-readiness-result/v1\",\"ready\":true,\"classification\":\"ready\",\"retryable\":false,\"remediation\":\"\",\"reason\":\"\",\"cache_key\":\"test\",\"identity\":{}}'"]
            }))
            .expect("readiness invocation"),
        );

        let (payload, _) = capacity_with_catalog(capacity_args(), catalog_with(vec![provider]))
            .expect("capacity report");

        let routes = payload["routes"].as_array().expect("routes");
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0]["capacity"]["state"], "unknown");
        assert_eq!(
            routes[0]["capacity"]["reason"],
            "the provider's readiness invocation does not report capacity"
        );
        assert!(payload["next_reset"].is_null());
    });
}

#[test]
fn capacity_filters_narrow_the_rotation_walk() {
    crate::test_support::with_isolated_home(|_| {
        let fake = FakePi::with_probes("exit 99", "printf 'test model\\ntest fallback\\n'");
        save_provider_policy(
            Some("pi"),
            Some(json!({"entries":[
                {"backend":"pi", "selector":"pi.agent-task-executor", "model":"test/model"},
                {"backend":"pi", "selector":"pi.agent-task-executor", "model":"test/fallback"}
            ]})),
        );
        let catalog = catalog_with(vec![fake.provider()]);
        let (payload, _) = capacity_with_catalog(
            AgentTaskCapacityArgs {
                backend: Some("pi".into()),
                selector: Some("pi.agent-task-executor".into()),
                model: Some("test/fallback".into()),
            },
            catalog.clone(),
        )
        .unwrap();
        let routes = payload["routes"].as_array().unwrap();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0]["models"], json!(["test/fallback"]));
        let (payload, _) = capacity_with_catalog(
            AgentTaskCapacityArgs {
                selector: Some("missing".into()),
                ..capacity_args()
            },
            catalog,
        )
        .unwrap();
        assert_eq!(payload["routes"], json!([]));
    });
}

#[test]
fn capacity_summary_renders_one_block_per_pool_and_line_per_account() {
    let payload = json!({
        "schema": "homeboy/agent-task-capacity/v1",
        "generated_at": "2026-09-24T20:00:00+00:00",
        "routes": [
            {
                "backend": "pi",
                "selector": null,
                "models": ["anthropic/claude-sonnet-5", "anthropic/claude-opus-5"],
                "scope": "pi:anthropic",
                "capacity": {
                    "state": "known", "remaining": 70, "limit": 100, "unit": "percent",
                    "accounts": [
                        { "account": "chubes@extrachill.com", "state": "exhausted", "remaining": 0, "reset_at": "2026-09-25T03:00:00Z" },
                        { "account": "claude@extrachill.com", "state": "available", "remaining": 70 },
                    ],
                },
            },
            {
                "backend": "pi",
                "selector": null,
                "models": ["openai/gpt-5.6"],
                "scope": "pi:openai",
                "capacity": {
                    "state": "exhausted",
                    "reset_at": "2026-09-27T15:26:09Z",
                    "reason": "5-hour usage limit reached",
                    "accounts": [],
                },
            },
            {
                "backend": "pi",
                "selector": null,
                "models": ["zai-coding-plan/glm-5.3"],
                "scope": "pi:zai-coding-plan",
                "capacity": {
                    "state": "known", "remaining": 37, "limit": 100, "unit": "percent",
                    "reset_at": "2026-09-29T02:00:00Z",
                },
            },
            {
                "backend": "pi",
                "selector": null,
                "models": ["pi-go/glm-5.3-flash"],
                "scope": null,
                "capacity": { "state": "unknown", "reason": "not published" },
            },
        ],
        "next_reset": "2026-09-25T03:00:00Z",
    });

    let summary = render_agent_task_summary(AgentTaskSummaryKind::Capacity, &payload)
        .expect("capacity summary");

    assert_eq!(
        summary,
        [
            "anthropic (2 plans): 70% remaining",
            "  chubes@extrachill.com  exhausted  resets 2026-09-25T03:00:00Z",
            "  claude@extrachill.com  available  70%",
            "openai: exhausted until 2026-09-27T15:26:09Z",
            "zai-coding-plan: 37% remaining, resets 2026-09-29T02:00:00Z",
            "pi-go/glm-5.3-flash: capacity not published",
        ]
        .join("\n")
    );
}

#[test]
fn capacity_summary_names_an_empty_rotation() {
    let payload = json!({
        "schema": "homeboy/agent-task-capacity/v1",
        "generated_at": "2026-09-24T20:00:00+00:00",
        "routes": [],
        "next_reset": Value::Null,
    });

    assert_eq!(
        render_agent_task_summary(AgentTaskSummaryKind::Capacity, &payload).as_deref(),
        Some("Agent task capacity: no configured provider routes"),
    );
}
