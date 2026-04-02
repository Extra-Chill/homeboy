#!/usr/bin/env bash
set -euo pipefail

if [[ -f ./build.sh ]]; then
  bash ./build.sh
  exit 0
fi

mkdir -p bin

if [[ -d ./cmd ]]; then
  built_any=false
  while IFS= read -r -d '' dir; do
    name="$(basename "$dir")"
    echo "Building ./cmd/$name -> ./bin/$name"
    go build -o "./bin/$name" "./cmd/$name"
    built_any=true
  done < <(find ./cmd -mindepth 1 -maxdepth 1 -type d -print0)

  if [[ "$built_any" == true ]]; then
    exit 0
  fi
fi

name="$(basename "$PWD")"
echo "Building current module -> ./bin/$name"
go build -o "./bin/$name" .
