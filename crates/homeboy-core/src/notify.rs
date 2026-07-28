//! Transport-neutral lifecycle notifications delivered by installed extensions.
//!
//! An event has two halves. The prose half (`title`/`body`) is the stable argv
//! contract every installed transport already implements. The structured half
//! ([`NotifyPayload`]) is what lets a consumer *act* on an event rather than
//! only display it, and it is delivered out-of-band through the transport
//! child's environment — see [`crate::notification_payload`] for why argv is
//! not extended.

use std::process::Command;

use serde::Serialize;

use crate::notification_payload::{
    NotifyEventKind, NotifyPayload, NOTIFICATION_PAYLOAD_SCHEMA, NOTIFY_KIND_ENV,
    NOTIFY_PAYLOAD_ENV, NOTIFY_PAYLOAD_SCHEMA_ENV, NOTIFY_PAYLOAD_TRUNCATED_ENV,
};
use crate::notification_route::NotificationRoute;

/// Bound on transport output homeboy will hold in memory and inspect.
const TRANSPORT_OUTPUT_INSPECTION_LIMIT: usize = 256 * 1024;
/// Bound on the transport diagnostic text folded into `NotifyOutcome::error`.
const TRANSPORT_ERROR_TAIL_CHARS: usize = 800;

/// A lifecycle event passed to extension transports as typed argv values.
#[derive(Debug, Clone, Serialize)]
pub struct NotifyEvent {
    pub run_id: String,
    pub status: String,
    pub title: String,
    pub body: String,
    /// Where this event sits in its subject's lifecycle. Terminal by default so
    /// producers written before lifecycle events existed keep their meaning.
    pub kind: NotifyEventKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route: Option<String>,
    /// The machine-readable half. Absent for producers that only have prose.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<NotifyPayload>,
}

impl NotifyEvent {
    pub fn run_completed(run_id: &str, status: &str) -> Self {
        Self {
            run_id: run_id.to_string(),
            status: status.to_string(),
            title: format!("homeboy run {status}"),
            body: format!("Run {run_id} finished with status {status}"),
            kind: NotifyEventKind::Completed,
            transport: None,
            route: None,
            payload: None,
        }
    }

    pub fn run_completed_with_route(
        run_id: &str,
        status: &str,
        route: Option<&NotificationRoute>,
    ) -> Self {
        Self::run_completed(run_id, status).with_route(route)
    }

    /// A lifecycle event for a subject that is not an observation run.
    ///
    /// `run_id` remains the argv field name every installed transport already
    /// reads, so a non-run subject identifies itself there rather than through
    /// a new flag. Producers should attach a payload whose subject carries the
    /// real class.
    pub fn lifecycle(kind: NotifyEventKind, subject_id: &str, status: &str) -> Self {
        Self {
            run_id: subject_id.to_string(),
            status: status.to_string(),
            title: format!("homeboy {} {status}", kind.as_str()),
            body: format!("{subject_id} is {status}"),
            kind,
            transport: None,
            route: None,
            payload: None,
        }
    }

