use serde::{Deserialize, Serialize};
use std::fs;

use crate::paths;
use crate::utils::io;

/// Root configuration structure for homeboy.json
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HomeboyConfig {
    #[serde(default)]
    pub defaults: Defaults,
}

/// All configurable defaults that can be overridden via homeboy.json
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Defaults {
    #[serde(default = "default_install_methods")]
    pub install_methods: InstallMethodsConfig,

    #[serde(default = "default_version_candidates")]
    pub version_candidates: Vec<VersionCandidateConfig>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub post_version_bump_commands: Vec<String>,

    #[serde(default = "default_deploy")]
    pub deploy: DeployConfig,

    #[serde(default = "default_permissions")]
    pub permissions: PermissionsConfig,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            install_methods: default_install_methods(),
            version_candidates: default_version_candidates(),
            post_version_bump_commands: Vec::new(),
            deploy: default_deploy(),
            permissions: default_permissions(),
        }
    }
}

/// Configuration for install method detection and upgrade commands
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallMethodsConfig {
    #[serde(default = "default_homebrew_config")]
    pub homebrew: InstallMethodConfig,

    #[serde(default = "default_cargo_config")]
    pub cargo: InstallMethodConfig,

    #[serde(default = "default_source_config")]
    pub source: InstallMethodConfig,

    #[serde(default = "default_binary_config")]
    pub binary: InstallMethodConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallMethodConfig {
    pub path_patterns: Vec<String>,
    pub upgrade_command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub list_command: Option<String>,
}

/// Configuration for version file detection
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionCandidateConfig {
    pub file: String,
    pub pattern: String,
}

/// Configuration for deploy operations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployConfig {
    #[serde(default = "default_scp_flags")]
    pub scp_flags: Vec<String>,

    #[serde(default = "default_artifact_prefix")]
    pub artifact_prefix: String,

    #[serde(default = "default_ssh_port")]
    pub default_ssh_port: u16,
}

/// Configuration for file permissions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionsConfig {
    #[serde(default = "default_local_permissions")]
    pub local: PermissionModes,

    #[serde(default = "default_remote_permissions")]
    pub remote: PermissionModes,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionModes {
    pub file_mode: String,
    pub dir_mode: String,
}

// =============================================================================
// Default value functions (match current hardcoded behavior)
// =============================================================================

fn default_install_methods() -> InstallMethodsConfig {
    InstallMethodsConfig {
        homebrew: default_homebrew_config(),
        cargo: default_cargo_config(),
        source: default_source_config(),
        binary: default_binary_config(),
    }
}

fn default_homebrew_config() -> InstallMethodConfig {
    InstallMethodConfig {
        path_patterns: vec!["/Cellar/".to_string(), "/homebrew/".to_string()],
        upgrade_command: "brew update && brew upgrade homeboy".to_string(),
        list_command: Some("brew list homeboy".to_string()),
    }
}

fn default_cargo_config() -> InstallMethodConfig {
    InstallMethodConfig {
        path_patterns: vec!["/.cargo/bin/".to_string()],
        upgrade_command: "cargo install homeboy".to_string(),
        list_command: None,
    }
}

fn default_source_config() -> InstallMethodConfig {
    InstallMethodConfig {
        path_patterns: vec!["/target/release/".to_string(), "/target/debug/".to_string()],
        upgrade_command: "git pull && . \"$HOME/.cargo/env\" && cargo build --release".to_string(),
        list_command: None,
    }
}

fn default_binary_config() -> InstallMethodConfig {
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
  echo "Expected extracted binary named 'homeboy'" >&2
  ls -la
  exit 1
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
"#.to_string(),
        list_command: None,
    }
}

fn default_version_candidates() -> Vec<VersionCandidateConfig> {
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

fn default_deploy() -> DeployConfig {
    DeployConfig {
        scp_flags: default_scp_flags(),
        artifact_prefix: default_artifact_prefix(),
        default_ssh_port: default_ssh_port(),
    }
}

fn default_scp_flags() -> Vec<String> {
    vec!["-O".to_string()]
}

fn default_artifact_prefix() -> String {
    ".homeboy-".to_string()
}

fn default_ssh_port() -> u16 {
    22
}

fn default_permissions() -> PermissionsConfig {
    PermissionsConfig {
        local: default_local_permissions(),
        remote: default_remote_permissions(),
    }
}

fn default_local_permissions() -> PermissionModes {
    PermissionModes {
        file_mode: "g+rw".to_string(),
        dir_mode: "g+rwx".to_string(),
    }
}

fn default_remote_permissions() -> PermissionModes {
    PermissionModes {
        file_mode: "g+w".to_string(),
        dir_mode: "g+w".to_string(),
    }
}

// =============================================================================
// Loading functions
// =============================================================================

/// Load defaults, merging file config with built-in defaults.
/// If homeboy.json is missing or invalid, silently returns built-in defaults.
pub fn load_defaults() -> Defaults {
    load_config().defaults
}

/// Load the full homeboy.json config, falling back to defaults on any error.
pub fn load_config() -> HomeboyConfig {
    load_config_from_file().unwrap_or_default()
}

/// Attempt to load config from homeboy.json file.
fn load_config_from_file() -> crate::Result<HomeboyConfig> {
    let path = paths::homeboy_json()?;

    if !path.exists() {
        return Err(crate::Error::other("homeboy.json not found"));
    }

    let content = io::read_file(&path, &format!("read {}", path.display()))?;

    let config: HomeboyConfig = serde_json::from_str(&content).map_err(|e| {
        crate::Error::validation_invalid_json(
            e,
            Some("parse homeboy.json".to_string()),
            Some(content.chars().take(200).collect::<String>()),
        )
    })?;

    Ok(config)
}

/// Save config to homeboy.json file (creates if missing).
pub fn save_config(config: &HomeboyConfig) -> crate::Result<()> {
    let path = paths::homeboy_json()?;

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            crate::Error::internal_io(e.to_string(), Some(format!("create {}", parent.display())))
        })?;
    }

    let content = serde_json::to_string_pretty(config).map_err(|e| {
        crate::Error::validation_invalid_json(e, Some("serialize homeboy.json".to_string()), None)
    })?;

    io::write_file(&path, &content, &format!("write {}", path.display()))?;

    Ok(())
}

/// Check if homeboy.json file exists
pub fn config_exists() -> bool {
    paths::homeboy_json().map(|p| p.exists()).unwrap_or(false)
}

/// Delete homeboy.json file (reset to defaults)
pub fn reset_config() -> crate::Result<bool> {
    let path = paths::homeboy_json()?;

    if path.exists() {
        fs::remove_file(&path).map_err(|e| {
            crate::Error::internal_io(e.to_string(), Some(format!("delete {}", path.display())))
        })?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Get the path to homeboy.json (for display purposes)
pub fn config_path() -> crate::Result<String> {
    Ok(paths::homeboy_json()?.display().to_string())
}

/// Get built-in defaults (ignoring any file config)
pub fn builtin_defaults() -> Defaults {
    Defaults::default()
}
