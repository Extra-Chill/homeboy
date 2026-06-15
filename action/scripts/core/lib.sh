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

should_enforce_source_binary_freshness() {
  [ "${BINARY_SOURCE:-}" = "source" ] && [ -n "${HOMEBOY_CLI_HEAD_SHA:-}" ]
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
        "directory_sprawl": "Directory sprawl reduced",
        "source_change": "Source files changed"
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

    ($insertions + $new_files + $proposals + $collected_edits + $decompose) as $structured |

    # RefactorSourceRun can report applied changed_files without collected_edits
    # when an extension applies fixes but does not emit the fix-results sidecar.
    [ .[] | select((.command // "") == "refactor.sources" and (.applied // false)) |
      (.changed_files // [])[] | {cat: "source_change", file: .}
    ] as $source_changes |
    [ .[] | select(((.autofix.changed_files // []) | length) > 0) |
      (.autofix.changed_files // [])[] | {cat: "source_change", file: .}
    ] as $lint_autofix_changes |

    # Prefer precise structured rows. Fall back to changed_files only when the
    # source run would otherwise look like a metadata-only change.
    (if ($structured | length) > 0 then $structured else ($source_changes + $lint_autofix_changes) end) |
    group_by(.cat) |
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

# Extract machine-readable autofix rows from homeboy JSON output files.
# Format: finding_type<TAB>count<TAB>comma-separated affected files.
extract_fix_report_from_output() {
  local output_dir="$1"

  local json_files
  json_files=$(find "${output_dir}" -name '*.json' -type f 2>/dev/null)
  if [ -z "${json_files}" ]; then
    return
  fi

  # shellcheck disable=SC2086
  jq -rsr '
    [.[] | .data // . ] | map(select(type == "object")) |

    def category:
      if type == "string" then .
      elif type == "object" then (keys[0] // "unknown")
      else "unknown"
      end;

    [.[].fixes // [] | .[] | .file as $file |
      .insertions[]? | {cat: (.kind | category), file: $file}
    ] as $insertions |
    [.[].new_files // [] | .[] | {cat: .finding, file}] as $new_files |
    [.[].proposals // [] | .[] | {cat: .rule_id, file}] as $proposals |
    [.[].collected_edits // [] | .[] | {cat: .rule_id, file}] as $collected_edits |
    [.[].decompose_plans // [] | .[] | {cat: .source_finding, file}] as $decompose |

    ($insertions + $new_files + $proposals + $collected_edits + $decompose) as $structured |
    [ .[] | select((.command // "") == "refactor.sources" and (.applied // false)) |
      (.changed_files // [])[] | {cat: "source_change", file: .}
    ] as $source_changes |
    [ .[] | select(((.autofix.changed_files // []) | length) > 0) |
      (.autofix.changed_files // [])[] | {cat: "source_change", file: .}
    ] as $lint_autofix_changes |

    (if ($structured | length) > 0 then $structured else ($source_changes + $lint_autofix_changes) end) |
    group_by(.cat) |
    map({
      cat: .[0].cat,
      count: length,
      files: ([.[].file] | unique | sort)
    }) |
    sort_by([-.count, .cat]) |
    .[] |
    [.cat, (.count | tostring), (.files | join(", "))] | @tsv
  ' ${json_files} 2>/dev/null || true
}

write_multiline_output() {
  local key="$1"
  local value="${2:-}"
  local marker="__HOMEBOY_${key}_$(date +%s%N)__"

  {
    printf '%s<<%s\n' "${key}" "${marker}"
    printf '%s\n' "${value}"
    printf '%s\n' "${marker}"
  } >> "${GITHUB_OUTPUT}"
}

autofix_changed_files_list() {
  printf '%s\n' "${AUTOFIX_CHANGED_FILES:-}" | sed '/^[[:space:]]*$/d' | sort -u
}

autofix_source_files_from_report() {
  printf '%s\n' "${AUTOFIX_REPORT:-}" | awk -F '\t' 'NF >= 3 { print $3 }' |
    tr ',' '\n' | sed 's/^[[:space:]]*//; s/[[:space:]]*$//' | sed '/^$/d' | sort -u
}

autofix_other_files() {
  local changed tmp_changed tmp_source
  tmp_changed="$(mktemp)"
  tmp_source="$(mktemp)"
  autofix_changed_files_list > "${tmp_changed}"
  autofix_source_files_from_report > "${tmp_source}"
  changed="$(comm -23 "${tmp_changed}" "${tmp_source}" || true)"
  rm -f "${tmp_changed}" "${tmp_source}"
  printf '%s\n' "${changed}" | sed '/^[[:space:]]*$/d'
}

autofix_is_metadata_change() {
  local file="$1"
  case "${file}" in
    homeboy.json|*.json|*.yml|*.yaml|*.toml|*.lock)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

autofix_review_files() {
  local workspace="${1:-$(resolve_workspace)}"
  local changed drift_files file

  changed="$(autofix_changed_files_list)"
  [ -n "${changed}" ] || return 0
  drift_files="$(drift_file_paths "${workspace}" | sort -u)"

  while IFS= read -r file; do
    [ -n "${file}" ] || continue
    if printf '%s\n' "${drift_files}" | grep -Fx -- "${file}" >/dev/null 2>&1; then
      continue
    fi
    if autofix_is_metadata_change "${file}"; then
      continue
    fi
    printf '%s\n' "${file}"
  done <<< "${changed}"
}

autofix_stage_source_files() {
  local workspace="${1:-$(resolve_workspace)}"
  local reported previous_changed

  reported="$(autofix_source_files_from_report)"
  if [ -n "${reported}" ]; then
    printf '%s\n' "${reported}"
    return 0
  fi

  previous_changed="${AUTOFIX_CHANGED_FILES-}"
  AUTOFIX_CHANGED_FILES="$(git_changed_files)"
  autofix_review_files "${workspace}"
  AUTOFIX_CHANGED_FILES="${previous_changed}"
}

autofix_unreported_review_files() {
  local workspace="${1:-$(resolve_workspace)}"
  local other drift_files file

  other="$(autofix_other_files)"
  [ -n "${other}" ] || return 0
  drift_files="$(drift_file_paths "${workspace}" | sort -u)"

  while IFS= read -r file; do
    [ -n "${file}" ] || continue
    if printf '%s\n' "${drift_files}" | grep -Fx -- "${file}" >/dev/null 2>&1; then
      continue
    fi
    if autofix_is_metadata_change "${file}"; then
      continue
    fi
    printf '%s\n' "${file}"
  done <<< "${other}"
}

autofix_synthesize_source_report() {
  local workspace="${1:-$(resolve_workspace)}"
  local files count joined

  files="$(autofix_review_files "${workspace}")"
  [ -n "${files}" ] || return 0
  count="$(printf '%s\n' "${files}" | sed '/^[[:space:]]*$/d' | wc -l | xargs)"
  joined="$(printf '%s\n' "${files}" | awk 'NR == 1 { out = $0; next } { out = out ", " $0 } END { print out }')"
  printf 'source_change\t%s\t%s\n' "${count}" "${joined}"
}

autofix_reconcile_source_report_to_changed_files() {
  local changed_file
  local changed_files="${AUTOFIX_CHANGED_FILES:-}"
  local reconciled=""

  [ -n "${AUTOFIX_REPORT:-}" ] || return 0
  [ -n "${changed_files}" ] || return 0

  while IFS=$'\t' read -r cat count files; do
    [ -n "${cat}" ] || continue

    if [ "${cat}" != "source_change" ]; then
      reconciled+="${cat}"$'\t'"${count}"$'\t'"${files}"$'\n'
      continue
    fi

    local kept=""
    local kept_count=0
    while IFS= read -r changed_file; do
      [ -n "${changed_file}" ] || continue
      if printf '%s\n' "${files}" | tr ',' '\n' | sed 's/^[[:space:]]*//; s/[[:space:]]*$//' | grep -Fx -- "${changed_file}" >/dev/null 2>&1; then
        if [ -n "${kept}" ]; then
          kept+=", "
        fi
        kept+="${changed_file}"
        kept_count=$((kept_count + 1))
      fi
    done <<< "$(autofix_changed_files_list)"

    if [ "${kept_count}" -gt 0 ]; then
      reconciled+="${cat}"$'\t'"${kept_count}"$'\t'"${kept}"$'\n'
    fi
  done <<< "${AUTOFIX_REPORT}"

  AUTOFIX_REPORT="$(printf '%s' "${reconciled}" | sed '/^[[:space:]]*$/d')"
}

autofix_prepare_pr_report() {
  local workspace="${1:-$(resolve_workspace)}"
  local total_fixes="${AUTOFIX_TOTAL_FIXES:-0}"

  if [ -z "${AUTOFIX_REPORT:-}" ] && [ "${total_fixes}" != "0" ]; then
    AUTOFIX_REPORT="$(autofix_synthesize_source_report "${workspace}")"
  fi

  autofix_reconcile_source_report_to_changed_files
}

autofix_pr_has_unsafe_unreported_changes() {
  local workspace="${1:-$(resolve_workspace)}"
  local unreported

  unreported="$(autofix_unreported_review_files "${workspace}")"
  [ -n "${unreported}" ]
}

autofix_count_report_rows() {
  printf '%s\n' "${AUTOFIX_REPORT:-}" | sed '/^[[:space:]]*$/d' | wc -l | xargs
}

autofix_total_report_fixes() {
  printf '%s\n' "${AUTOFIX_REPORT:-}" | awk -F '\t' 'NF >= 2 { total += $2 } END { print total + 0 }'
}

autofix_markdown_file_list() {
  local files="$1"
  local rendered=""
  local file
  while IFS= read -r file; do
    [ -n "${file}" ] || continue
    if [ -n "${rendered}" ]; then
      rendered+=" "
    fi
    rendered+="\`${file}\`"
  done <<< "$(printf '%s\n' "${files}" | tr ',' '\n' | sed 's/^[[:space:]]*//; s/[[:space:]]*$//' | sed '/^$/d')"
  printf '%s\n' "${rendered}"
}

autofix_render_other_change() {
  local file="$1"
  case "${file}" in
    homeboy.json)
      printf '%s\n' '- `homeboy.json` audit baseline metadata was refreshed.'
      ;;
    *.json|*.yml|*.yaml|*.toml|*.lock)
      printf '%s\n' "- \`${file}\` configuration or metadata was refreshed."
      ;;
    *)
      printf '%s\n' "- \`${file}\` changed outside the reported source-fix set."
      ;;
  esac
}

baseline_file_path() {
  local workspace="${1:-$(resolve_workspace)}"
  local prefix

  prefix="$(git -C "${workspace}" rev-parse --show-prefix 2>/dev/null || true)"
  printf '%shomeboy.json\n' "${prefix}"
}

# Files whose changes on `main` are merge-aftermath drift, never authored
# changes — the audit baseline (`homeboy.json`) plus extension-declared
# lockfiles (`Cargo.lock`, `composer.lock`, etc.) and any component-declared
# extras. All review-worthless. When autofix sees ONLY drift in the working
# tree, we commit it directly to the base branch instead of opening a PR.
# When autofix produces real source fixes, we strip drift files so they
# don't pile onto a reviewable change.
#
# The drift list comes from homeboy core via `homeboy component show`,
# which composes:
#   1. `homeboy.json` (always — the audit baseline file).
#   2. `build.lockfile_paths` from each enabled extension manifest.
#   3. `extra_drift_files` from the component's homeboy.json.
#
# Homeboy returns workspace-relative paths. We prefix with the component's
# git-root-relative directory so the resulting paths match `git diff
# --name-only` output (which is always repo-root-relative).
#
# Emits one path per line, repo-root-relative. Existence is the caller's
# responsibility — `drift_file_paths` includes paths that may not exist on
# disk (e.g. a wordpress component without a yarn.lock); push/restore
# helpers existence-check before staging.
#
# Falls back to a `homeboy.json`-only list when `homeboy component show`
# is unavailable or doesn't expose `drift_files` (older homeboy versions),
# preserving the pre-#209 behavior.
drift_file_paths() {
  local workspace="${1:-$(resolve_workspace)}"
  local prefix
  prefix="$(git -C "${workspace}" rev-parse --show-prefix 2>/dev/null || true)"

  local raw_paths=""

  if command -v homeboy >/dev/null 2>&1; then
    local show_output
    show_output="$(homeboy component show --path "${workspace}" 2>/dev/null || true)"
    if [ -n "${show_output}" ]; then
      raw_paths="$(printf '%s' "${show_output}" | jq -r '.data.entity.drift_files // empty | .[]?' 2>/dev/null || true)"
    fi
  fi

  if [ -z "${raw_paths}" ]; then
    # Older homeboy or no homeboy on PATH — emit just the audit baseline.
    printf '%shomeboy.json\n' "${prefix}"
    return 0
  fi

  while IFS= read -r path; do
    [ -n "${path}" ] || continue
    case "${path}" in
      /*)
        # Defensive: skip absolute paths the resolver shouldn't have emitted.
        continue
        ;;
      *)
        printf '%s%s\n' "${prefix}" "${path}"
        ;;
    esac
  done <<< "${raw_paths}"
}

git_changed_files() {
  git diff --name-only HEAD -- | sed '/^[[:space:]]*$/d' | sort -u
}

changes_are_only_drift() {
  local workspace="${1:-$(resolve_workspace)}"
  local changed drift_files diff_extra

  changed="$(git_changed_files)"
  [ -n "${changed}" ] || return 1

  drift_files="$(drift_file_paths "${workspace}" | sort -u)"

  # Every changed file must be in the drift set. `comm -23` lists lines in
  # changed-but-not-drift; non-empty means there's a non-drift change.
  diff_extra="$(comm -23 <(printf '%s\n' "${changed}") <(printf '%s\n' "${drift_files}"))"
  [ -z "${diff_extra}" ]
}

# Backwards-compat alias used by tests written against the old name.
changes_are_only_audit_baseline() {
  changes_are_only_drift "$@"
}

build_autofix_pr_body() {
  local run_url="$1"
  local branch="$2"
  local base_branch="$3"

  local changed_count="${AUTOFIX_FILE_COUNT:-0}"
  local total_fixes row_count
  total_fixes="$(autofix_total_report_fixes)"
  row_count="$(autofix_count_report_rows)"

  printf '## Summary\n'
  if [ "${total_fixes}" != "0" ]; then
    local label="finding"
    [ "${total_fixes}" = "1" ] || label="findings"
    if [ "${row_count}" = "1" ]; then
      local only_type
      only_type="$(printf '%s\n' "${AUTOFIX_REPORT}" | awk -F '\t' 'NF >= 1 { print $1; exit }')"
      if [ "${only_type}" = "source_change" ]; then
        local source_label="source file"
        [ "${total_fixes}" = "1" ] || source_label="source files"
        printf -- '- Applied autofix changes to **%s** %s.\n' "${total_fixes}" "${source_label}"
      else
        printf -- '- Fixed **%s** `%s` %s.\n' "${total_fixes}" "${only_type}" "${label}"
      fi
    else
      printf -- '- Fixed **%s** source %s across **%s** finding types.\n' "${total_fixes}" "${label}" "${row_count}"
    fi
  else
    printf -- '- No source fixes were reported by the autofix output.\n'
  fi
  printf -- '- Changed **%s** files.\n\n' "${changed_count}"

  if [ "${total_fixes}" != "0" ]; then
    printf '## Automated Fixes\n'
    printf '| Finding type | Count | Files |\n'
    printf '|---|---:|---|\n'
    while IFS=$'\t' read -r cat count files; do
      [ -n "${cat}" ] || continue
      printf '| `%s` | %s | %s |\n' "${cat}" "${count}" "$(autofix_markdown_file_list "${files}")"
    done <<< "${AUTOFIX_REPORT}"
    printf '\n'
  fi

  local other_files
  other_files="$(autofix_other_files)"
  if [ -n "${other_files}" ]; then
    printf '## Other Changes\n'
    while IFS= read -r file; do
      [ -n "${file}" ] || continue
      autofix_render_other_change "${file}"
    done <<< "${other_files}"
    printf '\n'
  fi

  printf '## Verification\n'
  printf -- '- Autofix branch pushed; use the PR branch checks as merge verification.\n'
  printf -- '- Workflow run: %s\n\n' "${run_url}"

  printf '## Context\n'
  printf -- '- Branch: %s\n' "${branch}"
  printf -- '- Base: %s\n' "${base_branch}"
  printf -- '- Generated automatically by Homeboy Action.\n'
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

# Sort commands into canonical order: audit → lint → test → refactor → bench.
# Audit/lint/test are the core quality gates; real refactor and bench commands
# run after them when explicitly requested. Release and operations commands are
# handled by dedicated steps and are filtered out here defensively.
canonicalize_commands() {
  local commands="$1"
  local audit="" lint="" test="" refactor="" bench="" others=()
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
      bench) bench="${cmd}" ;;
      release|fleet|deploy) ;;
      *)       others+=("${cmd}") ;;
    esac
  done

  local result=()
  [ -n "${audit}" ] && result+=("${audit}")
  [ -n "${lint}" ]  && result+=("${lint}")
  [ -n "${test}" ]  && result+=("${test}")
  [ -n "${refactor}" ] && result+=("${refactor}")
  [ -n "${bench}" ] && result+=("${bench}")
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

settings_json_flags() {
  local raw="${HOMEBOY_SETTINGS_JSON:-}"
  if [ -z "${raw}" ] || [ "${raw}" = "{}" ]; then
    return 0
  fi

  if ! printf '%s' "${raw}" | jq -e 'type == "object"' >/dev/null 2>&1; then
    echo "::warning::HOMEBOY_SETTINGS_JSON must be a JSON object; ignoring settings override"
    return 0
  fi

  local key
  local value
  local flag_value
  while IFS=$'\t' read -r key value; do
    [ -n "${key}" ] || continue
    flag_value="${key}=${value}"
    printf ' --setting-json %q' "${flag_value}"
  done < <(printf '%s' "${raw}" | jq -r 'to_entries[] | [.key, (.value | tojson)] | @tsv')
}

homeboy_global_flags() {
  local output_file="${1:-}"
  local flags=""

  if [ "${GITHUB_ACTIONS:-}" = "true" ]; then
    flags="${flags}--force-hot --allow-local-hot "
  fi

  # --output is a global flag and must appear before the subcommand
  # (clap global args don't propagate when placed after positional args)
  if [ -n "${output_file}" ]; then
    flags="${flags}--output ${output_file} "
  fi

  printf '%s' "${flags}"
}

build_run_command() {
  local cmd="$1"
  local component_id="$2"
  local workspace="$3"
  local output_file="${4:-}"
  local full_cmd
  local global_flags
  global_flags="$(homeboy_global_flags "${output_file}")"

  if [[ "${cmd}" == refactor* ]]; then
    full_cmd="homeboy ${global_flags}refactor ${component_id} ${cmd#refactor } --path ${workspace}"
  elif [[ "${cmd}" == bench* ]]; then
    local bench_args
    bench_args="$(printf '%s' "${cmd#bench}" | xargs)"
    full_cmd="homeboy ${global_flags}bench ${component_id}"
    [ -n "${bench_args}" ] && full_cmd="${full_cmd} ${bench_args}"
    full_cmd="${full_cmd} --path ${workspace}"
  else
    full_cmd="homeboy ${global_flags}${cmd} ${component_id} --path ${workspace}"
  fi

  local scope
  scope="$(scope_flags_for "${cmd}")"
  [ -n "${scope}" ] && full_cmd="${full_cmd} ${scope}"

  if [ -n "${EXTRA_ARGS:-}" ]; then
    full_cmd="${full_cmd} ${EXTRA_ARGS}"
  fi

  if [[ "${cmd}" == bench* ]]; then
    if [ -n "${BENCH_RIG:-}" ]; then
      full_cmd="${full_cmd} --rig ${BENCH_RIG}"
    fi
    if [ -n "${BENCH_SCENARIO:-}" ]; then
      full_cmd="${full_cmd} --scenario ${BENCH_SCENARIO}"
    fi
    if [ -n "${BENCH_RUNS:-}" ]; then
      full_cmd="${full_cmd} --runs ${BENCH_RUNS}"
    fi
    if [ -n "${BENCH_ITERATIONS:-}" ]; then
      full_cmd="${full_cmd} --iterations ${BENCH_ITERATIONS}"
    fi
    if [ -n "${BENCH_REGRESSION_THRESHOLD:-}" ]; then
      full_cmd="${full_cmd} --regression-threshold ${BENCH_REGRESSION_THRESHOLD}"
    fi
    full_cmd="${full_cmd}$(settings_json_flags)"
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

build_observation_export_command() {
  local since_window="$1"
  local output_dir="$2"

  printf 'homeboy runs export --since %s --output %s\n' "${since_window}" "${output_dir}"
}

build_observation_import_command() {
  local import_dir="$1"

  printf 'homeboy runs import %s\n' "${import_dir}"
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
  local global_flags
  global_flags="$(homeboy_global_flags "${output_file}")"

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
