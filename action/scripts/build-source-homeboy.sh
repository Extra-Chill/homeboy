#!/usr/bin/env bash

set -euo pipefail

echo "Building homeboy from source at ${SOURCE_PATH}..."

BUILD_EXIT=0
cargo build --release --manifest-path "${SOURCE_PATH}/Cargo.toml" 2>&1 || BUILD_EXIT=$?

if [ "${BUILD_EXIT}" -eq 0 ]; then
  BINARY=$(find "${SOURCE_PATH}/target/release" -maxdepth 1 -name "homeboy" -type f | head -1)
  if [ -n "${BINARY}" ]; then
    chmod +x "${BINARY}"
    sudo cp "${BINARY}" /usr/local/bin/homeboy
    echo "Built from source: $(homeboy --version)"
    echo "built=true" >> "${GITHUB_OUTPUT}"
  else
    echo "::warning::Source build succeeded but binary not found — falling back to release"
    echo "built=false" >> "${GITHUB_OUTPUT}"
  fi
else
  echo "::warning::Source build failed (exit ${BUILD_EXIT}) — falling back to release binary"
  echo "built=false" >> "${GITHUB_OUTPUT}"
fi
