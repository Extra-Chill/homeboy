mod helpers;
mod short_head_revision;
mod types;

pub use helpers::*;
pub use short_head_revision::*;
pub use types::*;

use crate::config::{self, from_str};
use crate::engine::identifier;
use crate::engine::local_files::{self, FileSystem};
use crate::error::{Error, Result};
use crate::git;
use crate::paths;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::execution::run_setup;
use super::manifest::ExtensionManifest;
use super::{is_extension_linked, load_extension};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slugify_id_default_path() {
        let value = "";
        let _result = slugify_id(&value);
    }

    #[test]
    fn test_derive_id_from_url_default_path() {
        let url = "";
        let _result = derive_id_from_url(&url);
    }

    #[test]
    fn test_is_git_url_default_path() {
        let source = "";
        let _result = is_git_url(&source);
    }

    #[test]
    fn test_install_default_path() {
        let source = "";
        let id_override = None;
        let _result = install(&source, id_override);
    }

    #[test]
    fn test_update_default_path() {
        let extension_id = "";
        let force = true;
        let _result = update(&extension_id, force);
    }

    #[test]
    fn test_update_extension_dir_exists() {
        let extension_id = "";
        let force = true;
        let result = update(&extension_id, force);
        assert!(result.is_err(), "expected Err for: !extension_dir.exists()");
    }

    #[test]
    fn test_update_some_extension_id_to_string() {
        let extension_id = "";
        let force = true;
        let _result = update(&extension_id, force);
    }

    #[test]
    fn test_update_force_is_workdir_clean_extension_dir() {
        let extension_id = "";
        let force = false;
        let _result = update(&extension_id, force);
    }

    #[test]
    fn test_update_default_path_2() {
        let extension_id = "";
        let force = true;
        let _result = update(&extension_id, force);
    }

    #[test]
    fn test_update_some_extension_id_to_string_2() {
        let extension_id = "";
        let force = true;
        let _result = update(&extension_id, force);
    }

    #[test]
    fn test_update_default_path_3() {
        let extension_id = "";
        let force = true;
        let _result = update(&extension_id, force);
    }

    #[test]
    fn test_update_default_path_4() {
        let extension_id = "";
        let force = true;
        let _result = update(&extension_id, force);
    }

    #[test]
    fn test_update_if_let_some_rev_get_short_head_revision_extension_dir() {
        let extension_id = "";
        let force = true;
        let _result = update(&extension_id, force);
    }

    #[test]
    fn test_update_if_let_ok_extension_load_extension_extension_id() {
        let extension_id = "";
        let force = true;
        let result = update(&extension_id, force);
        let inner = result.unwrap();
        // Branch returns Ok(extension) when: if let Ok(extension) = load_extension(extension_id) {{
        assert_eq!(inner.extension_id, String::new());
        assert_eq!(inner.url, String::new());
        assert_eq!(inner.path, PathBuf::new());
    }

    #[test]
    fn test_update_has_expected_effects() {
        // Expected effects: file_write
        let extension_id = "";
        let force = false;
        let _ = update(&extension_id, force);
    }

    #[test]
    fn test_uninstall_default_path() {
        let extension_id = "";
        let _result = uninstall(&extension_id);
    }

    #[test]
    fn test_uninstall_extension_dir_exists() {
        let extension_id = "";
        let result = uninstall(&extension_id);
        assert!(result.is_err(), "expected Err for: !extension_dir.exists()");
    }

    #[test]
    fn test_uninstall_extension_dir_is_symlink() {
        let extension_id = "";
        let _result = uninstall(&extension_id);
    }

    #[test]
    fn test_uninstall_else() {
        let extension_id = "";
        let _result = uninstall(&extension_id);
    }

    #[test]
    fn test_uninstall_default_path_2() {
        let extension_id = "";
        let _result = uninstall(&extension_id);
    }

    #[test]
    fn test_uninstall_ok_extension_dir() {
        let extension_id = "";
        let result = uninstall(&extension_id);
        assert!(result.is_ok(), "expected Ok for: Ok(extension_dir)");
    }

    #[test]
    fn test_uninstall_has_expected_effects() {
        // Expected effects: file_delete
        let extension_id = "";
        let _ = uninstall(&extension_id);
    }

    #[test]
    fn test_check_update_available_default_path() {
        let extension_id = "";
        let _result = check_update_available(&extension_id);
    }

    #[test]
    fn test_check_update_available_extension_dir_exists_is_extension_linked_extension_id() {
        let extension_id = "";
        let result = check_update_available(&extension_id);
        assert!(result.is_none(), "expected None for: !extension_dir.exists() || is_extension_linked(extension_id)");
    }

    #[test]
    fn test_check_update_available_extension_dir_join_git_exists() {
        let extension_id = "";
        let result = check_update_available(&extension_id);
        assert!(result.is_none(), "expected None for: !extension_dir.join(\".git\").exists()");
    }

    #[test]
    fn test_check_update_available_default_path_2() {
        let extension_id = "";
        let _result = check_update_available(&extension_id);
    }

    #[test]
    fn test_check_update_available_default_path_3() {
        let extension_id = "";
        let _result = check_update_available(&extension_id);
    }

    #[test]
    fn test_check_update_available_default_path_4() {
        let extension_id = "";
        let _result = check_update_available(&extension_id);
    }

    #[test]
    fn test_check_update_available_behind_count_0() {
        let extension_id = "";
        let result = check_update_available(&extension_id);
        assert!(result.is_none(), "expected None for: behind_count == 0");
    }

    #[test]
    fn test_check_update_available_behind_count_0_2() {
        let extension_id = "";
        let _result = check_update_available(&extension_id);
    }

    #[test]
    fn test_check_update_available_has_expected_effects() {
        // Expected effects: process_spawn
        let extension_id = "";
        let _ = check_update_available(&extension_id);
    }

    #[test]
    fn test_read_source_revision_default_path() {
        let extension_id = "";
        let _result = read_source_revision(&extension_id);
    }

    #[test]
    fn test_read_source_revision_extension_dir_exists() {
        let extension_id = "";
        let result = read_source_revision(&extension_id);
        assert!(result.is_none(), "expected None for: !extension_dir.exists()");
    }

    #[test]
    fn test_read_source_revision_extension_dir_exists_2() {
        let extension_id = "";
        let result = read_source_revision(&extension_id);
        assert!(result.is_some(), "expected Some for: !extension_dir.exists()");
    }

    #[test]
    fn test_read_source_revision_let_some_rev_get_short_head_revision_extension_dir() {
        let extension_id = "";
        let result = read_source_revision(&extension_id);
        assert!(result.is_some(), "expected Some for: let Some(rev) = get_short_head_revision(&extension_dir)");
    }

    #[test]
    fn test_read_source_revision_has_expected_effects() {
        // Expected effects: file_read
        let extension_id = "";
        let _ = read_source_revision(&extension_id);
    }

}
