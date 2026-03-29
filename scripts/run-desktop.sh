#!/usr/bin/env bash

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
vite_port=5173

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

echo "[cloneaprocess] building mac-recorder-service (incremental)"
cd "$repo_root/native/mac-recorder-service"
swift build

echo "[cloneaprocess] building mac-runner-service (incremental)"
cd "$repo_root/native/mac-runner-service"
swift build

cleanup_port_listener "$vite_port"

echo "[cloneaprocess] launching desktop app"
cd "$repo_root"
npm run tauri:dev --workspace @cloneaprocess/desktop
