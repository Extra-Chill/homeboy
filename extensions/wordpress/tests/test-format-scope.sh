#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORDPRESS_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "${TMPDIR}"' EXIT

COMPONENT_PATH="${TMPDIR}/component"
mkdir -p "${COMPONENT_PATH}/vendor/bin"

git -C "${TMPDIR}" init --quiet component

git -C "${COMPONENT_PATH}" config user.email test@example.com
git -C "${COMPONENT_PATH}" config user.name Test

printf '<?php\n$a = 1;\n' > "${COMPONENT_PATH}/changed.php"
printf '<?php\n$b = 1;\n' > "${COMPONENT_PATH}/clean.php"
git -C "${COMPONENT_PATH}" add changed.php clean.php
git -C "${COMPONENT_PATH}" commit --quiet -m init

printf '<?php\n$a = 2;\n' > "${COMPONENT_PATH}/changed.php"

cat > "${COMPONENT_PATH}/vendor/bin/phpcbf" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$@" > "$PHPCBF_ARGS_FILE"
SH
chmod +x "${COMPONENT_PATH}/vendor/bin/phpcbf"

PHPCBF_ARGS_FILE="${TMPDIR}/phpcbf-args" \
HOMEBOY_COMPONENT_PATH="${COMPONENT_PATH}" \
  "${WORDPRESS_DIR}/scripts/format.sh" changed.php >/tmp/homeboy-wordpress-format-test.log

if ! grep -Fxq "changed.php" "${TMPDIR}/phpcbf-args"; then
  printf 'FAIL: scoped file argument was not passed to phpcbf\n'
  exit 1
fi

if grep -Fxq "clean.php" "${TMPDIR}/phpcbf-args"; then
  printf 'FAIL: unscoped file was passed to phpcbf\n'
  exit 1
fi

printf 'PASS: format script uses core-provided target files\n'
