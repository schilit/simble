#!/usr/bin/env bash
# Copyright 2026 Bill Schilit
# SPDX-License-Identifier: Apache-2.0
#
# Fetches a standalone rootcanal into third_party/rootcanal/.
#
# rootcanal is the controller a real Android emulator runs, and the only one
# that models inquiry — which is why `tests/interop/classic_peer.py` could
# never run in CI. Getting it used to mean installing the Android SDK for
# `netsimd`. It does not: upstream publishes prebuilt binaries as GitHub
# release assets, ~16 MB, no SDK and no bazel.
#
# See tests/interop/rootcanal_link.py for what the two rootcanal builds
# differ on (BIG, notably) and how a script checks before trusting one.

set -euo pipefail

VERSION="${ROOTCANAL_VERSION:-1.12.0}"
DESTINATION="${ROOTCANAL_DIR:-third_party/rootcanal}"

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64)   PLATFORM="linux-x86_64" ;;
  Darwin-arm64)   PLATFORM="macos-arm64"  ;;
  Darwin-x86_64)
    # Upstream stopped shipping macos-x86_64 after v1.10.0.
    PLATFORM="macos-x86_64"
    echo "note: only rootcanal <= 1.10.0 ships macos-x86_64 builds" >&2
    ;;
  *)
    echo "no prebuilt rootcanal for $(uname -s)-$(uname -m)." >&2
    echo "Build it from https://github.com/google/rootcanal and point" >&2
    echo "\$SIMBLE_ROOTCANAL at the binary." >&2
    exit 1
    ;;
esac

ARCHIVE="rootcanal-${VERSION}-${PLATFORM}.zip"
URL="https://github.com/google/rootcanal/releases/download/v${VERSION}/${ARCHIVE}"

if [ -x "${DESTINATION}/bin/rootcanal" ]; then
  echo "already present: ${DESTINATION}/bin/rootcanal"
  exit 0
fi

WORKDIR="$(mktemp -d)"
trap 'rm -rf "${WORKDIR}"' EXIT

echo "fetching ${URL}"
curl --fail --location --silent --show-error --output "${WORKDIR}/${ARCHIVE}" "${URL}"
unzip -q "${WORKDIR}/${ARCHIVE}" -d "${WORKDIR}"

# The archive unpacks to rootcanal-<platform>/{bin,lib,include}.
mkdir -p "$(dirname "${DESTINATION}")"
rm -rf "${DESTINATION}"
mv "${WORKDIR}/rootcanal-${PLATFORM}" "${DESTINATION}"
chmod +x "${DESTINATION}/bin/rootcanal"

# macOS quarantines anything downloaded, and a quarantined binary dies with
# SIGKILL rather than an error a script could explain.
if [ "$(uname -s)" = "Darwin" ]; then
  xattr -d com.apple.quarantine "${DESTINATION}/bin/rootcanal" 2>/dev/null || true
fi

echo "installed ${DESTINATION}/bin/rootcanal (rootcanal ${VERSION}, ${PLATFORM})"

# Prove it is the real thing before anyone builds a green CI run on it.
if command -v python3 >/dev/null 2>&1; then
  python3 tests/interop/rootcanal_link.py || {
    echo "the fetched rootcanal did not answer as a real controller" >&2
    exit 1
  }
fi
