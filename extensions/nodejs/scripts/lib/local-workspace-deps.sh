#!/usr/bin/env bash
# Compatibility shim: the local workspace dependency helper is shared by the
# nodejs and wordpress extensions and lives in the collection's scripts/lib,
# which ships with every extension (a lone wordpress install has no nodejs/).
# shellcheck source=../../../scripts/lib/local-workspace-deps.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../scripts/lib" && pwd)/local-workspace-deps.sh"
