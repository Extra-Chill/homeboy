//! Declarative command policy for a provider attempt.
//!
//! `homeboy/agent-tool-policy/v1` already lets an operator decide *where* a
//! named tool executes (runner, control plane, or disabled). That is too coarse
//! for the resource-safety problem: a coding agent's shell tool is one tool
//! name, and the interesting distinction is *which command* it is about to run.
//! On a shared host, "do not compile here, push and let CI compile" is not a
//! style preference — it is the difference between a cook that produces edits
//! and one that burns its whole budget on `rustc` (#11481).
//!
//! Prompt text cannot carry that: a prompt is a request. This module is the
//! declarative constraint. It is additive to the existing tool policy (carried
//! as `AgentToolPolicy::commands`), so it travels through every path the tool
//! policy already travels: the provider request JSON, the
//! `HOMEBOY_AGENT_TOOL_POLICY_JSON` environment handshake, and the
//! `homeboy agent-task tool dispatch` boundary.
//!
//! ## Enforcement boundary (read this before trusting it)
//!
//! Homeboy structurally enforces this policy at the boundary it owns: any tool
//! request that reaches [`crate::agent_tool_control_plane::dispatch_agent_tool_request`]
//! is evaluated and refused before it runs. A provider runtime that executes
//! shell commands *inside its own process* (most coding-agent runtimes do)
//! never crosses that boundary, so for those runtimes this policy is a
//! declaration the runtime is expected to honour plus a hard constraint stated
//! in the prompt — not containment. That gap is deliberate and documented
//! rather than papered over.

use serde::{Deserialize, Serialize};

pub const AGENT_COMMAND_POLICY_SCHEMA: &str = "homeboy/agent-command-policy/v1";

/// Bound on the command text retained in a denial payload. Command text is
/// agent-authored and otherwise unbounded, so it is truncated before it lands
/// in a diagnostic, mirroring the bounded-capture pattern used elsewhere in the
/// agent-task layer.
const DENIAL_COMMAND_CAPTURE_LIMIT_BYTES: usize = 512;

/// Fallback explanation when neither the matched rule nor the policy supplies
/// one. A refusal without a reason wastes the attempt; a refusal that explains
/// the alternative converts it into correct behaviour.
pub const DEFAULT_COMMAND_DENIAL_REASON: &str =
    "this command is denied by the operator's Homeboy command policy for this host";

/// Guidance appended to every refusal so the agent does not spend the rest of
/// its budget trying to route around the policy.
pub const COMMAND_DENIAL_REMEDIATION: &str =
    "Do not retry this command, wrap it, or find an equivalent that evades the pattern. \
Make your edits, commit, and let the configured gate run the heavy commands.";

pub(crate) fn agent_command_policy_schema() -> String {
    AGENT_COMMAND_POLICY_SCHEMA.to_string()
}

/// How the rule sets combine.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentCommandPolicyMode {
    /// Everything is permitted except commands matching a `deny` rule. An
    /// `allow` rule is an explicit exemption and beats a `deny` rule.
    #[default]
    DenyList,
    /// Nothing is permitted unless it matches an `allow` rule, and a `deny`
    /// rule still refuses a command that an `allow` rule would have permitted.
    AllowList,
}

impl AgentCommandPolicyMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DenyList => "deny_list",
            Self::AllowList => "allow_list",
        }
    }
}

