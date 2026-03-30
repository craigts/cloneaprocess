#!/usr/bin/env bash

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
service_name="${CLONEAPROCESS_RECORDER_XPC_SERVICE:-com.cloneaprocess.recorder.dev}"
label="${CLONEAPROCESS_RECORDER_XPC_LABEL:-$service_name}"
staged_binary_path="$repo_root/apps/desktop/src-tauri/resources/macos/RecorderService"
fallback_binary_path="$repo_root/native/mac-recorder-service/.build/debug/RecorderService"
binary_path="${CLONEAPROCESS_RECORDER_HELPER_PATH:-}"
launch_agents_dir="${HOME}/Library/LaunchAgents"
plist_path="${launch_agents_dir}/${label}.plist"
domain="gui/$(id -u)"

if [[ -z "$binary_path" ]]; then
  if [[ -x "$staged_binary_path" ]]; then
    binary_path="$staged_binary_path"
  else
    binary_path="$fallback_binary_path"
  fi
fi

if [[ ! -x "$binary_path" ]]; then
  echo "[cloneaprocess] recorder binary missing at $binary_path" >&2
  exit 1
fi

mkdir -p "$launch_agents_dir"

cat > "$plist_path" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>${label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>${binary_path}</string>
    <string>--mach-service-name</string>
    <string>${service_name}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>MachServices</key>
  <dict>
    <key>${service_name}</key>
    <true/>
  </dict>
  <key>StandardOutPath</key>
  <string>/tmp/${label}.stdout.log</string>
  <key>StandardErrorPath</key>
  <string>/tmp/${label}.stderr.log</string>
</dict>
</plist>
PLIST

launchctl bootout "${domain}/${label}" >/dev/null 2>&1 || true
launchctl bootstrap "$domain" "$plist_path"
launchctl kickstart -k "${domain}/${label}"

echo "[cloneaprocess] recorder XPC launch agent ready: ${service_name}"
