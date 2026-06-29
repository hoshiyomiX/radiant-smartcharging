#!/usr/bin/env bash
# Cross-compile rsc for aarch64-linux-android.
#
# Prerequisites:
#   1. Android NDK installed (>= r25)
#      export ANDROID_NDK_HOME=$HOME/Android/Sdk/ndk/26.1.10909125
#   2. Rust target installed:
#      rustup target add aarch64-linux-android
#
# Output: target/aarch64-linux-android/release/rsc

set -euo pipefail

TARGET="aarch64-linux-android"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "${SCRIPT_DIR}"

if [ -z "${ANDROID_NDK_HOME:-}" ]; then
  echo "ERROR: ANDROID_NDK_HOME is not set"
  echo "  Example: export ANDROID_NDK_HOME=\$HOME/Android/Sdk/ndk/26.1.10909125"
  exit 1
fi

if [ ! -d "${ANDROID_NDK_HOME}" ]; then
  echo "ERROR: ANDROID_NDK_HOME path does not exist: ${ANDROID_NDK_HOME}"
  exit 1
fi

# Determine the NDK prebuilt host tag.
HOST_TAG="$(uname -s | tr '[:upper:]' '[:lower:]')-x86_64"
NDK_TOOLCHAIN="${ANDROID_NDK_HOME}/toolchains/llvm/prebuilt/${HOST_TAG}"
if [ ! -d "${NDK_TOOLCHAIN}" ]; then
  echo "ERROR: NDK toolchain not found at: ${NDK_TOOLCHAIN}"
  echo "  Available hosts:"
  ls "${ANDROID_NDK_HOME}/toolchains/llvm/prebuilt/" 2>/dev/null || true
  exit 1
fi

# API level 24 covers Android 7.0+ which is the practical floor for
# modern MTK devices. Bump to 29+ if you need scoped storage APIs.
API_LEVEL="${BATTD_API_LEVEL:-24}"
LINKER="${NDK_TOOLCHAIN}/bin/aarch64-linux-android${API_LEVEL}-clang"

if [ ! -x "${LINKER}" ]; then
  echo "ERROR: linker not found: ${LINKER}"
  echo "  Check API_LEVEL or NDK version"
  exit 1
fi

echo ">> Installing rust target ${TARGET}"
rustup target add "${TARGET}" 2>/dev/null || true

# Write .cargo/config.toml so cargo picks the right linker.
mkdir -p "${SCRIPT_DIR}/.cargo"
cat > "${SCRIPT_DIR}/.cargo/config.toml" <<EOF
[target.${TARGET}]
linker = "${LINKER}"

[build]
target = "${TARGET}"
EOF

echo ">> Building release for ${TARGET} (API ${API_LEVEL})"
cargo build --release --target "${TARGET}"

BINARY="${SCRIPT_DIR}/target/${TARGET}/release/rsc"
if [ ! -f "${BINARY}" ]; then
  echo "ERROR: binary not found at ${BINARY}"
  exit 1
fi

echo ">> Built:"
ls -lh "${BINARY}"
file "${BINARY}" 2>/dev/null || true

echo ""
echo ">> Next steps:"
echo "   adb push ${BINARY} /data/local/tmp/rsc"
echo "   adb shell su -c 'cp /data/local/tmp/rsc /system/bin/rsc && chmod 755 /system/bin/rsc'"
echo "   See install.sh for full installation procedure."
