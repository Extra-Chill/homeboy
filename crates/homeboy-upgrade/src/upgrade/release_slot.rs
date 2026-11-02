use homeboy_core::engine::shell::quote_path;
use homeboy_core::error::{Error, Result};
use std::path::{Path, PathBuf};

/// Acquire a verified release in stage-only mode and publish an immutable slot.
const MATERIALIZE_SCRIPT: &str = r#"set -e
printf '%s\n' 'HOMEBOY_RUNNER_PROGRESS {"schema":"homeboy/runner-progress/v1","phase":"fetch"}'
ref=@REF@
binaries=@BINARIES@
release_tag="$ref"
running_tag=@RUNNING@
homeboy_installer=@INSTALLER@
hash_binary() { (sha256sum "$1" 2>/dev/null || shasum -a 256 "$1") | awk '{print $1}'; }
release_stage=
cleanup_release_stage() {
  if [ -n "$release_stage" ]; then
    rm -rf "$release_stage"
    release_stage=
  fi
}
mkdir -p "$binaries"
  # A tag already acquired here (e.g. by the upgrade's early acquisition)
  # resolves to its content-addressed slot once the slot's hash re-verifies.
  release_marker="$binaries/.release-$release_tag.sha256"
  cached_sha=$(cat "$release_marker" 2>/dev/null || true)
  cached_binary="$binaries/homeboy-$cached_sha/homeboy"
  if [ -n "$cached_sha" ] && [ -x "$cached_binary" ] && [ "$(hash_binary "$cached_binary")" = "$cached_sha" ]; then
    binary="$cached_binary"
    echo "HOMEBOY_REFRESH_BINARY_ORIGIN=release-cache:$release_tag"
  else
    release_stage=$(mktemp -d "$binaries/.stage-XXXXXX")
    trap cleanup_release_stage EXIT HUP INT TERM
    HOMEBOY_INSTALL_STAGE_ONLY=1 HOMEBOY_INSTALL_PATH="$release_stage/homeboy" HOMEBOY_UPGRADE_RELEASE_TAG="$release_tag" HOMEBOY_UPGRADE_RELEASE_VERSION="${release_tag#v}" sh -c "$homeboy_installer"
    binary="$release_stage/homeboy"
    echo "HOMEBOY_REFRESH_BINARY_ORIGIN=release:$release_tag"
  fi
binary_sha=$(hash_binary "$binary")
if [ -z "$binary_sha" ]; then
  echo "could not hash materialized Homeboy binary" >&2
  exit 1
fi
printf '%s\n' 'HOMEBOY_RUNNER_PROGRESS {"schema":"homeboy/runner-progress/v1","phase":"install"}'
slot_dir="$binaries/homeboy-$binary_sha"
immutable_binary="$slot_dir/homeboy"
mkdir -p "$slot_dir"
if [ -e "$immutable_binary" ]; then
  existing_sha=$(hash_binary "$immutable_binary")
  if [ "$existing_sha" != "$binary_sha" ]; then
    echo "immutable Homeboy binary slot hash mismatch" >&2
    exit 1
  fi
else
  staged_binary=$(mktemp "$slot_dir/.homeboy.XXXXXX")
  trap 'rm -f "$staged_binary"; cleanup_release_stage' EXIT HUP INT TERM
  cp "$binary" "$staged_binary"
  chmod 0755 "$staged_binary"
  staged_sha=$(hash_binary "$staged_binary")
  if [ "$staged_sha" != "$binary_sha" ]; then
    echo "staged Homeboy binary hash mismatch" >&2
    exit 1
  fi
  if ! ln "$staged_binary" "$immutable_binary"; then
    if [ ! -e "$immutable_binary" ] || [ "$(hash_binary "$immutable_binary")" != "$binary_sha" ]; then
      echo "immutable Homeboy binary slot publication failed" >&2
      exit 1
    fi
  fi
  rm -f "$staged_binary"
  trap - EXIT HUP INT TERM
fi
cleanup_release_stage
if [ -n "$release_tag" ]; then
  staged_marker=$(mktemp "$binaries/.release-marker.XXXXXX")
  printf '%s' "$binary_sha" > "$staged_marker"
  mv -f "$staged_marker" "$release_marker"
