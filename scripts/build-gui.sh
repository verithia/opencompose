#!/usr/bin/env bash
# OpenCompose DAW - desktop bundle build.
#
# One command for a release bundle:
#
#   ./scripts/build-gui.sh                  # release .app + .dmg
#   ./scripts/build-gui.sh --debug          # debug bundle, for local testing
#   ./scripts/build-gui.sh --bundles app    # only the .app
#   ./scripts/build-gui.sh --target universal-apple-darwin
#
# Release bundles use the crate defaults: the in-crate SoundFont player and the
# MP3 encoder. Nothing probes for an external SoundFont library.
#
# The Tauri CLI runs the frontend build itself (version gate, tsc, vite), so
# this script only installs locked dependencies and reports what came out.
#
# Signing is optional: Tauri signs when a "Developer ID Application" identity
# is configured (APPLE_SIGNING_IDENTITY) and builds unsigned otherwise. See
# docs/release.md for the signed and notarised flow.
#
# Prerequisites: Node.js + npm, Rust, platform Tauri dependencies
# (macOS: Xcode Command Line Tools).
#
# Output (under $CARGO_TARGET_DIR when set, otherwise the workspace target/):
#   <target>/release/bundle/macos/OpenCompose.app
#   <target>/release/bundle/dmg/OpenCompose_<version>_<arch>.dmg

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
FRONTEND_DIR="$PROJECT_DIR/crates/oc-gui/frontend"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

log()  { echo -e "${GREEN}[build]${NC} $*"; }
warn() { echo -e "${YELLOW}[warn]${NC}  $*"; }
err()  { echo -e "${RED}[error]${NC} $*" >&2; exit 1; }

DEBUG=""
TAURI_ARGS=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --release|-r)
      # Release is the default; kept so old instructions still work.
      shift ;;
    --debug|-d)
      DEBUG=1
      TAURI_ARGS+=(--debug)
      shift ;;
    --target)
      [[ $# -ge 2 ]] || err "--target needs a triple"
      TAURI_ARGS+=(--target "$2")
      shift 2 ;;
    --bundles)
      [[ $# -ge 2 ]] || err "--bundles needs a value"
      TAURI_ARGS+=(--bundles "$2")
      shift 2 ;;
    --no-bundle)
      TAURI_ARGS+=(--no-bundle)
      shift ;;
    --help|-h)
      sed -n '2,26p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0 ;;
    *)
      err "Unknown argument: $1 (try --help)" ;;
  esac
done

# No cargo feature flags: a bare build is the shipping set.

log "Checking prerequisites"
for tool in node npm; do
  command -v "$tool" >/dev/null || err "$tool is required"
done
log "Node.js $(node -v)"
log "Rust $(rustc --version)"

log "Installing locked frontend dependencies"
cd "$FRONTEND_DIR"
if [[ ! -f node_modules/.package-lock.json ]] || \
   [[ package-lock.json -nt node_modules/.package-lock.json ]]; then
  npm ci
else
  log "node_modules is current (run npm ci to force a reinstall)"
fi

log "Tauri build (version check + frontend + app bundle)"
START="$(date +%s)"
npm run tauri build ${TAURI_ARGS[@]+"${TAURI_ARGS[@]}"}
ELAPSED="$(( $(date +%s) - START ))"

PROFILE="release"
[[ -n "$DEBUG" ]] && PROFILE="debug"
TARGET_DIR="${CARGO_TARGET_DIR:-$PROJECT_DIR/target}"
BUNDLE_DIR="$TARGET_DIR/$PROFILE/bundle"

echo ""
log "Build complete in ${ELAPSED}s"
log "Target dir: $TARGET_DIR"

FOUND=0
if [[ -d "$BUNDLE_DIR" ]]; then
  while IFS= read -r -d '' app; do
    echo "  app   $app ($(du -sh "$app" | cut -f1))"
    FOUND=1
  done < <(find "$BUNDLE_DIR" -maxdepth 3 -type d -name "*.app" -print0 2>/dev/null)

  while IFS= read -r -d '' image; do
    echo "  image $image ($(du -h "$image" | cut -f1))"
    if command -v shasum >/dev/null; then
      echo "        sha256 $(shasum -a 256 "$image" | cut -d' ' -f1)"
    fi
    FOUND=1
  done < <(find "$BUNDLE_DIR" -maxdepth 3 -type f \
      \( -name "*.dmg" -o -name "*.msi" -o -name "*.deb" -o -name "*.AppImage" \) \
      -print0 2>/dev/null)
fi

[[ "$FOUND" -eq 1 ]] || warn "No bundle artefacts under $BUNDLE_DIR"

echo ""
log "Smoke test the app bundle: cd crates/oc-gui/frontend && npm run smoke:macos"
log "Signed/notarised release: docs/release.md"
