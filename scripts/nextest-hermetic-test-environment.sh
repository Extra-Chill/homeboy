#!/bin/sh
set -eu

# Cargo owns the shared target for compilation. Test binaries must not inherit
# that production routing because several hermetic fixtures resolve source
# binaries and paths from their own isolated workspace.
unset CARGO_TARGET_DIR

# CI itself runs through Homeboy and exports its runtime root. The test harness
# owns its runtime root, so inheriting the controller's makes fixture state land
# outside each test's isolated home.
unset HOMEBOY_RUNTIME_TMPDIR
unset TMPDIR TMP TEMP
exec "$@"
