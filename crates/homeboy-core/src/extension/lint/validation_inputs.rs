//! Resolve release lint dependencies once, before any lint route executes.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use homeboy_core::git;
use homeboy_core::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(super) const PATHS_ENV: &str = "HOMEBOY_RELEASE_VALIDATION_DEPENDENCY_PATHS";

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum UnpinnedPolicy {
    #[default]
    ConfiguredCheckout,
    UpstreamDefault,
    FailClosed,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Policy {
    unpinned: UnpinnedPolicy,
    allow_stale: bool,
}

#[derive(Clone, Deserialize, PartialEq, Eq)]
struct Dependency {
    #[serde(alias = "slug", alias = "plugin_slug")]
    id: Option<String>,
    #[serde(alias = "local_path", alias = "dependency")]
    path: Option<String>,
    source: Option<String>,
    #[serde(alias = "repo")]
    repository: Option<String>,
    #[serde(alias = "ref", alias = "version")]
    revision: Option<String>,
    #[serde(
        alias = "subdir",
        alias = "subdirectory",
        alias = "packagePath",
        alias = "plugin_path",
        alias = "pluginPath"
    )]
    package_path: Option<PathBuf>,
    #[serde(alias = "type")]
    source_type: Option<String>,
}

impl Dependency {
    fn parse(value: &Value) -> Result<Self> {
        if let Some(token) = value.as_str().filter(|value| !value.trim().is_empty()) {
            return Ok(Self {
                id: None,
                path: Some(token.trim().into()),
                source: None,
                repository: None,
                revision: None,
                package_path: None,
                source_type: None,
            });
        }
        let mut dependency: Self =
            serde_json::from_value(value.clone()).map_err(|err| invalid(err.to_string()))?;
        if matches!(
            dependency.source_type.as_deref(),
            Some("git" | "github" | "github-repo")
        ) {
            if dependency.repository.is_none() {
                dependency.repository = dependency.source.take();
            }
        } else if dependency.path.is_none() {
            dependency.path = dependency.source.take();
        }
        Ok(dependency)
    }

    fn id(&self) -> Result<&str> {
        self.id
            .as_deref()
            .or_else(|| self.path.as_deref().and_then(basename))
            .or_else(|| self.repository.as_deref().and_then(basename))
            .filter(|id| {
                !id.is_empty() && *id != "." && *id != ".." && !id.contains(['/', '\n', '\r'])
            })
            .ok_or_else(|| invalid("Dependency needs a component ID or checkout path"))
    }
}

fn basename(path: &str) -> Option<&str> {
    Path::new(path)
        .file_name()?
        .to_str()
        .map(|s| s.trim_end_matches(".git"))
}

#[derive(Serialize)]
struct Identity {
    component_id: String,
    repository: String,
    requested_identity: String,
    resolved_sha: String,
    source_mode: &'static str,
    freshness: &'static str,
    stale_override: bool,
}

#[derive(Default)]
pub(super) struct ValidationInputs {
    paths: Vec<PathBuf>,
    identities: Vec<Identity>,
    seen: BTreeSet<String>,
    scratch: Option<tempfile::TempDir>,
}

