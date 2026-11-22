//! HTTP release resolution for dependency stack edges.
use super::dependency_graph::STACK_OUTDATED_RESOLVE_TIMEOUT;
use crate::component::DependencyStackEdgeSource;
use crate::git::RemoteReleaseCoordinate;
use crate::{Error, Result};

#[cfg(test)]
thread_local! {
    pub(super) static TEST_BASE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

pub(super) fn http_json(url: &str) -> Result<serde_json::Value> {
    http_json_with_timeout(url, STACK_OUTDATED_RESOLVE_TIMEOUT)
}

fn http_json_with_timeout(url: &str, timeout: std::time::Duration) -> Result<serde_json::Value> {
    let github = reqwest::Url::parse(url)
        .map_err(release_io_error)?
        .host_str()
        == Some("api.github.com");
    #[cfg(test)]
    let url = TEST_BASE
        .with(|base| base.borrow().clone())
        .unwrap_or_else(|| url.to_string());
    let headers = release_headers(url.as_ref(), |name| std::env::var(name).ok())?;
    let result = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .user_agent("homeboy")
        .build()
        .and_then(|client| client.get(url).headers(headers).send());
    let response = result.map_err(release_io_error)?;
    let status = response.status();
    if status.is_client_error() || status.is_server_error() {
        let message = if github {
            response
                .json::<serde_json::Value>()
                .ok()
                .and_then(|body| body["message"].as_str().map(str::to_owned))
        } else {
            None
        };
        return Err(release_io_error(format!(
            "HTTP {}: {}",
            status.as_u16(),
            message
                .as_deref()
                .unwrap_or_else(|| status.canonical_reason().unwrap_or("request failed"))
        )));
    }
    response.json().map_err(release_io_error)
}

// Keep the actionable cause in the public message: callers often display only it.
fn release_io_error(error: impl std::fmt::Display) -> Error {
    let message = format!("resolve stack release: {error}");
    let mut error = Error::internal_io(&message, Some("resolve stack release".into()));
    error.message = message;
    error
}

fn release_headers(
    url: &str,
    mut env: impl FnMut(&str) -> Option<String>,
) -> Result<reqwest::header::HeaderMap> {
    use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION};
    let mut headers = HeaderMap::new();
    let url = reqwest::Url::parse(url).map_err(release_io_error)?;
    if url.host_str() == Some("api.github.com") {
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(
            "X-GitHub-Api-Version",
            HeaderValue::from_static("2022-11-28"),
        );
        if let Some(token) = ["GH_TOKEN", "GITHUB_TOKEN"]
            .into_iter()
            .find_map(|name| env(name).filter(|value| !value.trim().is_empty()))
        {
            let mut value = HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|_| release_io_error("invalid GitHub token header"))?;
            value.set_sensitive(true);
            headers.insert(AUTHORIZATION, value);
        }
    }
    Ok(headers)
}

#[cfg(test)]
mod tests {
    use super::release_headers;
    use reqwest::header::{ACCEPT, AUTHORIZATION};

    #[test]
    fn github_headers_select_first_nonempty_token() {
        for (gh, github, expected) in [
            (Some("primary"), Some("fallback"), Some("primary")),
            (Some(""), Some("fallback"), Some("fallback")),
            (None, Some("fallback"), Some("fallback")),
            (Some(" "), None, None),
            (None, None, None),
        ] {
            let headers =
                release_headers("https://api.github.com/repos/org/tool/releases", |name| {
                    match name {
                        "GH_TOKEN" => gh,
                        "GITHUB_TOKEN" => github,
                        _ => panic!("unexpected env lookup"),
                    }
                    .map(str::to_owned)
                })
                .unwrap();
            assert_eq!(
                headers
                    .get(AUTHORIZATION)
                    .map(|v| v.to_str().unwrap().to_owned()),
                expected.map(|v| format!("Bearer {v}"))
            );
            assert_eq!(headers[ACCEPT], "application/vnd.github+json");
            assert_eq!(headers["X-GitHub-Api-Version"], "2022-11-28");
        }
    }

