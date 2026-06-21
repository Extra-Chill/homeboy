//! CI scope resolution.
//!
//! Translates a continuous-integration event into the Homeboy scope that
//! gated commands (audit, lint, test, review, refactor) should operate on.
//! Core already owns the `--changed-since` semantics; this module owns the
//! *event-context → scope* translation so provider-specific automation (the
//! GitHub Actions runner, for example) calls core instead of re-deriving the
//! mapping in shell.
//!
//! The model is split into two layers:
//!
//! - A **generic, provider-agnostic** core: [`ScopeContext`], [`ScopeMode`],
//!   [`ScopeRequest`], [`ResolvedScope`], and [`resolve_scope`]. These know
//!   nothing about GitHub — they only describe "what kind of event is this and
//!   what is its base ref".
//! - A clearly-delineated **GitHub Actions adapter**
//!   ([`GithubActionsContext`]) that reads GitHub event fields/environment and
//!   produces a [`ScopeRequest`].
//!
//! Differential gating exceptions (which commands accept changed-since scope)
//! are typed in [`SCOPED_COMMANDS`] rather than embedded in shell `case`
//! statements.

use serde::Serialize;

use crate::core::error::Result;
use crate::core::git;

/// Commands that accept a `--changed-since` scope. Everything else always runs
/// at full scope. Typed here instead of a shell `case` statement so the
/// exception set is auditable and testable.
pub const SCOPED_COMMANDS: &[&str] = &["audit", "lint", "test", "review", "refactor"];

/// The kind of CI event that triggered the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeContext {
    /// A pull/merge request against a base branch.
    PullRequest,
    /// A push to a branch (not a PR).
    Push,
    /// A scheduled/cron run.
    Cron,
    /// A manual / unknown trigger.
    Manual,
}

impl ScopeContext {
    pub fn as_str(self) -> &'static str {
        match self {
            ScopeContext::PullRequest => "pr",
            ScopeContext::Push => "push",
            ScopeContext::Cron => "cron",
            ScopeContext::Manual => "manual",
        }
    }
}

/// Whether the run should be scoped to changed files or run fully.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeMode {
    /// Only the files changed relative to a base ref.
    Changed,
    /// The whole component.
    Full,
}

impl ScopeMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ScopeMode::Changed => "changed",
            ScopeMode::Full => "full",
        }
    }
}

/// Provider-agnostic description of a CI event, normalized from whatever the
/// provider adapter produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeRequest {
    /// The normalized event context.
    pub context: ScopeContext,
    /// Base ref/SHA for changed-file diffs (PR base). `None` outside PRs or
    /// when the provider could not supply one.
    pub base_ref: Option<String>,
    /// Whether the change set originates from a fork (informational; affects
    /// downstream trust decisions, not the scope math itself).
    pub is_fork: bool,
}

impl ScopeRequest {
    pub fn new(context: ScopeContext) -> Self {
        Self {
            context,
            base_ref: None,
            is_fork: false,
        }
    }

    pub fn with_base_ref(mut self, base_ref: Option<String>) -> Self {
        self.base_ref = base_ref.filter(|r| !r.is_empty());
        self
    }

    pub fn with_fork(mut self, is_fork: bool) -> Self {
        self.is_fork = is_fork;
        self
    }
}

/// A fully resolved scope ready to be turned into command flags.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedScope {
    /// Normalized event context.
    pub context: ScopeContext,
    /// Resolved scope mode (`changed` or `full`).
    pub mode: ScopeMode,
    /// The base ref to diff against when `mode == Changed`. Empty otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_ref: Option<String>,
    /// Whether the change set originates from a fork.
    pub is_fork: bool,
    /// Human-readable note explaining a fallback to full scope, when one
    /// occurred. Lets the caller surface a deterministic warning.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
}

impl ResolvedScope {
    /// A deterministic full-scope result for the given context.
    fn full(context: ScopeContext, is_fork: bool, fallback_reason: Option<String>) -> Self {
        Self {
            context,
            mode: ScopeMode::Full,
            base_ref: None,
            is_fork,
            fallback_reason,
        }
    }

