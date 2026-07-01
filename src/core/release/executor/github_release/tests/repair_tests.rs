//! Tests for repair-command construction, enterprise proxy/env, and `gh` CLI env.

use std::collections::HashMap;

use crate::core::component::{GithubConfig, GithubHostConfig};
use crate::core::deploy::release_download::GitHubRepo;

use super::super::{gh_auth_failure_message, github_cli_env, github_release_repair_commands};
use super::test_repo;

#[test]
fn repair_commands_reuse_persisted_exact_body_when_available() {
    // With the persisted exact body, recovery must `--notes-file` THAT file
    // and must NOT regenerate notes (which could diverge) — issue #3508.
    let repair = github_release_repair_commands(
        "v0.10.6",
        &test_repo(),
        &GithubConfig::default(),
        &["build/studio-web.zip".to_string()],
        None,
        Some("build/v0.10.6-release-notes.md"),
    );

    assert!(repair.exact_body_available);
    assert_eq!(repair.notes_file, "build/v0.10.6-release-notes.md");
    assert!(repair
        .create_command
        .contains("--notes-file build/v0.10.6-release-notes.md"));
    // The generate step must not re-run note generation against the API.
    assert!(!repair.generate_notes_command.contains("generate-notes"));
    assert!(repair.notes_guidance.contains("byte-for-byte"));
}

#[test]
fn repair_commands_regenerate_notes_when_no_persisted_body() {
    // Without a persisted body (gh missing / unauth), recovery falls back to
    // regenerating notes into a fresh file.
    let repair = github_release_repair_commands(
        "v0.10.6",
        &test_repo(),
        &GithubConfig::default(),
        &["build/studio-web.zip".to_string()],
        None,
        None,
    );

    assert!(!repair.exact_body_available);
    assert!(repair.generate_notes_command.contains("generate-notes"));
    assert!(repair
        .create_command
        .contains("--notes-file build/v0.10.6-release-notes.md"));
}

#[test]
fn github_cli_env_sets_enterprise_host_and_proxy() {
    let github = GitHubRepo {
        host: "github.enterprise.test".to_string(),
        owner: "owner".to_string(),
        repo: "repo".to_string(),
    };
    let config = GithubConfig {
        hosts: HashMap::from([(
            "github.enterprise.test".to_string(),
            GithubHostConfig {
                proxy: Some("socks5://127.0.0.1:9999".to_string()),
                env: HashMap::new(),
            },
        )]),
    };

    let env = github_cli_env(&github, &config);

    assert_eq!(
        env,
        vec![
            ("GH_HOST".to_string(), "github.enterprise.test".to_string()),
            (
                "HTTPS_PROXY".to_string(),
                "socks5://127.0.0.1:9999".to_string()
            ),
        ]
    );
}

#[test]
fn repair_commands_include_configured_enterprise_proxy() {
    let github = GitHubRepo {
        host: "github.enterprise.test".to_string(),
        owner: "owner".to_string(),
        repo: "repo".to_string(),
    };
    let config = GithubConfig {
        hosts: HashMap::from([(
            "github.enterprise.test".to_string(),
            GithubHostConfig {
                proxy: Some("https://proxy.example.test:8443".to_string()),
                env: HashMap::new(),
            },
        )]),
    };

    let repair = github_release_repair_commands(
        "v1.2.3",
        &github,
        &config,
        &[],
        None,
        Some("build/v1.2.3-release-notes.md"),
    );

    assert!(repair.create_command.starts_with(
        "GH_HOST=github.enterprise.test HTTPS_PROXY=https://proxy.example.test:8443 gh release create v1.2.3"
    ));
    assert_eq!(
        repair.view_command,
        "GH_HOST=github.enterprise.test HTTPS_PROXY=https://proxy.example.test:8443 gh release view v1.2.3 -R owner/repo"
    );
    assert!(repair
        .env_hint
        .as_deref()
        .unwrap_or_default()
        .contains("Proxy environment is included"));
}

#[test]
fn repair_commands_include_configured_enterprise_proxy_env() {
    let github = GitHubRepo {
        host: "github.enterprise.test".to_string(),
        owner: "owner".to_string(),
        repo: "repo".to_string(),
    };
    let config = GithubConfig {
        hosts: HashMap::from([(
            "github.enterprise.test".to_string(),
            GithubHostConfig {
                proxy: None,
                env: HashMap::from([
                    (
                        "HTTP_PROXY".to_string(),
                        "http://proxy.example.test:8080".to_string(),
                    ),
                    (
                        "ALL_PROXY".to_string(),
                        "socks5://127.0.0.1:8080".to_string(),
                    ),
                ]),
            },
        )]),
    };

    let repair = github_release_repair_commands(
        "v1.2.3",
        &github,
        &config,
        &[],
        None,
        Some("build/v1.2.3-release-notes.md"),
    );

    assert!(repair
        .create_command
        .contains("GH_HOST=github.enterprise.test"));
    assert!(repair
        .create_command
        .contains("HTTP_PROXY=http://proxy.example.test:8080"));
    assert!(repair
        .create_command
        .contains("ALL_PROXY=socks5://127.0.0.1:8080"));
    assert!(repair.create_command.contains("gh release create v1.2.3"));
    let env_hint = repair.env_hint.as_deref().unwrap_or_default();
    assert!(env_hint.contains("HTTP_PROXY"));
    assert!(env_hint.contains("ALL_PROXY"));
}

#[test]
fn enterprise_auth_failure_mentions_proxy_keyring_context() {
    let github = GitHubRepo {
        host: "github.enterprise.test".to_string(),
        owner: "owner".to_string(),
        repo: "repo".to_string(),
    };
    let config = GithubConfig {
        hosts: HashMap::from([(
            "github.enterprise.test".to_string(),
            GithubHostConfig {
                proxy: Some("https://proxy.example.test:8443".to_string()),
                env: HashMap::new(),
            },
        )]),
    };
    let repair = github_release_repair_commands("v1.2.3", &github, &config, &[], None, None);

    let message = gh_auth_failure_message(&github, &repair);

    assert!(message.contains("gh auth status --hostname github.enterprise.test"));
    assert!(message.contains("Authenticate this host"));
    assert!(message.contains("proxy/keyring environment"));
}

#[test]
fn github_cli_env_allows_explicit_host_env_override() {
    let github = GitHubRepo {
        host: "github.enterprise.test".to_string(),
        owner: "owner".to_string(),
        repo: "repo".to_string(),
    };
    let config = GithubConfig {
        hosts: HashMap::from([(
            "github.enterprise.test".to_string(),
            GithubHostConfig {
                proxy: Some("socks5://127.0.0.1:9999".to_string()),
                env: HashMap::from([(
                    "HTTPS_PROXY".to_string(),
                    "https://proxy.example.test:8443".to_string(),
                )]),
            },
        )]),
    };

    let env = github_cli_env(&github, &config);

    assert!(env.contains(&("GH_HOST".to_string(), "github.enterprise.test".to_string())));
    assert!(env.contains(&(
        "HTTPS_PROXY".to_string(),
        "https://proxy.example.test:8443".to_string()
    )));
    assert!(!env.contains(&(
        "HTTPS_PROXY".to_string(),
        "socks5://127.0.0.1:9999".to_string()
    )));
}