    #[test]
    fn other_hosts_never_receive_github_headers_or_read_tokens() {
        for url in [
            "https://registry.npmjs.org/tool",
            "https://api.github.com.evil.example/releases",
            "https://api.github.com@evil.example/releases",
            "http://127.0.0.1:1234/releases",
        ] {
            assert!(
                release_headers(url, |_| panic!("must not read credentials"))
                    .unwrap()
                    .is_empty()
            );
        }
    }
}

/// GitHub's list-releases endpoint stops at 1000 results (10 pages of 100).
const GITHUB_RELEASE_LIST_MAX_PAGES: usize = 10;

pub(super) fn resolve_stack_source(
    source: &DependencyStackEdgeSource,
) -> Result<Option<RemoteReleaseCoordinate>> {
    let kinds = usize::from(!source.repo.is_empty())
        + usize::from(source.github_release.is_some())
        + usize::from(source.npm.is_some());
    if kinds != 1 {
        return Err(Error::internal_io(
            "declare exactly one stack release source",
            None,
        ));
    }
    if !source.repo.is_empty() {
        return crate::git::get_latest_remote_release_with_prefix(
            &source.repo,
            Some(&source.prefix),
            STACK_OUTDATED_RESOLVE_TIMEOUT,
        );
    }
    let (version, tag) = if let Some(repo) = &source.github_release {
        let mut url = format!("https://api.github.com/repos/{repo}/releases?per_page=100");
        let mut candidates = Vec::new();
        let mut page = 1;
        let deadline = std::time::Instant::now() + STACK_OUTDATED_RESOLVE_TIMEOUT;
        loop {
            let remaining = deadline
                .checked_duration_since(std::time::Instant::now())
                .filter(|remaining| !remaining.is_zero())
                .ok_or_else(|| Error::internal_io("stack release resolution timed out", None))?;
            let value = http_json_with_timeout(&url, remaining)?;
            let releases = value
                .as_array()
                .ok_or_else(|| Error::internal_io("invalid GitHub releases response", None))?;
            for release in releases {
                if release["draft"].as_bool() != Some(false)
                    || release["prerelease"].as_bool() != Some(false)
                {
                    continue;
                }
                let Some(tag) = release["tag_name"].as_str() else {
                    continue;
                };
                let Ok(version) = semver::Version::parse(tag.strip_prefix('v').unwrap_or(tag))
                else {
                    continue;
                };
                if version.pre.is_empty() {
                    candidates.push((version, tag.to_string()));
                }
            }
            // GitHub serves only the first 1000 listed releases and answers
            // page 11 with 422. The list is newest first, so that window
            // always holds the newest release; stop at its end.
            if releases.len() < 100 || page >= GITHUB_RELEASE_LIST_MAX_PAGES {
                break;
            }
            // GitHub pages are traversed even when none of their tags are semver.
            page += 1;
            url = format!("https://api.github.com/repos/{repo}/releases?per_page=100&page={page}");
        }
        let Some((version, tag)) = candidates.into_iter().max_by(|a, b| a.0.cmp(&b.0)) else {
            return Ok(None);
        };
        (version.to_string(), tag)
    } else {
        let package = source.npm.as_deref().unwrap();
        let encoded = package.replace('/', "%2F");
        let value = http_json(&format!("https://registry.npmjs.org/{encoded}"))?;
        let Some(latest) = value["dist-tags"]["latest"].as_str() else {
            return Ok(None);
        };
        let Some(version) = super::dependency_graph::parse_dependency_version(latest) else {
            return Ok(None);
        };
        if !version.pre.is_empty() {
            return Ok(None);
        }
        (version.to_string(), String::new())
    };
    Ok(Some(RemoteReleaseCoordinate {
        version,
        tag,
        commit: String::new(),
    }))
}

pub(super) fn version_file_path(
    root: &std::path::Path,
    relative: &str,
) -> std::result::Result<std::path::PathBuf, String> {
    use std::path::Component;
    let path = std::path::Path::new(relative);
    if relative.is_empty()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err("version-file path must be relative and cannot traverse parents".into());
    }
    let target = root
        .join(path)
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let root = root.canonicalize().map_err(|error| error.to_string())?;
    if !target.starts_with(root) {
        return Err("version-file escapes component root".into());
    }
    Ok(target)
}
