mod homeboy;
mod join_remote;
mod resolve_path;

pub use homeboy::*;
pub use join_remote::*;
pub use resolve_path::*;

use crate::error::{Error, Result};
use std::env;
use std::path::PathBuf;

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
        assert!(join_remote_path(None, "file.json").is_err());
    }

    #[test]
    fn join_remote_path_joins_relative_paths() {
        assert_eq!(
            join_remote_path(Some("/var/www/site"), "file.json").unwrap(),
            "/var/www/site/file.json"
        );

        assert_eq!(
            join_remote_path(Some("/var/www/site/"), "file.json").unwrap(),
            "/var/www/site/file.json"
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
    fn resolve_optional_base_path_trims_and_rejects_empty() {
        assert_eq!(
            resolve_optional_base_path(Some(" /var/www ")),
            Some("/var/www")
        );
        assert_eq!(resolve_optional_base_path(Some("   ")), None);
        assert_eq!(resolve_optional_base_path(None), None);
    }

    #[test]
    fn resolve_path_handles_relative() {
        let result = resolve_path_string("/base", "relative/path");
        assert_eq!(result, "/base/relative/path");
    }

    #[test]
    fn test_homeboy_default_path() {

        let _result = homeboy();
    }

    #[test]
    fn test_homeboy_ok_pathbuf_from_appdata_join_homeboy() {

        let result = homeboy();
        assert!(result.is_ok(), "expected Ok for: Ok(PathBuf::from(appdata).join(\"homeboy\"))");
    }

    #[test]
    fn test_homeboy_default_path_2() {

        let _result = homeboy();
    }

    #[test]
    fn test_homeboy_ok_pathbuf_from_home_join_config_join_homeboy() {

        let result = homeboy();
        assert!(result.is_ok(), "expected Ok for: Ok(PathBuf::from(home).join(\".config\").join(\"homeboy\"))");
    }

    #[test]
    fn test_homeboy_json_ok_homeboy_join_homeboy_json() {

        let result = homeboy_json();
        assert!(result.is_ok(), "expected Ok for: Ok(homeboy()?.join(\"homeboy.json\"))");
    }

    #[test]
    fn test_projects_ok_homeboy_join_projects() {

        let result = projects();
        assert!(result.is_ok(), "expected Ok for: Ok(homeboy()?.join(\"projects\"))");
    }

    #[test]
    fn test_project_dir_ok_projects_join_id() {
        let id = "";
        let result = project_dir(&id);
        assert!(result.is_ok(), "expected Ok for: Ok(projects()?.join(id))");
    }

    #[test]
    fn test_project_config_ok_projects_join_id_join_format_json_id() {
        let id = "";
        let result = project_config(&id);
        assert!(result.is_ok(), "expected Ok for: Ok(projects()?.join(id).join(format!(\"{{}}.json\", id)))");
    }

    #[test]
    fn test_servers_ok_homeboy_join_servers() {

        let result = servers();
        assert!(result.is_ok(), "expected Ok for: Ok(homeboy()?.join(\"servers\"))");
    }

    #[test]
    fn test_components_ok_homeboy_join_components() {

        let result = components();
        assert!(result.is_ok(), "expected Ok for: Ok(homeboy()?.join(\"components\"))");
    }

    #[test]
    fn test_extensions_ok_homeboy_join_extensions() {

        let result = extensions();
        assert!(result.is_ok(), "expected Ok for: Ok(homeboy()?.join(\"extensions\"))");
    }

    #[test]
    fn test_keys_ok_homeboy_join_keys() {

        let result = keys();
        assert!(result.is_ok(), "expected Ok for: Ok(homeboy()?.join(\"keys\"))");
    }

    #[test]
    fn test_backups_ok_homeboy_join_backups() {

        let result = backups();
        assert!(result.is_ok(), "expected Ok for: Ok(homeboy()?.join(\"backups\"))");
    }

    #[test]
    fn test_extension_ok_extensions_join_id() {
        let id = "";
        let result = extension(&id);
        assert!(result.is_ok(), "expected Ok for: Ok(extensions()?.join(id))");
    }

    #[test]
    fn test_extension_manifest_ok_extensions_join_id_join_format_json_id() {
        let id = "";
        let result = extension_manifest(&id);
        assert!(result.is_ok(), "expected Ok for: Ok(extensions()?.join(id).join(format!(\"{{}}.json\", id)))");
    }

    #[test]
    fn test_key_ok_keys_join_format_id_rsa_server_id() {
        let server_id = "";
        let result = key(&server_id);
        assert!(result.is_ok(), "expected Ok for: Ok(keys()?.join(format!(\"{{}}_id_rsa\", server_id)))");
    }

    #[test]
    fn test_resolve_path_default_path() {
        let base = "";
        let file = "";
        let _result = resolve_path(&base, &file);
    }

    #[test]
    fn test_resolve_path_string_default_path() {
        let base = "";
        let file = "";
        let _result = resolve_path_string(&base, &file);
    }

    #[test]
    fn test_resolve_optional_base_path_default_path() {

        let _result = resolve_optional_base_path();
    }

    #[test]
    fn test_join_remote_path_path_starts_with() {
        let base_path = None;
        let path = "/";
        let result = join_remote_path(base_path, &path);
        assert!(result.is_ok(), "expected Ok for: path.starts_with('/')");
    }

    #[test]
    fn test_join_remote_path_path_starts_with_2() {
        let base_path = None;
        let path = "/";
        let _result = join_remote_path(base_path, &path);
    }

    #[test]
    fn test_join_remote_path_path_starts_with_3() {
        let base_path = None;
        let path = "/";
        let result = join_remote_path(base_path, &path);
        assert!(result.is_err(), "expected Err for: path.starts_with('/')");
    }

    #[test]
    fn test_join_remote_path_base_ends_with() {
        let base_path = None;
        let path = "";
        let result = join_remote_path(base_path, &path);
        assert!(result.is_ok(), "expected Ok for: base.ends_with('/')");
    }

    #[test]
    fn test_join_remote_path_else() {
        let base_path = None;
        let path = "";
        let result = join_remote_path(base_path, &path);
        assert!(result.is_ok(), "expected Ok for: else");
    }

    #[test]
    fn test_join_remote_child_default_path() {

        let _result = join_remote_child();
    }

    #[test]
    fn test_join_remote_child_dir_path_ends_with() {

        let result = join_remote_child();
        assert!(result.is_ok(), "expected Ok for: dir_path.ends_with('/')");
    }

    #[test]
    fn test_join_remote_child_else() {

        let result = join_remote_child();
        assert!(result.is_ok(), "expected Ok for: else");
    }

}
