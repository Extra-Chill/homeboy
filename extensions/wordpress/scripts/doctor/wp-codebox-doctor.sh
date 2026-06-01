#!/usr/bin/env bash
set -euo pipefail

MODE="doctor"
FIX=0
STALE_AFTER_SECONDS="${HOMEBOY_WP_CODEBOX_STALE_AFTER_SECONDS:-3600}"
ARCHIVE_ROOTS=()

usage() {
    cat <<'USAGE'
Usage: wp-codebox-doctor.sh [doctor|cleanup] [--fix] [--stale-after-seconds N] [--archive-root DIR]

Checks WP Codebox runner health for Homeboy's WordPress extension.

Commands:
  doctor   Report binary/source fingerprint, Node/npm availability, stale recipe-run processes, and corrupt archives.
  cleanup  Run the same checks and remove safe stale/corrupt runtime state.

Options:
  --fix                      Allow mutating cleanup when command is doctor.
  --stale-after-seconds N    Age threshold for stale recipe-run processes. Default: 3600.
  --archive-root DIR         Additional archive/cache root to scan for corrupt .zip files.

Environment:
  HOMEBOY_WP_CODEBOX_BIN                 Specific wp-codebox binary.
  HOMEBOY_SETTINGS_JSON                  May provide wp_codebox_bin.
  HOMEBOY_WP_CODEBOX_ARCHIVE_ROOTS       Colon-separated archive/cache roots.
  HOMEBOY_WP_CODEBOX_STALE_AFTER_SECONDS Stale recipe-run age threshold.
USAGE
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        doctor|cleanup)
            MODE="$1"
            ;;
        --fix)
            FIX=1
            ;;
        --stale-after-seconds)
            shift
            if [ "$#" -eq 0 ] || ! [[ "${1:-}" =~ ^[0-9]+$ ]]; then
                echo "ERROR: --stale-after-seconds requires an integer value" >&2
                exit 2
            fi
            STALE_AFTER_SECONDS="$1"
            ;;
        --archive-root)
            shift
            if [ "$#" -eq 0 ] || [ -z "${1:-}" ]; then
                echo "ERROR: --archive-root requires a directory" >&2
                exit 2
            fi
            ARCHIVE_ROOTS+=("$1")
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "ERROR: Unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
    shift
done

if [ "$MODE" = "cleanup" ]; then
    FIX=1
fi

STATUS="ok"

mark_problem() {
    local level="$1"
    if [ "$level" = "error" ]; then
        STATUS="error"
    elif [ "$STATUS" = "ok" ]; then
        STATUS="warning"
    fi
}

print_check() {
    local level="$1"
    local key="$2"
    local message="$3"
    printf '[%s] %s: %s\n' "$level" "$key" "$message"
    case "$level" in
        error|warning)
            mark_problem "$level"
            ;;
    esac
}

sha256_file() {
    local file="$1"
    if command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$file" | awk '{print $1}'
    elif command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$file" | awk '{print $1}'
    else
        return 1
    fi
}

realpath_portable() {
    local target="$1"
    if command -v realpath >/dev/null 2>&1; then
        realpath "$target"
    else
        php -r 'echo realpath($argv[1]) ?: $argv[1];' "$target"
    fi
}

