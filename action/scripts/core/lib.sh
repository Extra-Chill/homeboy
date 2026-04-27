#!/usr/bin/env bash

set -euo pipefail

# Source scope module for flag generation
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/../scope/flags.sh"

# Prefix used for all autofix commits. Loop guards grep for this prefix,
# so the subject line can vary after it (e.g. fix types, file count).
AUTOFIX_COMMIT_PREFIX="chore(ci): homeboy autofix"
AUTOFIX_BOT_NAME="homeboy-ci[bot]"
AUTOFIX_BOT_EMAIL="266378653+homeboy-ci[bot]@users.noreply.github.com"

# Note: Guard logic (revert detection, bot HEAD detection, cap enforcement,
# disabled-label check, force-push detection) lives in homeboy core at
# src/core/refactor/auto/guard.rs. The refactor --write command checks all
# guards and returns a RefactorSourceRun with guard_block set when blocked.
# The action reads the JSON output to determine skip reasons.
# See the PR that introduced this change for the migration details.

# Check whether the current PR is still open.
# Returns 0 (true) if the PR is open, 1 (false) if merged/closed/unknown.
# Uses gh CLI when available, falls back to GitHub REST API via curl.
# Requires: GITHUB_REPOSITORY and PR_NUMBER (or $1) in the environment.
pr_is_active() {
  local pr_number="${1:-${PR_NUMBER:-}}"
  local repo="${GITHUB_REPOSITORY:-}"

  if [ -z "${pr_number}" ] || [ -z "${repo}" ]; then
    # Can't check — assume active to avoid false cancellations
    return 0
  fi

  local state=""
  if command -v gh >/dev/null 2>&1; then
    state=$(gh pr view "${pr_number}" --repo "${repo}" --json state -q '.state' 2>/dev/null || true)
  fi

  if [ -z "${state}" ]; then
    # Fallback to curl — use GH_TOKEN or GITHUB_TOKEN for auth
    local token="${GH_TOKEN:-${GITHUB_TOKEN:-}}"
    if [ -n "${token}" ]; then
      state=$(curl -sfL \
        -H "Authorization: Bearer ${token}" \
        -H "Accept: application/vnd.github+json" \
        "https://api.github.com/repos/${repo}/pulls/${pr_number}" 2>/dev/null \
        | jq -r '.state // empty' 2>/dev/null || true)
    fi
  fi

  case "${state}" in
    OPEN|open)
      return 0
      ;;
    MERGED|CLOSED|merged|closed)
      return 1
      ;;
    *)
      # Unknown state — assume active to avoid false cancellations
      return 0
      ;;
  esac
}

# Build an informative autofix commit message.
# Subject: chore(ci): homeboy autofix — audit (7 files, 33 fixes)
# Body: per-category fix counts with affected files.
build_autofix_commit_message() {
  local fix_types="$1"
  local file_count="$2"
  local fix_details="${3:-}"
  local total_fixes="${4:-}"

  local subject="${AUTOFIX_COMMIT_PREFIX}"
  if [ -n "${fix_types}" ]; then
    subject="${subject} — ${fix_types}"
  fi
  subject="${subject} (${file_count} files"
  if [ -n "${total_fixes}" ] && [ "${total_fixes}" != "0" ]; then
    subject="${subject}, ${total_fixes} fixes"
  fi
  subject="${subject})"

  local body=""
  if [ -n "${fix_details}" ]; then
    body="${fix_details}"
  else
    body="$(git diff --cached --name-only | sort)"
  fi

  printf '%s\n\n%s\n' "${subject}" "${body}"
}