    /// Bind an explicit destination. A `None` route leaves the event route-less,
    /// so it resolves through the configured operations default instead.
    pub fn with_route(mut self, route: Option<&NotificationRoute>) -> Self {
        if let Some(route) = route {
            self.transport = Some(route.transport.clone());
            self.route = Some(route.route.clone());
        }
        self
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    /// Attach the structured half and derive the prose from it.
    ///
    /// Rendering the body from the payload is what keeps this change additive:
    /// an already-installed text-only transport receives the component, phase,
    /// facts, links, next actions, and attachment handles immediately, with no
    /// coordinated extension release — and the prose can never drift from the
    /// structure because there is only one source for both.
    pub fn with_payload(mut self, payload: NotifyPayload) -> Self {
        self.kind = payload.kind;
        let rendered = payload.render_body();
        if !rendered.trim().is_empty() {
            self.body = rendered;
        }
        self.payload = Some(payload);
        self
    }

    /// The frozen argv contract.
    ///
    /// Do not add flags here. An installed transport validates its own argv and
    /// rejects unknown flags, so a new flag turns every notification into an
    /// input error until every installed extension — in every repo, at every
    /// pinned version — has shipped support for it. Structured additions travel
    /// through `payload_env` instead, which an unaware transport ignores.
    fn argv(&self) -> Vec<String> {
        let mut argv = vec![
            "--run-id".to_string(),
            self.run_id.clone(),
            "--status".to_string(),
            self.status.clone(),
            "--title".to_string(),
            self.title.clone(),
            "--body".to_string(),
            self.body.clone(),
        ];
        if let Some(transport) = &self.transport {
            argv.extend(["--transport".to_string(), transport.clone()]);
        }
        if let Some(route) = &self.route {
            argv.extend(["--route".to_string(), route.clone()]);
        }
        argv
    }

    /// Environment handed to the transport child alongside the argv contract.
    ///
    /// Always carries the lifecycle kind, and the serialized payload when the
    /// producer supplied one. A transport that knows nothing about these names
    /// is unaffected: unread environment variables cost it nothing, where an
    /// unknown flag would fail it outright.
    fn payload_env(&self) -> Vec<(&'static str, String)> {
        let mut env = vec![(NOTIFY_KIND_ENV, self.kind.as_str().to_string())];
        let Some(payload) = &self.payload else {
            return env;
        };
        let Some((json, truncated)) = payload.serialize_bounded() else {
            return env;
        };
        env.push((
            NOTIFY_PAYLOAD_SCHEMA_ENV,
            NOTIFICATION_PAYLOAD_SCHEMA.to_string(),
        ));
        env.push((NOTIFY_PAYLOAD_ENV, json));
        if truncated {
            env.push((NOTIFY_PAYLOAD_TRUNCATED_ENV, "1".to_string()));
        }
        env
    }
}

/// Whether a finished run still needs a human decision.
///
/// Resolved through the owned [`crate::observation::RunStatus`] vocabulary
/// rather than a private string list, so a new status classifies itself here.
/// An unowned label is conservatively treated as needing attention: silently
/// reporting an unrecognized outcome as a clean completion is the worse error.
fn run_status_needs_attention(status: &str) -> bool {
    use crate::observation::RunStatus;
    !matches!(
        RunStatus::from_label(status),
        Some(RunStatus::Pass | RunStatus::Skipped | RunStatus::Running)
    )
}

/// The structured half of a run-completion notification, built from the run
/// record every completion site already holds.
///
/// Shared by the controller's completion notifier, `runs watch --notify`, and
/// the runner's direct delivery so all three describe a finished run the same
/// way rather than each fabricating its own sentence.
pub fn run_completed_payload(run: &crate::observation::RunRecord) -> NotifyPayload {
    let kind = if run_status_needs_attention(&run.status) {
        NotifyEventKind::NeedsAttention
    } else {
        NotifyEventKind::Completed
    };
    let mut subject =
        crate::notification_payload::NotifySubject::new(run.kind.clone(), run.id.clone());
    subject.component = run.component_id.clone();
    NotifyPayload::new(kind, subject)
        .with_fact("Status", run.status.clone())
        .with_optional_fact("Command", run.command.clone())
        .with_optional_fact("Component", run.component_id.clone())
        .with_optional_fact("Finished", run.finished_at.clone())
        .with_optional_fact("Version", run.homeboy_version.clone())
        .with_optional_fact("Commit", run.git_sha.clone())
        .with_action(
            crate::notification_payload::NotifyAction::new(
                "show run",
                format!("homeboy runs show {}", run.id),
            )
            .with_kind("show"),
        )
        .with_action(
            crate::notification_payload::NotifyAction::new(
                "show evidence",
                format!("homeboy runs evidence {}", run.id),
            )
            .with_kind("show"),
        )
        .with_action(
            crate::notification_payload::NotifyAction::new(
                "list artifacts",
                format!("homeboy runs artifacts {}", run.id),
            )
            .with_kind("artifacts"),
        )
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NotifyDelivery {
    Transport {
        extension_id: String,
        transport_id: String,
        command: Vec<String>,
        exit_code: Option<i32>,
        /// Environment variable names carrying the structured payload. Recorded
        /// so the delivery record stays a complete reproduction of the child
        /// invocation now that not everything travels through argv.
        #[serde(skip_serializing_if = "Vec::is_empty")]
        payload_env: Vec<String>,
    },
    NotConfigured,
}

#[derive(Debug, Clone, Serialize)]
pub struct NotifyOutcome {
    pub delivered: bool,
    /// The lifecycle position of the event this outcome describes.
    pub event_kind: NotifyEventKind,
    pub delivery: NotifyDelivery,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The transport's own structured result envelope, when it emitted one.
    ///
    /// Transports classify their failures, count their retries, and report
    /// truncation on stdout. That work used to be discarded because homeboy
    /// only read the exit code.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
}

/// Deliver an event through the route's installed extension transport. Route-less
/// events use only the configured operations default and otherwise do nothing.
pub fn dispatch(event: &NotifyEvent) -> NotifyOutcome {
    let transport_id = event.transport.clone().or_else(|| {
        crate::defaults::load_config()
            .notifications
            .default_transport
    });
    let Some(transport_id) = transport_id else {
        return NotifyOutcome {
            delivered: false,
            event_kind: event.kind,
            delivery: NotifyDelivery::NotConfigured,
            error: None,
            result: None,
        };
    };

    let extensions = match crate::extension_store::load_all_extensions() {
        Ok(extensions) => extensions,
        Err(err) => return missing_transport(event.kind, &transport_id, err.message),
    };
    let matches: Vec<_> = extensions
        .iter()
        .flat_map(|extension| {
            extension
                .notification_transports
                .iter()
                .filter(|transport| transport.id == transport_id)
                .map(move |transport| (extension, transport))
        })
        .collect();
    let [(extension, transport)] = matches.as_slice() else {
        let detail = if matches.is_empty() {
            "is not declared by an installed extension".to_string()
        } else {
            "is declared by more than one installed extension".to_string()
        };
        return missing_transport(event.kind, &transport_id, detail);
    };

    // Resolve `{extension_path}` in the transport's literal argv so a manifest
    // can reference scripts it ships (e.g. `node {extension_path}/scripts/notify.mjs`)
    // without hardcoding an absolute install path. `extension_path` is populated
    // by the extension store when the manifest is loaded.
    let extension_path = extension.extension_path.clone().unwrap_or_default();
    let mut argv: Vec<String> = transport
        .command
        .iter()
        .map(|arg| arg.replace("{extension_path}", &extension_path))
        .collect();
    argv.extend(event.argv());
    let program = argv.first().expect("validated transport command").clone();
    let payload_env = event.payload_env();
    let payload_env_names = payload_env
        .iter()
        .map(|(name, _)| (*name).to_string())
        .collect();

    let mut command = Command::new(&program);
    command.args(&argv[1..]);
    for (name, value) in payload_env {
        command.env(name, value);
    }

    // `output()` rather than `status()`: a transport reports its delivery mode,
    // route kind, truncation, attempt count, and a classified error kind on
    // stdout. Reading only the exit code discarded all of it, so a failed
    // notification could not be told apart from a rate-limited one.
    match command.output() {
        Ok(output) => {
            let delivered = output.status.success();
            let result = parse_transport_result(&output.stdout);
            NotifyOutcome {
                delivered,
                event_kind: event.kind,
                delivery: NotifyDelivery::Transport {
                    extension_id: extension.id.clone(),
                    transport_id,
                    command: argv,
                    exit_code: output.status.code(),
                    payload_env: payload_env_names,
                },
                error: (!delivered).then(|| transport_failure_message(&output)),
                result,
            }
        }
        Err(err) => NotifyOutcome {
            delivered: false,
            event_kind: event.kind,
            delivery: NotifyDelivery::Transport {
                extension_id: extension.id.clone(),
                transport_id,
                command: argv,
                exit_code: None,
                payload_env: payload_env_names,
            },
            error: Some(format!(
                "failed to run notification transport `{program}`: {err}"
            )),
            result: None,
        },
    }
}

/// Parse a transport's structured result envelope. A transport that prints
/// nothing, prints prose, or prints oversized output is not an error — it just
/// contributes no structured result.
fn parse_transport_result(stdout: &[u8]) -> Option<serde_json::Value> {
    if stdout.is_empty() || stdout.len() > TRANSPORT_OUTPUT_INSPECTION_LIMIT {
        return None;
    }
    let text = std::str::from_utf8(stdout).ok()?.trim();
    // Transports emit one JSON line; take the last so leading log lines from a
    // chatty runtime do not defeat the parse.
    let line = text.lines().rev().find(|line| !line.trim().is_empty())?;
    serde_json::from_str::<serde_json::Value>(line.trim())
        .ok()
        .filter(serde_json::Value::is_object)
}

fn transport_failure_message(output: &std::process::Output) -> String {
    let status = output.status;
    let diagnostic = [&output.stderr, &output.stdout]
        .into_iter()
        .filter(|stream| stream.len() <= TRANSPORT_OUTPUT_INSPECTION_LIMIT)
        .filter_map(|stream| std::str::from_utf8(stream).ok())
        .map(str::trim)
        .find(|text| !text.is_empty())
        .map(|text| {
            let tail: String = text
                .chars()
                .rev()
                .take(TRANSPORT_ERROR_TAIL_CHARS)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            format!(": {tail}")
        })
        .unwrap_or_default();
    format!("notification transport exited with {status}{diagnostic}")
}

fn missing_transport(
    event_kind: NotifyEventKind,
    transport_id: &str,
    detail: String,
) -> NotifyOutcome {
    NotifyOutcome {
        delivered: false,
        event_kind,
        delivery: NotifyDelivery::NotConfigured,
        error: Some(format!(
            "notification transport `{transport_id}` {detail}; install or configure an extension that declares it"
        )),
        result: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn install_transport(id: &str, command: Vec<&str>) {
        let mut manifest: homeboy_extension_contract::ExtensionManifest =
            serde_json::from_value(serde_json::json!({
                "name": "Test transport",
                "version": "1.0.0",
                "notification_transports": [{
                    "schema": homeboy_extension_contract::notification_transport_config::NOTIFICATION_TRANSPORT_SCHEMA,
                    "id": id,
                    "command": command,
                }]
            }))
            .unwrap();
        manifest.id = "test-transport".to_string();
        crate::extension_store::save_manifest(&manifest).unwrap();
    }

    #[test]
    fn event_argv_keeps_opaque_route_as_one_value() {
        let route = NotificationRoute::new("discord.run-completion", "thread 42; opaque").unwrap();
        let event = NotifyEvent::run_completed_with_route("run-123", "pass", Some(&route));
        assert_eq!(
            event.argv(),
            [
                "--run-id",
                "run-123",
                "--status",
                "pass",
                "--title",
                "homeboy run pass",
                "--body",
                "Run run-123 finished with status pass",
                "--transport",
                "discord.run-completion",
                "--route",
                "thread 42; opaque"
            ]
        );
    }

    #[test]
    fn route_less_event_without_operations_policy_is_not_delivered() {
        crate::test_support::with_isolated_home(|_| {
            let outcome = dispatch(&NotifyEvent::run_completed("run-123", "pass"));
            assert!(!outcome.delivered);
            assert_eq!(outcome.delivery, NotifyDelivery::NotConfigured);
            assert!(outcome.error.is_none());
        });
    }

    #[test]
    fn installed_transport_receives_typed_event_argv() {
        crate::test_support::with_isolated_home(|_| {
            install_transport("test.run-completion", vec!["true"]);
            let route = NotificationRoute::new("test.run-completion", "route-42").unwrap();
            let outcome = dispatch(&NotifyEvent::run_completed_with_route(
                "run-123",
                "pass",
                Some(&route),
            ));
            assert!(outcome.delivered);
            let NotifyDelivery::Transport { command, .. } = outcome.delivery else {
                panic!("expected transport delivery");
            };
            assert_eq!(command[0], "true");
            assert!(command
                .windows(2)
                .any(|pair| pair == ["--route", "route-42"]));
        });
    }

    #[test]
    fn extension_path_placeholder_is_resolved_in_transport_command() {
        crate::test_support::with_isolated_home(|_| {
            install_transport(
                "test.run-completion",
                vec!["true", "{extension_path}/scripts/notify.mjs"],
            );
            let route = NotificationRoute::new("test.run-completion", "route-42").unwrap();
            let outcome = dispatch(&NotifyEvent::run_completed_with_route(
                "run-123",
                "pass",
                Some(&route),
            ));
            let NotifyDelivery::Transport { command, .. } = outcome.delivery else {
                panic!("expected transport delivery");
            };
            // The `{extension_path}` placeholder must be replaced with the
            // installed extension directory and never delivered literally.
            assert!(
                !command.iter().any(|arg| arg.contains("{extension_path}")),
                "unresolved placeholder in {command:?}"
            );
            assert!(
                command
                    .iter()
                    .any(|arg| arg.ends_with("/scripts/notify.mjs") && arg.starts_with('/')),
                "expected an absolute resolved script path in {command:?}"
            );
        });
    }

    #[test]
    fn missing_selected_transport_reports_diagnostic() {
        crate::test_support::with_isolated_home(|_| {
            let route = NotificationRoute::new("missing.transport", "route-42").unwrap();
            let outcome = dispatch(&NotifyEvent::run_completed_with_route(
                "run-123",
                "pass",
                Some(&route),
            ));
            assert!(!outcome.delivered);
            assert!(outcome.error.unwrap().contains("missing.transport"));
        });
    }

    #[test]
    fn route_less_event_uses_explicit_operations_default_transport() {
        crate::test_support::with_isolated_home(|_| {
            install_transport("test.run-completion", vec!["true"]);
            crate::defaults::save_config(&crate::defaults::HomeboyConfig {
                notifications: crate::defaults::NotificationConfig {
                    default_transport: Some("test.run-completion".to_string()),
                },
                ..Default::default()
            })
            .unwrap();
            assert!(dispatch(&NotifyEvent::run_completed("run-123", "pass")).delivered);
        });
    }

    fn cook_payload() -> NotifyPayload {
        NotifyPayload::new(
            NotifyEventKind::NeedsAttention,
            crate::notification_payload::NotifySubject::new("agent_task_cook", "cook-abc")
                .with_component("homeboy")
                .with_attempt(2),
        )
        .with_fact("Stop reason", "gate_failed")
        .with_action(
            crate::notification_payload::NotifyAction::new(
                "diagnose",
                "homeboy agent-task diagnose cook-abc",
            )
            .with_kind("repair"),
        )
    }

    #[test]
    fn structured_payload_does_not_change_the_argv_contract() {
        // The whole compatibility argument rests on this: an installed
        // transport at its current version must see exactly the argv it
        // already validates, with or without a payload.
        let plain = NotifyEvent::run_completed("run-123", "pass");
        let enriched = NotifyEvent::run_completed("run-123", "pass").with_payload(cook_payload());
        let flags = |event: &NotifyEvent| {
            event
                .argv()
                .into_iter()
                .filter(|arg| arg.starts_with("--"))
                .collect::<Vec<_>>()
        };
        assert_eq!(flags(&plain), flags(&enriched));
    }

    #[test]
    fn payload_replaces_the_fabricated_body_so_text_transports_gain_detail() {
        let event = NotifyEvent::run_completed("run-123", "pass").with_payload(cook_payload());
        // The old body was derived entirely from run id and status.
        assert_ne!(event.body, "Run run-123 finished with status pass");
        assert!(event.body.contains("Component: homeboy"), "{}", event.body);
        assert!(
            event.body.contains("Stop reason: gate_failed"),
            "{}",
            event.body
        );
        assert!(
            event.body.contains("homeboy agent-task diagnose cook-abc"),
            "{}",
            event.body
        );
        // Prose is still present and still delivered through argv.
        assert!(event.argv().contains(&event.body));
        assert_eq!(event.kind, NotifyEventKind::NeedsAttention);
    }

    #[test]
    fn payload_free_event_keeps_its_historical_prose_and_kind() {
        let event = NotifyEvent::run_completed("run-123", "pass");
        assert_eq!(event.body, "Run run-123 finished with status pass");
        assert_eq!(event.kind, NotifyEventKind::Completed);
        assert!(event.payload.is_none());
    }

    #[test]
    fn payload_env_carries_kind_without_a_payload() {
        let env = NotifyEvent::run_completed("run-123", "pass").payload_env();
        assert_eq!(env, vec![(NOTIFY_KIND_ENV, "completed".to_string())]);
    }

    #[test]
    fn payload_env_carries_schema_and_serialized_payload() {
        let event = NotifyEvent::run_completed("run-123", "fail").with_payload(cook_payload());
        let env = event.payload_env();
        let by_name = |name: &str| {
            env.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.clone())
        };
        assert_eq!(by_name(NOTIFY_KIND_ENV).as_deref(), Some("needs_attention"));
        assert_eq!(
            by_name(NOTIFY_PAYLOAD_SCHEMA_ENV).as_deref(),
            Some(NOTIFICATION_PAYLOAD_SCHEMA)
        );
        let payload: NotifyPayload =
            serde_json::from_str(&by_name(NOTIFY_PAYLOAD_ENV).expect("payload env")).unwrap();
        assert_eq!(payload.subject.unwrap().id, "cook-abc");
        assert!(by_name(NOTIFY_PAYLOAD_TRUNCATED_ENV).is_none());
    }

    #[cfg(unix)]
    fn install_recording_transport(
        id: &str,
        home: &std::path::Path,
        stdout_line: &str,
        exit_code: i32,
    ) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let record = home.join("notify-observed.txt");
        let script = home.join("notify-transport.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n\
                 {{\n\
                 echo \"kind=$HOMEBOY_NOTIFY_KIND\"\n\
                 echo \"schema=$HOMEBOY_NOTIFY_PAYLOAD_SCHEMA\"\n\
                 echo \"payload=$HOMEBOY_NOTIFY_PAYLOAD\"\n\
                 }} > {record}\n\
                 echo '{stdout_line}'\n\
                 exit {exit_code}\n",
                record = record.display(),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        install_transport(id, vec![script.to_str().unwrap()]);
        record
    }

    #[cfg(unix)]
    #[test]
    fn transport_child_receives_the_structured_payload_through_its_environment() {
        crate::test_support::with_isolated_home(|home| {
            let record = install_recording_transport(
                "test.run-completion",
                home.path(),
                "{\"schema\":\"test/notification-result/v1\",\"status\":\"delivered\",\"attempts\":2}",
                0,
            );
            let route = NotificationRoute::new("test.run-completion", "route-42").unwrap();
            let event = NotifyEvent::run_completed("run-123", "fail")
                .with_route(Some(&route))
                .with_payload(cook_payload());
            let outcome = dispatch(&event);
            assert!(outcome.delivered, "{:?}", outcome.error);

            let observed = std::fs::read_to_string(&record).unwrap();
            assert!(observed.contains("kind=needs_attention"), "{observed}");
            assert!(
                observed.contains(&format!("schema={NOTIFICATION_PAYLOAD_SCHEMA}")),
                "{observed}"
            );
            assert!(observed.contains("cook-abc"), "{observed}");

            let NotifyDelivery::Transport { payload_env, .. } = &outcome.delivery else {
                panic!("expected transport delivery");
            };
            assert!(payload_env.iter().any(|name| name == NOTIFY_PAYLOAD_ENV));
            assert_eq!(outcome.event_kind, NotifyEventKind::NeedsAttention);
        });
    }

    #[cfg(unix)]
    #[test]
    fn transport_structured_result_is_retained_instead_of_discarded() {
        crate::test_support::with_isolated_home(|home| {
            install_recording_transport(
                "test.run-completion",
                home.path(),
                "{\"schema\":\"test/notification-result/v1\",\"status\":\"delivered\",\"attempts\":3}",
                0,
            );
            let route = NotificationRoute::new("test.run-completion", "route-42").unwrap();
            let outcome = dispatch(&NotifyEvent::run_completed_with_route(
                "run-123",
                "pass",
                Some(&route),
            ));
            let result = outcome.result.expect("transport result envelope");
            assert_eq!(result["status"], "delivered");
            assert_eq!(result["attempts"], 3);
        });
    }

    #[cfg(unix)]
    #[test]
    fn failed_transport_reports_its_own_diagnostic_not_just_an_exit_code() {
        crate::test_support::with_isolated_home(|home| {
            install_recording_transport(
                "test.run-completion",
                home.path(),
                "{\"schema\":\"test/notification-result/v1\",\"status\":\"failed\",\"error\":{\"kind\":\"auth_error\"}}",
                1,
            );
            let route = NotificationRoute::new("test.run-completion", "route-42").unwrap();
            let outcome = dispatch(&NotifyEvent::run_completed_with_route(
                "run-123",
                "fail",
                Some(&route),
            ));
            assert!(!outcome.delivered);
            let error = outcome.error.expect("failure diagnostic");
            assert!(error.contains("auth_error"), "{error}");
            assert_eq!(outcome.result.expect("result")["status"], "failed");
        });
    }

    #[test]
    fn non_json_transport_output_is_not_reported_as_a_structured_result() {
        assert!(parse_transport_result(b"").is_none());
        assert!(parse_transport_result(b"delivered\n").is_none());
        // A bare JSON scalar is not a result envelope.
        assert!(parse_transport_result(b"42\n").is_none());
        assert!(parse_transport_result(b"warm-up log line\n{\"status\":\"ok\"}\n").is_some());
    }
}