/// One command pattern plus the operator's reason for it.
///
/// A pattern is a whitespace-separated token sequence matched against the
/// tokenized command line:
///
/// - `cargo test` matches `timeout 1200 cargo test -q -p homeboy-agents`
///   (the tokens appear contiguously somewhere in the command).
/// - `*` inside a token is a glob: `cargo *` matches any cargo subcommand.
/// - `**` as a whole token matches zero or more tokens: `cargo ** test`
///   matches `cargo --quiet test`.
/// - Shell operators (`;`, `|`, `&`, `(`, `)`, `{`, `}`, newline) are treated
///   as token separators, so `make && cargo build` still matches `cargo build`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentCommandRule {
    pub pattern: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl AgentCommandRule {
    pub fn new(pattern: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
            reason: None,
        }
    }

    pub fn with_reason(pattern: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
            reason: Some(reason.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentCommandPolicy {
    #[serde(default = "agent_command_policy_schema")]
    pub schema: String,
    #[serde(default)]
    pub mode: AgentCommandPolicyMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny: Vec<AgentCommandRule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<AgentCommandRule>,
    /// Policy-wide explanation used when a matched rule carries none. This is
    /// the operator's chance to say *why*, e.g. "this host routes builds to CI".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl Default for AgentCommandPolicy {
    fn default() -> Self {
        Self {
            schema: agent_command_policy_schema(),
            mode: AgentCommandPolicyMode::default(),
            deny: Vec::new(),
            allow: Vec::new(),
            reason: None,
        }
    }
}

impl AgentCommandPolicy {
    /// True when the policy constrains nothing. An allow-list with no `allow`
    /// rules would deny every command, which is never what an operator meant,
    /// so it is treated as unconfigured rather than as a total lockout.
    pub fn is_unconstrained(&self) -> bool {
        self.deny.is_empty() && self.allow.is_empty()
    }

    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }

    /// Decide whether `command` may run.
    pub fn evaluate(&self, command: &str) -> AgentCommandDecision {
        if self.is_unconstrained() {
            return AgentCommandDecision::Allowed;
        }
        let tokens = tokenize_command(command);
        if tokens.is_empty() {
            return AgentCommandDecision::Allowed;
        }

        let allowed = self
            .allow
            .iter()
            .find(|rule| rule_matches(&rule.pattern, &tokens));
        let denied = self
            .deny
            .iter()
            .find(|rule| rule_matches(&rule.pattern, &tokens));

        match self.mode {
            AgentCommandPolicyMode::DenyList => match (allowed, denied) {
                // An explicit exemption beats a deny rule in deny-list mode.
                (Some(_), _) => AgentCommandDecision::Allowed,
                (None, Some(rule)) => self.denial(command, Some(rule)),
                (None, None) => AgentCommandDecision::Allowed,
            },
            AgentCommandPolicyMode::AllowList => match (denied, allowed) {
                // A deny rule beats an allow rule in allow-list mode.
                (Some(rule), _) => self.denial(command, Some(rule)),
                (None, Some(_)) => AgentCommandDecision::Allowed,
                (None, None) => self.denial(command, None),
            },
        }
    }

    fn denial(&self, command: &str, rule: Option<&AgentCommandRule>) -> AgentCommandDecision {
        let reason = rule
            .and_then(|rule| rule.reason.clone())
            .or_else(|| self.reason.clone())
            .unwrap_or_else(|| match self.mode {
                AgentCommandPolicyMode::AllowList if rule.is_none() => {
                    "this host runs an allow-list command policy and this command is not on it"
                        .to_string()
                }
                _ => DEFAULT_COMMAND_DENIAL_REASON.to_string(),
            });
        AgentCommandDecision::Denied(AgentCommandDenial {
            command: bounded_command(command),
            matched_pattern: rule.map(|rule| rule.pattern.clone()),
            mode: self.mode,
            reason,
            remediation: COMMAND_DENIAL_REMEDIATION.to_string(),
        })
    }

    /// Human-facing constraint lines projected into the provider prompt. A
    /// runtime that does not route its shell tool through Homeboy will not be
    /// structurally blocked, so the constraint is also stated where the agent
    /// cannot miss it.
    pub fn prompt_constraints(&self) -> Option<String> {
        if self.is_unconstrained() {
            return None;
        }
        let mut lines = vec![
            "Command policy (declared by the operator and enforced at Homeboy's tool boundary):"
                .to_string(),
        ];
        if !self.deny.is_empty() {
            let patterns = self
                .deny
                .iter()
                .map(|rule| format!("`{}`", rule.pattern))
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!("- Refused command patterns: {patterns}"));
        }
        if !self.allow.is_empty() {
            let patterns = self
                .allow
                .iter()
                .map(|rule| format!("`{}`", rule.pattern))
                .collect::<Vec<_>>()
                .join(", ");
            let label = match self.mode {
                AgentCommandPolicyMode::AllowList => "Only these command patterns may run",
                AgentCommandPolicyMode::DenyList => "Explicitly permitted despite the refusals",
            };
            lines.push(format!("- {label}: {patterns}"));
        }
        if let Some(reason) = &self.reason {
            lines.push(format!("- Reason: {reason}"));
        }
        for rule in self.deny.iter().chain(self.allow.iter()) {
            if let Some(reason) = &rule.reason {
                lines.push(format!("- `{}`: {reason}", rule.pattern));
            }
        }
        lines.push(format!("- {COMMAND_DENIAL_REMEDIATION}"));
        Some(lines.join("\n"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentCommandDecision {
    Allowed,
    Denied(AgentCommandDenial),
}

impl AgentCommandDecision {
    pub fn denial(&self) -> Option<&AgentCommandDenial> {
        match self {
            Self::Denied(denial) => Some(denial),
            Self::Allowed => None,
        }
    }
}

/// A structured refusal. Carried into the tool result diagnostics and the tool
/// dispatch evidence so "the agent tried to compile and was refused" is visible
/// after the run instead of being invisible or indistinguishable from a
/// silently failing command.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentCommandDenial {
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched_pattern: Option<String>,
    pub mode: AgentCommandPolicyMode,
    pub reason: String,
    pub remediation: String,
}

impl AgentCommandDenial {
    pub fn message(&self) -> String {
        format!(
            "command '{}' is refused by the Homeboy agent command policy: {} {}",
            self.command, self.reason, self.remediation
        )
    }
}

fn bounded_command(command: &str) -> String {
    let trimmed = command.trim();
    if trimmed.len() <= DENIAL_COMMAND_CAPTURE_LIMIT_BYTES {
        return trimmed.to_string();
    }
    let mut end = DENIAL_COMMAND_CAPTURE_LIMIT_BYTES;
    while end > 0 && !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &trimmed[..end])
}

/// Split a command line into normalized comparison tokens. Shell control
/// characters are separators outside quotes, while quoted whitespace remains
/// part of its assignment or argument token.
pub(crate) fn tokenize_command(command: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quote = None;
    let mut escaped = false;

    for character in command.chars() {
        if escaped {
            token.push(character);
            escaped = false;
            continue;
        }
        if character == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if matches!(character, '\'' | '"') {
            if quote == Some(character) {
                quote = None;
                continue;
            }
            if quote.is_none() {
                quote = Some(character);
                continue;
            }
        }
        if quote.is_none()
            && (character.is_whitespace()
                || matches!(character, ';' | '|' | '&' | '(' | ')' | '{' | '}'))
        {
            if !token.is_empty() {
                tokens.push(std::mem::take(&mut token));
            }
            continue;
        }
        token.push(character);
    }
    if escaped {
        token.push('\\');
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    tokens
}

fn rule_matches(pattern: &str, tokens: &[String]) -> bool {
    let pattern_tokens = tokenize_command(pattern);
    if pattern_tokens.is_empty() {
        return false;
    }
    (0..tokens.len()).any(|start| match_at(&pattern_tokens, &tokens[start..]))
}

fn match_at(pattern: &[String], tokens: &[String]) -> bool {
    match pattern.split_first() {
        None => true,
        Some((first, rest)) if first == "**" => {
            (0..=tokens.len()).any(|skip| match_at(rest, &tokens[skip..]))
        }
        Some((first, rest)) => match tokens.split_first() {
            Some((token, remaining)) if glob_match(first, token) => match_at(rest, remaining),
            _ => false,
        },
    }
}

/// Glob match supporting `*` as "any run of characters" within a single token.
fn glob_match(pattern: &str, value: &str) -> bool {
    if !pattern.contains('*') {
        return pattern == value;
    }
    let mut segments = pattern.split('*');
    let Some(first) = segments.next() else {
        return true;
    };
    if !value.starts_with(first) {
        return false;
    }
    let mut cursor = first.len();
    let segments: Vec<&str> = segments.collect();
    let Some((last, middle)) = segments.split_last() else {
        return true;
    };
    for segment in middle {
        if segment.is_empty() {
            continue;
        }
        match value[cursor..].find(segment) {
            Some(index) => cursor += index + segment.len(),
            None => return false,
        }
    }
    if last.is_empty() {
        return true;
    }
    value.len() >= cursor + last.len() && value[cursor..].ends_with(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deny(patterns: &[&str]) -> AgentCommandPolicy {
        AgentCommandPolicy {
            deny: patterns
                .iter()
                .map(|pattern| AgentCommandRule::new(*pattern))
                .collect(),
            ..AgentCommandPolicy::default()
        }
    }

    #[test]
    fn empty_policy_allows_everything() {
        let policy = AgentCommandPolicy::default();

        assert_eq!(
            policy.evaluate("cargo test --workspace"),
            AgentCommandDecision::Allowed
        );
        assert!(policy.is_unconstrained());
        assert!(policy.prompt_constraints().is_none());
    }

    /// The exact command that ignored two escalating prompt prohibitions and
    /// burned the cook budget in #11481.
    #[test]
    fn deny_rule_refuses_the_command_that_escaped_prompt_text() {
        let policy = deny(&["cargo test", "cargo build"]);

        let decision = policy.evaluate(
            "timeout 1200 cargo test -q -j3 -p homeboy-agents --lib agent_task_scheduler::tests",
        );

        let denial = decision.denial().expect("denied");
        assert_eq!(denial.matched_pattern.as_deref(), Some("cargo test"));
        assert_eq!(denial.mode, AgentCommandPolicyMode::DenyList);
        assert!(denial.message().contains("refused"));
    }

    #[test]
    fn deny_rule_survives_shell_operator_chaining() {
        let policy = deny(&["cargo build"]);

        assert!(policy
            .evaluate("make deps && cargo build --release")
            .denial()
            .is_some());
        assert!(policy.evaluate("true;cargo build").denial().is_some());
    }

    #[test]
    fn glob_pattern_covers_every_subcommand() {
        let policy = deny(&["cargo *"]);

        assert!(policy
            .evaluate("cargo clippy --workspace")
            .denial()
            .is_some());
        assert!(policy.evaluate("cargo check").denial().is_some());
        assert_eq!(policy.evaluate("git status"), AgentCommandDecision::Allowed);
    }

    #[test]
    fn double_star_token_spans_intervening_flags() {
        let policy = deny(&["cargo ** test"]);

        assert!(policy
            .evaluate("cargo --quiet --offline test")
            .denial()
            .is_some());
        assert!(policy.evaluate("cargo test").denial().is_some());
    }

    #[test]
    fn unrelated_command_is_allowed_by_a_deny_list() {
        let policy = deny(&["cargo test"]);

        assert_eq!(
            policy.evaluate("cargo fmt --check"),
            AgentCommandDecision::Allowed
        );
    }

    #[test]
    fn allow_rule_is_an_exemption_in_deny_list_mode() {
        let policy = AgentCommandPolicy {
            deny: vec![AgentCommandRule::new("cargo *")],
            allow: vec![AgentCommandRule::new("cargo fmt")],
            ..AgentCommandPolicy::default()
        };

        assert_eq!(
            policy.evaluate("cargo fmt --check"),
            AgentCommandDecision::Allowed
        );
        assert!(policy.evaluate("cargo build").denial().is_some());
    }

    #[test]
    fn allow_list_mode_refuses_anything_not_listed() {
        let policy = AgentCommandPolicy {
            mode: AgentCommandPolicyMode::AllowList,
            allow: vec![
                AgentCommandRule::new("cargo fmt"),
                AgentCommandRule::new("git *"),
            ],
            ..AgentCommandPolicy::default()
        };

        assert_eq!(policy.evaluate("git status"), AgentCommandDecision::Allowed);
        let denial = policy
            .evaluate("cargo build")
            .denial()
            .cloned()
            .expect("denied");
        assert_eq!(denial.matched_pattern, None);
        assert_eq!(denial.mode, AgentCommandPolicyMode::AllowList);
        assert!(denial.reason.contains("allow-list"));
    }

    #[test]
    fn deny_rule_beats_allow_rule_in_allow_list_mode() {
        let policy = AgentCommandPolicy {
            mode: AgentCommandPolicyMode::AllowList,
            allow: vec![AgentCommandRule::new("cargo *")],
            deny: vec![AgentCommandRule::new("cargo build")],
            ..AgentCommandPolicy::default()
        };

        assert_eq!(policy.evaluate("cargo fmt"), AgentCommandDecision::Allowed);
        assert!(policy.evaluate("cargo build").denial().is_some());
    }

    #[test]
    fn rule_reason_beats_policy_reason_which_beats_the_default() {
        let policy = AgentCommandPolicy {
            deny: vec![
                AgentCommandRule::with_reason("cargo build", "builds go to CI on this host"),
                AgentCommandRule::new("cargo test"),
            ],
            reason: Some("15Gi shared VPS".to_string()),
            ..AgentCommandPolicy::default()
        };

        assert_eq!(
            policy
                .evaluate("cargo build")
                .denial()
                .expect("denied")
                .reason,
            "builds go to CI on this host"
        );
        assert_eq!(
            policy
                .evaluate("cargo test")
                .denial()
                .expect("denied")
                .reason,
            "15Gi shared VPS"
        );

        let bare = deny(&["cargo test"]);
        assert_eq!(
            bare.evaluate("cargo test").denial().expect("denied").reason,
            DEFAULT_COMMAND_DENIAL_REASON
        );
    }

    #[test]
    fn every_denial_carries_actionable_remediation() {
        let denial = deny(&["cargo test"])
            .evaluate("cargo test")
            .denial()
            .cloned()
            .expect("denied");

        assert_eq!(denial.remediation, COMMAND_DENIAL_REMEDIATION);
        assert!(denial.remediation.contains("let the configured gate run"));
    }

    #[test]
    fn denial_command_text_is_bounded() {
        let command = format!("cargo test {}", "x".repeat(4096));

        let denial = deny(&["cargo test"])
            .evaluate(&command)
            .denial()
            .cloned()
            .expect("denied");

        assert!(denial.command.len() <= DENIAL_COMMAND_CAPTURE_LIMIT_BYTES + 4);
        assert!(denial.command.ends_with('…'));
    }

    #[test]
    fn prompt_constraints_state_patterns_reason_and_remediation() {
        let policy = AgentCommandPolicy {
            deny: vec![AgentCommandRule::new("cargo test")],
            reason: Some("this host routes builds to CI".to_string()),
            ..AgentCommandPolicy::default()
        };

        let constraints = policy.prompt_constraints().expect("constraints");

        assert!(constraints.contains("`cargo test`"));
        assert!(constraints.contains("this host routes builds to CI"));
        assert!(constraints.contains(COMMAND_DENIAL_REMEDIATION));
    }

    #[test]
    fn policy_round_trips_through_json_with_schema_default() {
        let raw = serde_json::json!({
            "deny": [{ "pattern": "cargo test", "reason": "CI compiles" }],
            "reason": "shared host"
        });

        let policy: AgentCommandPolicy = serde_json::from_value(raw).expect("policy");

        assert_eq!(policy.schema, AGENT_COMMAND_POLICY_SCHEMA);
        assert_eq!(policy.mode, AgentCommandPolicyMode::DenyList);
        assert!(policy.evaluate("cargo test").denial().is_some());

        let encoded = serde_json::to_value(&policy).expect("encode");
        let decoded: AgentCommandPolicy = serde_json::from_value(encoded).expect("decode");
        assert_eq!(decoded, policy);
    }

    #[test]
    fn glob_match_handles_prefix_infix_and_suffix() {
        assert!(glob_match("cargo*", "cargo"));
        assert!(glob_match("*test", "integration-test"));
        assert!(glob_match("a*c", "abc"));
        assert!(!glob_match("a*c", "abd"));
        assert!(glob_match("*", "anything"));
    }

    #[test]
    fn tokenizer_splits_on_shell_operators() {
        assert_eq!(
            tokenize_command("a && b | c;d (e)"),
            vec!["a", "b", "c", "d", "e"]
        );
    }

    #[test]
    fn tokenizer_preserves_quoted_assignment_values() {
        assert_eq!(
            tokenize_command(r#"RUSTFLAGS="-D warnings" cargo test"#),
            vec!["RUSTFLAGS=-D warnings", "cargo", "test"]
        );
    }
}
