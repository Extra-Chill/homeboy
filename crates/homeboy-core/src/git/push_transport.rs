//! Shared classification of Git remote-transport failures and effective
//! push-URL resolution.
//!
//! Hosts whose SSH agent is hardware-backed may refuse to sign while the
//! screen is locked, and proxy tunnels can drop; these are recoverable
//! operator conditions, not candidate failures (#10734). The classifier is
//! shared by agent-task promotion's base preflight and agent-task
//! finalization's publication push so both report one taxonomy. It covers
//! *transport authentication* signing only — commit-signature failures
//! (gpg/ssh commit signing) are deliberately kept out and classify as
//! [`GitPushTransportClass::Other`].

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::error::{Error, Result};

use super::transport::{apply_configured_transport, remote_host};

/// Budget applied to a non-interactive push when the caller supplies none.
pub const DEFAULT_NON_INTERACTIVE_PUSH_TIMEOUT: Duration = Duration::from_secs(300);

/// SSH command layered over the user's existing ssh configuration when the
/// caller has not provided one. `-o BatchMode=yes` disables interactive
/// prompts; config files are still read because no `-F` is passed.
pub const NON_INTERACTIVE_PUSH_SSH_COMMAND: &str = "ssh -o BatchMode=yes";

/// Typed outcome of a failed Git remote operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitPushTransportClass {
    /// The SSH agent refused an authentication signing request
    /// (`agent refused operation` / `sign_and_send_pubkey`), e.g. a
    /// user-presence-gated agent while the screen is locked.
    SshAgentRefused,
    /// The SSH agent socket is missing or no connection to the agent could
    /// be opened.
    SshAgentUnavailable,
    /// Credentials were rejected (`Permission denied (publickey)`, HTTPS
    /// authentication failure, 401/403).
    AuthRejected,
    /// The remote could not be reached (proxy, DNS, connection
    /// refused/reset/timeout, unable to access).
    TransportUnreachable,
    /// Anything else, including commit-signature failures — the caller keeps
    /// its generic failure handling for this class.
    Other,
}

impl GitPushTransportClass {
    pub fn label(self) -> &'static str {
        match self {
            Self::SshAgentRefused => "ssh_agent_refused",
            Self::SshAgentUnavailable => "ssh_agent_unavailable",
            Self::AuthRejected => "auth_rejected",
            Self::TransportUnreachable => "transport_unreachable",
            Self::Other => "other",
        }
    }
}

/// Commit-signature evidence (gpg or ssh commit signing). Checked before
/// transport needles so a commit-signing failure is never reported as a push
/// transport outcome, even when the agent figures in its stderr.
const COMMIT_SIGNATURE_NEEDLES: &[&str] = &[
    "gpg failed to sign the data",
    "failed to write commit object",
    "gpg: signing failed",
    "ssh signing failed",
];

const SSH_AGENT_REFUSED_NEEDLES: &[&str] = &["agent refused operation", "sign_and_send_pubkey"];

const SSH_AGENT_UNAVAILABLE_NEEDLES: &[&str] = &[
    "could not open connection to agent",
    "could not open a connection to your authentication agent",
    "error connecting to agent",
];

const AUTH_REJECTED_NEEDLES: &[&str] = &[
    "permission denied (publickey",
    "authentication failed",
    "returned error: 401",
    "returned error: 403",
    "http basic: access denied",
];

const TRANSPORT_UNREACHABLE_NEEDLES: &[&str] = &[
    "unable to access",
    "couldn't connect",
    "could not resolve host",
    "timed out",
    "connection refused",
    "connection reset",
    "connection closed",
    "network is unreachable",
    "no route to host",
    "broken pipe",
    "proxy connect",
    "proxy error",
    "proxy authentication",
    "proxycommand",
    "tls handshake",
    "ssl certificate",
    "ssl connect error",
    "could not read from remote repository",
    "host key verification failed",
];

