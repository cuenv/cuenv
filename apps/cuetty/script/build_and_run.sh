#!/usr/bin/env bash
set -euo pipefail

MODE="${1:-run}"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_NAME="Cuetty"
PROCESS_NAME="cuetty"
BUNDLE_ID="com.cuenv.cuetty"
APP_BUNDLE="$ROOT_DIR/target/cuetty.app"
APP_CONTENTS="$APP_BUNDLE/Contents"
APP_MACOS="$APP_CONTENTS/MacOS"
APP_BINARY="$APP_MACOS/$PROCESS_NAME"
INFO_PLIST="$APP_CONTENTS/Info.plist"
TOOLCHAIN="${TOOLCHAINS:-com.apple.dt.toolchain.Metal.32023.917.2}"

pkill -x "$PROCESS_NAME" >/dev/null 2>&1 || true

cd "$ROOT_DIR"
TOOLCHAINS="$TOOLCHAIN" cargo build --release --locked

rm -rf "$APP_BUNDLE"
mkdir -p "$APP_MACOS"
cp "target/release/$PROCESS_NAME" "$APP_BINARY"
chmod +x "$APP_BINARY"

cat > "$INFO_PLIST" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDisplayName</key>
  <string>$APP_NAME</string>
  <key>CFBundleExecutable</key>
  <string>$PROCESS_NAME</string>
  <key>CFBundleIdentifier</key>
  <string>$BUNDLE_ID</string>
  <key>CFBundleName</key>
  <string>$APP_NAME</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>0.1.0</string>
  <key>CFBundleVersion</key>
  <string>0.1.0</string>
  <key>LSMinimumSystemVersion</key>
  <string>14.0</string>
  <key>NSPrincipalClass</key>
  <string>NSApplication</string>
</dict>
</plist>
PLIST

# Finder/FileProvider metadata is not part of the app and can make an
# otherwise valid ad-hoc signature fail launch validation. Clear inherited
# metadata before signing; clearing it after signing would invalidate the
# signature again.
xattr -rc "$APP_BUNDLE" 2>/dev/null || true
/usr/bin/codesign --force --deep --sign - "$APP_BUNDLE" >/dev/null
/usr/bin/codesign --verify --deep --strict "$APP_BUNDLE"

open_app() {
  # LaunchServices owns the app process outside the invoking terminal. A
  # direct background child is reaped by some terminal hosts when this script
  # exits, which looks like a Cuetty crash even though the binary is healthy.
  open -na "$APP_BUNDLE"
  APP_PID=""
  for _ in {1..50}; do
    APP_PID="$(pgrep -x "$PROCESS_NAME" | tail -n 1 || true)"
    [[ -n "$APP_PID" ]] && break
    sleep 0.1
  done
  if [[ -z "$APP_PID" ]]; then
    echo "Cuetty did not appear after LaunchServices launch" >&2
    return 1
  fi
}

case "$MODE" in
  run)
    open_app
    ;;
  --debug|debug)
    lldb -- "$APP_BINARY"
    ;;
  --logs|logs)
    open_app
    /usr/bin/log stream --info --style compact --predicate "process == \"$PROCESS_NAME\""
    ;;
  --telemetry|telemetry)
    open_app
    /usr/bin/log stream --info --style compact --predicate "subsystem == \"$BUNDLE_ID\""
    ;;
  --verify|verify)
    open_app
    sleep 1
    kill -0 "$APP_PID"
    kill "$APP_PID" >/dev/null 2>&1 || true
    ;;
  *)
    echo "usage: $0 [run|--debug|--logs|--telemetry|--verify]" >&2
    exit 2
    ;;
esac
