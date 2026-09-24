//! Unit tests for the `homeboy agent-task capacity` command (#15024).
//!
//! Fake readiness scripts follow the existing `dispatchability.rs` pattern:
//! node one-liners that read the readiness request from stdin, log what they
//! were asked (mode and model), and answer with a readiness result carrying
//! typed capacity evidence.

use super::*;
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

/// A readiness script that reports which mode and model it was asked about,
/// and answers capacity-mode requests with a two-account pool whose exhausted
/// account reset instant comes from `argv[4]`. `argv[3]` is the reported
/// pool scope (an empty string publishes no scope).
fn mode_echo_script() -> String {
    "const fs=require('fs');const request=JSON.parse(fs.readFileSync(0,'utf8'));const mode=request.mode||'live';const model=request.effective_config.model||null;fs.appendFileSync(process.argv[2],JSON.stringify({mode,model})+'\\n');const result={schema:'homeboy/agent-task-provider-readiness-result/v1',ready:true,classification:'ready',retryable:false,remediation:'',reason:'',cache_key:String(model),identity:{model}};if(mode==='capacity'){result.capacity={remaining:70,limit:100,unit:'percent',reset_at:'2026-09-29T02:00:00Z',scope:process.argv[3],accounts:[{account:'plan-a@example.com',state:'exhausted',remaining:0,reset_at:process.argv[4]},{account:'plan-b@example.com',state:'available',remaining:70}]};}process.stdout.write(JSON.stringify(result));"
        .to_string()
}

fn scoped_capacity_provider(
    root: &std::path::Path,
    log: &std::path::Path,
    id: &str,
    backend: &str,
    scope: &str,
    exhausted_reset: &str,
) -> AgentTaskExecutorProvider {
    let script = root.join(format!("{id}-readiness.js"));
    std::fs::write(&script, mode_echo_script()).expect("readiness script");
    let mut provider: AgentTaskExecutorProvider =
        serde_json::from_value(json!({ "id": id, "backend": backend })).expect("provider fixture");
    provider.readiness_invocation = Some(
        serde_json::from_value(json!({
            "argv": [
                "node",
                script.display().to_string(),
                log.display().to_string(),
                scope,
                exhausted_reset,
            ]
        }))
        .expect("readiness invocation"),
    );
    provider
}

fn failing_provider(id: &str, backend: &str) -> AgentTaskExecutorProvider {
    let mut provider: AgentTaskExecutorProvider =
        serde_json::from_value(json!({ "id": id, "backend": backend })).expect("provider fixture");
    provider.readiness_invocation = Some(
        serde_json::from_value(json!({
            "argv": ["sh", "-c", "cat >/dev/null; exit 3"]
        }))
        .expect("readiness invocation"),
    );
    provider
}

fn probe_log(log: &std::path::Path) -> Vec<(String, Option<String>)> {
    std::fs::read_to_string(log)
        .expect("probe log")
        .lines()
        .map(|line| {
            let entry: Value = serde_json::from_str(line).expect("log line");
            (
                entry["mode"].as_str().unwrap_or("live").to_string(),
                entry["model"].as_str().map(str::to_string),
            )
        })
        .collect()
}

/// The rotation walk covers the configured default backend plus every
/// rotation entry, deduplicates the route the default and the bare entry
/// share, and every probe runs in capacity-only mode — never live inference.
#[test]
fn capacity_walks_the_rotation_dedupes_routes_and_probes_capacity_only() {
    crate::test_support::with_isolated_home(|home| {
        let log = home.path().join("probes.log");
        save_provider_policy(
            Some("walk"),
            Some(json!({
                "entries": [
                    { "backend": "walk" },
                    { "backend": "walk", "model": "other-model" },
                ]
            })),
        );
        let provider = scoped_capacity_provider(
            home.path(),
            &log,
            "walk.agent",
            "walk",
            "",
            "2026-09-25T03:00:00Z",
        );

        let (payload, status) =
            capacity_with_catalog(capacity_args(), catalog_with(vec![provider]))
                .expect("capacity report");

        assert_eq!(status, 0);
        assert_eq!(payload["schema"], "homeboy/agent-task-capacity/v1");
        assert_eq!(
            probe_log(&log),
            vec![("capacity".to_string(), Some("other-model".to_string()))],
            "the bare default route is subsumed by the modeled rotation entry on its backend, and every probe is capacity-only"
        );
        let routes = payload["routes"].as_array().expect("routes");
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0]["backend"], "walk");
        assert_eq!(routes[0]["models"], json!(["other-model"]));
        assert!(routes[0]["scope"].is_null());
        assert_eq!(routes[0]["capacity"]["state"], "known");
        assert_eq!(routes[0]["capacity"]["remaining"], 70);
        assert_eq!(
            routes[0]["capacity"]["accounts"]
                .as_array()
                .expect("accounts")
                .len(),
            2
        );
    });
}

