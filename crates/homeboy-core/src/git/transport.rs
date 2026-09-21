use std::path::Path;
use std::process::Command;

use crate::component::GithubConfig;

use super::gh_client::github_cli_env;
use super::primitives_query::{output_optional, remote_url};

/// Process-scoped Git transport for a remote host.
///
/// Reuses [`crate::component::GithubHostConfig`] proxy and env values. Settings
/// are applied only to the child Git process (and its inherited helpers) so
/// persistent Git config and stored remote URLs stay unchanged. Routing is
/// keyed by hostname: unrelated hosts do not receive another host's rewrite,
/// proxy, or credential-helper policy. Credential secrets stay helper-owned
/// and are never copied into command diagnostics.
pub fn git_transport_env(host: &str, config: &GithubConfig) -> Vec<(String, String)> {
    strip_gh_host(github_cli_env(host, config))
}

/// Git transport for a remote URL, or empty when the URL has no hostname.
pub fn git_transport_env_for_remote(
    remote_url: Option<&str>,
    config: &GithubConfig,
) -> Vec<(String, String)> {
    remote_url
        .and_then(remote_host)
        .map(|host| git_transport_env(&host, config))
        .unwrap_or_default()
}

/// Git transport for a repository's resolved default remote.
pub fn git_transport_env_for_repo(git_root: &Path, config: &GithubConfig) -> Vec<(String, String)> {
    git_transport_env_for_remote(default_remote_url(git_root).as_deref(), config)
}

pub fn git_transport_env_for_command(
    root: &Path,
    args: &[&str],
    config: &GithubConfig,
) -> Vec<(String, String)> {
    command_remote_host(root, args)
        .map(|host| git_transport_env(&host, config))
        .unwrap_or_default()
}

/// Hostname of an HTTP(S) or SSH remote URL, if one can be parsed.
pub fn remote_host(remote: &str) -> Option<String> {
    let remote = remote.trim();
    let normalized;
    let url = if remote.contains("://") {
        remote
    } else {
        // SCP syntax has no slash before its host/path separator. Parse the
        // authority with the same URL primitive used for HTTPS and SSH URLs.
        let end = if let Some(close) = remote.find(']') {
            close + 1
        } else {
            remote.find(':')?
        };
        if remote[..end].contains('/') || remote.as_bytes().get(end) != Some(&b':') {
            return None;
        }
        normalized = format!("ssh://{}/{}", &remote[..end], &remote[end + 1..]);
        &normalized
    };
    let parsed = reqwest::Url::parse(url).ok()?;
    if !matches!(parsed.scheme(), "https" | "http" | "ssh" | "git") {
        return None;
    }
    parsed
        .host_str()
        .map(|host| host.trim_matches(['[', ']']).to_string())
}

pub(crate) fn apply_configured_transport(
    command: &mut Command,
    git_root: &Path,
    args: &[&str],
    explicit: &[(String, String)],
) {
    let env = if git_command_may_contact_remote(args) {
        merge_explicit_git_env(
            command_remote_host(git_root, args)
                .map(|host| git_transport_env(&host, &GithubConfig::default()))
                .unwrap_or_default(),
            explicit,
        )
    } else {
        merge_explicit_git_env(Vec::new(), explicit)
    };
    if env.iter().any(|(key, _)| structured_config_key(key)) {
        let inherited_keys = std::env::vars_os().map(|(key, _)| key);
        let command_keys = command.get_envs().map(|(key, _)| key.to_os_string());
        let keys: Vec<_> = inherited_keys.chain(command_keys).collect();
        for key in keys {
            if structured_config_key(&key.to_string_lossy()) {
                command.env_remove(key);
            }
        }
    }
    for (key, value) in env {
        command.env(key, value);
    }
}