# Extract a detailed fix breakdown from homeboy JSON output files.
# Reads FixResult JSON and produces a human-readable summary grouped by fix category.
# First line: total fix count. Remaining lines: per-category breakdown.
extract_fix_details_from_output() {
  local output_dir="$1"

  # Find all JSON output files
  local json_files
  json_files=$(find "${output_dir}" -name '*.json' -type f 2>/dev/null)
  if [ -z "${json_files}" ]; then
    return
  fi

  # Use jq to aggregate fix details across all output files.
  # Insertion kinds can be strings ("import_add") or objects ({"visibility_change": {...}}).
  # We normalize to the top-level key name and map to human-readable labels.
  # shellcheck disable=SC2086
  jq -rs '
    [.[] | .data // . ] | map(select(type == "object")) |

    # Normalize insertion kind to a category string
    def category:
      if type == "string" then .
      elif type == "object" then (keys[0] // "unknown")
      else "unknown"
      end;

    # Human-readable category names.
    # Keys come from two sources:
    #   - InsertionKind (snake_case strings or object keys): function_removal, visibility_change, etc.
    #   - AuditFinding (snake_case enum via serde): orphaned_test, god_file, etc.
    def humanize:
      {
        "function_removal": "Orphaned tests removed",
        "visibility_change": "Visibility narrowed (pub → pub(crate))",
        "reexport_removal": "Unused re-exports removed",
        "import_add": "Missing imports added",
        "method_stub": "Method stubs added",
        "file_move": "Files moved",
        "line_replacement": "Lines replaced",
        "line_removal": "Lines removed",
        "missing_test_file": "Test files generated",
        "missing_test_method": "Test stubs generated",
        "unreferenced_export": "Unreferenced exports narrowed",
        "duplicate_function": "Duplicate functions removed",
        "god_file": "God files decomposed",
        "high_item_count": "Large files decomposed",
        "orphaned_test": "Orphaned tests removed",
        "near_duplicate": "Near-duplicate functions consolidated",
        "unused_parameter": "Unused parameters removed",
        "compiler_warning": "Compiler warnings fixed",
        "todo_marker": "TODO markers resolved",
        "legacy_comment": "Legacy comments cleaned",
        "intra_method_duplicate": "Intra-method duplicates extracted",
        "stale_doc_reference": "Stale doc references fixed",
        "broken_doc_reference": "Broken doc references fixed",
        "missing_import": "Missing imports added",
        "namespace_mismatch": "Namespace mismatches fixed",
        "directory_sprawl": "Directory sprawl reduced"
      }[.] // .;

    # Collect insertions with normalized category (FixResult format)
    [.[].fixes // [] | .[] | .file as $file |
      .insertions[]? | {cat: (.kind | category), file: $file}
    ] as $insertions |

    # Collect new files (FixResult format)
    [.[].new_files // [] | .[] | {cat: .finding, file}] as $new_files |

    # Collect proposals (RefactorPlan format — from refactor --from all)
    [.[].proposals // [] | .[] | {cat: .rule_id, file}] as $proposals |

    # Collect collected_edits (RefactorSourceRun format — from refactor --from all --write)
    [.[].collected_edits // [] | .[] | {cat: .rule_id, file}] as $collected_edits |

    # Collect decompose plans (FixResult format — structural decompose operations)
    [.[].decompose_plans // [] | .[] | {cat: .source_finding, file}] as $decompose |

    # Combine all sources and group by category
    ($insertions + $new_files + $proposals + $collected_edits + $decompose) | group_by(.cat) |
    map({
      cat: .[0].cat,
      count: length,
      files: [.[].file] | unique | map(
        split("/") | if length > 1 and .[-1] == "mod.rs"
          then [.[-2], .[-1]] | join("/")
          else .[-1]
        end
      ) | unique | sort
    }) |
    sort_by(-.count) |

    # Total
    (map(.count) | add // 0) as $total |

    # Format: first line is total, rest is breakdown
    "\($total)\n" +
    (map("\(.cat | humanize): \(.count)\n  \(.files | join(", "))") | join("\n"))
  ' ${json_files} 2>/dev/null || true
}

resolve_component_id() {
  if [ -n "${COMPONENT_NAME:-}" ]; then
    printf '%s\n' "${COMPONENT_NAME}"
  elif [ -n "${component_id:-}" ]; then
    printf '%s\n' "${component_id}"
  elif [ -f "homeboy.json" ]; then
    local from_portable
    from_portable="$(jq -r '.id // empty' homeboy.json 2>/dev/null || true)"
    if [ -n "${from_portable}" ]; then
      printf '%s\n' "${from_portable}"
    else
      basename "${GITHUB_REPOSITORY}"
    fi
  else
    basename "${GITHUB_REPOSITORY}"
  fi
}

resolve_workspace() {
  # When running in a multi-component repo, COMPONENT_DIR points to the
  # subdirectory containing the component's homeboy.json. Homeboy core
  # uses --path to read config and scope operations to this directory.
  local component_dir="${COMPONENT_DIR:-}"
  if [ -n "${component_dir}" ] && [ "${component_dir}" != "." ]; then
    printf '%s/%s\n' "$(pwd)" "${component_dir}"
  else
    pwd
  fi
}

resolve_pr_target_repo() {
  if [ -n "${PR_HEAD_REPO:-}" ]; then
    printf '%s\n' "${PR_HEAD_REPO}"
  else
    printf '%s\n' "${GITHUB_REPOSITORY}"
  fi
}

resolve_pr_target_branch() {
  if [ -n "${GITHUB_HEAD_REF:-}" ]; then
    printf '%s\n' "${GITHUB_HEAD_REF}"
  elif [ -n "${GITHUB_REF_NAME:-}" ]; then
    printf '%s\n' "${GITHUB_REF_NAME}"
  else
    git rev-parse --abbrev-ref HEAD 2>/dev/null || true
  fi
}

build_github_remote_url() {
  local repo="$1"
  local token="${2:-}"

  if [ -n "${token}" ]; then
    printf 'https://x-access-token:%s@github.com/%s.git\n' "${token}" "${repo}"
  else
    printf 'https://github.com/%s.git\n' "${repo}"
  fi
}

resolve_push_target() {
  local repo="$1"
  local token="${2:-}"

  if [ -n "${token}" ]; then
    build_github_remote_url "${repo}" "${token}"
  elif [ "${repo}" = "${GITHUB_REPOSITORY:-}" ]; then
    printf 'origin\n'
  else
    build_github_remote_url "${repo}"
  fi
}

# Sort commands into canonical order: audit → lint → test → refactor.
# Audit/lint/test are the core quality gates; real refactor commands run after
# them when explicitly requested. Fleet/deploy are operations commands handled
# separately by run-operations.sh and are filtered out here.
canonicalize_commands() {
  local commands="$1"
  local audit="" lint="" test="" refactor="" others=()
  local cmd base_cmd

  IFS=',' read -ra CMD_ARRAY <<< "${commands}"
  for cmd in "${CMD_ARRAY[@]}"; do
    cmd=$(echo "${cmd}" | xargs)
    base_cmd=$(printf '%s' "${cmd}" | awk '{print $1}')
    case "${base_cmd}" in
      audit)   audit="audit" ;;
      lint)    lint="lint" ;;
      test)    test="test" ;;
      refactor) refactor="${cmd}" ;;
      # Fleet/deploy are operations commands — handled by run-operations.sh
      fleet|deploy) ;;
      *)       others+=("${cmd}") ;;
    esac
  done

  local result=()
  [ -n "${audit}" ] && result+=("${audit}")
  [ -n "${lint}" ]  && result+=("${lint}")
  [ -n "${test}" ]  && result+=("${test}")
  [ -n "${refactor}" ] && result+=("${refactor}")
  result+=("${others[@]+"${others[@]}"}")

  local IFS=','
  printf '%s\n' "${result[*]}"
}

has_lint_command() {
  local commands="$1"
  local cmd
  IFS=',' read -ra CMD_ARRAY <<< "${commands}"

  for cmd in "${CMD_ARRAY[@]}"; do
    if [ "$(echo "${cmd}" | xargs)" = "lint" ]; then
      printf '%s\n' "true"
      return 0
    fi
  done

  printf '%s\n' "false"
}

build_run_command() {
  local cmd="$1"
  local component_id="$2"
  local workspace="$3"
  local output_file="${4:-}"
  local full_cmd
  local global_flags=""

  # --output is a global flag and must appear before the subcommand
  # (clap global args don't propagate when placed after positional args)
  if [ -n "${output_file}" ]; then
    global_flags="--output ${output_file} "
  fi

  if [[ "${cmd}" == refactor* ]]; then
    full_cmd="homeboy ${global_flags}refactor ${component_id} ${cmd#refactor } --path ${workspace}"
  else
    full_cmd="homeboy ${global_flags}${cmd} ${component_id} --path ${workspace}"
  fi

  local scope
  scope="$(scope_flags_for "${cmd}")"
  [ -n "${scope}" ] && full_cmd="${full_cmd} ${scope}"

  if [ -n "${EXTRA_ARGS:-}" ]; then
    full_cmd="${full_cmd} ${EXTRA_ARGS}"
  fi

  printf '%s\n' "${full_cmd}"
}

build_review_report_command() {
  local component_id="$1"
  local workspace="$2"
  local full_cmd

  full_cmd="homeboy review ${component_id} --path ${workspace} --report=pr-comment"

  local scope
  scope="$(scope_flags_for "review")"
  [ -n "${scope}" ] && full_cmd="${full_cmd} ${scope}"

  printf '%s\n' "${full_cmd}"
}

command_output_stem() {
  local cmd="$1"
  local stem
  stem="$(printf '%s' "${cmd}" | sed -E 's/[^[:alnum:]._-]+/-/g; s/^-+//; s/-+$//')"
  stem="${stem#-}"
  stem="${stem%-}"
  if [ -z "${stem}" ]; then
    stem="homeboy-output"
  fi
  printf '%s\n' "${stem}"
}

build_autofix_command() {
  local fix_cmd="$1"
  local component_id="$2"
  local workspace="$3"
  local output_file="${4:-}"
  local full_cmd
  local global_flags=""

  # --output is a global flag and must appear before the subcommand
  if [ -n "${output_file}" ]; then
    global_flags="--output ${output_file} "
  fi

  if [[ "${fix_cmd}" == refactor* ]]; then
    full_cmd="homeboy ${global_flags}refactor ${component_id} ${fix_cmd#refactor } --path ${workspace}"
  else
    full_cmd="homeboy ${global_flags}${fix_cmd} ${component_id} --path ${workspace}"
  fi

  local scope
  scope="$(scope_flags_for "${fix_cmd}")"
  [ -n "${scope}" ] && full_cmd="${full_cmd} ${scope}"

  if [ -n "${EXTRA_ARGS:-}" ]; then
    full_cmd="${full_cmd} ${EXTRA_ARGS}"
  fi

  printf '%s\n' "${full_cmd}"
}
