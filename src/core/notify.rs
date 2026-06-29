//! Pluggable, OS-agnostic local completion notifications.
//!
//! Homeboy never hardcodes a specific desktop notifier. Operators wire
//! whichever local notifier they prefer — `terminal-notifier`, `notify-send`,
//! a `curl` webhook, a Slack post, a `say` voice line — through a single
//! command template. The template is read from [`NOTIFY_COMMAND_ENV`] (or an
//! explicit per-call override) and is the only coupling point to the host
//! environment.
//!
//! Both `homeboy runs watch --notify` and the local daemon's completion
//! tracker dispatch through here so the notification surface is defined in
//! exactly one place.

use std::process::Command;

use serde::Serialize;

/// Environment variable holding the notify command template.
///
/// The template is split on whitespace into an argv vector and then each token
/// has its `{placeholder}` substrings replaced from the event fields. Splitting
/// before substituting means a single `{body}` token expands to one argv
/// element even though the body contains spaces — the template author controls
/// tokenization, the event controls values.
///
/// Supported placeholders: `{run_id}`, `{status}`, `{title}`, `{body}`,
/// `{message}` (title + " — " + body).
///
/// Examples:
/// - `notify-send {title} {body}`
/// - `terminal-notifier -title {title} -message {body}`
/// - `say {message}`
pub const NOTIFY_COMMAND_ENV: &str = "HOMEBOY_NOTIFY_COMMAND";

/// A completion event worth surfacing to the operator who walked away.
#[derive(Debug, Clone, Serialize)]
pub struct NotifyEvent {
    pub run_id: String,
    pub status: String,
    pub title: String,
    pub body: String,
}

impl NotifyEvent {
    /// Build the conventional "a watched run finished" event.
    pub fn run_completed(run_id: &str, status: &str) -> Self {
        Self {
            run_id: run_id.to_string(),
            status: status.to_string(),
            title: format!("homeboy run {status}"),
            body: format!("Run {run_id} finished with status {status}"),
        }
    }

    fn message(&self) -> String {
        format!("{} — {}", self.title, self.body)
    }

    fn substitute(&self, token: &str) -> String {
        token
            .replace("{run_id}", &self.run_id)
            .replace("{status}", &self.status)
            .replace("{title}", &self.title)
            .replace("{body}", &self.body)
            .replace("{message}", &self.message())
    }
}

/// How a notification was (or was not) delivered.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NotifyDelivery {
    /// A configured command was executed. `exit_code` is `None` when the
    /// process was spawned but its status could not be read.
    Command {
        command: String,
        exit_code: Option<i32>,
    },
    /// No command was configured; the event was written to stderr as a
    /// best-effort fallback so the operator still sees the completion locally.
    Stderr,
}

/// Result of attempting to deliver one notification.
#[derive(Debug, Clone, Serialize)]
pub struct NotifyOutcome {
    pub delivered: bool,
    pub delivery: NotifyDelivery,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The configured notify command template, if any, from the environment.
pub fn configured_command() -> Option<String> {
    std::env::var(NOTIFY_COMMAND_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Render a command template into an argv vector for one event.
///
/// Pure and deterministic: whitespace-tokenizes the template, then substitutes
/// placeholders within each token. Empty tokens (from repeated whitespace) are
/// dropped.
pub fn render_argv(template: &str, event: &NotifyEvent) -> Vec<String> {
    template
        .split_whitespace()
        .map(|token| event.substitute(token))
        .filter(|token| !token.is_empty())
        .collect()
}

/// Deliver one notification, best-effort.
///
/// Resolution order for the command template: explicit `command_override`, then
/// [`NOTIFY_COMMAND_ENV`]. When neither is set the event is written to stderr so
/// a local operator still sees it. A spawn/exec failure is captured in the
/// returned outcome and never propagated — a missing notifier must not fail the
/// run the caller is watching.
pub fn dispatch(event: &NotifyEvent, command_override: Option<&str>) -> NotifyOutcome {
    let template = command_override
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(configured_command);

    let Some(template) = template else {
        eprintln!(
            "homeboy notify [{}]: {}",
            event.status,
            event.message()
        );
        return NotifyOutcome {
            delivered: true,
            delivery: NotifyDelivery::Stderr,
            error: None,
        };
    };

    let argv = render_argv(&template, event);
    let Some((program, args)) = argv.split_first() else {
        return NotifyOutcome {
            delivered: false,
            delivery: NotifyDelivery::Command {
                command: template,
                exit_code: None,
            },
            error: Some("notify command template produced no program token".to_string()),
        };
    };

    match Command::new(program).args(args).status() {
        Ok(status) => NotifyOutcome {
            delivered: status.success(),
            delivery: NotifyDelivery::Command {
                command: template,
                exit_code: status.code(),
            },
            error: if status.success() {
                None
            } else {
                Some(format!("notify command exited with {status}"))
            },
        },
        Err(err) => NotifyOutcome {
            delivered: false,
            delivery: NotifyDelivery::Command {
                command: template,
                exit_code: None,
            },
            error: Some(format!("failed to run notify command `{program}`: {err}")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event() -> NotifyEvent {
        NotifyEvent::run_completed("run-123", "pass")
    }

    #[test]
    fn run_completed_builds_status_message() {
        let event = event();
        assert_eq!(event.run_id, "run-123");
        assert_eq!(event.status, "pass");
        assert!(event.message().contains("run-123"));
        assert!(event.message().contains("pass"));
    }

    #[test]
    fn render_argv_substitutes_placeholders_and_preserves_value_spaces() {
        let argv = render_argv("notify-send {title} {body}", &event());
        assert_eq!(argv[0], "notify-send");
        assert_eq!(argv[1], "homeboy run pass");
        // `{body}` carries spaces but stays a single argv element.
        assert_eq!(argv[2], "Run run-123 finished with status pass");
        assert_eq!(argv.len(), 3);
    }

    #[test]
    fn render_argv_supports_message_and_status_tokens() {
        let argv = render_argv("logger -t homeboy-{status} {message}", &event());
        assert_eq!(argv[0], "logger");
        assert_eq!(argv[1], "-t");
        assert_eq!(argv[2], "homeboy-pass");
        assert_eq!(argv[3], "homeboy run pass — Run run-123 finished with status pass");
    }

    #[test]
    fn dispatch_without_command_falls_back_to_stderr() {
        let outcome = dispatch(&event(), None);
        assert!(outcome.delivered);
        assert_eq!(outcome.delivery, NotifyDelivery::Stderr);
        assert!(outcome.error.is_none());
    }

    #[test]
    fn dispatch_runs_configured_command_override() {
        // `true` is a portable no-op that exits 0 on the supported platforms.
        let outcome = dispatch(&event(), Some("true"));
        assert!(outcome.delivered);
        match outcome.delivery {
            NotifyDelivery::Command { exit_code, .. } => assert_eq!(exit_code, Some(0)),
            other => panic!("expected command delivery, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_reports_missing_program_as_error_without_panicking() {
        let outcome = dispatch(&event(), Some("homeboy-no-such-notifier-binary {message}"));
        assert!(!outcome.delivered);
        assert!(outcome.error.is_some());
    }
}