fi
echo "HOMEBOY_REFRESH_BINARY_SHA256=$binary_sha"
echo "HOMEBOY_REFRESH_BINARY_PATH=$immutable_binary"
printf '%s\n' 'HOMEBOY_RUNNER_PROGRESS {"schema":"homeboy/runner-progress/v1","phase":"verify"}'
identity=$("$immutable_binary" self identity)
identity_record=$(printf '%s' "$identity" | tr -d '\n')
provenance="$slot_dir/provenance"
staged_provenance=$(mktemp "$slot_dir/.provenance.XXXXXX")
trap 'rm -f "$staged_provenance"' EXIT HUP INT TERM
printf '%s\n%s\n%s\n%s\n%s\n%s\n' 'https://github.com/Extra-Chill/homeboy.git' "" "$ref" "$ref" "$binary_sha" "$identity_record" > "$staged_provenance"
# Source/cache/ref describe acquisition, not immutable content identity.
provenance_matches() {
  { IFS= read -r recorded_source && IFS= read -r recorded_cache && IFS= read -r recorded_ref && IFS= read -r recorded_target && IFS= read -r recorded_sha && IFS= read -r recorded_identity && ! IFS= read -r extra; } < "$provenance" &&
    [ "$recorded_sha" = "$binary_sha" ] &&
    [ "$recorded_identity" = "$identity_record" ]
}
if [ -e "$provenance" ]; then
  provenance_matches || { echo "immutable Homeboy slot provenance mismatch" >&2; exit 1; }
elif ! ln "$staged_provenance" "$provenance"; then
  [ -e "$provenance" ] && provenance_matches || { echo "immutable Homeboy slot provenance publication failed" >&2; exit 1; }
fi
rm -f "$staged_provenance"
trap - EXIT HUP INT TERM
# Keep the running release and the release just acquired, never a live stage.
running_sha=$(cat "$binaries/.release-$running_tag.sha256" 2>/dev/null || true)
for slot in "$binaries"/homeboy-*; do
  [ -d "$slot" ] || continue
  [ "$slot" = "$slot_dir" ] && continue
  [ "$slot" = "$binaries/homeboy-$running_sha" ] && continue
  rm -rf "$slot"
done
for marker in "$binaries"/.release-*.sha256; do
  [ "$marker" = "$release_marker" ] && continue
  [ "$marker" = "$binaries/.release-$running_tag.sha256" ] && continue
  rm -f "$marker"
done
printf '%s\n' "$identity"
"#;

pub fn materialize_script(git_ref: &str, binaries_dir: &str) -> String {
    let installer = quote_path(super::RELEASE_INSTALLER_SCRIPT);
    fill_script_template(
        MATERIALIZE_SCRIPT,
        &[
            ("@REF@", &quote_path(git_ref)),
            ("@BINARIES@", &quote_path(binaries_dir)),
            (
                "@RUNNING@",
                &quote_path(&format!("v{}", super::current_version())),
            ),
            ("@INSTALLER@", &installer),
        ],
    )
}

/// Substitute each placeholder, in template order, in a single pass so a
/// substituted value is never rescanned for later placeholders.
fn fill_script_template(template: &str, values: &[(&str, &str)]) -> String {
    let mut script = String::with_capacity(template.len());
    let mut rest = template;
    for (placeholder, value) in values {
        let (head, tail) = rest
            .split_once(placeholder)
            .expect("script template placeholders are in order");
        script.push_str(head);
        script.push_str(value);
        rest = tail;
    }
    script.push_str(rest);
    script
}

/// Controller-local slot store. Acquisition never selects or promotes a binary.
pub fn binaries_dir() -> Result<PathBuf> {
    Ok(homeboy_core::paths::PathRoots::from_environment()?
        .data()
        .join("binaries"))
}

/// Acquire a verified release, reusing its hash-checked immutable slot on a hit.
pub fn acquire_release_slot(tag: &str, binaries: &Path) -> Result<PathBuf> {
    semver::Version::parse(tag.trim_start_matches('v'))
        .map_err(|error| Error::internal_unexpected(format!("invalid release tag: {error}")))?;
    let output = super::execution::supervise_installer_shell_command(
        &materialize_script(tag, &binaries.to_string_lossy()),
        std::time::Duration::from_secs(20 * 60),
        "acquire release slot",
        |_| Ok(()),
        |_| {},
    )?;
    if !output.status.success() {
        return Err(Error::internal_unexpected(format!(
            "release acquisition failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("HOMEBOY_REFRESH_BINARY_PATH="))
        .map(PathBuf::from)
        .ok_or_else(|| Error::internal_unexpected("release acquisition returned no slot"))
}

#[cfg(test)]
mod tests;
