//! Local checkout file-permission normalization.
//!
//! Ensures files in a local component checkout have group read/write before
//! they're archived, so the build zip carries correct permissions (some tools
//! create files with restrictive 600 perms). Used by the extension build path;
//! lives at core level (not under `deploy`) so build doesn't depend on deploy.

use std::process::Command;

use crate::defaults;
use crate::engine::shell;

/// Fix local file permissions before build.
///
/// Ensures files have group read/write so the zip archive contains correct permissions.
/// This addresses the issue where Claude Code sometimes creates files with 600 permissions.
pub fn fix_local_permissions(local_path: &str) {
    let quoted_path = shell::quote_path(local_path);
    let perms = defaults::load_defaults().permissions.local;

    log_status!(
        "build",
        "Fixing local file permissions in {} (files: {}, dirs: {})",
        local_path,
        perms.file_mode,
        perms.dir_mode
    );

    // Fix files (configurable mode, default: g+rw)
    let file_cmd = format!(
        "find {} -type f -exec chmod {} {{}} + 2>/dev/null || true",
        quoted_path, perms.file_mode
    );
    Command::new("sh").args(["-c", &file_cmd]).output().ok();

    // Fix directories (configurable mode, default: g+rwx)
    let dir_cmd = format!(
        "find {} -type d -exec chmod {} {{}} + 2>/dev/null || true",
        quoted_path, perms.dir_mode
    );
    Command::new("sh").args(["-c", &dir_cmd]).output().ok();
}
