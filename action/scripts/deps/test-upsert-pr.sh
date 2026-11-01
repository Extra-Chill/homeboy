#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir "$tmp/bin"
export LOG="$tmp/log" GITHUB_REPOSITORY=org/consumer GITHUB_RUN_ID=1
export GH_TOKEN=test BASE_BRANCH=main GATE_OUTCOME=failure RUNNER_TEMP="$tmp"
cat > "$tmp/bin/git" <<'EOF'
#!/usr/bin/env bash
printf 'git %s\n' "$*" >> "$LOG"
case "$1" in
  rev-parse) echo base ;;
  ls-remote) [[ "$*" != *refs/heads/main* ]] || printf 'base\trefs/heads/main\n' ;;
  diff) exit 1 ;;
esac
EOF
cat > "$tmp/bin/gh" <<'EOF'
#!/usr/bin/env bash
printf 'gh %s\n' "$*" >> "$LOG"
if [[ "$*" == 'pr list '* ]]; then printf '%s' "${PR_NUMBER:-}"; fi
EOF
chmod +x "$tmp/bin/"*
export PATH="$tmp/bin:$PATH"
for number in '' 42; do
  export PR_NUMBER="$number"
  echo 'Dependency summary' > "$tmp/deps-refresh.md"
  bash "$root/scripts/deps/upsert-pr.sh"
  grep -q 'Gates: \*\*failure\*\*' "$tmp/deps-refresh.md"
done
grep -q 'git push --force-with-lease=refs/heads/homeboy/deps-refresh:' "$LOG"
grep -q 'gh pr create' "$LOG"
grep -q 'gh pr edit 42' "$LOG"
echo 'PASS: stable branch, lease, create/update, failed gate disclosure'
