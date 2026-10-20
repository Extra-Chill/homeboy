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
    #[cfg(test)]
    let url = TEST_BASE
        .with(|base| base.borrow().clone())
        .unwrap_or_else(|| url.to_string());
    let result = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .user_agent("homeboy")
        .build()
        .and_then(|client| client.get(url).send())
        .and_then(|response| response.error_for_status())
        .and_then(|response| response.json());
    result.map_err(|error| {
        Error::internal_io(error.to_string(), Some("resolve stack release".into()))
    })
}

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
            if releases.len() < 100 {
                break;
            }
            // GitHub pages are traversed even when none of their tags are semver.
            let next = url
                .split("&page=")
                .nth(1)
                .and_then(|n| n.parse::<usize>().ok())
                .unwrap_or(1)
                + 1;
            url = format!("https://api.github.com/repos/{repo}/releases?per_page=100&page={next}");
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
