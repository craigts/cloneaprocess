#!/usr/bin/env bash

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

echo "[cloneaprocess] building mac-recorder-service (incremental)"
cd "$repo_root/native/mac-recorder-service"
swift build

echo "[cloneaprocess] building mac-runner-service (incremental)"
cd "$repo_root/native/mac-runner-service"
swift build

echo "[cloneaprocess] launching desktop app"
cd "$repo_root"
npm run tauri:dev --workspace @cloneaprocess/desktop
