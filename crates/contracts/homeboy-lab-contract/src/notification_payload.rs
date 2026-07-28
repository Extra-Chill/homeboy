//! Transport-neutral structured payload carried alongside notification prose.
//!
//! A notification used to be two fabricated sentences derived from the run id
//! and status, so a transport could display an event but never act on one. This
//! module adds the machine-readable half: what the event is about, the facts a
//! reader needs, the exact commands that advance it, and the evidence a
//! capable transport can attach.
//!
//! Two invariants keep this additive:
//!
//! 1. **Prose stays authoritative for rendering.** Every producer still emits
//!    `title`/`body`. [`NotifyPayload::render_body`] derives that prose from the
//!    payload, so a text-only transport gets the richer content with no change
//!    at all, and the two halves cannot drift.
//! 2. **The payload never reaches a transport through argv.** Installed
//!    transports validate their argv and reject unknown flags, so appending new
//!    flags would break every already-installed transport at its current
//!    version. The payload travels as process environment instead, which an
//!    unaware transport simply never reads.

use serde::{Deserialize, Serialize};

pub const NOTIFICATION_PAYLOAD_SCHEMA: &str = "homeboy/notification-payload/v1";

/// Event kind. Read by transports that want to route or style by lifecycle
/// position without parsing the payload.
pub const NOTIFY_KIND_ENV: &str = "HOMEBOY_NOTIFY_KIND";
/// The serialized [`NotifyPayload`].
pub const NOTIFY_PAYLOAD_ENV: &str = "HOMEBOY_NOTIFY_PAYLOAD";
/// The payload schema id, so a transport can version-gate before parsing.
pub const NOTIFY_PAYLOAD_SCHEMA_ENV: &str = "HOMEBOY_NOTIFY_PAYLOAD_SCHEMA";
/// Set to `1` only when bounding dropped payload members.
pub const NOTIFY_PAYLOAD_TRUNCATED_ENV: &str = "HOMEBOY_NOTIFY_PAYLOAD_TRUNCATED";

/// Serialized payload budget. `ARG_MAX` covers argv *and* environment on Linux,
/// and a notification must never be the reason a transport fails to spawn.
pub const NOTIFY_PAYLOAD_MAX_BYTES: usize = 48 * 1024;

/// Rendered prose budget. Independent of any one transport's message limit;
/// this only stops an unbounded body from reaching a child process at all.
pub const NOTIFY_BODY_MAX_CHARS: usize = 3500;

const BOUNDED_FACTS: usize = 12;
const BOUNDED_ACTIONS: usize = 6;
const BOUNDED_LINKS: usize = 6;
const BOUNDED_ATTACHMENTS: usize = 20;
const FIELD_MAX_CHARS: usize = 512;

/// Where an event sits in its subject's lifecycle.
///
/// `Completed` is the default so a producer that only knows "this finished"
/// keeps the historical meaning of a notification.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotifyEventKind {
    /// Durable identity exists; execution has not begun.
    Queued,
    /// Execution has begun and the subject is addressable.
    Started,
    /// A bounded intermediate boundary (an attempt or retry), never a heartbeat.
    Progress,
    /// The subject reached a terminal state.
    #[default]
    Completed,
    /// Terminal, but a human decision is required before anything advances.
    NeedsAttention,
}

impl NotifyEventKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Started => "started",
            Self::Progress => "progress",
            Self::Completed => "completed",
            Self::NeedsAttention => "needs_attention",
        }
    }

    /// Whether the subject will not transition further on its own.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::NeedsAttention)
    }
}

/// What the event is about. `kind` is a generic subject class (`run`,
/// `agent_task_cook`, `schedule`, `activity`), never a transport concept.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifySubject {
    pub kind: String,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component: Option<String>,
    /// The batch, plan, or parent run this subject belongs to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    /// The producer's own phase label at emission time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
}

impl NotifySubject {
    pub fn new(kind: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            id: id.into(),
            ..Default::default()
        }
    }

    pub fn with_component(mut self, component: impl Into<String>) -> Self {
        self.component = Some(component.into());
        self
    }

    pub fn with_parent(mut self, parent_id: impl Into<String>) -> Self {
        self.parent_id = Some(parent_id.into());
        self
    }

    pub fn with_attempt(mut self, attempt: u32) -> Self {
        self.attempt = Some(attempt);
        self
    }

    pub fn with_phase(mut self, phase: impl Into<String>) -> Self {
        self.phase = Some(phase.into());
        self
    }
}

/// One ordered label/value fact. Producers put the facts an operator would
/// otherwise have to re-gather by hand here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyFact {
    pub label: String,
    pub value: String,
}

impl NotifyFact {
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
        }
    }
}