/// Classify Git push (and base-preflight) stderr into a typed transport
/// outcome.
///
/// Every needle names diagnostic evidence emitted by Git, SSH, or the TLS
/// stack. A bare `proxy` needle must never match here: a remote host that
/// merely contains that word is not transport evidence.
pub fn classify_git_push_failure(stderr: &str) -> GitPushTransportClass {
    let stderr = stderr.to_ascii_lowercase();
    // Precedence matters: the Secure Enclave refusal is followed by a plain
    // `Permission denied (publickey)` line and a 401 also contains `unable to
    // access`, so the specific classes are checked before the generic ones.
    if contains_any(&stderr, COMMIT_SIGNATURE_NEEDLES) {
        return GitPushTransportClass::Other;
    }
    if contains_any(&stderr, SSH_AGENT_REFUSED_NEEDLES) {
        return GitPushTransportClass::SshAgentRefused;
    }
    if contains_any(&stderr, SSH_AGENT_UNAVAILABLE_NEEDLES) {
        return GitPushTransportClass::SshAgentUnavailable;
    }
    if contains_any(&stderr, AUTH_REJECTED_NEEDLES) {
        return GitPushTransportClass::AuthRejected;
    }
    if contains_any(&stderr, TRANSPORT_UNREACHABLE_NEEDLES) {
        return GitPushTransportClass::TransportUnreachable;
    }
    GitPushTransportClass::Other
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

/// Transport family of an effective push URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitPushTransportKind {
    Ssh,
    Https,
    Local,
    Unknown,
}

impl GitPushTransportKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ssh => "ssh",
            Self::Https => "https",
            Self::Local => "local",
            Self::Unknown => "unknown",
        }
    }
}

/// The effective push target of a remote, as the push itself would resolve it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitPushRemote {
    /// Remote name the push targets (e.g. `origin`).
    pub remote: String,
    /// Effective push URL with credentials redacted. Git ignores
    /// `url.<base>.pushInsteadOf` rewrites when `remote.<name>.pushurl` is
    /// set explicitly, so this — not `remote.<name>.url` — is the only
    /// reliable transport signal.
    pub effective_push_url: String,
    pub transport_kind: GitPushTransportKind,
    pub host: Option<String>,
}

/// Resolve the effective push URL of `remote` exactly as a push would: the
/// push URL after `pushInsteadOf` rewrites, with the same process-scoped host
/// transport environment (`github_hosts.<host>.env`) the push receives.
pub fn resolve_effective_push_url(git_root: &Path, remote: &str) -> Result<GitPushRemote> {
    resolve_effective_push_url_with_target(git_root, remote).map(|(resolved, _)| resolved)
}

