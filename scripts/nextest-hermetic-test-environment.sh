#!/bin/sh
set -eu

# Cargo owns the shared target for compilation. Test binaries must not inherit
# that production routing because several hermetic fixtures resolve source
# binaries and paths from their own isolated workspace.
unset CARGO_TARGET_DIR
exec "$@"
