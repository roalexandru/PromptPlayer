#!/usr/bin/env bash
# E2E smoke for the macOS bundle. Verifies bundle metadata + that the app
# launches and stays alive without crashing.
#
# Args:
#   $1 = expected version (e.g. "0.1.0")
# Env:
#   TARGET_TRIPLE — defaults to aarch64-apple-darwin
#
# Tauri 2 cleans the standalone .app after rolling it into the .dmg, so the
# only artifact that survives `tauri build` is the .dmg. This script mounts
# it (read-only) to read Info.plist + the bundled binary, then launches a
# COPY of the .app from /tmp (because launching directly off a read-only
# DMG mount can fail with TCC permission noise on CI runners).
#
# Sets PROMPT_PLAYER_E2E=1 during launch so the in-process telemetry init
# drops events instead of pinging Aptabase as a real user.

set -euo pipefail

EXPECTED_VERSION="${1:-}"
if [ -z "$EXPECTED_VERSION" ]; then
  EXPECTED_VERSION=$(node -p "require('./package.json').version")
fi
TARGET="${TARGET_TRIPLE:-aarch64-apple-darwin}"
# Workspace target dir lives at the repo root (Cargo.toml at ./), NOT under
# src-tauri/. Tauri's `--target` flag preserves the per-triple subdir.
BUNDLE_DIR="target/$TARGET/release/bundle"

echo "==> E2E target: $TARGET, expected version: $EXPECTED_VERSION"

# 1. DMG exists and is non-empty. `find` (instead of `ls $glob`) so the
# space in "Prompt Player" doesn't word-split the path into two args.
DMG=$(find "$BUNDLE_DIR/dmg" -maxdepth 1 -type f -name "Prompt Player_${EXPECTED_VERSION}_*.dmg" 2>/dev/null | head -1)
if [ -z "$DMG" ] || [ ! -s "$DMG" ]; then
  echo "::error::missing or empty DMG under $BUNDLE_DIR/dmg/"
  ls -la "$BUNDLE_DIR/dmg/" 2>/dev/null || true
  echo "tree under target/:"
  find target -maxdepth 5 -type d -name bundle 2>/dev/null || true
  exit 1
fi
DMG_SIZE=$(stat -f%z "$DMG")
echo "  [ok] DMG: $(basename "$DMG") ($DMG_SIZE bytes)"

# 2. Mount the DMG read-only and copy the .app to /tmp for inspection
MOUNT_DIR="/tmp/pp-e2e-mount"
mkdir -p "$MOUNT_DIR"
trap 'hdiutil detach "$MOUNT_DIR" -quiet 2>/dev/null || true; rm -rf /tmp/pp-e2e-app' EXIT
hdiutil attach "$DMG" -nobrowse -readonly -mountpoint "$MOUNT_DIR" >/dev/null
APP_IN_DMG="$MOUNT_DIR/Prompt Player.app"
if [ ! -d "$APP_IN_DMG" ]; then
  echo "::error::DMG mounted but no Prompt Player.app inside"
  ls -la "$MOUNT_DIR" || true
  exit 1
fi
APP="/tmp/pp-e2e-app/Prompt Player.app"
mkdir -p /tmp/pp-e2e-app
cp -R "$APP_IN_DMG" "$APP"
hdiutil detach "$MOUNT_DIR" -quiet
echo "  [ok] .app extracted to $APP"

# 3. Info.plist: bundle identifier matches the locked value
PLIST="$APP/Contents/Info.plist"
BUNDLE_ID=$(/usr/libexec/PlistBuddy -c "Print :CFBundleIdentifier" "$PLIST")
if [ "$BUNDLE_ID" != "com.roalexandru.promptplayer" ]; then
  echo "::error::bundle ID drift — got '$BUNDLE_ID', expected 'com.roalexandru.promptplayer'"
  exit 1
fi
echo "  [ok] CFBundleIdentifier = $BUNDLE_ID"

# 4. Info.plist: version matches package.json
SHORT_VERSION=$(/usr/libexec/PlistBuddy -c "Print :CFBundleShortVersionString" "$PLIST")
if [ "$SHORT_VERSION" != "$EXPECTED_VERSION" ]; then
  echo "::error::version drift — Info.plist=$SHORT_VERSION, expected=$EXPECTED_VERSION"
  exit 1
fi
echo "  [ok] CFBundleShortVersionString = $SHORT_VERSION"

# 5. Icon file present and non-trivial (>10 KB rules out empty placeholder)
ICON="$APP/Contents/Resources/icon.icns"
if [ ! -s "$ICON" ]; then
  echo "::error::missing icon at $ICON"
  exit 1
fi
ICON_SIZE=$(stat -f%z "$ICON")
if [ "$ICON_SIZE" -lt 10240 ]; then
  echo "::error::icon suspiciously small ($ICON_SIZE bytes) — placeholder?"
  exit 1
fi
echo "  [ok] icon.icns ($ICON_SIZE bytes)"

# 6. Main executable exists and is mach-o
EXE="$APP/Contents/MacOS/prompt-player"
if [ ! -x "$EXE" ]; then
  echo "::error::missing or non-executable binary at $EXE"
  exit 1
fi
file "$EXE" | grep -q "Mach-O" || { echo "::error::$EXE is not Mach-O"; exit 1; }
echo "  [ok] Mach-O binary"

# 7. Launch test — clear quarantine (unsigned) on the copy, launch, verify
# alive after 5s, kill cleanly. CI runners have no Accessibility approval;
# the app must still stay alive (hook init may fail to attach but must not
# panic).
xattr -cr "$APP" || true

echo "==> Launching with PROMPT_PLAYER_E2E=1"
PROMPT_PLAYER_E2E=1 "$EXE" >/tmp/pp-e2e-stdout.log 2>/tmp/pp-e2e-stderr.log &
PID=$!
echo "  pid=$PID"

# Give it 5s to either crash or stabilize.
sleep 5

if ! kill -0 "$PID" 2>/dev/null; then
  echo "::error::process exited within 5s"
  echo "---- stdout ----"; cat /tmp/pp-e2e-stdout.log || true
  echo "---- stderr ----"; cat /tmp/pp-e2e-stderr.log || true
  exit 1
fi
echo "  [ok] process alive after 5s"

# Clean shutdown — SIGTERM, then SIGKILL fallback.
kill -TERM "$PID" 2>/dev/null || true
sleep 2
if kill -0 "$PID" 2>/dev/null; then
  kill -KILL "$PID" 2>/dev/null || true
fi
wait "$PID" 2>/dev/null || true
echo "  [ok] clean shutdown"

echo "==> E2E mac PASSED"