pub(crate) fn git_command_may_contact_remote(args: &[&str]) -> bool {
    let mut iter = args.iter().copied();
    while let Some(arg) = iter.next() {
        match arg {
            "-c" | "-C" => {
                let _ = iter.next();
            }
            arg if arg.starts_with('-') => {}
            "fetch" | "push" | "pull" | "ls-remote" => return true,
            "worktree" => return iter.any(|value| value == "add"),
            _ => return false,
        }
    }
    false
}

pub(crate) fn merge_explicit_git_env(
    mut base: Vec<(String, String)>,
    explicit: &[(String, String)],
) -> Vec<(String, String)> {
    if explicit.iter().any(|(key, _)| structured_config_key(key)) {
        base.retain(|(key, _)| !structured_config_key(key));
        // A partial layer must never borrow a count or slots from another
        // policy (including the parent process).
        if !explicit.iter().any(|(key, _)| key == "GIT_CONFIG_COUNT") {
            base.push(("GIT_CONFIG_COUNT".to_string(), "0".to_string()));
        }
    }
    for (key, value) in explicit {
        base.retain(|(existing, _)| existing != key);
        base.push((key.clone(), value.clone()));
    }
    base
}

pub(crate) fn structured_config_key(key: &str) -> bool {
    key == "GIT_CONFIG_COUNT"
        || key.starts_with("GIT_CONFIG_KEY_")
        || key.starts_with("GIT_CONFIG_VALUE_")
}

fn command_remote_host(root: &Path, args: &[&str]) -> Option<String> {
    let mut args = args.iter().copied();
    let operation = loop {
        let arg = args.next()?;
        match arg {
            "-c" => {
                let setting = args.next()?;
                if setting.starts_with("remote.") || setting.starts_with("url.") {
                    return None;
                }
            }
            "-C" => return None,
            value if value.starts_with('-') => return None,
            value => break value,
        }
    };
    let config = |key: &str| output_optional(root, &["config", "--get", key]);
    let remotes = output_optional(root, &["remote"]).unwrap_or_default();
    let resolve = |name: &str, push: bool| -> Vec<String> {
        let mut query = vec!["remote", "get-url", "--all"];
        if push {
            query.push("--push");
        }
        query.push(name);
        output_optional(root, &query)
            .map(|urls| urls.lines().map(str::to_string).collect())
            .unwrap_or_else(|| vec![name.to_string()])
    };
    let mut urls = Vec::new();
    if operation == "worktree" {
        if !args.any(|arg| arg == "add") {
            return None;
        }
        for remote in remotes.lines() {
            if config(&format!("remote.{remote}.promisor")).as_deref() == Some("true") {
                urls.extend(resolve(remote, false));
            }
        }
    } else {
        let mut target = None;
        while let Some(arg) = args.next() {
            match arg {
                "--all" | "--multiple" => return None,
                "--repo" => {
                    target = args.next();
                    break;
                }
                "--depth" | "--deepen" | "--shallow-since" | "--shallow-exclude"
                | "--upload-pack" | "--receive-pack" | "--exec" | "--filter"
                | "--refmap" | "-o" | "--push-option" | "--server-option" => {
                    args.next()?;
                }
                "--" => {
                    target = args.next();
                    break;
                }
                value if value.starts_with("--repo=") => {
                    target = Some(&value[7..]);
                    break;
                }
                "-q" | "--quiet" | "-v" | "--verbose" | "--prune" | "-p"
                | "--tags" | "--no-tags" | "--force" | "-f" | "--dry-run" | "-n"
                | "--symref" | "--heads" | "--branches" | "--refs" | "--exit-code"
                | "--get-url" | "--atomic" | "--mirror" | "--set-upstream" | "-u"
                | "--delete" | "-d" | "--porcelain" | "--no-recurse-submodules"
                | "--recurse-submodules" | "--ff-only" | "--rebase" | "--no-rebase" => {}
                value if value.starts_with('-') && value.contains('=') => {}
                value if value.starts_with('-') => return None,
                value => {
                    target = Some(value);
                    break;
                }
            }
        }
        let branch = output_optional(root, &["symbolic-ref", "--short", "HEAD"]);
        let push = operation == "push";
        let default = branch.as_ref().and_then(|branch| {
            if push {
                config(&format!("branch.{branch}.pushRemote"))
                    .or_else(|| config("remote.pushDefault"))
                    .or_else(|| config(&format!("branch.{branch}.remote")))
            } else {
                config(&format!("branch.{branch}.remote"))
            }
        }).or_else(|| if push { config("remote.pushDefault") } else { None });
        urls.extend(resolve(target.or(default.as_deref()).unwrap_or("origin"), push));
    }
    // Process-wide proxies/helpers cannot safely represent multiple hosts.
    let mut hosts = urls.iter().map(|url| remote_host(url));
    let host = hosts.next()??;
    hosts.all(|other| other.as_ref() == Some(&host)).then_some(host)
}