/// Like [`resolve_effective_push_url`], but also returns the raw push URL,
/// which may carry embedded credentials, for remote-contacting commands that
/// must target the push destination itself (#10734). Diagnostics must use the
/// redacted [`GitPushRemote::effective_push_url`], never the raw URL.
pub(crate) fn resolve_effective_push_url_with_target(
    git_root: &Path,
    remote: &str,
) -> Result<(GitPushRemote, String)> {
    let mut command = Command::new("git");
    command
        .args(["remote", "get-url", "--push", remote])
        .current_dir(git_root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // `remote get-url` is not itself a remote-contacting command, so mirror
    // the push's argument vector to select the same host transport env the
    // real push would apply (including env-configured pushInsteadOf rewrites).
    apply_configured_transport(&mut command, git_root, &["push", remote], &[]);
    let output = command.output().map_err(|error| {
        Error::git_command_failed(format!(
            "git remote get-url --push {remote} failed: {error}"
        ))
    })?;
    if !output.status.success() {
        return Err(Error::git_command_failed(format!(
            "git remote get-url --push {remote} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let url = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    Ok((
        GitPushRemote {
            remote: remote.to_string(),
            transport_kind: push_transport_kind(&url),
            host: remote_host(&url),
            effective_push_url: redact_push_url(&url),
        },
        url,
    ))
}

/// Transport family of a Git URL (ssh, https, local, or unknown).
pub fn push_transport_kind(url: &str) -> GitPushTransportKind {
    let url = url.trim();
    if url.is_empty() {
        return GitPushTransportKind::Unknown;
    }
    if url.starts_with("ssh://") || url.starts_with("git+ssh://") || url.starts_with("ssh+git://") {
        return GitPushTransportKind::Ssh;
    }
    if url.starts_with("https://") || url.starts_with("http://") {
        return GitPushTransportKind::Https;
    }
    if url.starts_with("file://")
        || url.starts_with('/')
        || url.starts_with("./")
        || url.starts_with("../")
    {
        return GitPushTransportKind::Local;
    }
    // SCP syntax (user@host:path) has a colon before its first slash and no
    // scheme; anything else recognizable as host-bearing is treated as SSH.
    if !url.contains("://")
        && url
            .split('/')
            .next()
            .is_some_and(|first| first.contains(':'))
    {
        return GitPushTransportKind::Ssh;
    }
    GitPushTransportKind::Unknown
}

/// Redact an effective push URL: query-string secrets via the shared policy,
/// plus URL userinfo passwords (`https://user:token@host/`), which the
/// query-focused policy does not cover. A bare username (`git@host`) is not a
/// secret and survives.
pub fn redact_push_url(url: &str) -> String {
    let policy = crate::redaction::RedactionPolicy::default();
    redact_url_password(&policy.redact_url(url.trim()))
}

fn redact_url_password(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return url.to_string();
    };
    let rest = &url[scheme_end + 3..];
    // Userinfo can only appear in the authority segment; `path@name` and
    // `host:port` must not be mistaken for credentials.
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let Some(userinfo_end) = authority.rfind('@') else {
        return url.to_string();
    };
    let userinfo = &authority[..userinfo_end];
    let Some(secret_start) = userinfo.find(':') else {
        return url.to_string();
    };
    format!(
        "{}{}:[REDACTED]{}",
        &url[..scheme_end + 3],
        &userinfo[..secret_start],
        &rest[userinfo_end..]
    )
}

/// Environment that makes a Git push non-interactive: no terminal prompts,
/// and SSH `BatchMode=yes` — but only when the caller has not already
/// provided a `GIT_SSH_COMMAND`, so the user's ssh configuration and command
/// are preserved.
pub fn non_interactive_push_env(caller_git_ssh_command: Option<&str>) -> Vec<(String, String)> {
    let mut env = vec![("GIT_TERMINAL_PROMPT".to_string(), "0".to_string())];
    if caller_git_ssh_command.is_none() {
        env.push((
            "GIT_SSH_COMMAND".to_string(),
            NON_INTERACTIVE_PUSH_SSH_COMMAND.to_string(),
        ));
    }
    env
}

/// Resolve the non-interactive push environment for a concrete push,
/// consulting every layer that could already carry an SSH command: the
/// process environment, the configured host transport env
/// (`github_hosts.<host>.env`), and `core.sshCommand`.
pub(crate) fn non_interactive_push_env_for(
    git_root: &Path,
    push_args: &[&str],
) -> Vec<(String, String)> {
    let caller_git_ssh_command = std::env::var("GIT_SSH_COMMAND")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            super::transport::git_transport_env_for_command(
                git_root,
                push_args,
                &crate::component::GithubConfig::default(),
            )
            .iter()
            .any(|(key, _)| key == "GIT_SSH_COMMAND")
            .then_some(String::new())
        })
        .or_else(|| {
            super::primitives_query::output_optional(
                git_root,
                &["config", "--get", "core.sshCommand"],
            )
            .filter(|value| !value.trim().is_empty())
            .map(|_| String::new())
        });
    non_interactive_push_env(caller_git_ssh_command.as_deref())
}

/// A remote branch head read through the effective push transport.
#[derive(Debug, Clone)]
pub struct GitRemoteHeadRead {
    /// The effective push destination the read targeted; credentials
    /// redacted.
    pub resolved: GitPushRemote,
    /// The branch head on the destination. `None` on a successful read means
    /// the branch does not exist there yet (the normal first-publication
    /// state).
    pub head: Option<String>,
    /// Whether the read reached the destination.
    pub success: bool,
    /// Trimmed, redacted stderr for transport classification and diagnostics
    /// when the read failed; empty on success.
    pub stderr: String,
}