    /// Produce the CLI flags a given base command should receive under this
    /// scope. Returns an empty vec for full scope or for commands that are not
    /// in [`SCOPED_COMMANDS`].
    ///
    /// `command` may be a compound invocation (e.g. `"refactor --from all"`);
    /// only the leading base command is inspected.
    pub fn command_flags(&self, command: &str) -> Vec<String> {
        let base_cmd = command.split_whitespace().next().unwrap_or("");
        match (&self.mode, &self.base_ref) {
            (ScopeMode::Changed, Some(base_ref))
                if SCOPED_COMMANDS.contains(&base_cmd) && !base_ref.is_empty() =>
            {
                vec!["--changed-since".to_string(), base_ref.clone()]
            }
            _ => Vec::new(),
        }
    }
}

/// How merge-base resolution should be performed during [`resolve_scope`].
pub enum MergeBaseResolver<'a> {
    /// Resolve the merge base against a real git checkout at `path`,
    /// deepening shallow CI clones as needed.
    Git { path: &'a str },
    /// Skip git interaction; trust the request's base ref verbatim. Useful for
    /// callers that have already verified ancestry or for tests.
    TrustBaseRef,
}

/// Resolve a normalized [`ScopeRequest`] into a [`ResolvedScope`].
///
/// Rules:
/// - Non-PR contexts (push/cron/manual) always resolve to full scope.
/// - PR contexts with a usable base ref resolve to changed scope; the base
///   ref's merge base with HEAD is verified (and shallow clones deepened) via
///   the [`MergeBaseResolver`].
/// - Any failure to establish a base ref or merge base falls back
///   deterministically to full scope, with a `fallback_reason` recorded.
pub fn resolve_scope(request: &ScopeRequest, resolver: MergeBaseResolver) -> Result<ResolvedScope> {
    if request.context != ScopeContext::PullRequest {
        return Ok(ResolvedScope::full(request.context, request.is_fork, None));
    }

    let Some(base_ref) = request.base_ref.as_ref().filter(|r| !r.is_empty()) else {
        return Ok(ResolvedScope::full(
            request.context,
            request.is_fork,
            Some("pull request event without a base ref — using full scope".to_string()),
        ));
    };

    match resolver {
        MergeBaseResolver::TrustBaseRef => Ok(ResolvedScope {
            context: request.context,
            mode: ScopeMode::Changed,
            base_ref: Some(base_ref.clone()),
            is_fork: request.is_fork,
            fallback_reason: None,
        }),
        MergeBaseResolver::Git { path } => match git::resolve_merge_base(path, base_ref) {
            Ok(_) => Ok(ResolvedScope {
                context: request.context,
                mode: ScopeMode::Changed,
                base_ref: Some(base_ref.clone()),
                is_fork: request.is_fork,
                fallback_reason: None,
            }),
            Err(error) => Ok(ResolvedScope::full(
                request.context,
                request.is_fork,
                Some(format!(
                    "could not resolve merge base for {base_ref} — using full scope ({error})"
                )),
            )),
        },
    }
}

/// GitHub Actions adapter: maps GitHub event context to a [`ScopeRequest`].
///
/// This is the only GitHub-specific surface in the module. Fields mirror the
/// values the GitHub Actions workflow already exposes; [`from_env`] reads them
/// from the standard environment variables so the action can stop deriving the
/// mapping in shell.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GithubActionsContext {
    /// `GITHUB_EVENT_NAME` (e.g. `pull_request`, `push`, `schedule`).
    pub event_name: Option<String>,
    /// PR base commit SHA (`github.event.pull_request.base.sha`).
    pub base_sha: Option<String>,
    /// Full name of the PR head repository (`owner/repo`), for fork detection.
    pub head_repo: Option<String>,
    /// Full name of the base repository (`GITHUB_REPOSITORY`).
    pub base_repo: Option<String>,
}