/// An exact command that advances the subject.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyAction {
    pub label: String,
    pub command: String,
    /// Generic intent hint (`show`, `watch`, `artifacts`, `repair`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

impl NotifyAction {
    pub fn new(label: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            command: command.into(),
            kind: None,
        }
    }

    pub fn with_kind(mut self, kind: impl Into<String>) -> Self {
        self.kind = Some(kind.into());
        self
    }
}

/// An external reference (pull request, issue, CI run).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyLink {
    pub label: String,
    pub url: String,
}

impl NotifyLink {
    pub fn new(label: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            url: url.into(),
        }
    }
}

/// Evidence an attachment-capable transport can deliver directly.
///
/// This is the declarative half of grouped-artifact delivery: a transport that
/// cannot attach files ignores it and still renders `fetch_command`, while a
/// transport that can attach resolves `uri`/`fetch_command` itself. `group_id`
/// plus `role` carry the relationship (`source`/`candidate`/`diff`) explicitly,
/// so no consumer has to parse filename conventions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyAttachment {
    pub id: String,
    /// Generic class: `artifact` or `evidence`.
    pub kind: String,
    pub uri: String,
    /// The exact command that materializes the bytes locally.
    pub fetch_command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub byte_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    /// Stable id shared by every attachment presented together.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    /// Semantic role within `group_id`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

impl NotifyAttachment {
    pub fn new(
        id: impl Into<String>,
        kind: impl Into<String>,
        uri: impl Into<String>,
        fetch_command: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            kind: kind.into(),
            uri: uri.into(),
            fetch_command: fetch_command.into(),
            media_type: None,
            display_name: None,
            byte_size: None,
            caption: None,
            group_id: None,
            role: None,
        }
    }

    pub fn with_group(mut self, group_id: impl Into<String>, role: impl Into<String>) -> Self {
        self.group_id = Some(group_id.into());
        self.role = Some(role.into());
        self
    }

    pub fn with_media_type(mut self, media_type: impl Into<String>) -> Self {
        self.media_type = Some(media_type.into());
        self
    }
}

/// The structured half of a notification.
///
/// Deserializable so a transport written in Rust — and homeboy's own tests —
/// can round-trip it. Deliberately *not* `deny_unknown_fields`: a payload
/// produced by a newer homeboy must still parse in an older consumer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyPayload {
    #[serde(default = "notification_payload_schema")]
    pub schema: String,
    #[serde(default)]
    pub kind: NotifyEventKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<NotifySubject>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facts: Vec<NotifyFact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<NotifyAction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<NotifyLink>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<NotifyAttachment>,
}

fn notification_payload_schema() -> String {
    NOTIFICATION_PAYLOAD_SCHEMA.to_string()
}

impl Default for NotifyPayload {
    fn default() -> Self {
        Self {
            schema: notification_payload_schema(),
            kind: NotifyEventKind::default(),
            subject: None,
            facts: Vec::new(),
            actions: Vec::new(),
            links: Vec::new(),
            attachments: Vec::new(),
        }
    }
}

impl NotifyPayload {
    pub fn new(kind: NotifyEventKind, subject: NotifySubject) -> Self {
        Self {
            kind,
            subject: Some(subject),
            ..Default::default()
        }
    }

    pub fn with_fact(mut self, label: impl Into<String>, value: impl Into<String>) -> Self {
        self.facts.push(NotifyFact::new(label, value));
        self
    }

    /// Add a fact only when the value is present and non-empty, so an absent
    /// field never renders as `Component: ` with nothing after it.
    pub fn with_optional_fact(
        self,
        label: impl Into<String>,
        value: Option<impl Into<String>>,
    ) -> Self {
        let value: Option<String> = value.map(Into::into);
        match value.filter(|value| !value.trim().is_empty()) {
            Some(value) => self.with_fact(label, value),
            None => self,
        }
    }

    pub fn with_action(mut self, action: NotifyAction) -> Self {
        self.actions.push(action);
        self
    }

    pub fn with_link(mut self, link: NotifyLink) -> Self {
        self.links.push(link);
        self
    }

    pub fn with_attachment(mut self, attachment: NotifyAttachment) -> Self {
        self.attachments.push(attachment);
        self
    }

    pub fn with_attachments(
        mut self,
        attachments: impl IntoIterator<Item = NotifyAttachment>,
    ) -> Self {
        self.attachments.extend(attachments);
        self
    }

    /// Whether this payload carries anything a consumer could not already
    /// derive from `run_id` and `status` alone.
    pub fn is_empty(&self) -> bool {
        self.subject.is_none()
            && self.facts.is_empty()
            && self.actions.is_empty()
            && self.links.is_empty()
            && self.attachments.is_empty()
    }

