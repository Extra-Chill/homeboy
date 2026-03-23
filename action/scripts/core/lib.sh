#!/usr/bin/env bash

set -euo pipefail

# Source scope module for flag generation
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/../scope/flags.sh"

# Prefix used for all autofix commits. Loop guards grep for this prefix,
# so the subject line can vary after it (e.g. fix types, file count).
AUTOFIX_COMMIT_PREFIX="chore(ci): homeboy autofix"

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

    # Human-readable category names
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
        "missing_test_file": "Test files generated"
      }[.] // .;

    # Collect insertions with normalized category
    [.[].fixes // [] | .[] | .file as $file |
      .insertions[]? | {cat: (.kind | category), file: $file}
    ] as $insertions |

    # Collect new files
    [.[].new_files // [] | .[] | {cat: .finding, file}] as $new_files |

    # Combine and group by category
    ($insertions + $new_files) | group_by(.cat) |
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
# them when explicitly requested.
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

# Validate autofix changes compile before committing.
#
# Detects the project type and runs a compile check that covers test code.
# Returns 0 on success, 1 on failure (with changes reverted).
# If no known build tool is found, returns 0 (no validation possible).
validate_autofix_changes() {
  local workspace="${1:-.}"

  # Skip if no files changed
  if git diff --quiet && git diff --cached --quiet; then
    return 0
  fi

  local check_cmd=""

  # Rust: cargo check --tests validates #[cfg(test)] modules
  if [ -f "${workspace}/Cargo.toml" ]; then
    check_cmd="cargo check --tests"
  # TypeScript: tsc --noEmit validates all TypeScript
  elif [ -f "${workspace}/tsconfig.json" ]; then
    check_cmd="npx tsc --noEmit"
  # Go: go vet validates compilation
  elif [ -f "${workspace}/go.mod" ]; then
    check_cmd="go vet ./..."
  fi

  if [ -z "${check_cmd}" ]; then
    return 0
  fi

  echo "Validating autofix changes compile: ${check_cmd}"
  set +e
  local output
  output=$(cd "${workspace}" && eval "${check_cmd}" 2>&1)
  local exit_code=$?
  set -e

  if [ "${exit_code}" -ne 0 ]; then
    echo "::warning::Autofix generated code that does not compile — reverting all changes"
    echo "${output}" | tail -20
    git checkout -- .
    git clean -fd
    return 1
  fi

  echo "Compilation validation passed"
  return 0
}