/// Routes reporting the same non-empty scope share one pool entry listing all
/// their models; a route without a scope stays individual. `next_reset` is the
/// soonest exhausted-account reset across every reported route.
#[test]
fn capacity_groups_scoped_routes_and_selects_the_soonest_next_reset() {
    crate::test_support::with_isolated_home(|home| {
        let log = home.path().join("probes.log");
        save_provider_policy(
            Some("pool"),
            Some(json!({
                "entries": [
                    { "backend": "pool", "model": "model-a" },
                    { "backend": "pool", "model": "model-b" },
                    { "backend": "solo", "model": "solo-model" },
                ]
            })),
        );
        let pool = scoped_capacity_provider(
            home.path(),
            &log,
            "pool.agent",
            "pool",
            "claude-plans",
            "2026-09-25T03:00:00Z",
        );
        let solo = scoped_capacity_provider(
            home.path(),
            &log,
            "solo.agent",
            "solo",
            "",
            "2026-09-24T00:00:00Z",
        );

        let (payload, _) = capacity_with_catalog(capacity_args(), catalog_with(vec![pool, solo]))
            .expect("capacity report");

        let routes = payload["routes"].as_array().expect("routes");
        assert_eq!(routes.len(), 2, "{}", payload);
        assert_eq!(routes[0]["backend"], "pool");
        assert_eq!(
            routes[0]["models"],
            json!(["model-a", "model-b"]),
            "the shared pool lists every route's model once"
        );
        assert_eq!(routes[0]["scope"], "claude-plans");
        assert_eq!(routes[1]["backend"], "solo");
        assert_eq!(routes[1]["models"], json!(["solo-model"]));
        assert!(routes[1]["scope"].is_null());
        assert_eq!(
            payload["next_reset"], "2026-09-24T00:00:00+00:00",
            "the soonest exhausted-account reset wins, even across routes"
        );
    });
}

/// A route whose capacity lookup fails reports `unknown` with its diagnostic
/// while every other route still returns.
#[test]
fn capacity_reports_a_failing_route_as_unknown_and_keeps_the_others() {
    crate::test_support::with_isolated_home(|home| {
        let log = home.path().join("probes.log");
        save_provider_policy(
            Some("broken"),
            Some(json!({ "entries": [{ "backend": "healthy", "model": "m" }] })),
        );
        let broken = failing_provider("broken.agent", "broken");
        let healthy = scoped_capacity_provider(
            home.path(),
            &log,
            "healthy.agent",
            "healthy",
            "",
            "2026-09-26T10:00:00Z",
        );

        let (payload, status) =
            capacity_with_catalog(capacity_args(), catalog_with(vec![broken, healthy]))
                .expect("capacity report still succeeds");

        assert_eq!(status, 0);
        let routes = payload["routes"].as_array().expect("routes");
        assert_eq!(routes.len(), 2);
        let broken_route = routes
            .iter()
            .find(|route| route["backend"] == "broken")
            .expect("broken route reported");
        assert_eq!(broken_route["capacity"]["state"], "unknown");
        assert!(broken_route["capacity"]["reason"]
            .as_str()
            .expect("diagnostic")
            .contains("capacity probe failed"),);
        let healthy_route = routes
            .iter()
            .find(|route| route["backend"] == "healthy")
            .expect("healthy route reported");
        assert_eq!(healthy_route["capacity"]["state"], "known");
        assert_eq!(payload["next_reset"], "2026-09-26T10:00:00+00:00");
    });
}

