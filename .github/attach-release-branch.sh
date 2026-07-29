#!/usr/bin/env bash
set -euo pipefail

EVENT_BRANCH="${1:?event branch is required}"
EVENT_SHA="${2:?event SHA is required}"
HEAD_SHA="$(git rev-parse HEAD)"

if [ "${EVENT_BRANCH}" != "main" ] || [ "${HEAD_SHA}" != "${EVENT_SHA}" ]; then
  echo "Refusing release identity branch=${EVENT_BRANCH} event=${EVENT_SHA} checkout=${HEAD_SHA}" >&2
  exit 1
fi

git switch --force-create "${EVENT_BRANCH}" "${EVENT_SHA}"
test "$(git branch --show-current)" = "${EVENT_BRANCH}"
test "$(git rev-parse HEAD)" = "${EVENT_SHA}"