fn strip_gh_host(env: Vec<(String, String)>) -> Vec<(String, String)> {
    env.into_iter()
        .filter(|(key, _)| key != "GH_HOST")
        .collect()
}

fn default_remote_url(git_root: &Path) -> Option<String> {
    let remotes = output_optional(git_root, &["remote"]).unwrap_or_default();
    let remotes: Vec<&str> = remotes
        .lines()
        .map(str::trim)
        .filter(|remote| !remote.is_empty())
        .collect();
    let remote = if remotes.iter().any(|candidate| *candidate == "origin") {
        "origin"
    } else if let [only] = remotes.as_slice() {
        *only
    } else {
        "origin"
    };
    remote_url(git_root, remote)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{GithubConfig, GithubHostConfig};
    use crate::defaults::{save_config, HomeboyConfig};
    use crate::test_support::{run_git_command as git, with_isolated_home};
    use std::collections::HashMap;

    #[test]
    fn remote_host_parses_https_and_ssh_urls() {
        for url in [
            "https://user@[::1]:443/path@name/repo.git",
            "ssh://git@[::1]:2222/path@name/repo.git",
            "git@[::1]:path@name/repo.git",
        ] {
            assert_eq!(remote_host(url).as_deref(), Some("::1"));
        }
        assert_eq!(remote_host("/tmp/path@name/repo.git"), None);
        assert_eq!(remote_host("./path@name:repo.git"), None);
        assert_eq!(
            remote_host("https://git.example.test/owner/repo.git"),
            Some("git.example.test".to_string())
        );
        assert_eq!(
            remote_host("https://git.example.test:1/owner/repo.git"),
            Some("git.example.test".to_string())
        );
        assert_eq!(
            remote_host("ssh://git@git.example.test:2222/owner/repo.git"),
            Some("git.example.test".to_string())
        );
        assert_eq!(
            remote_host("git@git.example.test:owner/repo.git"),
            Some("git.example.test".to_string())
        );
    }

    #[test]
    fn actual_remote_pushurl_and_promisor_select_only_their_host() {
        with_isolated_home(|_| {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path();
            git(root, &["init", "-q"]);
            git(root, &["remote", "add", "origin", "https://a.example.test/repo"]);
            git(root, &["remote", "add", "other", "https://b.example.test/repo"]);
            git(root, &["remote", "set-url", "--push", "origin", "ssh://git@b.example.test/repo"]);
            git(root, &["config", "remote.other.promisor", "true"]);
            let config = GithubConfig {
                hosts: HashMap::from([
                    ("a.example.test".to_string(), GithubHostConfig {
                        proxy: Some("https://a-proxy.example.test".to_string()),
                        env: HashMap::from([("GIT_ASKPASS".to_string(), "/a-helper".to_string())]),
                    }),
                    ("b.example.test".to_string(), GithubHostConfig {
                        proxy: Some("https://b-proxy.example.test".to_string()),
                        env: HashMap::new(),
                    }),
                ]),
                ..GithubConfig::default()
            };
            save_config(&HomeboyConfig {
                github_hosts: config.hosts.clone(),
                ..HomeboyConfig::default()
            }).unwrap();
            for args in [
                vec!["fetch", "other"],
                vec!["push", "origin", "HEAD"],
                vec!["ls-remote", "--symref", "https://b.example.test/repo", "HEAD"],
                vec!["worktree", "add", "../checkout"],
            ] {
                let env = git_transport_env_for_command(root, &args, &config);
                assert!(env.contains(&("HTTPS_PROXY".to_string(), "https://b-proxy.example.test".to_string())));
                assert!(!env.iter().any(|(key, value)| key == "GIT_ASKPASS" || value == "https://a-proxy.example.test"));
                let mut command = Command::new("git");
                apply_configured_transport(&mut command, root, &args, &[]);
                let pairs: Vec<_> = command.get_envs()
                    .filter_map(|(key, value)| value.map(|value| (
                        key.to_string_lossy().to_string(),
                        value.to_string_lossy().to_string(),
                    )))
                    .collect();
                assert_eq!(pairs.iter().find(|(key, _)| key == "HTTPS_PROXY").map(|(_, value)| value.as_str()), Some("https://b-proxy.example.test"));
                assert!(!pairs.iter().any(|(key, _)| key == "GIT_ASKPASS"));
            }
            git(root, &["config", "remote.origin.promisor", "true"]);
            assert!(git_transport_env_for_command(root, &["worktree", "add"], &config).is_empty());
            assert_eq!(remote_url(root, "origin").as_deref(), Some("https://a.example.test/repo"));
        });
    }

    #[test]
    fn structured_config_layers_replace_whole_slot_sets() {
        let global = vec![
            ("GIT_CONFIG_COUNT".to_string(), "1".to_string()),
            ("GIT_CONFIG_KEY_0".to_string(), "credential.helper".to_string()),
            ("GIT_CONFIG_VALUE_0".to_string(), "global-helper".to_string()),
        ];
        let partial = vec![("GIT_CONFIG_VALUE_0".to_string(), "component-helper".to_string())];
        let merged = merge_explicit_git_env(global.clone(), &partial);
        assert!(merged.contains(&("GIT_CONFIG_COUNT".to_string(), "0".to_string())));
        assert!(!merged.iter().any(|(key, _)| key == "GIT_CONFIG_KEY_0"));
        let explicit = vec![("GIT_CONFIG_COUNT".to_string(), "0".to_string())];
        assert_eq!(merge_explicit_git_env(global, &explicit), explicit);
        let temp = tempfile::tempdir().unwrap();
        let mut command = Command::new("git");
        command.env("GIT_CONFIG_KEY_0", "credential.helper");
        command.env("GIT_CONFIG_VALUE_0", "inherited-helper");
        apply_configured_transport(&mut command, temp.path(), &["status"], &explicit);
        assert!(command.get_envs().any(|(key, value)| key == "GIT_CONFIG_KEY_0" && value.is_none()));
        assert!(command.get_envs().any(|(key, value)| key == "GIT_CONFIG_VALUE_0" && value.is_none()));
    }

    #[test]
    fn component_and_explicit_config_do_not_borrow_global_slots() {
        with_isolated_home(|_| {
            let host = "git.example.test".to_string();
            save_config(&HomeboyConfig {
                github_hosts: HashMap::from([(host.clone(), GithubHostConfig {
                    proxy: Some("https://global-proxy.example.test".to_string()),
                    env: HashMap::from([
                        ("GIT_CONFIG_COUNT".to_string(), "1".to_string()),
                        ("GIT_CONFIG_KEY_0".to_string(), "credential.helper".to_string()),
                        ("GIT_CONFIG_VALUE_0".to_string(), "global-helper".to_string()),
                    ]),
                })]),
                ..HomeboyConfig::default()
            }).unwrap();
            let component = GithubConfig {
                hosts: HashMap::from([(host.clone(), GithubHostConfig {
                    proxy: Some("https://component-proxy.example.test".to_string()),
                    env: HashMap::from([
                        ("GIT_CONFIG_COUNT".to_string(), "1".to_string()),
                        ("GIT_CONFIG_KEY_0".to_string(), "http.version".to_string()),
                        ("GIT_CONFIG_VALUE_0".to_string(), "HTTP/1.1".to_string()),
                    ]),
                })]),
                ..GithubConfig::default()
            };
            let env = git_transport_env(&host, &component);
            assert!(env.contains(&("HTTPS_PROXY".to_string(), "https://component-proxy.example.test".to_string())));
            assert!(!env.iter().any(|(_, value)| value == "credential.helper" || value == "global-helper"));
            let explicit = merge_explicit_git_env(env, &[
                ("GIT_CONFIG_COUNT".to_string(), "0".to_string()),
                ("HTTPS_PROXY".to_string(), "https://explicit-proxy.example.test".to_string()),
            ]);
            assert!(!explicit.iter().any(|(key, _)| key == "GIT_CONFIG_KEY_0" || key == "GIT_CONFIG_VALUE_0"));
            assert!(explicit.contains(&("HTTPS_PROXY".to_string(), "https://explicit-proxy.example.test".to_string())));
        });
    }

    #[test]
    fn git_command_may_contact_remote_covers_fetch_push_and_worktree_add() {
        assert!(git_command_may_contact_remote(&["fetch", "origin"]));
        assert!(git_command_may_contact_remote(&["push", "origin", "HEAD"]));
        assert!(git_command_may_contact_remote(&["pull"]));
        assert!(git_command_may_contact_remote(&[
            "ls-remote",
            "--symref",
            "origin",
            "HEAD"
        ]));
        assert!(git_command_may_contact_remote(&[
            "worktree",
            "add",
            "-b",
            "fix/topic",
            "../topic",
            "HEAD"
        ]));
        assert!(git_command_may_contact_remote(&[
            "-c",
            "http.https://git.example.test/.extraheader=",
            "push"
        ]));
        assert!(!git_command_may_contact_remote(&[
            "worktree",
            "remove",
            "../topic"
        ]));
        assert!(!git_command_may_contact_remote(&[
            "rev-parse",
            "--show-toplevel"
        ]));
        assert!(!git_command_may_contact_remote(&["config", "--get", "remote.origin.url"]));
    }

    #[test]
    fn explicit_env_overrides_configured_transport_keys() {
        let merged = merge_explicit_git_env(
            vec![
                (
                    "HTTPS_PROXY".to_string(),
                    "https://configured.example.test:8443".to_string(),
                ),
                (
                    "GIT_ASKPASS".to_string(),
                    "/configured/credential-helper".to_string(),
                ),
            ],
            &[(
                "HTTPS_PROXY".to_string(),
                "https://explicit.example.test:9443".to_string(),
            )],
        );

        assert!(merged.contains(&(
            "HTTPS_PROXY".to_string(),
            "https://explicit.example.test:9443".to_string()
        )));
        assert!(merged.contains(&(
            "GIT_ASKPASS".to_string(),
            "/configured/credential-helper".to_string()
        )));
        assert!(!merged.contains(&(
            "HTTPS_PROXY".to_string(),
            "https://configured.example.test:8443".to_string()
        )));
    }

    #[test]
    fn matching_host_reuses_proxy_and_env_without_gh_host() {
        with_isolated_home(|_| {
            let mut hosts = HashMap::new();
            hosts.insert(
                "git.example.test".to_string(),
                GithubHostConfig {
                    proxy: Some("socks5://127.0.0.1:8080".to_string()),
                    env: HashMap::from([
                        (
                            "GIT_ASKPASS".to_string(),
                            "/opt/credential-helper".to_string(),
                        ),
                        ("GIT_CONFIG_COUNT".to_string(), "1".to_string()),
                        (
                            "GIT_CONFIG_KEY_0".to_string(),
                            "url.https://git-proxy.example.test/.insteadOf".to_string(),
                        ),
                        (
                            "GIT_CONFIG_VALUE_0".to_string(),
                            "https://git.example.test/".to_string(),
                        ),
                    ]),
                },
            );
            save_config(&HomeboyConfig {
                github_hosts: hosts,
                ..HomeboyConfig::default()
            })
            .expect("save host transport");

            let env = git_transport_env("git.example.test", &GithubConfig::default());

            assert!(env.contains(&(
                "HTTPS_PROXY".to_string(),
                "socks5://127.0.0.1:8080".to_string()
            )));
            assert!(env.contains(&(
                "GIT_ASKPASS".to_string(),
                "/opt/credential-helper".to_string()
            )));
            assert!(env.contains(&(
                "GIT_CONFIG_KEY_0".to_string(),
                "url.https://git-proxy.example.test/.insteadOf".to_string()
            )));
            assert!(!env.iter().any(|(key, _)| key == "GH_HOST"));
        });
    }

    #[test]
    fn unrelated_host_does_not_receive_another_hosts_route() {
        with_isolated_home(|_| {
            let mut hosts = HashMap::new();
            hosts.insert(
                "git.example.test".to_string(),
                GithubHostConfig {
                    proxy: Some("socks5://127.0.0.1:8080".to_string()),
                    env: HashMap::from([(
                        "GIT_CONFIG_KEY_0".to_string(),
                        "url.https://git-proxy.example.test/.insteadOf".to_string(),
                    )]),
                },
            );
            save_config(&HomeboyConfig {
                github_hosts: hosts,
                ..HomeboyConfig::default()
            })
            .expect("save host transport");

            let env = git_transport_env("other.example.test", &GithubConfig::default());

            assert!(!env.contains(&(
                "HTTPS_PROXY".to_string(),
                "socks5://127.0.0.1:8080".to_string()
            )));
            assert!(!env
                .iter()
                .any(|(_, value)| value.contains("git-proxy.example.test")));
            assert!(!env.iter().any(|(key, _)| key == "GH_HOST"));
        });
    }

    #[test]
    fn repo_transport_follows_the_stored_remote_host() {
        with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("tempdir");
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
            git(
                repo,
                &[
                    "remote",
                    "add",
                    "upstream",
                    "https://other.example.test/acme/repo.git",
                ],
            );

            let mut hosts = HashMap::new();
            hosts.insert(
                "git.example.test".to_string(),
                GithubHostConfig {
                    proxy: Some("https://proxy.example.test:8443".to_string()),
                    env: HashMap::from([(
                        "GIT_ASKPASS".to_string(),
                        "/opt/credential-helper".to_string(),
                    )]),
                },
            );
            hosts.insert(
                "other.example.test".to_string(),
                GithubHostConfig {
                    proxy: Some("https://other-proxy.example.test:8443".to_string()),
                    env: HashMap::new(),
                },
            );
            save_config(&HomeboyConfig {
                github_hosts: hosts,
                ..HomeboyConfig::default()
            })
            .expect("save host transport");

            let origin_env = git_transport_env_for_repo(repo, &GithubConfig::default());
            assert!(origin_env.contains(&(
                "HTTPS_PROXY".to_string(),
                "https://proxy.example.test:8443".to_string()
            )));
            assert!(!origin_env
                .iter()
                .any(|(_, value)| value.contains("other-proxy")));

            git(
                repo,
                &[
                    "remote",
                    "set-url",
                    "origin",
                    "https://other.example.test/acme/repo.git",
                ],
            );
            let other_env = git_transport_env_for_repo(repo, &GithubConfig::default());
            assert!(other_env.contains(&(
                "HTTPS_PROXY".to_string(),
                "https://other-proxy.example.test:8443".to_string()
            )));
            assert!(!other_env.contains(&(
                "HTTPS_PROXY".to_string(),
                "https://proxy.example.test:8443".to_string()
            )));
        });
    }
}