impl GithubActionsContext {
    /// Read the context from the standard GitHub Actions environment.
    pub fn from_env() -> Self {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        Self {
            event_name: var("GITHUB_EVENT_NAME"),
            base_sha: var("BASE_SHA"),
            head_repo: var("PR_HEAD_REPO"),
            base_repo: var("GITHUB_REPOSITORY"),
        }
    }

    /// Map the GitHub event name to a normalized [`ScopeContext`]. Unknown
    /// events fall back to [`ScopeContext::Manual`] deterministically.
    pub fn context(&self) -> ScopeContext {
        match self.event_name.as_deref() {
            Some("pull_request") | Some("pull_request_target") => ScopeContext::PullRequest,
            Some("push") => ScopeContext::Push,
            Some("schedule") => ScopeContext::Cron,
            // workflow_dispatch and anything else → manual.
            _ => ScopeContext::Manual,
        }
    }

    /// Detect whether the PR originates from a fork. Only meaningful for PR
    /// events; non-PR events are never forks for scoping purposes.
    pub fn is_fork(&self) -> bool {
        if self.context() != ScopeContext::PullRequest {
            return false;
        }
        match (self.head_repo.as_deref(), self.base_repo.as_deref()) {
            (Some(head), Some(base)) => head != base,
            _ => false,
        }
    }