/// A runtime that ignores `mode` still answers (its normal probe result), but
/// the command reports only what that answer's capacity object contains —
/// nothing, so `unknown` — and never a dispatchability verdict.
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

/// `--backend`, `--model`, and `--selector` narrow the rotation walk.
#[test]
fn capacity_filters_narrow_the_rotation_walk() {
    crate::test_support::with_isolated_home(|home| {
        let log = home.path().join("probes.log");
        save_provider_policy(
            Some("alpha"),
            Some(json!({
                "entries": [
                    { "backend": "alpha", "model": "alpha-model" },
                    { "backend": "beta", "model": "beta-model" },
                    { "backend": "beta", "selector": "beta.other", "model": "beta-model" },
                ]
            })),
        );
        let alpha = scoped_capacity_provider(
            home.path(),
            &log,
            "alpha.agent",
            "alpha",
            "",
            "2026-09-27T00:00:00Z",
        );
        let beta = scoped_capacity_provider(
            home.path(),
            &log,
            "beta.agent",
            "beta",
            "",
            "2026-09-28T00:00:00Z",
        );
        let mut beta_other = scoped_capacity_provider(
            home.path(),
            &log,
            "beta.other",
            "beta",
            "",
            "2026-09-29T00:00:00Z",
        );
        beta_other.id = "beta.other".to_string();
        let catalog = catalog_with(vec![alpha, beta, beta_other]);

        let (payload, _) = capacity_with_catalog(
            AgentTaskCapacityArgs {
                backend: Some("beta".to_string()),
                ..capacity_args()
            },
            catalog.clone(),
        )
        .expect("backend-filtered report");
        let backends = payload["routes"]
            .as_array()
            .expect("routes")
            .iter()
            .map(|route| route["backend"].clone())
            .collect::<Vec<_>>();
        assert_eq!(backends, vec![json!("beta"), json!("beta")]);
        assert_eq!(payload["next_reset"], "2026-09-28T00:00:00+00:00");

        let (payload, _) = capacity_with_catalog(
            AgentTaskCapacityArgs {
                backend: Some("beta".to_string()),
                selector: Some("beta.other".to_string()),
                model: Some("beta-model".to_string()),
            },
            catalog,
        )
        .expect("selector-and-model-filtered report");
        let routes = payload["routes"].as_array().expect("routes");
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0]["selector"], "beta.other");
        assert_eq!(routes[0]["models"], json!(["beta-model"]));
    });
}

/// The human summary renders one block per pool with a headline and one
/// indented line per account, matching the #15024 example shape.
#[test]
fn capacity_summary_renders_one_block_per_pool_and_line_per_account() {
    let payload = json!({
        "schema": "homeboy/agent-task-capacity/v1",
        "generated_at": "2026-09-24T20:00:00+00:00",
        "routes": [
            {
                "backend": "opencode",
                "selector": null,
                "models": ["anthropic/claude-sonnet-5", "anthropic/claude-opus-5"],
                "scope": "opencode:anthropic",
                "capacity": {
                    "state": "known", "remaining": 70, "limit": 100, "unit": "percent",
                    "accounts": [
                        { "account": "chubes@extrachill.com", "state": "exhausted", "remaining": 0, "reset_at": "2026-09-25T03:00:00Z" },
                        { "account": "claude@extrachill.com", "state": "available", "remaining": 70 },
                    ],
                },
            },
            {
                "backend": "opencode",
                "selector": null,
                "models": ["openai/gpt-5.6"],
                "scope": "opencode:openai",
                "capacity": {
                    "state": "exhausted",
                    "reset_at": "2026-09-27T15:26:09Z",
                    "reason": "5-hour usage limit reached",
                    "accounts": [],
                },
            },
            {
                "backend": "opencode",
                "selector": null,
                "models": ["zai-coding-plan/glm-5.3"],
                "scope": "opencode:zai-coding-plan",
                "capacity": {
                    "state": "known", "remaining": 37, "limit": 100, "unit": "percent",
                    "reset_at": "2026-09-29T02:00:00Z",
                },
            },
            {
                "backend": "opencode",
                "selector": null,
                "models": ["opencode-go/glm-5.3-flash"],
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
            "opencode-go/glm-5.3-flash: capacity not published",
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
