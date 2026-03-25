//! default_value_functions — extracted from defaults.rs.

use serde::{Deserialize, Serialize};
use super::VersionCandidateConfig;
use super::InstallMethodsConfig;
use super::DeployConfig;
use super::PermissionModes;
use super::PermissionsConfig;
use super::InstallMethodConfig;


pub(crate) fn default_true() -> bool {
    true
}

pub(crate) fn default_install_methods() -> InstallMethodsConfig {
    InstallMethodsConfig {
        homebrew: default_homebrew_config(),
        cargo: default_cargo_config(),
        source: default_source_config(),
        binary: default_binary_config(),
    }
}

pub(crate) fn default_homebrew_config() -> InstallMethodConfig {
    InstallMethodConfig {
        path_patterns: vec!["/Cellar/".to_string(), "/homebrew/".to_string()],
        upgrade_command: "brew update && brew upgrade homeboy".to_string(),
        list_command: Some("brew list homeboy".to_string()),
    }
}

pub(crate) fn default_cargo_config() -> InstallMethodConfig {
    InstallMethodConfig {
        path_patterns: vec!["/.cargo/bin/".to_string()],
        upgrade_command: "cargo install homeboy".to_string(),
        list_command: None,
    }
}

pub(crate) fn default_source_config() -> InstallMethodConfig {
    InstallMethodConfig {
        path_patterns: vec!["/target/release/".to_string(), "/target/debug/".to_string()],
        upgrade_command: "git pull && . \"$HOME/.cargo/env\" && cargo build --release".to_string(),
        list_command: None,
    }
}

pub(crate) fn default_binary_config() -> InstallMethodConfig {
    // A downloaded release binary (e.g. ~/bin/homeboy, /usr/local/bin/homeboy).
    //
    // This default upgrade command is intentionally shell-based so it works without
    // introducing new Rust deps (tar/xz/sha256). It can be overridden via homeboy.json.
    InstallMethodConfig {
        // Matches typical install locations. We intentionally key off "/bin/homeboy" so both
        // /usr/local/bin/homeboy and ~/bin/homeboy are detected.
        path_patterns: vec!["/bin/homeboy".to_string(), "homeboy.exe".to_string()],
        upgrade_command: r#"set -e

BIN_PATH="$(command -v homeboy)"
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"

case "${OS}-${ARCH}" in
  linux-x86_64)  ASSET="homeboy-x86_64-unknown-linux-gnu.tar.xz" ;;
  linux-aarch64|linux-arm64) ASSET="homeboy-aarch64-unknown-linux-gnu.tar.xz" ;;
  darwin-x86_64) ASSET="homeboy-x86_64-apple-darwin.tar.xz" ;;
  darwin-aarch64|darwin-arm64) ASSET="homeboy-aarch64-apple-darwin.tar.xz" ;;
  *) echo "Unsupported platform for binary upgrade: ${OS}-${ARCH}" >&2; exit 1 ;;
esac

BASE_URL="https://github.com/Extra-Chill/homeboy/releases/latest/download"
TMP_DIR="$(mktemp -d)"

cleanup() { rm -rf "$TMP_DIR"; }
trap cleanup EXIT

curl -fsSL "${BASE_URL}/${ASSET}" -o "${TMP_DIR}/${ASSET}"
curl -fsSL "${BASE_URL}/${ASSET}.sha256" -o "${TMP_DIR}/${ASSET}.sha256"

cd "$TMP_DIR"

if command -v sha256sum >/dev/null 2>&1; then
  sha256sum -c "${ASSET}.sha256"
elif command -v shasum >/dev/null 2>&1; then
  # macOS
  SHASUM_EXPECTED="$(cut -d" " -f1 "${ASSET}.sha256")"
  SHASUM_ACTUAL="$(shasum -a 256 "${ASSET}" | cut -d" " -f1)"
  [ "$SHASUM_EXPECTED" = "$SHASUM_ACTUAL" ]
else
  echo "No sha256 tool found (sha256sum or shasum)." >&2
  exit 1
fi

# Extract and install
if tar -xJf "${ASSET}" 2>/dev/null; then
  true
else
  tar -xf "${ASSET}"
fi

if [ ! -f "homeboy" ]; then
  # cargo-dist packages the binary inside a subdirectory (e.g. homeboy-x86_64-unknown-linux-gnu/)
  FOUND=$(find . -maxdepth 2 -name "homeboy" -type f ! -name "*.sha256" | head -1)
  if [ -n "$FOUND" ]; then
    cp "$FOUND" ./homeboy
  else
    echo "Expected extracted binary named 'homeboy'" >&2
    ls -laR
    exit 1
  fi
fi

# Install with permission-aware behavior
if [ -w "$BIN_PATH" ] || [ -w "$(dirname "$BIN_PATH")" ]; then
  install -m 0755 homeboy "$BIN_PATH"
else
  if command -v sudo >/dev/null 2>&1; then
    if sudo -n true >/dev/null 2>&1; then
      sudo install -m 0755 homeboy "$BIN_PATH"
    else
      echo "Insufficient permissions to write to $BIN_PATH. Re-run with sudo:" >&2
      echo "  sudo homeboy upgrade --method binary" >&2
      exit 1
    fi
  else
    echo "Insufficient permissions to write to $BIN_PATH (and sudo not found)." >&2
    exit 1
  fi
fi
"#
        .to_string(),
        list_command: None,
    }
}

pub(crate) fn default_version_candidates() -> Vec<VersionCandidateConfig> {
    vec![
        VersionCandidateConfig {
            file: "Cargo.toml".to_string(),
            pattern: r#"version\s*=\s*"(\d+\.\d+\.\d+)""#.to_string(),
        },
        VersionCandidateConfig {
            file: "package.json".to_string(),
            pattern: r#""version"\s*:\s*"(\d+\.\d+\.\d+)""#.to_string(),
        },
        VersionCandidateConfig {
            file: "composer.json".to_string(),
            pattern: r#""version"\s*:\s*"(\d+\.\d+\.\d+)""#.to_string(),
        },
        VersionCandidateConfig {
            file: "style.css".to_string(),
            pattern: r"Version:\s*(\d+\.\d+\.\d+)".to_string(),
        },
    ]
}

pub(crate) fn default_deploy() -> DeployConfig {
    DeployConfig {
        scp_flags: default_scp_flags(),
        artifact_prefix: default_artifact_prefix(),
        default_ssh_port: default_ssh_port(),
    }
}

pub(crate) fn default_scp_flags() -> Vec<String> {
    vec!["-O".to_string()]
}

pub(crate) fn default_artifact_prefix() -> String {
    ".homeboy-".to_string()
}

pub(crate) fn default_ssh_port() -> u16 {
    22
}

pub(crate) fn default_permissions() -> PermissionsConfig {
    PermissionsConfig {
        local: default_local_permissions(),
        remote: default_remote_permissions(),
    }
}

pub(crate) fn default_local_permissions() -> PermissionModes {
    PermissionModes {
        file_mode: "g+rw".to_string(),
        dir_mode: "g+rwx".to_string(),
    }
}

pub(crate) fn default_remote_permissions() -> PermissionModes {
    PermissionModes {
        file_mode: "g+w".to_string(),
        dir_mode: "g+w".to_string(),
    }
}