    /// Normalize this GitHub context into a provider-agnostic [`ScopeRequest`].
    pub fn to_request(&self) -> ScopeRequest {
        let context = self.context();
        let base_ref = if context == ScopeContext::PullRequest {
            self.base_sha.clone()
        } else {
            None
        };
        ScopeRequest::new(context)
            .with_base_ref(base_ref)
            .with_fork(self.is_fork())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_pr_events_resolve_to_full_scope() {
        for context in [ScopeContext::Push, ScopeContext::Cron, ScopeContext::Manual] {
            let request = ScopeRequest::new(context).with_base_ref(Some("abc123".to_string()));
            let resolved =
                resolve_scope(&request, MergeBaseResolver::TrustBaseRef).expect("resolve");
            assert_eq!(resolved.mode, ScopeMode::Full);
            assert_eq!(resolved.base_ref, None);
            assert_eq!(resolved.context, context);
        }
    }

    #[test]
    fn pr_with_base_ref_resolves_to_changed_scope() {
        let request =
            ScopeRequest::new(ScopeContext::PullRequest).with_base_ref(Some("abc123".to_string()));
        let resolved = resolve_scope(&request, MergeBaseResolver::TrustBaseRef).expect("resolve");
        assert_eq!(resolved.mode, ScopeMode::Changed);
        assert_eq!(resolved.base_ref.as_deref(), Some("abc123"));
        assert!(resolved.fallback_reason.is_none());
    }

    #[test]
    fn pr_without_base_ref_falls_back_to_full_scope() {
        let request = ScopeRequest::new(ScopeContext::PullRequest);
        let resolved = resolve_scope(&request, MergeBaseResolver::TrustBaseRef).expect("resolve");
        assert_eq!(resolved.mode, ScopeMode::Full);
        assert!(resolved.fallback_reason.is_some());
    }

    #[test]
    fn empty_base_ref_is_treated_as_missing() {
        let request =
            ScopeRequest::new(ScopeContext::PullRequest).with_base_ref(Some(String::new()));
        assert_eq!(request.base_ref, None);
        let resolved = resolve_scope(&request, MergeBaseResolver::TrustBaseRef).expect("resolve");
        assert_eq!(resolved.mode, ScopeMode::Full);
    }

    #[test]
    fn changed_scope_emits_changed_since_flags_only_for_scoped_commands() {
        let resolved = ResolvedScope {
            context: ScopeContext::PullRequest,
            mode: ScopeMode::Changed,
            base_ref: Some("base123".to_string()),
            is_fork: false,
            fallback_reason: None,
        };

        for cmd in SCOPED_COMMANDS {
            assert_eq!(
                resolved.command_flags(cmd),
                vec!["--changed-since".to_string(), "base123".to_string()],
                "command {cmd} should be scoped"
            );
        }

        // Compound invocations only inspect the base command.
        assert_eq!(
            resolved.command_flags("refactor --from all --write"),
            vec!["--changed-since".to_string(), "base123".to_string()]
        );

        // Non-scoped commands get no flags.
        for cmd in ["release", "deploy", "fleet"] {
            assert!(
                resolved.command_flags(cmd).is_empty(),
                "command {cmd} must not be scoped"
            );
        }
    }

    #[test]
    fn full_scope_emits_no_flags() {
        let resolved = ResolvedScope::full(ScopeContext::Push, false, None);
        for cmd in SCOPED_COMMANDS {
            assert!(resolved.command_flags(cmd).is_empty());
        }
    }

    #[test]
    fn github_event_names_map_to_contexts() {
        let cases = [
            ("pull_request", ScopeContext::PullRequest),
            ("pull_request_target", ScopeContext::PullRequest),
            ("push", ScopeContext::Push),
            ("schedule", ScopeContext::Cron),
            ("workflow_dispatch", ScopeContext::Manual),
            ("something_unknown", ScopeContext::Manual),
        ];
        for (event, expected) in cases {
            let ctx = GithubActionsContext {
                event_name: Some(event.to_string()),
                ..Default::default()
            };
            assert_eq!(ctx.context(), expected, "event {event}");
        }
    }

    #[test]
    fn missing_event_name_falls_back_to_manual() {
        let ctx = GithubActionsContext::default();
        assert_eq!(ctx.context(), ScopeContext::Manual);
    }

    #[test]
    fn fork_detection_compares_head_and_base_repo() {
        let same = GithubActionsContext {
            event_name: Some("pull_request".to_string()),
            head_repo: Some("Extra-Chill/homeboy".to_string()),
            base_repo: Some("Extra-Chill/homeboy".to_string()),
            ..Default::default()
        };
        assert!(!same.is_fork());

        let fork = GithubActionsContext {
            event_name: Some("pull_request".to_string()),
            head_repo: Some("contributor/homeboy".to_string()),
            base_repo: Some("Extra-Chill/homeboy".to_string()),
            ..Default::default()
        };
        assert!(fork.is_fork());

        // Fork detection is only meaningful for PR events.
        let push = GithubActionsContext {
            event_name: Some("push".to_string()),
            head_repo: Some("contributor/homeboy".to_string()),
            base_repo: Some("Extra-Chill/homeboy".to_string()),
            ..Default::default()
        };
        assert!(!push.is_fork());
    }

    #[test]
    fn github_pr_context_normalizes_to_changed_request() {
        let ctx = GithubActionsContext {
            event_name: Some("pull_request".to_string()),
            base_sha: Some("deadbeef".to_string()),
            head_repo: Some("contributor/homeboy".to_string()),
            base_repo: Some("Extra-Chill/homeboy".to_string()),
        };
        let request = ctx.to_request();
        assert_eq!(request.context, ScopeContext::PullRequest);
        assert_eq!(request.base_ref.as_deref(), Some("deadbeef"));
        assert!(request.is_fork);

        let resolved = resolve_scope(&request, MergeBaseResolver::TrustBaseRef).expect("resolve");
        assert_eq!(resolved.mode, ScopeMode::Changed);
        assert_eq!(resolved.base_ref.as_deref(), Some("deadbeef"));
        assert!(resolved.is_fork);
    }

    #[test]
    fn github_push_context_ignores_base_sha() {
        let ctx = GithubActionsContext {
            event_name: Some("push".to_string()),
            base_sha: Some("deadbeef".to_string()),
            ..Default::default()
        };
        let request = ctx.to_request();
        assert_eq!(request.context, ScopeContext::Push);
        assert_eq!(request.base_ref, None);
    }
}
