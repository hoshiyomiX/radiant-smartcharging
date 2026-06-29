# Installation Guide — rsc v1.0.0

Complete step-by-step guide to install rsc on Transsion MTK Android devices.

## Prerequisites

1. **Rooted device** with Magisk / KernelSU / APatch
2. **dm-verity disabled**: `adb disable-verity && adb reboot` (or DFE for Transsion)
3. **MTK SoC** with paths:
   - `/proc/mtk_battery_cmd/en_power_path`
   - `/proc/mtk_battery_cmd/current_cmd`
   - `/sys/devices/platform/battery/disable_nafg`
   - `/sys/devices/platform/battery/ntc_disable_nafg`
4. **Android 7.0+** (API 24+)

Verify your device:
```bash
ls /proc/mtk_battery_cmd/
ls /sys/devices/platform/battery/disable_nafg
cat /vendor/etc/selinux/plat_sepolicy_vers.txt   # → 30.0 (Android 11)
```

## Files in this package

| File | Destination | Purpose |
|------|-------------|---------|
| `rsc` | `/vendor/bin/rsc` | Daemon binary (aarch64, 483 KB) |
| `rsc.rc` | `/vendor/etc/init/rsc.rc` | Android init service definition |
| `vendor_sepolicy.cil` | `/vendor/etc/selinux/vendor_sepolicy.cil` | Patched SELinux CIL policy |
| `vendor_file_contexts` | `/vendor/etc/selinux/vendor_file_contexts` | Patched file labels |
| `config.example.toml` | `/data/adb/rsc/config.toml` | Example config (optional — defaults work) |

## Installation (Termux + su)

### Step 1: Push files to device

Transfer all files to `/data/local/tmp/`:

```bash
# Via adb (from PC):
adb push rsc /data/local/tmp/
adb push rsc.rc /data/local/tmp/
adb push vendor_sepolicy.cil /data/local/tmp/
adb push vendor_file_contexts /data/local/tmp/
adb push config.example.toml /data/local/tmp/

# Or via MT Manager / file manager: copy to /data/local/tmp/
```

### Step 2: Apply SELinux policy + init script

```bash
su
mount -o rw,remount /vendor

# Backup originals
mkdir -p /data/local/tmp/rsc-backup
cp /vendor/etc/selinux/vendor_sepolicy.cil /data/local/tmp/rsc-backup/
cp /vendor/etc/selinux/vendor_file_contexts /data/local/tmp/rsc-backup/

# Apply patched SELinux policy
cp /data/local/tmp/vendor_sepolicy.cil /vendor/etc/selinux/vendor_sepolicy.cil
cp /data/local/tmp/vendor_file_contexts /vendor/etc/selinux/vendor_file_contexts
chmod 644 /vendor/etc/selinux/vendor_sepolicy.cil /vendor/etc/selinux/vendor_file_contexts
chown root:root /vendor/etc/selinux/vendor_sepolicy.cil /vendor/etc/selinux/vendor_file_contexts

# CRITICAL: Delete precompiled_sepolicy hash files
# Without this, init loads OLD binary policy and ignores CIL edits
rm -f /vendor/etc/selinux/precompiled_sepolicy
rm -f /vendor/etc/selinux/precompiled_sepolicy.*.sha256
rm -f /vendor/etc/selinux/selinux_denial_metadata

# Verify hash files gone
ls /vendor/etc/selinux/precompiled_sepolicy* 2>&1
# Expected: No such file or directory

# Copy init script
cp /data/local/tmp/rsc.rc /vendor/etc/init/rsc.rc
chmod 644 /vendor/etc/init/rsc.rc
chown root:root /vendor/etc/init/rsc.rc

mount -o ro,remount /vendor
```

### Step 3: Install binary (use delete + recreate trick)

```bash
su
mount -o rw,remount /vendor

# Delete + recreate trick (ensures clean xattr)
if [ -f /vendor/bin/rsc ]; then
  rm -f /vendor/bin/rsc
fi
cp /data/local/tmp/rsc /vendor/bin/rsc
chmod 755 /vendor/bin/rsc
chown root:root /vendor/bin/rsc

# Set SELinux label at creation time
chcon u:object_r:rsc_exec:s0 /vendor/bin/rsc

# Verify label
ls -Z /vendor/bin/rsc
# Expected: u:object_r:rsc_exec:s0 /vendor/bin/rsc

mount -o ro,remount /vendor
```

