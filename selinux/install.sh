#!/system/bin/sh
# rsc CIL patch installer — direct vendor implant (NO Magisk overlay)
#
# Usage:
#   adb push selinux-cil-direct/ /data/local/tmp/selinux-cil-direct/
#   adb shell su -c 'sh /data/local/tmp/selinux-cil-direct/install.sh'
#
# What this does:
#   1. Remounts /vendor as rw
#   2. Appends rsc.cil to vendor_sepolicy.cil
#   3. Appends file_contexts.patch to vendor_file_contexts
#   4. INVALIDATES precompiled_sepolicy.*.sha256 so init recompiles CIL
#      on next boot (otherwise Android uses the precompiled binary policy
#      and ignores our CIL edits)
#   5. Copies binary + .rc to /vendor (if supplied)
#   6. Remounts /vendor as ro
#   7. Reboots (user must run `adb reboot` manually to confirm)

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

if [ "$(id -u)" -ne 0 ]; then
  echo "ERROR: must run as root"
  exit 1
fi

# --- Sanity checks ---
for f in rsc.cil file_contexts.patch; do
  if [ ! -f "${SCRIPT_DIR}/${f}" ]; then
    echo "ERROR: ${SCRIPT_DIR}/${f} not found"
    exit 1
  fi
done

VENDOR_SELINUX=/vendor/etc/selinux

if [ ! -f "${VENDOR_SELINUX}/vendor_sepolicy.cil" ]; then
  echo "ERROR: ${VENDOR_SELINUX}/vendor_sepolicy.cil not found"
  echo "  This device may not use CIL-based vendor sepolicy."
  echo "  Check: ls /vendor/etc/selinux/"
  exit 1
fi

# --- Disable dm-verity if needed (only if /vendor cannot be remounted rw) ---
remount_vendor() {
  if mount | grep -q " /vendor "; then
    mount -o rw,remount /vendor && return 0
  fi
  # Try mounting first
  mount /vendor 2>/dev/null || mount -t auto /vendor 2>/dev/null || true
  mount -o rw,remount /vendor && return 0
  return 1
}

echo ">> Attempting to remount /vendor as rw"
if ! remount_vendor; then
  echo "ERROR: cannot remount /vendor rw"
  echo ""
  echo "  This usually means dm-verity is enabled. Disable it:"
  echo "    adb disable-verity && adb reboot && adb root && adb wait-for-device"
  echo "    Then re-run this installer."
  echo ""
  echo "  On some devices, you also need:"
  echo "    adb shell su -c 'avbctl disable-verification && reboot'"
  exit 1
fi

# --- Backup originals (idempotent — don't overwrite existing backups) ---
BACKUP_DIR=/data/local/tmp/selinux-backup-$(date +%Y%m%d-%H%M%S)
mkdir -p "${BACKUP_DIR}"
echo ">> Backing up originals to ${BACKUP_DIR}"
cp "${VENDOR_SELINUX}/vendor_sepolicy.cil" "${BACKUP_DIR}/"
cp "${VENDOR_SELINUX}/vendor_file_contexts" "${BACKUP_DIR}/"
for sha in precompiled_sepolicy.plat_sepolicy_and_mapping.sha256 \
           precompiled_sepolicy.system_ext_sepolicy_and_mapping.sha256 \
           precompiled_sepolicy.product_sepolicy_and_mapping.sha256; do
  if [ -f "${VENDOR_SELINUX}/${sha}" ]; then
    cp "${VENDOR_SELINUX}/${sha}" "${BACKUP_DIR}/"
  fi
done

# --- Append CIL patch ---
echo ">> Appending rsc.cil to vendor_sepolicy.cil"
# Idempotency: check if already patched
if grep -q "^; rsc CIL policy patch" "${VENDOR_SELINUX}/vendor_sepolicy.cil"; then
  echo "   (already patched — skipping append)"
else
  cat "${SCRIPT_DIR}/rsc.cil" >> "${VENDOR_SELINUX}/vendor_sepolicy.cil"
fi

# --- Append file_contexts patch ---
echo ">> Appending file_contexts.patch to vendor_file_contexts"
if grep -q "rsc_exec" "${VENDOR_SELINUX}/vendor_file_contexts"; then
  echo "   (already patched — skipping append)"
else
  cat "${SCRIPT_DIR}/file_contexts.patch" >> "${VENDOR_SELINUX}/vendor_file_contexts"
fi

