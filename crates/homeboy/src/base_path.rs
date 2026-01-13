use crate::{Error, Result};

pub fn resolve_optional_base_path(base_path: Option<&str>) -> Option<&str> {
    base_path.and_then(|value| (!value.trim().is_empty()).then_some(value.trim()))
}

pub fn resolve_required_base_path<'a>(
    base_path: Option<&'a str>,
    context_label: &str,
) -> Result<&'a str> {
    resolve_optional_base_path(base_path)
        .ok_or_else(|| Error::config_missing_key("basePath", Some(context_label.to_string())))
}

pub fn join_remote_path(base_path: Option<&str>, path: &str) -> Result<String> {
    let path = path.trim();

    if path.is_empty() {
        return Err(Error::validation_invalid_argument(
            "path",
            "Path cannot be empty",
            None,
            None,
        ));
    }

    if path.starts_with('/') {
        return Ok(path.to_string());
    }

    let Some(base) = resolve_optional_base_path(base_path) else {
        return Err(Error::config_missing_key("basePath", None));
    };

    if base.ends_with('/') {
        Ok(format!("{}{}", base, path))
    } else {
        Ok(format!("{}/{}", base, path))
    }
}

pub fn join_remote_child(base_path: Option<&str>, dir: &str, child: &str) -> Result<String> {
    let dir_path = join_remote_path(base_path, dir)?;
    let child = child.trim();

    if child.is_empty() {
        return Err(Error::validation_invalid_argument(
            "child",
            "Child path cannot be empty",
            None,
            None,
        ));
    }

    if dir_path.ends_with('/') {
        Ok(format!("{}{}", dir_path, child))
    } else {
        Ok(format!("{}/{}", dir_path, child))
    }
}

pub fn remote_dirname(path: &str) -> Result<String> {
    let path = path.trim();

    if path.is_empty() {
        return Err(Error::validation_invalid_argument(
            "path",
            "Path cannot be empty",
            None,
            None,
        ));
    }

    let without_trailing = path.trim_end_matches('/');

    let Some((parent, _)) = without_trailing.rsplit_once('/') else {
        return Ok("/".to_string());
    };

    if parent.is_empty() {
        Ok("/".to_string())
    } else {
        Ok(parent.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_remote_path_allows_absolute_paths_without_base() {
        assert_eq!(
            join_remote_path(None, "/var/log/syslog").unwrap(),
            "/var/log/syslog"
        );
    }

    #[test]
    fn join_remote_path_rejects_relative_paths_without_base() {
        assert!(join_remote_path(None, "config.json").is_err());
    }

    #[test]
    fn join_remote_path_joins_relative_paths() {
        assert_eq!(
            join_remote_path(Some("/var/www/site"), "config.json").unwrap(),
            "/var/www/site/config.json"
        );

        assert_eq!(
            join_remote_path(Some("/var/www/site/"), "config.json").unwrap(),
            "/var/www/site/config.json"
        );
    }

    #[test]
    fn join_remote_child_appends_child() {
        assert_eq!(
            join_remote_child(Some("/var/www/site"), "logs", "error.log").unwrap(),
            "/var/www/site/logs/error.log"
        );

        assert_eq!(
            join_remote_child(Some("/var/www/site"), "/var/log", "syslog").unwrap(),
            "/var/log/syslog"
        );
    }

    #[test]
    fn remote_dirname_gets_parent_dir() {
        assert_eq!(
            remote_dirname("/var/www/site/file.zip").unwrap(),
            "/var/www/site"
        );
        assert_eq!(remote_dirname("/file.zip").unwrap(), "/");
        assert_eq!(remote_dirname("/var/www/site/").unwrap(), "/var/www");
    }

    #[test]
    fn resolve_optional_base_path_trims_and_rejects_empty() {
        assert_eq!(
            resolve_optional_base_path(Some(" /var/www ")),
            Some("/var/www")
        );
        assert_eq!(resolve_optional_base_path(Some("   ")), None);
        assert_eq!(resolve_optional_base_path(None), None);
    }
}
