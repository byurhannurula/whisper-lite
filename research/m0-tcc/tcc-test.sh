#!/bin/bash
# M0 question 3: does the macOS Accessibility (TCC) grant survive a rebuild?
#
# Why this matters: macOS keys the Accessibility grant to the app's code signature.
# whisper-lite needs Accessibility to inject text, and auto-updates itself. If the signature
# changes on every build, every update silently revokes the grant and the app stops working
# until the user re-toggles it by hand. That is the single worst UX outcome available to us.
#
# There is no Apple Developer account here, so the candidate fix is a FREE self-signed
# certificate applied consistently to every build. This script tests whether that actually
# produces a stable identity.
#
# Usage:
#   ./tcc-test.sh setup     # create the self-signed cert (one time, interactive)
#   ./tcc-test.sh build v1  # build + sign a probe app
#   ./tcc-test.sh check     # ask the app whether it currently has Accessibility
#   ./tcc-test.sh build v2  # rebuild with changed content, same cert
#   ./tcc-test.sh check     # did the grant survive?

set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP="$DIR/build/TCCProbe.app"
CERT_NAME="whisper-lite-selfsigned"
BUNDLE_ID="com.byrhn.whisperlite.tccprobe"

usage() { sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 1; }

cmd_setup() {
  if security find-certificate -c "$CERT_NAME" >/dev/null 2>&1; then
    echo "✓ Certificate '$CERT_NAME' already exists."
    return
  fi

  cat <<EOF
The self-signed certificate must be created through the Keychain Access GUI —
there is no non-interactive command for a code-signing cert.

  1. Open Keychain Access
  2. Menu: Keychain Access → Certificate Assistant → Create a Certificate...
  3. Name:              $CERT_NAME
     Identity Type:     Self Signed Root
     Certificate Type:  Code Signing
     (tick "Let me override defaults" only if you want a longer validity)
  4. Create, then re-run:  ./tcc-test.sh setup

EOF
  exit 1
}

cmd_build() {
  cmd_build_inner "${1:-v1}"
  codesign --force --deep --sign "$CERT_NAME" --identifier "$BUNDLE_ID" "$APP"
  echo "✓ Built and signed ${1:-v1}"
  codesign -dvvv "$APP" 2>&1 | grep -E "^(Authority|Identifier|CDHash)=" || true
}

cmd_build_inner() {
  local variant="${1:-v1}"
  rm -rf "$APP"
  mkdir -p "$APP/Contents/MacOS"

  cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>TCCProbe</string>
  <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
  <key>CFBundleName</key><string>TCCProbe</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>1.0</string>
  <key>LSUIElement</key><true/>
</dict>
</plist>
EOF

  # The probe reports whether this process currently holds the Accessibility grant.
  # AXIsProcessTrusted() is the same check the real app will make at launch.
  cat > "$DIR/build/probe.swift" <<EOF
import ApplicationServices
import Foundation

// Build variant: $variant — changes the binary's content so the rebuild is genuinely
// different, which is the whole point of the test.
let variant = "$variant"

let trusted = AXIsProcessTrusted()
print("variant=\\(variant) accessibility=\\(trusted ? "GRANTED" : "DENIED")")
exit(trusted ? 0 : 1)
EOF

  swiftc -O "$DIR/build/probe.swift" -o "$APP/Contents/MacOS/TCCProbe" 2>&1 | grep -v "^$" || true
}

cmd_check() {
  if [ ! -d "$APP" ]; then echo "No probe built yet. Run: ./tcc-test.sh build v1"; exit 1; fi
  set +e
  out="$("$APP/Contents/MacOS/TCCProbe")"
  code=$?
  set -e
  echo "$out"
  if [ $code -eq 0 ]; then
    echo "→ GRANTED"
  else
    echo "→ DENIED (grant it in System Settings → Privacy & Security → Accessibility,"
    echo "   adding $APP, then re-run check)"
  fi
}

# The decisive test, and it needs no permission grant at all.
#
# TCC does not key the grant on the binary bytes — it keys on the code signature's *designated
# requirement*. If two builds produce the same DR, macOS considers them the same app and the
# Accessibility grant carries over. If the DR changes, the grant is dropped.
#
# So: build twice with different content, diff the DR. Deterministic, and no GUI toggling.
cmd_compare() {
  local mode="${1:-adhoc}"
  local sign_as

  case "$mode" in
    adhoc) sign_as="-" ;;
    cert)
      if ! security find-certificate -c "$CERT_NAME" >/dev/null 2>&1; then
        echo "Certificate '$CERT_NAME' not found. Run: ./tcc-test.sh setup"
        exit 1
      fi
      sign_as="$CERT_NAME"
      ;;
    *) echo "mode must be 'adhoc' or 'cert'"; exit 1 ;;
  esac

  echo "== Comparing designated requirements across a rebuild (signing: $mode) =="
  echo

  build_signed_as "$sign_as" v1
  local dr1 cd1
  dr1="$(codesign -d -r- "$APP" 2>&1 | sed -n 's/^#\{0,1\} *designated => //p')"
  cd1="$(codesign -dvvv "$APP" 2>&1 | sed -n 's/^CDHash=//p')"

  build_signed_as "$sign_as" v2
  local dr2 cd2
  dr2="$(codesign -d -r- "$APP" 2>&1 | sed -n 's/^#\{0,1\} *designated => //p')"
  cd2="$(codesign -dvvv "$APP" 2>&1 | sed -n 's/^CDHash=//p')"

  echo "  build 1 cdhash: ${cd1:-n/a}"
  echo "  build 2 cdhash: ${cd2:-n/a}"
  echo
  echo "  build 1 designated requirement:"
  echo "    ${dr1:-<none>}"
  echo "  build 2 designated requirement:"
  echo "    ${dr2:-<none>}"
  echo

  if [ "$cd1" = "$cd2" ]; then
    echo "  ! cdhashes identical — the two builds were not actually different."
    echo "    Result is inconclusive; check that the variant string is reaching the binary."
    exit 1
  fi

  if [ "$dr1" = "$dr2" ] && [ -n "$dr1" ]; then
    echo "  ✓ STABLE — the designated requirement survived a rebuild."
    echo "    The Accessibility grant will persist across updates with $mode signing."
  else
    echo "  ✗ UNSTABLE — the designated requirement changed."
    echo "    Every update revokes Accessibility and the user must re-grant it by hand."
  fi
}

build_signed_as() {
  local identity="$1" variant="$2"
  cmd_build_inner "$variant"
  codesign --force --deep --sign "$identity" --identifier "$BUNDLE_ID" "$APP" 2>/dev/null
}

case "${1:-}" in
  setup) cmd_setup ;;
  build) cmd_build "${2:-v1}" ;;
  check) cmd_check ;;
  compare) cmd_compare "${2:-adhoc}" ;;
  *) usage ;;
esac
