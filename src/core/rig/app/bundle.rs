//! macOS script-backed launcher bundle generation.

use std::fs;
use std::path::Path;

use super::ResolvedLauncher;
use crate::error::{Error, Result};
use crate::rig::spec::AppLauncherPreflight;
use crate::rig::RigSpec;

pub(super) fn write_macos_bundle(rig: &RigSpec, launcher: &ResolvedLauncher) -> Result<()> {
    let contents = launcher.launcher_path.join("Contents");
    let macos = contents.join("MacOS");
    fs::create_dir_all(&macos).map_err(|e| {
        Error::internal_unexpected(format!(
            "Failed to create launcher bundle {}: {}",
            macos.display(),
            e
        ))
    })?;

    let plist_path = contents.join("Info.plist");
    fs::write(&plist_path, render_info_plist(launcher)).map_err(|e| {
        Error::internal_unexpected(format!(
            "Failed to write launcher plist {}: {}",
            plist_path.display(),
            e
        ))
    })?;

    let script_path = macos.join("launch");
    fs::write(&script_path, render_launcher_script(rig, launcher)).map_err(|e| {
        Error::internal_unexpected(format!(
            "Failed to write launcher script {}: {}",
            script_path.display(),
            e
        ))
    })?;
    make_executable(&script_path)?;
    Ok(())
}

pub(super) fn planned_files(launcher: &ResolvedLauncher) -> Vec<String> {
    vec![
        launcher.launcher_path.display().to_string(),
        launcher
            .launcher_path
            .join("Contents/Info.plist")
            .display()
            .to_string(),
        launcher
            .launcher_path
            .join("Contents/MacOS/launch")
            .display()
            .to_string(),
    ]
}

pub(super) fn render_info_plist(launcher: &ResolvedLauncher) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key>
  <string>en</string>
  <key>CFBundleExecutable</key>
  <string>launch</string>
  <key>CFBundleIdentifier</key>
  <string>{}</string>
  <key>CFBundleName</key>
  <string>{}</string>
  <key>CFBundleDisplayName</key>
  <string>{}</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleVersion</key>
  <string>1</string>
  <key>CFBundleShortVersionString</key>
  <string>1.0</string>
</dict>
</plist>
"#,
        xml_escape(&launcher.bundle_id),
        xml_escape(&launcher.display_name),
        xml_escape(&launcher.display_name)
    )
}

pub(super) fn render_launcher_script(rig: &RigSpec, launcher: &ResolvedLauncher) -> String {
    let mut preflight = String::new();
    for step in &launcher.preflight {
        match step {
            AppLauncherPreflight::RigCheck => push_rig_check_preflight(rig, &mut preflight),
        }
    }

    format!(
        r#"#!/bin/sh
set -eu

HOMEBOY_BIN="${{HOMEBOY_BIN:-homeboy}}"
TARGET_APP={}

{}
"$HOMEBOY_BIN" rig up {}

if [ -d "$TARGET_APP" ]; then
  exec open -n "$TARGET_APP" --args "$@"
fi

exec "$TARGET_APP" "$@"
"#,
        sh_single_quote(&launcher.target_path),
        preflight,
        sh_single_quote(&rig.id)
    )
}

fn push_rig_check_preflight(rig: &RigSpec, preflight: &mut String) {
    let rig_id = sh_single_quote(&rig.id);
    let terminal_command =
        applescript_string_literal(&format!("homeboy rig status {}", sh_single_quote(&rig.id)));
    preflight.push_str(&format!(
        r#"if ! "$HOMEBOY_BIN" rig check {}; then
  osascript -e 'display alert "Homeboy rig check failed" message "Run homeboy rig status {} for details."' >/dev/null 2>&1 || true
  osascript -e 'tell application "Terminal" to do script {}' >/dev/null 2>&1 || true
  exit 1
fi
"#,
        rig_id, rig.id, terminal_command
    ));
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)
        .map_err(|e| {
            Error::internal_unexpected(format!("Failed to stat {}: {}", path.display(), e))
        })?
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).map_err(|e| {
        Error::internal_unexpected(format!(
            "Failed to chmod launcher script {}: {}",
            path.display(),
            e
        ))
    })
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}

fn sh_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn applescript_string_literal(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}