### Step 4: Setup config (optional)

```bash
su
mkdir -p /data/adb/rsc
cp /data/local/tmp/config.example.toml /data/adb/rsc/config.toml
chmod 644 /data/adb/rsc/config.toml

# Edit config if needed (default: cutoff=80%, resume=70%)
# vi /data/adb/rsc/config.toml
```

### Step 5: Reboot

```bash
reboot
```

### Step 6: Verify

After reboot (wait ~30 seconds):

```bash
su
# Service running?
getprop init.svc.rsc
# Expected: running

# Process in correct domain?
ps -A -Z | grep rsc
# Expected: u:r:rsc:s0 root <pid> ... rsc

# Binary labeled correctly?
ls -Z /vendor/bin/rsc
# Expected: u:object_r:rsc_exec:s0

# Log file created?
ls -la /data/adb/rsc/
# Expected: rsc.log present

# Check log content
tail -20 /data/adb/rsc/rsc.log
# Expected:
#   [ts INFO] rsc v0.1.0 starting
#   [ts INFO] config: cutoff=80%, resume=70%, ...
#   [ts INFO] MTK battery paths verified
#   [ts INFO] uevent listener initialized (fd=3)
#   [ts INFO] uevent-driven mode active (no polling fallback)

# No AVC denials?
dmesg | grep "avc.*rsc" | tail -5
# Expected: empty
```

## Configuration

Edit `/data/adb/rsc/config.toml`:

```toml
cutoff = 80                    # Cut off charging at 80%
resume = 70                    # Resume at 70% (hysteresis)

debug = false                  # Set true for verbose logging
log_file = "/data/adb/rsc/rsc.log"
log_max_size_kb = 512
log_keep = 3
```

Note: Log timestamps are in GMT+8 (Asia/Makassar / WITA). Previous-boot
log is saved as `rsc-lastboot.log` by `rsc --cleanup` at boot.

Apply changes: `setprop ctl.restart rsc`

## Testing

### Test cut-off + resume cycle

```bash
# Monitor log in real-time
tail -f /data/adb/rsc/rsc.log

# 1. Plug charger → log shows "Charging" + "thermal delimiter ENABLED"
# 2. Wait until 80% → log shows "CUTTING OFF charging"
# 3. Battery discharges to 70% → log shows "RESUMING charging"
# 4. Cycle repeats: 80% → cut → 70% → resume → 80% → ...
```

### Test thermal delimiter

```bash
# Unplug charger → log shows "thermal delimiter DISABLED"
# Plug charger → log shows "thermal delimiter ENABLED"
```

### Enable debug mode

```bash
su
echo 'debug = true' >> /data/adb/rsc/config.toml
setprop ctl.restart rsc
tail -f /data/adb/rsc/rsc.log
# Now you'll see every uevent received, every sysfs read, every tick
```

## Troubleshooting

See [docs/TROUBLESHOOTING.md](docs/TROUBLESHOOTING.md) for:
- AVC denial patterns + fixes
- Silent failure diagnosis
- Label stuck at `vendor_file`
- restorecon issues
- CIL compile errors

## Rollback

```bash
su
mount -o rw,remount /vendor

# Restore originals
cp /data/local/tmp/rsc-backup/vendor_sepolicy.cil /vendor/etc/selinux/
cp /data/local/tmp/rsc-backup/vendor_file_contexts /vendor/etc/selinux/

# Remove daemon files
rm -f /vendor/bin/rsc
rm -f /vendor/etc/init/rsc.rc

mount -o ro,remount /vendor
rm -rf /data/adb/rsc/

reboot
```

## Compatibility

- **Verified**: Infinix X695C (Helio G95, Android 11, RP1A.200720.011)
- **Should work**: Other Transsion MTK devices (Infinix/Tecno/Itel) with same paths
- **Android 12+**: May need `_31_0` suffix adjustments in CIL type names
- **Non-MTK**: Not supported (different battery control interfaces)