    /// Render the human-facing body for transports that only display text.
    ///
    /// This is the compatibility bridge: the same information a payload-aware
    /// transport reads structurally is written here as bounded prose, so an
    /// already-installed text transport gets the richer message without any
    /// coordinated release.
    pub fn render_body(&self) -> String {
        let mut lines: Vec<String> = Vec::new();
        if let Some(subject) = &self.subject {
            if let Some(component) = subject.component.as_deref().filter(|it| !it.is_empty()) {
                lines.push(format!("Component: {component}"));
            }
            if let Some(parent) = subject.parent_id.as_deref().filter(|it| !it.is_empty()) {
                lines.push(format!("Batch: {parent}"));
            }
            if let Some(attempt) = subject.attempt {
                lines.push(format!("Attempt: {attempt}"));
            }
            if let Some(phase) = subject.phase.as_deref().filter(|it| !it.is_empty()) {
                lines.push(format!("Phase: {phase}"));
            }
        }
        for fact in &self.facts {
            lines.push(format!("{}: {}", fact.label, fact.value));
        }
        for link in &self.links {
            lines.push(format!("{}: {}", link.label, link.url));
        }
        for action in &self.actions {
            lines.push(format!("Next — {}: {}", action.label, action.command));
        }
        if !self.attachments.is_empty() {
            lines.push(format!("Attachments: {}", self.attachments.len()));
            for attachment in self.attachments.iter().take(3) {
                let name = attachment
                    .display_name
                    .as_deref()
                    .unwrap_or(attachment.id.as_str());
                lines.push(format!("  {name}: {}", attachment.fetch_command));
            }
        }
        bound_chars(&lines.join("\n"), NOTIFY_BODY_MAX_CHARS)
    }

    /// Serialize within [`NOTIFY_PAYLOAD_MAX_BYTES`].
    ///
    /// Returns the JSON plus whether members were dropped to fit. Bounding is
    /// progressive rather than all-or-nothing: a payload with a thousand
    /// artifacts still delivers its subject, facts, and repair actions.
    pub fn serialize_bounded(&self) -> Option<(String, bool)> {
        let json = serde_json::to_string(self).ok()?;
        if json.len() <= NOTIFY_PAYLOAD_MAX_BYTES {
            return Some((json, false));
        }

        let mut bounded = self.clone();
        bounded.facts.truncate(BOUNDED_FACTS);
        bounded.actions.truncate(BOUNDED_ACTIONS);
        bounded.links.truncate(BOUNDED_LINKS);
        bounded.attachments.truncate(BOUNDED_ATTACHMENTS);
        for fact in &mut bounded.facts {
            fact.value = bound_chars(&fact.value, FIELD_MAX_CHARS);
        }
        let json = serde_json::to_string(&bounded).ok()?;
        if json.len() <= NOTIFY_PAYLOAD_MAX_BYTES {
            return Some((json, true));
        }

        // Last resort: identity and actions only. A consumer that can still
        // answer "what is this and what do I run next?" is strictly better
        // than a dropped payload.
        let minimal = Self {
            schema: bounded.schema,
            kind: bounded.kind,
            subject: bounded.subject,
            facts: Vec::new(),
            actions: bounded.actions,
            links: Vec::new(),
            attachments: Vec::new(),
        };
        let json = serde_json::to_string(&minimal).ok()?;
        (json.len() <= NOTIFY_PAYLOAD_MAX_BYTES).then_some((json, true))
    }
}

