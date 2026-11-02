#!/usr/bin/env bash
set -euo pipefail
EXTENSION_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$EXTENSION_DIR/scripts/lib/project-scripts.sh"
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT
mkdir -p "$fixture/packages/client" "$fixture/bin"
printf '%s\n' '{"private":true,"workspaces":["packages/*"]}' > "$fixture/package.json"
printf '%s\n' '{"name":"workspace-client","version":"1.0.0","scripts":{"validate":"true"}}' > "$fixture/packages/client/package.json"
printf '{}\n' > "$fixture/package-lock.json"
# Record cwd without network or registry access.
printf '#!/usr/bin/env bash\nprintf "%%s:%%s\\n" "$PWD" "$*" >> "$NPM_LOG"\n' > "$fixture/bin/npm"
chmod +x "$fixture/bin/npm"
export NPM_LOG="$fixture/npm.log"
export PATH="$fixture/bin:$PATH"
member="$fixture/packages/client"
homeboy_project_init --ecosystem nodejs --path "$member"
[[ "$HOMEBOY_PROJECT_ROOT" = "$member" ]]
[[ "$HOMEBOY_PROJECT_DEPENDENCY_ROOT" = "$fixture" ]]
HOMEBOY_COMPONENT_PATH="$member" bash "$EXTENSION_DIR/scripts/deps/deps-runner.sh" status > "$fixture/status.json"
node -e 'const s = require(process.argv[1]); if (s.package_manager !== "npm" || !s.lockfiles.includes("package-lock.json")) process.exit(1)' "$fixture/status.json"
(cd "$member" && bash "$EXTENSION_DIR/scripts/release/validate.sh")
grep -Fx "$fixture:ci" "$NPM_LOG"
grep -Fx "$member:run validate" "$NPM_LOG"
# Object-form workspaces must also work.
printf '%s\n' '{"workspaces":{"packages":["packages/*"]}}' > "$fixture/package.json"
homeboy_project_init --ecosystem nodejs --path "$member"
[[ "$HOMEBOY_PROJECT_DEPENDENCY_ROOT" = "$fixture" ]]
# A member-local lockfile wins over the workspace fallback.
printf '{}\n' > "$member/package-lock.json"
homeboy_project_init --ecosystem nodejs --path "$member"
[[ "$HOMEBOY_PROJECT_DEPENDENCY_ROOT" = "$member" ]]
rm "$member/package-lock.json"
# Unrelated ancestors cannot supply a lockfile.
printf '%s\n' '{"workspaces":["other/*"]}' > "$fixture/package.json"
homeboy_project_init --ecosystem nodejs --path "$member"
[[ "$HOMEBOY_PROJECT_DEPENDENCY_ROOT" = "$member" ]]
printf 'npm workspace release smoke: passed\n'
