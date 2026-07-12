//! Transport-neutral completion notifications delivered by installed extensions.

use std::process::Command;

use serde::Serialize;

use crate::core::notification_route::NotificationRoute;

/// A completion event passed to extension transports as typed argv values.
#[derive(Debug, Clone, Serialize)]
pub struct NotifyEvent {
    pub run_id: String,
    pub status: String,
    pub title: String,
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route: Option<String>,
}

impl NotifyEvent {
    pub fn run_completed(run_id: &str, status: &str) -> Self {
        Self {
            run_id: run_id.to_string(),
            status: status.to_string(),
            title: format!("homeboy run {status}"),
            body: format!("Run {run_id} finished with status {status}"),
            transport: None,
            route: None,
        }
    }

    pub fn run_completed_with_route(
        run_id: &str,
        status: &str,
        route: Option<&NotificationRoute>,
    ) -> Self {
        let mut event = Self::run_completed(run_id, status);
        if let Some(route) = route {
            event.transport = Some(route.transport.clone());
            event.route = Some(route.route.clone());
        }
        event
    }

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
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NotifyDelivery {
    Transport {
        extension_id: String,
        transport_id: String,
        command: Vec<String>,
        exit_code: Option<i32>,
    },
    NotConfigured,
}

#[derive(Debug, Clone, Serialize)]
pub struct NotifyOutcome {
    pub delivered: bool,
    pub delivery: NotifyDelivery,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Deliver an event through the route's installed extension transport. Route-less
/// events use only the configured operations default and otherwise do nothing.
pub fn dispatch(event: &NotifyEvent) -> NotifyOutcome {
    let transport_id = event.transport.clone().or_else(|| {
        crate::core::defaults::load_config()
            .notifications
            .default_transport
    });
    let Some(transport_id) = transport_id else {
        return NotifyOutcome {
            delivered: false,
            delivery: NotifyDelivery::NotConfigured,
            error: None,
        };
    };

    let extensions = match crate::core::extension::load_all_extensions() {
        Ok(extensions) => extensions,
        Err(err) => return missing_transport(&transport_id, err.message),
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
        return missing_transport(&transport_id, detail);
    };

    let mut argv = transport.command.clone();
    argv.extend(event.argv());
    let program = argv.first().expect("validated transport command").clone();
    match Command::new(&program).args(&argv[1..]).status() {
        Ok(status) => NotifyOutcome {
            delivered: status.success(),
            delivery: NotifyDelivery::Transport {
                extension_id: extension.id.clone(),
                transport_id,
                command: argv,
                exit_code: status.code(),
            },
            error: (!status.success())
                .then(|| format!("notification transport exited with {status}")),
        },
        Err(err) => NotifyOutcome {
            delivered: false,
            delivery: NotifyDelivery::Transport {
                extension_id: extension.id.clone(),
                transport_id,
                command: argv,
                exit_code: None,
            },
            error: Some(format!(
                "failed to run notification transport `{program}`: {err}"
            )),
        },
    }
}

fn missing_transport(transport_id: &str, detail: String) -> NotifyOutcome {
    NotifyOutcome {
        delivered: false,
        delivery: NotifyDelivery::NotConfigured,
        error: Some(format!(
            "notification transport `{transport_id}` {detail}; install or configure an extension that declares it"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn install_transport(id: &str, command: Vec<&str>) {
        let mut manifest: crate::core::extension::ExtensionManifest =
            serde_json::from_value(serde_json::json!({
                "name": "Test transport",
                "version": "1.0.0",
                "notification_transports": [{
                    "schema": crate::core::extension::NOTIFICATION_TRANSPORT_SCHEMA,
                    "id": id,
                    "command": command,
                }]
            }))
            .unwrap();
        manifest.id = "test-transport".to_string();
        crate::core::extension::save_manifest(&manifest).unwrap();
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
            crate::core::defaults::save_config(&crate::core::defaults::HomeboyConfig {
                notifications: crate::core::defaults::NotificationConfig {
                    default_transport: Some("test.run-completion".to_string()),
                },
                ..Default::default()
            })
            .unwrap();
            assert!(dispatch(&NotifyEvent::run_completed("run-123", "pass")).delivered);
        });
    }
}