resolve_wp_codebox_bin() {
    local bin="${HOMEBOY_WP_CODEBOX_BIN:-}"
    if [ -z "$bin" ] && [ -n "${HOMEBOY_SETTINGS_JSON:-}" ] && [ "${HOMEBOY_SETTINGS_JSON}" != "{}" ] && command -v jq >/dev/null 2>&1; then
        bin=$(printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -r '.wp_codebox_bin // empty' 2>/dev/null || true)
    fi
    bin="${bin:-wp-codebox}"

    if [[ "$bin" = */* ]]; then
        printf '%s\n' "$bin"
        return 0
    fi

    command -v "$bin" 2>/dev/null || printf '%s\n' "$bin"
}

find_package_root() {
    local start="$1"
    local dir
    dir="$(dirname "$start")"
    while [ "$dir" != "/" ] && [ -n "$dir" ]; do
        if [ -f "$dir/package.json" ]; then
            printf '%s\n' "$dir"
            return 0
        fi
        dir="$(dirname "$dir")"
    done
    return 1
}

git_head_for_dir() {
    local dir="$1"
    if command -v git >/dev/null 2>&1 && git -C "$dir" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
        git -C "$dir" rev-parse HEAD 2>/dev/null || true
    fi
}

check_runtime_tools() {
    if command -v node >/dev/null 2>&1; then
        print_check ok node "$(node --version) at $(command -v node)"
    else
        print_check error node "node is not available on PATH"
    fi

    if command -v npm >/dev/null 2>&1; then
        print_check ok npm "$(npm --version) at $(command -v npm)"
    else
        print_check warning npm "npm is not available on PATH"
    fi
}

check_wp_codebox_binary() {
    local bin="$1"
    if [ ! -e "$bin" ] && ! command -v "$bin" >/dev/null 2>&1; then
        print_check error wp-codebox "wp-codebox not found; set HOMEBOY_WP_CODEBOX_BIN, settings wp_codebox_bin, or install wp-codebox"
        return 0
    fi

    local resolved="$bin"
    if [ -e "$bin" ]; then
        resolved="$(realpath_portable "$bin")"
    elif command -v "$bin" >/dev/null 2>&1; then
        resolved="$(realpath_portable "$(command -v "$bin")")"
    fi

    if [ -f "$resolved" ]; then
        local digest
        digest="$(sha256_file "$resolved" 2>/dev/null || true)"
        if [ -n "$digest" ]; then
            print_check ok wp-codebox.binary "${resolved} sha256:${digest}"
        else
            print_check warning wp-codebox.binary "${resolved}; sha256 tool unavailable"
        fi
    else
        print_check ok wp-codebox.binary "$resolved"
    fi

    local package_root
    package_root="$(find_package_root "$resolved" 2>/dev/null || true)"
    if [ -n "$package_root" ]; then
        local source_digest git_head
        source_digest="$(sha256_file "$package_root/package.json" 2>/dev/null || true)"
        git_head="$(git_head_for_dir "$package_root")"
        if [ -n "$git_head" ]; then
            print_check ok wp-codebox.source "${package_root} git:${git_head}"
        elif [ -n "$source_digest" ]; then
            print_check ok wp-codebox.source "${package_root}/package.json sha256:${source_digest}"
        else
            print_check ok wp-codebox.source "$package_root"
        fi
    else
        print_check warning wp-codebox.source "package root not found for ${resolved}"
    fi
}

recipe_run_process_rows() {
    if ! command -v ps >/dev/null 2>&1; then
        return 0
    fi
    if ps -axo pid=,etimes=,command= >/dev/null 2>&1; then
        ps -axo pid=,etimes=,command= 2>/dev/null | awk '
            /wp-codebox/ && /recipe-run/ { print }
            /homeboy-wp-codebox-task-runner/ { print }
        ' || true
        return 0
    fi

    ps -axo pid=,command= 2>/dev/null | awk '
        /wp-codebox/ && /recipe-run/ { pid=$1; $1=""; sub(/^ +/, ""); print pid, 0, $0 }
        /homeboy-wp-codebox-task-runner/ { pid=$1; $1=""; sub(/^ +/, ""); print pid, 0, $0 }
    ' || true
}

check_stale_recipe_runs() {
    local rows stale_count cleaned_count
    rows="$(recipe_run_process_rows)"
    stale_count=0
    cleaned_count=0

    if [ -z "$rows" ]; then
        print_check ok wp-codebox.processes "no recipe-run processes found"
        return 0
    fi

    while IFS= read -r row; do
        [ -n "$row" ] || continue
        local pid age command
        pid="$(printf '%s\n' "$row" | awk '{print $1}')"
        age="$(printf '%s\n' "$row" | awk '{print $2}')"
        command="$(printf '%s\n' "$row" | awk '{$1=""; $2=""; sub(/^ +/, ""); print}')"
        if [ -z "$pid" ] || [ "$pid" = "$$" ] || ! [[ "$age" =~ ^[0-9]+$ ]]; then
            continue
        fi
        if [ "$age" -ge "$STALE_AFTER_SECONDS" ]; then
            stale_count=$((stale_count + 1))
            if [ "$FIX" -eq 1 ]; then
                if kill "$pid" >/dev/null 2>&1; then
                    cleaned_count=$((cleaned_count + 1))
                    print_check ok wp-codebox.cleanup "sent TERM to stale recipe-run pid ${pid} age=${age}s"
                else
                    print_check warning wp-codebox.cleanup "failed to terminate stale recipe-run pid ${pid} age=${age}s"
                fi
            else
                print_check warning wp-codebox.process "stale recipe-run pid ${pid} age=${age}s command=${command}"
            fi
        fi
    done <<< "$rows"

    if [ "$stale_count" -eq 0 ]; then
        print_check ok wp-codebox.processes "recipe-run processes found, none older than ${STALE_AFTER_SECONDS}s"
    elif [ "$FIX" -eq 1 ]; then
        print_check ok wp-codebox.cleanup "terminated ${cleaned_count}/${stale_count} stale recipe-run process(es)"
    fi
}

add_archive_roots_from_env() {
    local value="${HOMEBOY_WP_CODEBOX_ARCHIVE_ROOTS:-}"
    [ -n "$value" ] || return 0
    local root
    IFS=':' read -r -a roots <<< "$value"
    for root in "${roots[@]}"; do
        [ -n "$root" ] && ARCHIVE_ROOTS+=("$root")
    done
}

add_default_archive_roots() {
    local root
    for root in \
        "${HOME}/.cache/wp-codebox" \
        "${HOME}/.wp-codebox" \
        "${HOME}/.cache/wordpress-playground" \
        "${HOME}/.wordpress-playground"; do
        ARCHIVE_ROOTS+=("$root")
    done
}

zip_is_valid() {
    local file="$1"
    if command -v unzip >/dev/null 2>&1; then
        unzip -tq "$file" >/dev/null 2>&1
        return $?
    fi
    if command -v python3 >/dev/null 2>&1; then
        python3 - "$file" <<'PY' >/dev/null 2>&1
import sys, zipfile
with zipfile.ZipFile(sys.argv[1]) as archive:
    bad = archive.testzip()
    if bad:
        raise SystemExit(1)
PY
        return $?
    fi
    return 2
}

check_archives() {
    add_archive_roots_from_env
    add_default_archive_roots

    local existing_roots=()
    local root
    for root in "${ARCHIVE_ROOTS[@]}"; do
        [ -d "$root" ] || continue
        existing_roots+=("$root")
    done

    if [ "${#existing_roots[@]}" -eq 0 ]; then
        print_check ok wp-codebox.archives "no known WP Codebox/Playground archive roots found"
        return 0
    fi

    local checked=0 corrupt=0 removed=0 skipped=0 file rc
    while IFS= read -r -d '' file; do
        checked=$((checked + 1))
        set +e
        zip_is_valid "$file"
        rc=$?
        set -e
        if [ "$rc" -eq 0 ]; then
            continue
        fi
        if [ "$rc" -eq 2 ]; then
            skipped=$((skipped + 1))
            continue
        fi
        corrupt=$((corrupt + 1))
        if [ "$FIX" -eq 1 ]; then
            if rm -f "$file"; then
                removed=$((removed + 1))
                print_check ok wp-codebox.archive-cleanup "removed corrupt archive ${file}"
            else
                print_check warning wp-codebox.archive-cleanup "failed to remove corrupt archive ${file}"
            fi
        else
            print_check warning wp-codebox.archive "corrupt archive ${file}"
        fi
    done < <(find "${existing_roots[@]}" -type f \( -name '*.zip' -o -name '*.zip.tmp' \) -print0 2>/dev/null || true)

    if [ "$skipped" -gt 0 ]; then
        print_check warning wp-codebox.archives "archive validation skipped; install unzip or python3"
    elif [ "$corrupt" -eq 0 ]; then
        print_check ok wp-codebox.archives "checked ${checked} archive(s); no corrupt archives found"
    elif [ "$FIX" -eq 1 ]; then
        print_check ok wp-codebox.archive-cleanup "removed ${removed}/${corrupt} corrupt archive(s)"
    fi
}

echo "WP Codebox runner ${MODE}"
echo "  stale_after_seconds: ${STALE_AFTER_SECONDS}"
echo "  cleanup_enabled: ${FIX}"

check_runtime_tools
WP_CODEBOX_BIN="$(resolve_wp_codebox_bin)"
check_wp_codebox_binary "$WP_CODEBOX_BIN"
check_stale_recipe_runs
check_archives

case "$STATUS" in
    ok)
        echo "WP Codebox runner ${MODE} complete: ok"
        ;;
    warning)
        echo "WP Codebox runner ${MODE} complete: warnings"
        ;;
    error)
        echo "WP Codebox runner ${MODE} complete: errors"
        exit 1
        ;;
esac
