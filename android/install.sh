#!/system/bin/sh
# Install rsc onto a rooted Android device.
#
# Run from the directory containing the compiled binary, the .rc file,
# and the example config. Typically invoked via adb:
#
#   adb push rsc /data/local/tmp/
#   adb push rsc.rc /data/local/tmp/
#   adb push config.example.toml /data/local/tmp/
#   adb push install.sh /data/local/tmp/
#   adb shell su -c 'sh /data/local/tmp/install.sh'
#
# Requires: rooted device, /system remountable (Magisk users should convert
# this to a Magisk module instead — see README).

set -e

BIN_DIR="/system/bin"
INIT_DIR="/system/etc/init"
DATA_DIR="/data/adb/rsc"

SRC_DIR="$(dirname "$0")"
BINARY="${SRC_DIR}/rsc"
RC_FILE="${SRC_DIR}/rsc.rc"
CONFIG_FILE="${SRC_DIR}/config.example.toml"

# --- Sanity checks ---
if [ ! -f "${BINARY}" ]; then
  echo "ERROR: ${BINARY} not found"
  exit 1
fi
if [ ! -f "${RC_FILE}" ]; then
  echo "ERROR: ${RC_FILE} not found"
  exit 1
fi

if [ "$(id -u)" -ne 0 ]; then
  echo "ERROR: must run as root"
  exit 1
fi

# --- Remount /system writable ---
echo ">> Remounting /system as rw"
mount -o rw,remount /system 2>/dev/null || {
  echo "ERROR: cannot remount /system rw"
  echo "  If you are using Magisk or KernelSU, do NOT install to /system."
  echo "  Convert this to a Magisk module instead (see README)."
  exit 1
}

# --- Install binary ---
echo ">> Installing binary to ${BIN_DIR}/rsc"
cp "${BINARY}" "${BIN_DIR}/rsc"
chmod 0755 "${BIN_DIR}/rsc"
chown root:root "${BIN_DIR}/rsc"
restorecon "${BIN_DIR}/rsc" 2>/dev/null || true

# --- Install init script ---
echo ">> Installing init script to ${INIT_DIR}/rsc.rc"
mkdir -p "${INIT_DIR}"
cp "${RC_FILE}" "${INIT_DIR}/rsc.rc"
chmod 0644 "${INIT_DIR}/rsc.rc"
chown root:root "${INIT_DIR}/rsc.rc"
restorecon "${INIT_DIR}/rsc.rc" 2>/dev/null || true

# --- Setup data directory + config ---
echo ">> Setting up ${DATA_DIR}"
mkdir -p "${DATA_DIR}"
chmod 0755 "${DATA_DIR}"

if [ ! -f "${DATA_DIR}/config.toml" ]; then
  if [ -f "${CONFIG_FILE}" ]; then
    cp "${CONFIG_FILE}" "${DATA_DIR}/config.toml"
  fi
fi
chmod 0644 "${DATA_DIR}/config.toml" 2>/dev/null || true

# --- Remount /system read-only ---
echo ">> Remounting /system as ro"
mount -o ro,remount /system

echo ""
echo ">> Installation complete."
echo "   Binary:   ${BIN_DIR}/rsc"
echo "   Init RC:  ${INIT_DIR}/rsc.rc"
echo "   Config:   ${DATA_DIR}/config.toml"
echo "   Log:      ${DATA_DIR}/rsc.log"
echo ""
echo ">> Reboot the device to start rsc, or run manually:"
echo "   start rsc"
echo "   (or) ${BIN_DIR}/rsc &"