impl ValidationInputs {
    pub(super) fn resolve(source: &Path, settings: &Value, wordpress: bool) -> Result<Self> {
        let manifest_path = source.join("homeboy.json");
        let manifest = match fs::read_to_string(&manifest_path) {
            Ok(content) => serde_json::from_str::<Value>(&content)
                .map_err(|err| invalid(format!("{}: {err}", manifest_path.display())))?,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Value::Null,
            Err(err) => return Err(io_error(err)),
        };
        let policy = settings
            .get("validation_dependency_policy")
            .or_else(|| manifest.get("validation_dependency_policy"))
            .map(|value| serde_json::from_value::<Policy>(value.clone()))
            .transpose()
            .map_err(|err| invalid(format!("Invalid validation_dependency_policy: {err}")))?
            .unwrap_or_default();
        let mut inputs = Self::default();
        let declarations = settings
            .get("validation_dependencies")
            .filter(|value| !value.is_null())
            .or_else(|| {
                settings
                    .get("depends_on")
                    .filter(|value| wordpress && !value.is_null())
            })
            .or_else(|| manifest.get("validation_dependencies"));
        let mut declared = BTreeMap::new();
        let mut order = Vec::new();
        if let Some(value) = declarations.filter(|value| !value.is_null()) {
            let entries = match value {
                Value::Array(entries) => entries.clone(),
                Value::String(csv) if wordpress => csv
                    .split(',')
                    .map(str::trim)
                    .filter(|token| !token.is_empty())
                    .map(|token| Value::String(token.into()))
                    .collect(),
                _ => return Err(invalid("Expected validation_dependencies to be an array")),
            };
            for entry in &entries {
                let dep = Dependency::parse(entry)?;
                let id = dep.id()?.to_owned();
                if let Some(previous) = declared.insert(id.clone(), dep) {
                    if previous != declared[&id] {
                        return Err(invalid(format!("Conflicting declarations for `{id}`")));
                    }
                } else {
                    order.push(id);
                }
            }
        }
        for id in order {
            inputs.visit(source, declared[&id].clone(), &policy, wordpress, &declared)?;
        }
        if wordpress {
            for id in required_plugins(source)? {
                inputs.visit(
                    source,
                    Dependency::parse(&Value::String(id))?,
                    &policy,
                    true,
                    &declared,
                )?;
            }
        }
        Ok(inputs)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    pub(super) fn paths_env(&self) -> String {
        self.paths
            .iter()
            .map(|path| path.to_string_lossy())
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub(super) fn evidence(&self) -> Value {
        serde_json::json!(self.identities)
    }

    fn visit(
        &mut self,
        source: &Path,
        dep: Dependency,
        policy: &Policy,
        wordpress: bool,
        declared: &BTreeMap<String, Dependency>,
    ) -> Result<()> {
        let id = dep.id()?.to_owned();
        if !self.seen.insert(id.clone()) {
            return Ok(());
        }
        let (path, identity) = self.materialize(source, &dep, &id, policy).map_err(|err| {
            let mut error = invalid(format!("Validation dependency `{id}`: {}", err.message));
            error.details["dependency_error"] = err.details;
            error.details["validation_dependencies"] = self.evidence();
            error
        })?;
        self.identities.push(identity);
        if wordpress {
            for required in required_plugins(&path)? {
                let dependency = match declared.get(&required) {
                    Some(dependency) => dependency.clone(),
                    None => Dependency::parse(&Value::String(required))?,
                };
                self.visit(source, dependency, policy, true, declared)?;
            }
        }
        self.paths.push(path);
        Ok(())
    }

    fn materialize(
        &mut self,
        source: &Path,
        dep: &Dependency,
        id: &str,
        policy: &Policy,
    ) -> Result<(PathBuf, Identity)> {
        if dep.source_type.as_deref().is_some_and(|kind| {
            !matches!(kind, "github" | "git" | "github-repo" | "local" | "slug")
        }) {
            return Err(invalid(
                "Release lint requires a Git repository; declare a repository and revision instead",
            ));
        }
        if dep.repository.is_some() && dep.path.is_some() {
            return Err(invalid("Declare either path or repository; use package_path for a member within a repository"));
        }
        let local = if dep.repository.is_none() {
            let token = dep.path.as_deref().unwrap_or(id);
            let direct = source.join(token);
            Some(if direct.is_dir() {
                direct.canonicalize().map_err(io_error)?
            } else {
                homeboy_core::hygiene::resolve_validation_dependency_path(source, token)?
            })
        } else {
            None
        };
        let root = local
            .as_ref()
            .map(|path| {
                git::repo_root(path).ok_or_else(|| {
                    invalid("Checkout has no Git commit; use a committed local repository")
                })
            })
            .transpose()?;
        let origin = root.as_ref().and_then(|root| {
            git::output_allow_empty(root, &["remote", "get-url", "origin"]).map(|url| {
                if !url.contains(':') && !Path::new(&url).is_absolute() {
                    root.join(url).display().to_string()
                } else {
                    url
                }
            })
        });
        let explicit_remote = dep.repository.as_ref().map(|repository| {
            if matches!(dep.source_type.as_deref(), Some("github" | "github-repo"))
                && !repository.contains(':')
                && !Path::new(repository).is_absolute()
            {
                format!(
                    "https://github.com/{}.git",
                    repository.trim_end_matches(".git")
                )
            } else if !repository.contains(':') && !Path::new(repository).is_absolute() {
                source.join(repository).display().to_string()
            } else {
                repository.clone()
            }
        });
        let remote = explicit_remote.as_deref().or(origin.as_deref());
        let repository = remote
            .map(git::redact_push_url)
            .or_else(|| root.as_ref().map(|path| path.display().to_string()));
        let same_repo = explicit_remote.is_none()
            && root
                .as_ref()
                .is_some_and(|root| git::repo_root(source).as_ref() == Some(root));
        let package = dep
            .package_path
            .as_deref()
            .or_else(|| {
                local
                    .as_ref()
                    .zip(root.as_ref())
                    .and_then(|(path, root)| path.strip_prefix(root).ok())
            })
            .unwrap_or(Path::new(""));
        if package
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return Err(invalid(
                "package_path must be relative to the dependency repository",
            ));
        }
        let (requested, sha, mode, freshness, stale, checkout) = if same_repo
            && dep.revision.is_none()
        {
            let root = root.as_deref().expect("same repository has a root");
            let sha = revision(root, "HEAD")?;
            (
                "release_source".to_owned(),
                sha,
                "release_source",
                "same_repository",
                false,
                self.clone_into(id, None, Some(root))?,
            )
        } else if let Some(pin) = dep.revision.as_deref() {
            // A named ref belongs to the declared upstream when there is one.
            // A full commit ID can be satisfied from local objects without network access.
            let immutable =
                matches!(pin.len(), 40 | 64) && pin.bytes().all(|byte| byte.is_ascii_hexdigit());
            let clone_remote = if immutable {
                explicit_remote.as_deref()
            } else {
                remote
            };
            let checkout = self.clone_into(id, clone_remote, root.as_deref())?;
            let sha = match resolve_pin(&checkout, pin) {
                Ok(sha) => sha,
                Err(error) => {
                    let remote = remote.ok_or(error)?;
                    run_git(&checkout, &["fetch", "--quiet", "--", remote, pin])?;
                    revision(&checkout, "FETCH_HEAD")?
                }
            };
            if immutable && !sha.eq_ignore_ascii_case(pin) {
                return Err(invalid("Resolved commit does not match the declared SHA"));
            }
            (pin.to_owned(), sha, "pinned", "pinned", false, checkout)
        } else {
            match policy.unpinned {
                UnpinnedPolicy::FailClosed => return Err(invalid(
                    "Unpinned dependency; declare a revision or choose a different unpinned policy",
                )),
                UnpinnedPolicy::UpstreamDefault => {
                    let checkout = self.clone_into(id, Some(remote.ok_or_else(|| invalid("No remote default branch; pin the local commit or use configured_checkout"))?), None)?;
                    let sha = revision(&checkout, "HEAD")?;
                    (
                        "upstream_default".into(),
                        sha,
                        "upstream_default",
                        "current",
                        false,
                        checkout,
                    )
                }
                UnpinnedPolicy::ConfiguredCheckout => {
                    let root = root.as_deref().ok_or_else(|| {
                        invalid("No configured checkout; pin a revision or use upstream_default")
                    })?;
                    let sha = revision(root, "HEAD")?;
                    let (freshness, stale) = checkout_freshness(root, &sha, policy.allow_stale)
                        .map_err(|mut error| {
                            error.details["identity"] = serde_json::json!(Identity {
                                component_id: id.into(),
                                repository: repository.clone().unwrap_or_default(),
                                requested_identity: "configured_checkout".into(),
                                resolved_sha: sha.clone(),
                                source_mode: "configured_checkout",
                                freshness: if error.message.contains("stale validation dependency")
                                {
                                    "stale"
                                } else {
                                    "unverified"
                                },
                                stale_override: false,
                            });
                            error
                        })?;
                    let checkout = self.clone_into(id, None, Some(root))?;
                    (
                        "configured_checkout".into(),
                        sha,
                        "configured_checkout",
                        freshness,
                        stale,
                        checkout,
                    )
                }
            }
        };
        run_git(&checkout, &["checkout", "--quiet", "--detach", &sha])?;
        let mut path = checkout.join(package);
        if local.is_none() && dep.package_path.is_none() {
            for container in ["plugins", "themes", "packages"] {
                let member = checkout.join(container).join(id);
                if member.is_dir() {
                    path = member;
                    break;
                }
            }
        }
        let path = path.canonicalize().map_err(io_error)?;
        if !path.is_dir()
            || path.to_string_lossy().contains(['\n', '\r'])
            || !path.starts_with(checkout.canonicalize().map_err(io_error)?)
        {
            return Err(invalid(
                "Dependency package path is not a directory within its checkout",
            ));
        }
        let identity = Identity {
            component_id: id.into(),
            repository: repository.unwrap_or_default(),
            requested_identity: requested,
            resolved_sha: sha,
            source_mode: mode,
            freshness,
            stale_override: stale,
        };
        Ok((path, identity))
    }

    fn clone_into(
        &mut self,
        id: &str,
        remote: Option<&str>,
        local: Option<&Path>,
    ) -> Result<PathBuf> {
        if self.scratch.is_none() {
            self.scratch = Some(tempfile::tempdir().map_err(io_error)?);
        }
        let path = self
            .scratch
            .as_ref()
            .expect("scratch initialized")
            .path()
            .join(id);
        let parent = path.parent().expect("scratch parent");
        let url = remote
            .map(str::to_owned)
            .or_else(|| local.map(|path| path.display().to_string()))
            .ok_or_else(|| invalid("No repository available"))?;
        run_git(
            parent,
            &["clone", "--quiet", "--no-checkout", "--", &url, id],
        )?;
        Ok(path)
    }
}

fn checkout_freshness(root: &Path, sha: &str, allow_stale: bool) -> Result<(&'static str, bool)> {
    if !run_git(root, &["status", "--porcelain", "--untracked-files=all"])?.is_empty() {
        return Err(invalid(
            "Configured checkout is dirty; commit its changes or pin a committed revision",
        ));
    }
    let remotes = run_git(root, &["remote"])?;
    if remotes.is_empty() {
        return Ok(("local_only", false));
    }
    let branch = git::output_allow_empty(root, &["symbolic-ref", "--short", "HEAD"]);
    let branch_config = |key: &str| {
        branch.as_ref().and_then(|branch| {
            git::output_allow_empty(
                root,
                &["config", "--get", &format!("branch.{branch}.{key}")],
            )
        })
    };
    let remote = branch_config("remote").unwrap_or_else(|| "origin".into());
    let reference = branch_config("merge").unwrap_or_else(|| "HEAD".into());
    let upstream = run_git(
        root,
        &["ls-remote", "--exit-code", "--", &remote, &reference],
    ).map_err(|err| invalid(format!("Cannot verify checkout against {remote} {reference}; restore upstream access or pin a revision: {}", err.message)))?;
    if upstream
        .lines()
        .any(|line| line.split_once('\t') == Some((sha, reference.as_str())))
    {
        return Ok(("current", false));
    }
    if allow_stale {
        return Ok(("stale", true));
    }
    Err(invalid(format!("stale validation dependency: configured HEAD {sha} differs from {remote} {reference}; update the checkout, pin a revision, or explicitly set validation_dependency_policy.allow_stale")))
}

fn resolve_pin(path: &Path, pin: &str) -> Result<String> {
    if pin.starts_with("refs/heads/") {
        return revision(
            path,
            &pin.replacen("refs/heads/", "refs/remotes/origin/", 1),
        );
    }
    revision(path, pin).or_else(|_| revision(path, &format!("refs/remotes/origin/{pin}")))
}

fn revision(path: &Path, reference: &str) -> Result<String> {
    run_git(
        path,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{reference}^{{commit}}"),
        ],
    )
}

