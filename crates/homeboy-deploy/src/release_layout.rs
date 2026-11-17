//! Versioned release directories with an atomic symlink flip (#18365).
//!
//! The in-place archive install exchanges the live directory, so the target
//! path never changes and only its contents do. A request that already
//! resolved the target can then read old and new files in one execution.
//! With a release layout every deploy gets its own directory and real path:
//!
//! ```text
//! <parent>/<slug>                -> .homeboy-releases/<slug>/<id>/<slug>   (relative symlink)
//! <parent>/.homeboy-releases/<slug>/<id>/<slug>/...                        (active release)
//! <parent>/.homeboy-releases/<slug>/<older-id>/<slug>/...                  (retained previous)
//! <parent>/.homeboy-releases/<slug>/.deactivated-<older-id>                (deactivation marker)
//! ```
//!
//! The release tree keeps the target basename as its own directory name so
//! code that derives its identity from its directory name (`basename(__DIR__)`
//! and similar) sees the same name it saw before the layout.
//!
//! This module only renders shell fragments for the generated archive install
//! script. It knows nothing about any runtime: whether a runtime is safe to
//! serve through a flipped symlink is the extension's `precondition_command`.
//!
//! Script contract. The archive install prelude has already set `target`,
//! `parent`, `backup`, `zip_root`, `staged` (adjacent temp dir, removed by an
//! EXIT trap) and `extracted` (the staged tree). [`decide_and_install`] sets
//! `use_release=1` only after the target points at the new release; otherwise
//! `use_release=0` and `extracted` names the tree the in-place flow publishes.

use homeboy_core::engine::shell;
use homeboy_extension_contract::DeployReleaseLayoutPolicy;

use super::install_script::{LOG_MARKER, WARN_MARKER};

/// `rename(2)` of a temp symlink over the target via python3, for hosts whose
/// `mv` lacks `-T`. No single quotes or braces, like the renameat2 helper.
const PY_REPLACE: &str = "import os, sys; os.replace(sys.argv[1], sys.argv[2])";

/// Shell that decides between the release layout and the in-place flow and,
/// when the layout is chosen, publishes the staged tree as a new release.
///
/// `exchange_py` is the already-quoted python3 `renameat2(RENAME_EXCHANGE)`
/// helper shared with the in-place flow.
pub(crate) fn decide_and_install(layout: &DeployReleaseLayoutPolicy, exchange_py: &str) -> String {
    if let Err(error) = layout.validate() {
        return format!(
            "use_release=0\nrelease_dir=\nprintf '\\n{WARN_MARKER}%s\\n' {}\n",
            shell::quote_arg(&format!(
                "Release layout disabled: {error}; installing with the in-place exchange flow"
            ))
        );
    }
    let precondition = match layout.precondition_command.as_deref() {
        Some(command) => PRECONDITION.replace("@PRECONDITION@", command),
        None => format!(
            "use_release=1\necho '{LOG_MARKER}Release layout enabled (no precondition declared)'\n"
        ),
    };
    let script = format!("{HEADER}{precondition}{INSTALL}");
    script
        .replace("@ROOT@", &layout.release_root_dir)
        .replace("@EXCHANGE_PY@", exchange_py)
        .replace("@REPLACE_PY@", &shell::quote_arg(PY_REPLACE))
        .replace("@LOG@", LOG_MARKER)
        .replace("@WARN@", WARN_MARKER)
}

/// Shell run after the in-place flow when a release directory was prepared
/// but the layout fell back: drop it and any release root it left empty.
pub(crate) fn discard_unused_release(layout: &DeployReleaseLayoutPolicy) -> String {
    format!(
        "if [ \"$use_release\" != 1 ] && [ -n \"$release_dir\" ]; then rm -rf \"$release_dir\"; discard_release=; rmdir \"$release_base\" 2>/dev/null; rmdir \"$parent/{}\" 2>/dev/null; fi\n",
        layout.release_root_dir
    )
}

/// Shell run after a successful flip (and after the after-switch command)
/// that prunes old releases. Best-effort: it never fails the deploy and never
/// removes the active release.
pub(crate) fn garbage_collect(layout: &DeployReleaseLayoutPolicy) -> String {
    GC.replace("@KEEP@", &layout.keep_previous.to_string())
        .replace("@RECENT@", &layout.keep_recent_minutes.to_string())
        .replace("@LOG@", LOG_MARKER)
}

const HEADER: &str = r#"use_release=0
release_dir=
discard_release=
trap 'rm -rf "$staged"; if [ -n "$discard_release" ]; then rm -rf "$discard_release"; fi' EXIT
release_slug=$(basename "$target")
release_base="$parent/@ROOT@/$release_slug"
release_rel_prefix="@ROOT@/$release_slug"
"#;

// The precondition runs in a subshell with its output captured, so `exit` in
// the extension command is scoped and its output becomes evidence.
const PRECONDITION: &str = r#"precondition_log="$staged/.homeboy-release-precondition.log"
( @PRECONDITION@
) >"$precondition_log" 2>&1; precondition_rc=$?
cat "$precondition_log" 2>/dev/null
if [ "$precondition_rc" -eq 0 ]; then
use_release=1
echo '@LOG@Release layout precondition passed'
else
precondition_tail=$(tail -c 1500 "$precondition_log" 2>/dev/null | tr '\n\t' '  ')
printf '\n@WARN@%s\n' "Release layout precondition failed (exit $precondition_rc); installing with the in-place exchange flow instead: $precondition_tail"
fi
rm -f "$precondition_log"
"#;

