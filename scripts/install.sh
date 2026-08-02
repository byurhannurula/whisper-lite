#!/bin/bash
#
# Build Whisper Lite, sign it, and install it to /Applications.
#
# The signing step is not optional. macOS ties the Accessibility permission to the app's code
# signature, so an unsigned build gets a new identity every time and you would have to re-grant
# Accessibility after every single install. Signing with the same self-signed certificate keeps
# the designated requirement stable, which is what makes the grant persist. See M0-RESULTS.md.
#
# Usage:
#   ./scripts/install.sh            build, sign, install, relaunch
#   ./scripts/install.sh --no-open  install without relaunching

set -euo pipefail

APP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CERT="whisper-lite-selfsigned"
BUNDLE_ID="com.byrhn.whisperlite"
BUILT="$APP_DIR/src-tauri/target/release/bundle/macos/Whisper Lite.app"
INSTALLED="/Applications/Whisper Lite.app"

cd "$APP_DIR"

step() { printf "\n\033[1m==> %s\033[0m\n" "$1"; }

# ---------------------------------------------------------------------------
step "Checking signing certificate"

if ! security find-certificate -c "$CERT" >/dev/null 2>&1; then
  cat <<EOF
Certificate '$CERT' not found.

Create it once, through the GUI (there is no CLI equivalent for a code-signing cert):

  Keychain Access → Certificate Assistant → Create a Certificate...
    Name:             $CERT
    Identity Type:    Self Signed Root
    Certificate Type: Code Signing

Then re-run this script.
EOF
  exit 1
fi
echo "ok"

# ---------------------------------------------------------------------------
step "Running tests"
cargo test --release --manifest-path src-tauri/Cargo.toml --quiet 2>&1 | tail -5

# ---------------------------------------------------------------------------
step "Building"
# `|| true` on the pipeline would discard the build's exit status even under `set -o pipefail`,
# and a leftover bundle from a previous run would then be signed and installed as if it were
# fresh. Check the build's own status explicitly.
set +e
pnpm tauri build --bundles app 2>&1 | grep -E "Finished|error|warning: unused"
BUILD_STATUS=${PIPESTATUS[0]}
set -e

if [ "$BUILD_STATUS" -ne 0 ]; then
  echo "Build failed (exit $BUILD_STATUS) — not installing."
  exit 1
fi

if [ ! -d "$BUILT" ]; then
  echo "Build produced no app bundle at $BUILT"
  exit 1
fi

# ---------------------------------------------------------------------------
step "Signing"
codesign --force --deep --sign "$CERT" --identifier "$BUNDLE_ID" "$BUILT"

# Fail loudly if the designated requirement is pinned to the binary hash — that means the
# signature did not take, and the Accessibility grant would be lost on the next install.
DR="$(codesign -d -r- "$BUILT" 2>&1 | sed -n 's/^#\{0,1\} *designated => //p')"
if [[ "$DR" == cdhash* ]]; then
  echo "Signing did not take — designated requirement is still the binary hash:"
  echo "  $DR"
  exit 1
fi
echo "designated requirement: $DR"

# ---------------------------------------------------------------------------
step "Installing to /Applications"

# Quit the running copy first, or the replace fails while it holds open file handles.
if pgrep -f "Whisper Lite.app" >/dev/null 2>&1; then
  echo "quitting running instance"
  pkill -f "Whisper Lite.app" || true
  sleep 1
fi

rm -rf "$INSTALLED"
cp -R "$BUILT" "$INSTALLED"

# The build directory is not quarantined, but a copied bundle can inherit the attribute. Strip
# it so Gatekeeper does not block the first launch.
xattr -dr com.apple.quarantine "$INSTALLED" 2>/dev/null || true

echo "installed to $INSTALLED"

# ---------------------------------------------------------------------------
if [ "${1:-}" != "--no-open" ]; then
  step "Launching"
  # `open` matters: launching the binary directly makes it a child of this terminal, and macOS
  # then attributes microphone and Accessibility permissions to the terminal instead of to
  # whisper-lite.
  open "$INSTALLED"
  echo "running — look for the waveform glyph in your menu bar"
fi

cat <<EOF

Log:      ~/Library/Application Support/whisper-lite/whisper-lite.log
Settings: ~/Library/Application Support/whisper-lite/settings.json
EOF
