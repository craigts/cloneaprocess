#!/usr/bin/env bash

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
resource_dir="$repo_root/apps/desktop/src-tauri/resources/macos"
recorder_binary="$repo_root/native/mac-recorder-service/.build/debug/RecorderService"
runner_binary="$repo_root/native/mac-runner-service/.build/debug/RunnerService"

mkdir -p "$resource_dir"

echo "[cloneaprocess] building mac-recorder-service (incremental)"
cd "$repo_root/native/mac-recorder-service"
swift build

echo "[cloneaprocess] building mac-runner-service (incremental)"
cd "$repo_root/native/mac-runner-service"
swift build

install -m 755 "$recorder_binary" "$resource_dir/RecorderService"
install -m 755 "$runner_binary" "$resource_dir/RunnerService"

echo "[cloneaprocess] staged native helpers in $resource_dir"