/// Read a remote branch head over the effective push transport (#10734).
///
/// Callers checking a branch they are about to publish to must observe the
/// same destination the push would contact — the push URL after
/// `pushInsteadOf` rewrites, not the fetch URL — so this runs `ls-remote`
/// against the resolved effective push URL through the core Git execution
/// path that applies configured host transport, with the same
/// non-interactive, bounded environment as a publication push:
/// `GIT_TERMINAL_PROMPT=0`, SSH `BatchMode=yes` only when the caller has no
/// `GIT_SSH_COMMAND`, and the shared non-interactive push deadline. Callers
/// classify a failed read's `stderr` with [`classify_git_push_failure`].
pub fn remote_branch_head_over_push_transport(
    git_root: &Path,
    remote: &str,
    branch: &str,
) -> Result<GitRemoteHeadRead> {
    let (resolved, push_url) = resolve_effective_push_url_with_target(git_root, remote)?;
    let pattern = format!("refs/heads/{branch}");
    let args = ["ls-remote", "--heads", push_url.as_str(), pattern.as_str()];
    let env = non_interactive_push_env_for(git_root, &args);
    let output = super::primitives::run_git_output_with_env_timeout(
        git_root,
        &args,
        &format!("git ls-remote {remote} {pattern} over the effective push URL"),
        &env,
        DEFAULT_NON_INTERACTIVE_PUSH_TIMEOUT,
    );
    let policy = crate::redaction::RedactionPolicy::default();
    // The raw push URL may carry embedded credentials and Git echoes it in
    // failure output; swap it for the redacted form before any diagnostics.
    let display_push_url = resolved.effective_push_url.clone();
    let redact = move |stderr: &str| {
        policy
            .redact_embedded_urls(&stderr.replace(push_url.as_str(), &display_push_url))
            .trim()
            .to_string()
    };
    match output {
        Ok(output) if output.status.success() => Ok(GitRemoteHeadRead {
            resolved,
            head: String::from_utf8_lossy(&output.stdout)
                .split_whitespace()
                .next()
                .map(str::to_string),
            success: true,
            stderr: String::new(),
        }),
        Ok(output) => Ok(GitRemoteHeadRead {
            resolved,
            head: None,
            success: false,
            stderr: redact(&String::from_utf8_lossy(&output.stderr)),
        }),
        // Spawn failures and deadline expiry keep the push's single
        // stderr-driven failure path: report them as a failed read whose
        // synthesized message classifies like any other transport failure.
        Err(error) => Ok(GitRemoteHeadRead {
            resolved,
            head: None,
            success: false,
            stderr: redact(&error.message),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::defaults::{save_config, HomeboyConfig};
    use crate::test_support::{run_git_command as git, with_isolated_home, EnvVarGuard};
    use std::collections::HashMap;

    #[test]
    fn classifies_agent_refusal_with_its_trailing_permission_denied() {
        assert_eq!(
            classify_git_push_failure(
                "sign_and_send_pubkey: signing failed for ECDSA \"~/.ssh/example.pub\" from agent: agent refused operation\ngit@git.example.test: Permission denied (publickey).\nfatal: Could not read from remote repository."
            ),
            GitPushTransportClass::SshAgentRefused
        );
        assert_eq!(
            classify_git_push_failure("sign_and_send_pubkey: signing failed"),
            GitPushTransportClass::SshAgentRefused
        );
    }

    #[test]
    fn classifies_unavailable_agent_sockets() {
        for stderr in [
            "ssh_add: could not open connection to agent",
            "Could not open a connection to your authentication agent.",
            "error connecting to agent",
        ] {
            assert_eq!(
                classify_git_push_failure(stderr),
                GitPushTransportClass::SshAgentUnavailable,
                "stderr: {stderr}"
            );
        }
    }

    #[test]
    fn classifies_rejected_credentials() {
        for stderr in [
            "git@git.example.test: Permission denied (publickey).\nfatal: Could not read from remote repository.",
            "fatal: Authentication failed for 'https://git.example.test/acme/repo.git'",
            "fatal: unable to access 'https://git.example.test/acme/repo.git/': The requested URL returned error: 401",
            "remote: HTTP Basic: Access denied",
        ] {
            assert_eq!(
                classify_git_push_failure(stderr),
                GitPushTransportClass::AuthRejected,
                "stderr: {stderr}"
            );
        }
    }

    #[test]
    fn classifies_unreachable_transport() {
        for stderr in [
            "fatal: unable to access 'https://git.example.test/acme/repo.git/': Could not resolve host: git.example.test",
            "ssh: connect to host git.example.test port 22: Connection refused",
            "Failed to connect to 127.0.0.1 port 8080: Connection timed out",
            "fatal: unable to access 'https://git.example.test/': Empty reply from server; proxy error",
            "Host key verification failed.",
            // The bounded runner's synthesized deadline message.
            "git push timed out after 300s; terminated child process group.",
        ] {
            assert_eq!(
                classify_git_push_failure(stderr),
                GitPushTransportClass::TransportUnreachable,
                "stderr: {stderr}"
            );
        }
    }

    #[test]
    fn commit_signature_failures_are_not_push_transport() {
        for stderr in [
            "error: gpg failed to sign the data\nfatal: failed to write commit object",
            "gpg: signing failed: No secret key",
            "error: ssh signing failed: agent refused operation",
        ] {
            assert_eq!(
                classify_git_push_failure(stderr),
                GitPushTransportClass::Other,
                "stderr: {stderr}"
            );
        }
    }

    #[test]
    fn unrelated_failures_stay_other() {
        // Parity with promotion's preflight: a hostname is not evidence, and
        // a genuinely invalid ref must stay a non-transport failure even when
        // the remote is named like a proxy.
        for stderr in [
            "fatal: couldn't find remote ref refs/heads/nope on proxy.example.test",
            " ! [remote rejected] HEAD -> feature (pre-receive hook declined)",
        ] {
            assert_eq!(
                classify_git_push_failure(stderr),
                GitPushTransportClass::Other,
                "stderr: {stderr}"
            );
        }
    }

    #[test]
    fn transport_kinds_cover_ssh_https_local_and_unknown() {
        assert_eq!(
            push_transport_kind("ssh://git@git.example.test:2222/acme/repo.git"),
            GitPushTransportKind::Ssh
        );
        assert_eq!(
            push_transport_kind("git@git.example.test:acme/repo.git"),
            GitPushTransportKind::Ssh
        );
        assert_eq!(
            push_transport_kind("https://git.example.test/acme/repo.git"),
            GitPushTransportKind::Https
        );
        assert_eq!(
            push_transport_kind("http://git.example.test/acme/repo.git"),
            GitPushTransportKind::Https
        );
        assert_eq!(
            push_transport_kind("file:///tmp/origin"),
            GitPushTransportKind::Local
        );
        assert_eq!(
            push_transport_kind("/tmp/origin/acme/repo.git"),
            GitPushTransportKind::Local
        );
        assert_eq!(
            push_transport_kind("./relative/origin"),
            GitPushTransportKind::Local
        );
        assert_eq!(
            push_transport_kind("git://git.example.test/acme/repo.git"),
            GitPushTransportKind::Unknown
        );
    }

    #[test]
    fn redact_push_url_hides_userinfo_passwords_and_query_tokens() {
        // The shared policy collapses userinfo credentials entirely; the
        // local password scrub only covers shapes it misses.
        let credentials =
            redact_push_url("https://x-access-token:sekret@git.example.test/acme/repo.git");
        assert!(!credentials.contains("sekret"), "{credentials}");
        assert!(credentials.starts_with("https://"), "{credentials}");
        assert!(
            credentials.contains("git.example.test/acme/repo.git"),
            "{credentials}"
        );
        // Ports and `path@name` segments are not credentials.
        assert_eq!(
            redact_push_url("https://git.example.test:8443/acme/path@name/repo.git"),
            "https://git.example.test:8443/acme/path@name/repo.git"
        );
        // A bare SCP username survives.
        assert_eq!(
            redact_push_url("git@git.example.test:acme/repo.git"),
            "git@git.example.test:acme/repo.git"
        );
        let query = redact_push_url("https://git.example.test/acme/repo.git?token=sekret");
        assert!(
            query.contains("[REDACTED]"),
            "query token redacted: {query}"
        );
        assert!(!query.contains("sekret"));
    }

    #[test]
    fn non_interactive_env_sets_batch_mode_only_without_a_caller_ssh_command() {
        let env = non_interactive_push_env(None);
        assert!(env.contains(&("GIT_TERMINAL_PROMPT".to_string(), "0".to_string())));
        assert!(env.contains(&(
            "GIT_SSH_COMMAND".to_string(),
            "ssh -o BatchMode=yes".to_string()
        )));

        let env = non_interactive_push_env(Some("ssh -J hop.example.test"));
        assert!(env.contains(&("GIT_TERMINAL_PROMPT".to_string(), "0".to_string())));
        assert!(!env.iter().any(|(key, _)| key == "GIT_SSH_COMMAND"));
    }

    #[test]
    fn non_interactive_env_preserves_a_configured_host_ssh_command() {
        with_isolated_home(|_| {
            save_config(&HomeboyConfig {
                github_hosts: HashMap::from([(
                    "git.example.test".to_string(),
                    crate::component::GithubHostConfig {
                        proxy: None,
                        env: HashMap::from([(
                            "GIT_SSH_COMMAND".to_string(),
                            "/opt/presence-gated-ssh".to_string(),
                        )]),
                    },
                )]),
                ..HomeboyConfig::default()
            })
            .expect("save host transport");

            let temp = tempfile::tempdir().expect("repo");
            let repo = temp.path();
            git(repo, &["init", "-q"]);
            git(
                repo,
                &[
                    "remote",
                    "add",
                    "origin",
                    "git@git.example.test:acme/repo.git",
                ],
            );
            let _guard = EnvVarGuard::unset("GIT_SSH_COMMAND");

            let env = non_interactive_push_env_for(repo, &["push", "origin"]);
            assert!(env.contains(&("GIT_TERMINAL_PROMPT".to_string(), "0".to_string())));
            assert!(
                !env.iter().any(|(key, _)| key == "GIT_SSH_COMMAND"),
                "host-configured ssh command must be preserved, got {env:?}"
            );
        });
    }

    #[test]
    fn effective_push_url_honors_explicit_pushurl_over_rewrites() {
        with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("repo");
            let repo = temp.path();
            git(repo, &["init", "-q"]);
            git(
                repo,
                &[
                    "remote",
                    "add",
                    "origin",
                    "https://git.example.test/acme/repo.git",
                ],
            );
            // An explicit pushurl suppresses pushInsteadOf, so the effective
            // push URL stays SSH even while the rewrite below is active.
            git(
                repo,
                &[
                    "remote",
                    "set-url",
                    "--push",
                    "origin",
                    "git@git.example.test:acme/repo.git",
                ],
            );
            let _count = EnvVarGuard::set("GIT_CONFIG_COUNT", "1");
            let _key = EnvVarGuard::set(
                "GIT_CONFIG_KEY_0",
                "url.https://git.example.test/.pushInsteadOf",
            );
            let _value = EnvVarGuard::set("GIT_CONFIG_VALUE_0", "git@git.example.test:");

            let resolved = resolve_effective_push_url(repo, "origin").expect("effective url");
            assert_eq!(resolved.remote, "origin");
            assert_eq!(
                resolved.effective_push_url,
                "git@git.example.test:acme/repo.git"
            );
            assert_eq!(resolved.transport_kind, GitPushTransportKind::Ssh);
            assert_eq!(resolved.host.as_deref(), Some("git.example.test"));
        });
    }

    #[test]
    fn effective_push_url_applies_env_supplied_push_instead_of() {
        with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("repo");
            let repo = temp.path();
            git(repo, &["init", "-q"]);
            git(
                repo,
                &[
                    "remote",
                    "add",
                    "origin",
                    "git@git.example.test:acme/repo.git",
                ],
            );
            let _count = EnvVarGuard::set("GIT_CONFIG_COUNT", "1");
            let _key = EnvVarGuard::set(
                "GIT_CONFIG_KEY_0",
                "url.https://git.example.test/.pushInsteadOf",
            );
            let _value = EnvVarGuard::set("GIT_CONFIG_VALUE_0", "git@git.example.test:");

            let resolved = resolve_effective_push_url(repo, "origin").expect("effective url");
            assert_eq!(
                resolved.effective_push_url,
                "https://git.example.test/acme/repo.git"
            );
            assert_eq!(resolved.transport_kind, GitPushTransportKind::Https);
            assert_eq!(resolved.host.as_deref(), Some("git.example.test"));
        });
    }

    #[test]
    fn effective_push_url_redacts_embedded_credentials() {
        with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("repo");
            let repo = temp.path();
            git(repo, &["init", "-q"]);
            git(
                repo,
                &[
                    "remote",
                    "add",
                    "origin",
                    "https://x-access-token:sekret@git.example.test/acme/repo.git",
                ],
            );

            let resolved = resolve_effective_push_url(repo, "origin").expect("effective url");
            assert!(!resolved.effective_push_url.contains("sekret"));
            assert!(resolved
                .effective_push_url
                .contains("git.example.test/acme/repo.git"));
            assert_eq!(resolved.transport_kind, GitPushTransportKind::Https);
            assert_eq!(resolved.host.as_deref(), Some("git.example.test"));
        });
    }

    #[test]
    fn effective_push_url_fails_closed_for_an_unknown_remote() {
        with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("repo");
            git(temp.path(), &["init", "-q"]);
            assert!(resolve_effective_push_url(temp.path(), "origin").is_err());
        });
    }

    #[test]
    fn remote_head_read_targets_the_push_instead_of_destination() {
        with_isolated_home(|_| {
            let origin = tempfile::tempdir().expect("origin");
            git(origin.path(), &["init", "--bare", "-b", "main"]);
            let temp = tempfile::tempdir().expect("repo");
            let repo = temp.path();
            git(repo, &["init", "-q", "-b", "main"]);
            git(repo, &["config", "user.email", "test@example.com"]);
            git(repo, &["config", "user.name", "Test"]);
            std::fs::write(repo.join("base.txt"), "base").expect("base file");
            git(repo, &["add", "."]);
            git(repo, &["commit", "-m", "base"]);
            git(repo, &["push", origin.path().to_str().unwrap(), "main"]);
            // The fetch URL stays an SSH remote; pushes are routed to the
            // reachable local destination by the env-configured rewrite.
            git(
                repo,
                &[
                    "remote",
                    "add",
                    "origin",
                    "git@git.example.test:acme/repo.git",
                ],
            );
            let head = crate::git::run_git(repo, &["rev-parse", "main"], "git rev-parse main")
                .expect("main head")
                .trim()
                .to_string();
            git(
                repo,
                &[
                    "config",
                    &format!("url.{}.pushInsteadOf", origin.path().display()),
                    "git@git.example.test:acme/repo.git",
                ],
            );

            let read = remote_branch_head_over_push_transport(repo, "origin", "main")
                .expect("read through the effective push URL");

            assert!(read.success);
            assert_eq!(read.head.as_deref(), Some(head.as_str()));
            assert_eq!(read.stderr, "");
            assert_eq!(read.resolved.transport_kind, GitPushTransportKind::Local);
            assert_eq!(
                read.resolved.effective_push_url,
                origin.path().to_str().unwrap()
            );
        });
    }

    #[test]
    fn remote_head_read_failure_keeps_classified_transport_evidence() {
        with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("repo");
            let repo = temp.path();
            git(repo, &["init", "-q"]);
            git(
                repo,
                &[
                    "remote",
                    "add",
                    "origin",
                    "git@git.example.test:acme/repo.git",
                ],
            );
            // A configured core.sshCommand keeps BatchMode from being layered
            // on and fails the transport deterministically, without touching
            // process-global environment other tests may be reading.
            let fixture = tempfile::tempdir().expect("fixture");
            let ssh = fixture.path().join("down-ssh");
            std::fs::write(
                &ssh,
                "#!/bin/sh\nprintf '%s\\n' 'ssh: connect to host git.example.test port 22: Connection refused' >&2\nexit 255\n",
            )
            .expect("ssh fixture");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755))
                    .expect("chmod ssh fixture");
            }
            git(repo, &["config", "core.sshCommand", ssh.to_str().unwrap()]);

            let read = remote_branch_head_over_push_transport(repo, "origin", "feature")
                .expect("read outcome");

            assert!(!read.success);
            assert_eq!(read.head, None);
            assert_eq!(
                classify_git_push_failure(&read.stderr),
                GitPushTransportClass::TransportUnreachable
            );
            assert!(read.stderr.contains("Connection refused"));
            assert_eq!(read.resolved.host.as_deref(), Some("git.example.test"));
            assert_eq!(read.resolved.transport_kind, GitPushTransportKind::Ssh);
        });
    }

    #[test]
    fn remote_head_read_reports_absent_branch_as_a_successful_none() {
        with_isolated_home(|_| {
            let origin = tempfile::tempdir().expect("origin");
            git(origin.path(), &["init", "--bare", "-b", "main"]);
            let temp = tempfile::tempdir().expect("repo");
            let repo = temp.path();
            git(repo, &["init", "-q"]);
            git(
                repo,
                &["remote", "add", "origin", origin.path().to_str().unwrap()],
            );

            let read = remote_branch_head_over_push_transport(repo, "origin", "feature")
                .expect("read outcome");

            assert!(read.success);
            assert_eq!(read.head, None);
        });
    }
}
