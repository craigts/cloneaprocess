#!/usr/bin/env bash

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
vite_port=5173
recorder_xpc_service="${CLONEAPROCESS_RECORDER_XPC_SERVICE:-com.cloneaprocess.recorder.dev}"

cleanup_port_listener() {
  local port="$1"
  local pids
  pids="$(lsof -tiTCP:"$port" -sTCP:LISTEN 2>/dev/null || true)"
  if [[ -n "$pids" ]]; then
    echo "[cloneaprocess] stopping stale listener on port $port: $pids"
    kill $pids 2>/dev/null || true
    sleep 1

    pids="$(lsof -tiTCP:"$port" -sTCP:LISTEN 2>/dev/null || true)"
    if [[ -n "$pids" ]]; then
      echo "[cloneaprocess] force stopping listener on port $port: $pids"
      kill -9 $pids 2>/dev/null || true
      sleep 1
    fi

    pids="$(lsof -tiTCP:"$port" -sTCP:LISTEN 2>/dev/null || true)"
    if [[ -n "$pids" ]]; then
      echo "[cloneaprocess] port $port is still occupied: $pids" >&2
      exit 1
    fi
  fi
}

cleanup_port_listener "$vite_port"

echo "[cloneaprocess] staging native helpers"
cd "$repo_root"
bash "$repo_root/scripts/stage-native-helpers.sh"

echo "[cloneaprocess] bootstrapping recorder XPC launch agent"
export CLONEAPROCESS_RECORDER_HELPER_PATH="$repo_root/apps/desktop/src-tauri/resources/macos/RecorderService"
bash "$repo_root/scripts/bootstrap-recorder-xpc.sh"

cleanup_port_listener "$vite_port"

echo "[cloneaprocess] launching desktop app"
cd "$repo_root"
export CLONEAPROCESS_RECORDER_TRANSPORT="xpc_service"
export CLONEAPROCESS_RECORDER_XPC_SERVICE="$recorder_xpc_service"
npm run tauri:dev --workspace @cloneaprocess/desktop
