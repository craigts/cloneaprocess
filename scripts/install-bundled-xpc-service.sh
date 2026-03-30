#!/usr/bin/env bash

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
app_bundle_path="${1:-$repo_root/apps/desktop/src-tauri/target/release/bundle/macos/Clone a Process.app}"
service_identifier="${CLONEAPROCESS_RECORDER_BUNDLED_SERVICE_ID:-com.cloneaprocess.recorder}"
service_name="RecorderService"
source_binary="$repo_root/apps/desktop/src-tauri/resources/macos/$service_name"
service_root="$app_bundle_path/Contents/XPCServices/$service_name.xpc"
service_contents="$service_root/Contents"
service_binary_dir="$service_contents/MacOS"
service_binary_path="$service_binary_dir/$service_name"
service_plist_path="$service_contents/Info.plist"
app_identifier="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$app_bundle_path/Contents/Info.plist")"

if [[ ! -d "$app_bundle_path" ]]; then
  echo "[cloneaprocess] app bundle missing at $app_bundle_path" >&2
  exit 1
fi

if [[ ! -x "$source_binary" ]]; then
  echo "[cloneaprocess] staged recorder helper missing at $source_binary" >&2
  exit 1
fi

mkdir -p "$service_binary_dir"
install -m 755 "$source_binary" "$service_binary_path"

cat > "$service_plist_path" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key>
  <string>en</string>
  <key>CFBundleExecutable</key>
  <string>${service_name}</string>
  <key>CFBundleIdentifier</key>
  <string>${service_identifier}</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundlePackageType</key>
  <string>XPC!</string>
  <key>CFBundleShortVersionString</key>
  <string>0.1.0</string>
  <key>CFBundleVersion</key>
  <string>1</string>
  <key>XPCService</key>
  <dict>
    <key>JoinExistingSession</key>
    <true/>
    <key>RunLoopType</key>
    <string>NSRunLoop</string>
  </dict>
</dict>
</plist>
PLIST

codesign --force --sign - --timestamp=none --identifier "$service_identifier" "$service_root"
codesign --force --sign - --timestamp=none --identifier "$app_identifier" "$app_bundle_path"

echo "[cloneaprocess] installed bundled XPC service at $service_root"