fn run_git(path: &Path, args: &[&str]) -> Result<String> {
    git::run_git_with_env_timeout(
        path,
        args,
        "resolve release validation dependency",
        &[("GIT_TERMINAL_PROMPT".into(), "0".into())],
        Duration::from_secs(60),
    )
    .map(|stdout| stdout.trim_end().to_owned())
    .map_err(|mut error| {
        error.message = homeboy_core::redaction::redact_string(&error.message);
        error.details = homeboy_core::redaction::redact_json(&error.details);
        error
    })
}

fn required_plugins(path: &Path) -> Result<Vec<String>> {
    let mut files = fs::read_dir(path)
        .map_err(io_error)?
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(io_error)?;
    files.sort_by_key(|entry| entry.file_name());
    for file in files {
        let path = file.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("php")
            || !file.file_type().map_err(io_error)?.is_file()
        {
            continue;
        }
        let bytes = fs::read(path).map_err(io_error)?;
        let header = String::from_utf8_lossy(&bytes[..bytes.len().min(8192)]);
        if !header.lines().any(|line| line.contains("Plugin Name:")) {
            continue;
        }
        return Ok(header
            .lines()
            .find_map(|line| line.split_once("Requires Plugins:").map(|(_, slugs)| slugs))
            .into_iter()
            .flat_map(|slugs| slugs.trim_end_matches("*/").split(','))
            .map(str::trim)
            .filter(|slug| !slug.is_empty())
            .map(str::to_owned)
            .collect());
    }
    Ok(Vec::new())
}

fn invalid(message: impl Into<String>) -> Error {
    Error::validation_invalid_argument("validation_dependencies", message, None, None)
}

fn io_error(err: std::io::Error) -> Error {
    Error::internal_io(
        err.to_string(),
        Some("prepare release validation dependencies".into()),
    )
}