fn bound_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max.saturating_sub(3)).collect();
    format!("{kept}...")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cook_payload() -> NotifyPayload {
        NotifyPayload::new(
            NotifyEventKind::NeedsAttention,
            NotifySubject::new("agent_task_cook", "cook-abc")
                .with_component("homeboy")
                .with_attempt(2)
                .with_phase("promotion"),
        )
        .with_fact("Status", "failed")
        .with_optional_fact("Stop reason", Some("gate_failed"))
        .with_optional_fact("Ignored", Option::<String>::None)
        .with_optional_fact("Blank", Some("   "))
        .with_action(
            NotifyAction::new("diagnose", "homeboy agent-task diagnose cook-abc")
                .with_kind("repair"),
        )
        .with_link(NotifyLink::new(
            "pull request",
            "https://example.test/pull/1",
        ))
    }

    #[test]
    fn rendered_body_carries_every_structured_member() {
        let body = cook_payload().render_body();
        assert!(body.contains("Component: homeboy"), "{body}");
        assert!(body.contains("Attempt: 2"), "{body}");
        assert!(body.contains("Phase: promotion"), "{body}");
        assert!(body.contains("Status: failed"), "{body}");
        assert!(body.contains("Stop reason: gate_failed"), "{body}");
        assert!(
            body.contains("Next — diagnose: homeboy agent-task diagnose cook-abc"),
            "{body}"
        );
        assert!(body.contains("https://example.test/pull/1"), "{body}");
    }

    #[test]
    fn optional_facts_skip_absent_and_blank_values() {
        let body = cook_payload().render_body();
        assert!(!body.contains("Ignored"), "{body}");
        assert!(!body.contains("Blank"), "{body}");
    }

    #[test]
    fn rendered_body_is_bounded() {
        let payload = NotifyPayload::new(
            NotifyEventKind::Completed,
            NotifySubject::new("run", "run-1"),
        )
        .with_fact("Detail", "x".repeat(NOTIFY_BODY_MAX_CHARS * 2));
        let body = payload.render_body();
        assert!(body.chars().count() <= NOTIFY_BODY_MAX_CHARS);
        assert!(body.ends_with("..."));
    }

    #[test]
    fn payload_round_trips_through_json() {
        let payload = cook_payload();
        let json = serde_json::to_string(&payload).unwrap();
        assert_eq!(
            serde_json::from_str::<NotifyPayload>(&json).unwrap(),
            payload
        );
    }

    #[test]
    fn payload_accepts_unknown_fields_from_a_newer_producer() {
        // A newer homeboy must not break an older structured consumer.
        let value = serde_json::json!({
            "schema": NOTIFICATION_PAYLOAD_SCHEMA,
            "kind": "started",
            "subject": {"kind": "run", "id": "run-1", "unreleased_field": 7},
            "unreleased_member": [1, 2, 3],
        });
        let payload: NotifyPayload = serde_json::from_value(value).unwrap();
        assert_eq!(payload.kind, NotifyEventKind::Started);
        assert_eq!(payload.subject.unwrap().id, "run-1");
    }

    #[test]
    fn payload_defaults_to_completed_for_a_producer_that_omits_kind() {
        let payload: NotifyPayload = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(payload.kind, NotifyEventKind::Completed);
        assert_eq!(payload.schema, NOTIFICATION_PAYLOAD_SCHEMA);
        assert!(payload.is_empty());
    }

    #[test]
    fn oversized_payload_is_bounded_but_keeps_identity_and_actions() {
        let attachments = (0..5_000).map(|index| {
            NotifyAttachment::new(
                format!("artifact-{index}"),
                "artifact",
                format!("file:///tmp/artifact-{index}.png"),
                format!("homeboy runs artifact get run-1 artifact-{index}"),
            )
            .with_group("visual_compare", "diff")
        });
        let payload = NotifyPayload::new(
            NotifyEventKind::Completed,
            NotifySubject::new("run", "run-1"),
        )
        .with_action(NotifyAction::new("show", "homeboy runs show run-1"))
        .with_attachments(attachments);

        let (json, truncated) = payload.serialize_bounded().expect("bounded payload");
        assert!(truncated);
        assert!(json.len() <= NOTIFY_PAYLOAD_MAX_BYTES);
        let parsed: NotifyPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.subject.unwrap().id, "run-1");
        assert_eq!(parsed.actions.len(), 1);
        assert!(parsed.attachments.len() <= BOUNDED_ATTACHMENTS);
    }

    #[test]
    fn small_payload_is_not_reported_as_truncated() {
        let (json, truncated) = cook_payload().serialize_bounded().expect("payload");
        assert!(!truncated);
        assert!(json.contains("cook-abc"));
    }

    #[test]
    fn event_kind_terminality_is_explicit() {
        assert!(NotifyEventKind::Completed.is_terminal());
        assert!(NotifyEventKind::NeedsAttention.is_terminal());
        assert!(!NotifyEventKind::Queued.is_terminal());
        assert!(!NotifyEventKind::Started.is_terminal());
        assert!(!NotifyEventKind::Progress.is_terminal());
        assert_eq!(NotifyEventKind::NeedsAttention.as_str(), "needs_attention");
    }

    #[test]
    fn attachment_group_metadata_survives_serialization() {
        let attachment = NotifyAttachment::new(
            "visual_source",
            "artifact",
            "file:///tmp/source.png",
            "homeboy runs artifact get run-1 visual_source",
        )
        .with_group("visual_compare_27", "source")
        .with_media_type("image/png");
        let json = serde_json::to_string(&attachment).unwrap();
        let parsed: NotifyAttachment = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.group_id.as_deref(), Some("visual_compare_27"));
        assert_eq!(parsed.role.as_deref(), Some("source"));
        assert_eq!(parsed.media_type.as_deref(), Some("image/png"));
    }
}