# --- CRITICAL: invalidate precompiled_sepolicy hash files ---
# Android 11's init checks these .sha256 files at boot. If they match the
# compiled policy in precompiled_sepolicy, init uses the precompiled
# binary directly and IGNORES the .cil source files. By deleting the
# hash files, we force init to recompile from the (now-patched) CIL.
#
# Without this step, file_contexts entries referencing new types like
# rsc_exec will be silently SKIPPED (because the OLD precompiled policy
# doesn't have those types). The binary will stay labeled vendor_file
# even though file_contexts.patched is in place. This is the #1 cause of
# "I patched everything but label is still vendor_file" reports.
echo ">> CRITICAL: Invalidating precompiled_sepolicy hash files"
echo ">> (Without this, init uses OLD binary policy and ignores CIL edits)"
for sha in precompiled_sepolicy.plat_sepolicy_and_mapping.sha256 \
           precompiled_sepolicy.system_ext_sepolicy_and_mapping.sha256 \
           precompiled_sepolicy.product_sepolicy_and_mapping.sha256; do
  if [ -f "${VENDOR_SELINUX}/${sha}" ]; then
    echo "   removing: ${sha}"
    rm -f "${VENDOR_SELINUX}/${sha}"
  else
    echo "   not present: ${sha} (already deleted or never existed)"
  fi
done

# Also remove the precompiled_sepolicy binary itself — without the hash
# files, init will try to recompile, but some Android builds fall back to
# the precompiled binary if recompile fails. Removing the binary forces
# recompile-only behavior.
if [ -f "${VENDOR_SELINUX}/precompiled_sepolicy" ]; then
  echo "   removing: precompiled_sepolicy (will be rebuilt at boot)"
  rm -f "${VENDOR_SELINUX}/precompiled_sepolicy"
fi

# Verify all precompiled_sepolicy files are gone
echo ">> Verifying precompiled_sepolicy files are deleted:"
REMAINING=$(ls "${VENDOR_SELINUX}"/precompiled_sepolicy* 2>/dev/null)
if [ -z "$REMAINING" ]; then
  echo "   OK: no precompiled_sepolicy files remain"
else
  echo "   WARNING: these files still exist:"
  echo "$REMAINING" | sed 's/^/     /'
  echo "   Init will use these instead of recompiling CIL!"
  echo "   Delete them manually before reboot:"
  echo "     rm -f /vendor/etc/selinux/precompiled_sepolicy*"
  exit 1
fi

# --- Optional: copy binary + init script if supplied ---
if [ -f "${SCRIPT_DIR}/rsc-binary" ]; then
  echo ">> Copying rsc binary to /vendor/bin/rsc"
  cp "${SCRIPT_DIR}/rsc-binary" /vendor/bin/rsc
  chmod 0755 /vendor/bin/rsc
  chown root:root /vendor/bin/rsc
  # NOTE: restorecon here uses the OLD policy (precompiled binary),
  # so it will label binary as vendor_file. After reboot, init will
  # recompile CIL with new types, and the file_contexts entry
  # `/vendor/bin/rsc u:object_r:rsc_exec:s0` will be applied
  # automatically at boot via the "restorecon_recursive" pass.
  # No manual restorecon needed — just reboot.
  restorecon /vendor/bin/rsc 2>/dev/null || true
fi

if [ -f "${SCRIPT_DIR}/rsc-vendor.rc" ]; then
  echo ">> Copying rsc.rc to /vendor/etc/init/rsc.rc"
  mkdir -p /vendor/etc/init
  cp "${SCRIPT_DIR}/rsc-vendor.rc" /vendor/etc/init/rsc.rc
  chmod 0644 /vendor/etc/init/rsc.rc
  chown root:root /vendor/etc/init/rsc.rc
  restorecon /vendor/etc/init/rsc.rc 2>/dev/null || true
fi

# --- Remount /vendor read-only ---
echo ">> Remounting /vendor as ro"
mount -o ro,remount /vendor 2>/dev/null || \
  echo "   WARNING: could not remount /vendor ro — verify before reboot"

# --- Summary ---
echo ""
echo ">> Installation complete."
echo "   Backups: ${BACKUP_DIR}"
echo ""
echo ">> NEXT STEPS:"
echo "   1. Reboot the device:"
echo "        adb reboot"
echo "   2. After reboot, verify the policy loaded:"
echo "        adb shell getprop init.svc.rsc          # should be 'running'"
echo "        adb shell ps -A -Z | grep rsc           # should show u:r:rsc:s0"
echo "        adb shell ls -Z /vendor/bin/rsc         # should show rsc_exec"
echo "        adb shell ls -Z /proc/mtk_battery_cmd/    # should show rsc_mtk_battery_proc"
echo "        adb logcat -d | grep avc.*rsc           # should be empty (no denials)"
echo ""
echo ">> If rsc fails to start (init.svc.rsc=stopped), check:"
echo "   - dmesg | grep -i selinux                     # for policy compile errors"
echo "   - logcat | grep -i 'sepolicy.*load'           # for load failures"
echo "   - If policy compile failed, restore backup:"
echo "       cp ${BACKUP_DIR}/vendor_sepolicy.cil /vendor/etc/selinux/"
echo "       cp ${BACKUP_DIR}/vendor_file_contexts /vendor/etc/selinux/"
echo "       (re-mount /vendor rw first)"
