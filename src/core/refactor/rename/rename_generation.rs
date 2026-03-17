//! rename_generation — extracted from mod.rs.

use std::path::{Path, PathBuf};
use crate::core::refactor::rename::types::RenameResult;
use crate::core::refactor::rename::rename_spec::RenameSpec;
use crate::core::refactor::rename::rename_targeting::RenameTargeting;
use crate::core::refactor::rename::default;
use crate::core::refactor::rename::generate_renames_with_targeting;
use crate::core::refactor::*;


/// Generate file edits and file renames from found references.
pub fn generate_renames(spec: &RenameSpec, root: &Path) -> RenameResult {
    generate_renames_with_targeting(spec, root, &RenameTargeting::default())
}

pub(crate) fn target_files(files: Vec<PathBuf>, root: &Path, targeting: &RenameTargeting) -> Vec<PathBuf> {
    files
        .into_iter()
        .filter(|file| {
            let relative = file
                .strip_prefix(root)
                .unwrap_or(file)
                .to_string_lossy()
                .replace('\\', "/");

            if !targeting.include_globs.is_empty()
                && !targeting
                    .include_globs
                    .iter()
                    .any(|glob| glob_match::glob_match(glob, &relative))
            {
                return false;
            }

            if targeting
                .exclude_globs
                .iter()
                .any(|glob| glob_match::glob_match(glob, &relative))
            {
                return false;
            }

            true
        })
        .collect()
}
