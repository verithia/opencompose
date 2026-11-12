#!/usr/bin/env bash
# OpenCompose DAW - bundle smoke test (macOS).
#
# Launches the bundled Mach-O directly, proves it stays alive, quits it, and
# fails if a crash report appeared. Also checks that the bundle's Info.plist
# reports the same version as the manifests.
#
#   npm run smoke:macos                       # default bundle path
#   npm run smoke:macos -- /path/OpenCompose.app
#
# Target dir follows CARGO_TARGET_DIR, so the same script works for the default
# src-tauri/target and for an isolated build cache.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FRONTEND_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
TAURI_DIR="$FRONTEND_DIR/src-tauri"

ALIVE_SECONDS="${SMOKE_ALIVE_SECONDS:-8}"
APP_PATH="${1:-}"

if [[ -z "$APP_PATH" ]]; then
  TARGET_DIR="${CARGO_TARGET_DIR:-$TAURI_DIR/target}"
  APP_PATH="$TARGET_DIR/release/bundle/macos/OpenCompose.app"
fi

fail() { echo "smoke: FAIL $*" >&2; exit 1; }

[[ -d "$APP_PATH" ]] || fail "bundle not found: $APP_PATH"
[[ -f "$APP_PATH/Contents/Info.plist" ]] || fail "no Info.plist in $APP_PATH"

BUNDLE_EXE="$(plutil -extract CFBundleExecutable raw -o - "$APP_PATH/Contents/Info.plist" 2>/dev/null || true)"
BIN="$APP_PATH/Contents/MacOS/$BUNDLE_EXE"
if [[ -z "$BUNDLE_EXE" || ! -x "$BIN" ]]; then
  BIN=""
  for candidate in "$APP_PATH/Contents/MacOS"/*; do
    if [[ -f "$candidate" && -x "$candidate" ]]; then BIN="$candidate"; break; fi
  done
fi
[[ -n "$BIN" ]] || fail "no executable in $APP_PATH/Contents/MacOS"

echo "smoke: bundle $APP_PATH"
echo "smoke: binary $BIN ($(du -h "$BIN" | cut -f1))"
echo "smoke: version gate"
node "$SCRIPT_DIR/check-version.mjs" --bundle "$APP_PATH"

SIGNATURE="$(codesign -dv "$APP_PATH" 2>&1 | head -3 || true)"
echo "smoke: signature: ${SIGNATURE:-<none>}"

LOG="$(mktemp -t opencompose-smoke)"
CRASH_DIR="$HOME/Library/Logs/DiagnosticReports"
CRASH_MARK="$(mktemp -t opencompose-crash-mark)"
START_EPOCH="$(date +%s)"

cleanup() {
  if [[ -n "${PID:-}" ]] && kill -0 "$PID" 2>/dev/null; then
    kill -KILL "$PID" 2>/dev/null || true
  fi
  rm -f "$CRASH_MARK"
}
trap cleanup EXIT

echo "smoke: launching"
"$BIN" >"$LOG" 2>&1 &
PID=$!
# Keep job-control notices out of the report; the process is still ours to kill.
disown "$PID" 2>/dev/null || true

for i in $(seq 1 10); do
  if ! kill -0 "$PID" 2>/dev/null; then
    echo "smoke: process exited within ${i}s; log:" >&2
    cat "$LOG" >&2
    fail "app exited during startup"
  fi
  sleep 1
done

echo "smoke: alive after 10s (pid $PID); holding for ${ALIVE_SECONDS}s"
sleep "$ALIVE_SECONDS"
kill -0 "$PID" 2>/dev/null || fail "app died while idle"

echo "smoke: quitting"
kill -TERM "$PID" 2>/dev/null || true
for _ in $(seq 1 40); do
  kill -0 "$PID" 2>/dev/null || break
  sleep 0.25
done
if kill -0 "$PID" 2>/dev/null; then
  echo "smoke: SIGTERM ignored, sending SIGKILL"
  kill -KILL "$PID" 2>/dev/null || true
  sleep 1
fi

if kill -0 "$PID" 2>/dev/null; then
  fail "process still running after quit"
fi

if [[ -d "$CRASH_DIR" ]]; then
  NEW_CRASHES="$(find "$CRASH_DIR" -maxdepth 1 -name 'OpenCompose*' -newer "$CRASH_MARK" 2>/dev/null || true)"
  if [[ -n "$NEW_CRASHES" ]]; then
    echo "smoke: crash reports:" >&2
    echo "$NEW_CRASHES" >&2
    fail "crash report written during smoke test"
  fi
fi

if [[ -s "$LOG" ]]; then
  echo "smoke: log (last 20 lines):"
  tail -20 "$LOG" | sed 's/^/  | /'
else
  echo "smoke: log empty (release log level is Warn)"
fi
rm -f "$LOG"

echo "smoke: PASS - bundle launched, stayed alive $((10 + ALIVE_SECONDS))s, quit, no crash report"