const INSTALL: &str = r#"if [ "$use_release" = 1 ]; then
artifact_sum=$(cksum < "{{stagingArtifact}}" 2>/dev/null | awk '{ printf "%08x", $1 }')
release_id="$(date -u +%Y%m%dT%H%M%SZ)-${artifact_sum:-00000000}-$$"
release_dir="$release_base/$release_id"
release_link="$release_rel_prefix/$release_id/$release_slug"
if ! { mkdir -p "$release_base" && mkdir "$release_dir"; }; then echo "ERROR: could not create release directory $release_dir" && exit 1; fi
discard_release="$release_dir"
if ! mv "$extracted" "$release_dir/$release_slug"; then echo 'ERROR: could not move the staged tree into the release directory' && exit 1; fi
extracted="$release_dir/$release_slug"
previous_id=
link_tmp="$parent/.homeboy-link.$release_id"
rm -f "$link_tmp"
if ! ln -s "$release_link" "$link_tmp"; then echo 'ERROR: could not create the release symlink' && exit 1; fi
if [ -L "$target" ]; then
current_link=$(readlink "$target")
case "$current_link" in
"$release_rel_prefix"/*) previous_id=${current_link#"$release_rel_prefix"/}; previous_id=${previous_id%%/*} ;;
*) printf '\n@WARN@%s\n' "Deploy target $target was an unmanaged symlink to $current_link; replacing it with the release symlink (the old link target is left untouched)" ;;
esac
if mv -fT "$link_tmp" "$target" 2>/dev/null || python3 -c @REPLACE_PY@ "$link_tmp" "$target" 2>/dev/null; then
echo "@LOG@Release $release_id activated by atomic symlink flip"
else
rm -f "$link_tmp"
echo "ERROR: release symlink flip failed; $target still points at the previous release" && exit 1
fi
elif [ -d "$target" ]; then
echo 'Target is a real directory; migrating to the release layout with an atomic exchange'
if mv -T --exchange "$link_tmp" "$target" 2>/dev/null || python3 -c @EXCHANGE_PY@ "$link_tmp" "$target" 2>/dev/null; then
previous_id="$release_id-migrated"
if mkdir "$release_base/$previous_id" && mv "$link_tmp" "$release_base/$previous_id/$release_slug"; then
echo "@LOG@Migrated $target to the release layout; the previous tree is kept as release $previous_id"
else
printf '\n@WARN@%s\n' "Migrated $target to the release layout, but the previous tree could not be filed under $release_base; it remains at $link_tmp"
rmdir "$release_base/$previous_id" 2>/dev/null
previous_id=
fi
else
rm -f "$link_tmp"
printf '\n@WARN@%s\n' "Release layout migration needs an atomic exchange (mv --exchange or renameat2 via python3), unavailable on this host; installing with the in-place exchange flow instead"
use_release=0
fi
elif [ -e "$target" ]; then
rm -f "$link_tmp"
printf '\n@WARN@%s\n' "Deploy target $target is neither a directory nor a symlink; installing with the in-place exchange flow instead"
use_release=0
else
if mv -fT "$link_tmp" "$target" 2>/dev/null || python3 -c @REPLACE_PY@ "$link_tmp" "$target" 2>/dev/null; then
echo "@LOG@Release $release_id installed (first install)"
else
rm -f "$link_tmp"
printf '\n@WARN@%s\n' "Could not place the release symlink (needs mv -T or python3); installing with the in-place exchange flow instead"
use_release=0
fi
fi
if [ "$use_release" = 1 ]; then
discard_release=
if [ -n "$previous_id" ]; then touch "$release_base/.deactivated-$previous_id" 2>/dev/null || true; fi
test -d "$target" || { echo 'ERROR: target missing after release flip' && exit 1; }
fi
fi
"#;

// Ordering: previous releases sort by deactivation time (marker mtime, or the
// release directory's mtime when it was never activated), newest first. Keep
// the first @KEEP@ and anything deactivated within @RECENT@ minutes; the
// active release is re-read from the symlink before every removal.
const GC: &str = r#"( keep=@KEEP@; recent=$((@RECENT@ * 60)); now=$(date +%s)
mtime() { stat -c %Y "$1" 2>/dev/null || stat -f %m "$1" 2>/dev/null || echo "$now"; }
for candidate in "$release_base"/*; do
[ -d "$candidate" ] && [ ! -L "$candidate" ] || continue
name=${candidate##*/}
[ "$name" = "$release_id" ] && continue
marker="$release_base/.deactivated-$name"
if [ -e "$marker" ]; then printf '%s %s\n' "$(mtime "$marker")" "$name"; else printf '%s %s\n' "$(mtime "$candidate")" "$name"; fi
done | sort -rn | {
rank=0; pruned=0; kept=0
while read -r stamp name; do
rank=$((rank + 1))
if [ "$rank" -le "$keep" ] || [ $((now - stamp)) -lt "$recent" ]; then kept=$((kept + 1)); continue; fi
case "$(readlink "$target" 2>/dev/null)" in "$release_rel_prefix/$name"|"$release_rel_prefix/$name"/*) kept=$((kept + 1)); continue ;; esac
if rm -rf "${release_base:?}/$name"; then rm -f "$release_base/.deactivated-$name"; pruned=$((pruned + 1)); fi
done
echo "@LOG@Release GC: kept $kept previous release(s), pruned $pruned"
}
for marker in "$release_base"/.deactivated-*; do
[ -e "$marker" ] || continue
[ -d "$release_base/${marker##*/.deactivated-}" ] || rm -f "$marker"
done
) || true
"#;

#[cfg(test)]
mod tests;
