# rsc SELinux CIL Direct Implant — Infinix X695C

This patch modifies the vendor SELinux policy **directly** by appending
to `vendor_sepolicy.cil` and `vendor_file_contexts`. No Magisk overlay,
no runtime `magiskpolicy` — pure vendor partition implant.

## Why direct CIL editing instead of Magisk overlay

| Aspect | Magisk overlay | Direct CIL (this patch) |
| --- | --- | --- |
| Persistence | Survives OTA via Magisk | Survives OTA only if vendor image untouched |
| Boot time | Slight delay (Magisk post-fs-data) | Native — init loads policy normally |
| SELinux enforcement | Runtime-injected by magiskpolicy | Compiled into binary policy at boot |
| Magisk dependency | Required | Not required |
| Rollback | Disable module in Magisk UI | Restore from backup + reboot |
| neverallow handling | magiskpolicy auto-bypasses some | Must be respected by the rules |

Choose this method if you want a **clean, native** install without Magisk
in the boot chain.

## Files in this patch

| File | Purpose |
| --- | --- |
| `rsc.cil` | CIL policy to append to `/vendor/etc/selinux/vendor_sepolicy.cil` |
| `file_contexts.patch` | Entries to append to `/vendor/etc/selinux/vendor_file_contexts` |
| `install.sh` | On-device installer: remount, append, invalidate hash, reboot |
| `README.md` | This file |

## Audit findings against vendor dump

Audited `vendor_sepolicy.cil` (13129 lines, 1019 KB) and
`vendor_file_contexts` (978 lines). Key findings:

### Path labels (from existing CIL)

| Path | Existing label | Source |
| --- | --- | --- |
| `/sys/devices/platform/battery/disable_nafg` | `sysfs_batteryinfo_30_0` | CIL line 345 (genfscon) |
| `/sys/devices/platform/battery/ntc_disable_nafg` | `sysfs_batteryinfo_30_0` | CIL line 345 (genfscon, prefix match) |
| `/proc/mtk_battery_cmd/en_power_path` | (none — generic `proc`) | Not in CIL |
| `/proc/mtk_battery_cmd/current_cmd` | (none — generic `proc`) | Not in CIL |

### Existing allow rules we can leverage

Other vendor domains already write to `sysfs_batteryinfo_30_0`:
- `factory` (CIL lines 5366-5368): full read/write
- `meta_tst` (CIL line 6253): full read/write
- `fuelgauged` (CIL lines 5445-5447): read-only

This means our policy of granting `rsc` write access to
`sysfs_batteryinfo_30_0` is consistent with existing vendor practice —
no neverallow violation because the neverallow rules in CIL target the
generic `sysfs_30_0` type, NOT the specific `sysfs_batteryinfo_30_0`.

### neverallow rule analysis

The vendor CIL has 329 neverallow rules; the platform CIL has 2037. Key
rules that COULD block us:

1. **`(neverallow base_typeattr_626_30_0 sysfs_30_0 (file (write ...)))`**
   — blocks writes to GENERIC sysfs. We avoid by writing to
   `sysfs_batteryinfo_30_0` (a different type).

2. **`proc` write neverallow** — avoided by labeling
   `/proc/mtk_battery_cmd/*` with our own type `rsc_mtk_battery_proc`
   (via genfscon in rsc.cil), then writing to that type.

3. **MLS restrictions** — bypassed by adding `rsc` to
   `mlstrustedsubject` attribute (same approach as `aee_aedv`,
   `thermalloadalgod`, `md_monitor`, etc. at CIL line 410).

## How CIL policy loading works on Android 11

At boot, Android's init does the following:

1. Reads `/vendor/etc/selinux/precompiled_sepolicy.*.sha256` files.
2. If the hashes match the running system's compiled policy, init loads
   `/vendor/etc/selinux/precompiled_sepolicy` (a pre-compiled binary).
3. If the hashes DON'T match (or files are missing), init RECOMPILES the
   policy from CIL sources:
   - `/system/etc/selinux/plat_sepolicy.cil`
   - `/vendor/etc/selinux/vendor_sepolicy.cil`
   - `/system_ext/etc/selinux/system_ext_sepolicy.cil`
   - `/product/etc/selinux/product_sepolicy.cil`
4. The compiled binary policy is loaded into the kernel.

This means **editing `vendor_sepolicy.cil` alone is NOT enough** — you
must also invalidate the hash files so init knows to recompile. The
`install.sh` script handles this for you by deleting:

- `precompiled_sepolicy.plat_sepolicy_and_mapping.sha256`
- `precompiled_sepolicy.system_ext_sepolicy_and_mapping.sha256`
- `precompiled_sepolicy.product_sepolicy_and_mapping.sha256`
- `precompiled_sepolicy` (the binary itself, to force recompile-only)

## Installation

### Prerequisites

1. **Rooted device** with ability to remount `/vendor` as rw.
2. **dm-verity disabled** (otherwise `/vendor` remount fails):
   ```bash
   adb disable-verity && adb reboot
   # Wait for reboot, then:
   adb root && adb wait-for-device
   # Some devices also need:
   adb shell su -c 'avbctl disable-verification && reboot'
   ```
3. **rsc binary** already compiled (see `/home/z/my-project/download/rsc-aarch64-android24`).
4. **rsc-vendor.rc** (the init service definition).

### Push files to device

```bash
# Push the patch
adb push selinux-cil-direct/ /data/local/tmp/selinux-cil-direct/

# Push the binary (rename for installer convention)
adb push rsc-aarch64-android24 /data/local/tmp/selinux-cil-direct/rsc-binary

# Push the init script
adb push rsc-vendor.rc /data/local/tmp/selinux-cil-direct/rsc-vendor.rc
```

### Run installer

```bash
adb shell su -c 'sh /data/local/tmp/selinux-cil-direct/install.sh'
```

### Reboot and verify

```bash
adb reboot
adb wait-for-device

# 1. Service running?
adb shell getprop init.svc.rsc
# Expected: running

# 2. Process in correct domain?
adb shell ps -A -Z | grep rsc
# Expected: u:r:rsc:s0 root <pid> ... rsc

# 3. File labels correct?
adb shell ls -Z /vendor/bin/rsc
# Expected: u:object_r:rsc_exec:s0

adb shell ls -Z /proc/mtk_battery_cmd/
# Expected: u:object_r:rsc_mtk_battery_proc:s0 for each file

adb shell ls -Z /sys/devices/platform/battery/disable_nafg
# Expected: u:object_r:sysfs_batteryinfo:s0 (existing label, unchanged)

adb shell ls -Z /data/adb/rsc/
# Expected: u:object_r:rsc_data_file:s0

# 4. No SELinux denials?
adb logcat -d | grep -i "avc.*denied.*rsc"
# Expected: empty

# 5. rsc.log shows successful startup?
adb shell tail /data/adb/rsc/rsc.log
# Expected:
#   [TIMESTAMP INFO] rsc v0.1.0 starting
#   [TIMESTAMP INFO] config: cutoff=80%, resume=70%, ...
#   [TIMESTAMP INFO] thermal delimiter ENABLED (charging detected)
```

## Troubleshooting

### Symptom: `init.svc.rsc = stopped` immediately after boot

The service failed to start. Check in order:

```bash
# 1. Did the policy compile at boot?
adb shell dmesg | grep -i "selinux.*policy\|sepolicy.*load\|cil.*error"
# Look for compile errors — most common cause of failure

# 2. Is the binary executable?
adb shell ls -la /vendor/bin/rsc
# Should be -rwxr-xr-x root root

# 3. Run rsc manually to see stderr
adb shell su -c '/vendor/bin/rsc'
# If it crashes immediately, look at stderr output

# 4. Check for SELinux denials blocking exec
adb logcat -d | grep -i "avc.*denied"
```

### Symptom: `SELinux: unable to load policy` in dmesg

The CIL patch has a syntax error. To recover:

```bash
# Remount /vendor rw
adb shell su -c 'mount -o rw,remount /vendor'

# Restore from backup (created by install.sh)
adb shell su -c 'cp /data/local/tmp/selinux-backup-*/vendor_sepolicy.cil /vendor/etc/selinux/'
adb shell su -c 'cp /data/local/tmp/selinux-backup-*/vendor_file_contexts /vendor/etc/selinux/'

# Reboot
adb reboot
```

Then examine the boot logs to find the exact CIL syntax error:
```bash
adb shell dmesg | grep -B2 -A5 "cil\|sepolicy"
```

Common syntax errors:
- Unbalanced parentheses — verify with `python3 check_cil.py` (included)
- Type referenced before declaration — CIL requires forward declaration
- Unknown typeattribute — must use existing attribute names from
  `plat_pub_versioned.cil`

### Symptom: `avc: denied { write } for ... tcontext=u:object_r:sysfs_batteryinfo:s0`

This shouldn't happen — our CIL explicitly allows this. If it does:

1. Verify the CIL was actually loaded (not the precompiled binary):
   ```bash
   adb shell su -c 'ls -la /vendor/etc/selinux/precompiled_sepolicy*'
   # Should show: No such file or directory (deleted by install.sh)
   ```

2. If `precompiled_sepolicy` exists, the hash invalidation failed. Run:
   ```bash
   adb shell su -c 'rm -f /vendor/etc/selinux/precompiled_sepolicy*'
   adb reboot
   ```

3. After reboot, check if rsc is in the right domain:
   ```bash
   adb shell ps -A -Z | grep rsc
   # If shows u:r:init:s0 (init's domain), the type_transition didn't fire
   # — likely the binary is not labeled rsc_exec
   adb shell restorecon /vendor/bin/rsc
   adb reboot
   ```

### Symptom: `avc: denied { write } for ... tcontext=u:object_r:proc:s0`

The `/proc/mtk_battery_cmd/*` paths are still labeled `proc` (generic).
This means our genfscon rule didn't take effect.

Cause: genfscon rules are applied at filesystem mount time, not at
policy load time. You need to **remount /proc** for the new labels to
apply — but /proc cannot be remounted on a running system. The labels
will only apply after a full reboot.

Fix: reboot the device. If the labels are STILL `proc` after reboot,
the genfscon rule has a typo — verify with:
```bash
adb shell su -c 'cat /sys/fs/selinux/initial_contexts' 2>/dev/null
# OR
adb shell su -c 'sesearch -r -s rsc -t rsc_mtk_battery_proc -p write /sys/fs/selinux/policy'
```

### Symptom: neverallow violation at boot

If `dmesg` shows something like:
```
SELinux: ... neverallow ... violated by allow rsc ...
```

This means our policy tried to grant access that a neverallow rule
forbids. Two fixes:

1. **Use the mlstrustedsubject bypass** (already active in rsc.cil).
   Verify it's still in the file:
   ```bash
   adb shell su -c 'grep mlstrustedsubject /vendor/etc/selinux/vendor_sepolicy.cil | tail -5'
   ```
   If missing, re-run the installer.

2. **Find the specific neverallow**:
   ```bash
   adb shell su -c 'grep -B5 "neverallow.*rsc" /vendor/etc/selinux/vendor_sepolicy.cil'
   ```
   Read the neverallow rule, identify what access it blocks, and
   remove the corresponding allow rule from `rsc.cil`. Then re-run
   the installer.

### Symptom: works on Infinix X695C but breaks on other MTK devices

This patch is calibrated for the X695C vendor dump (RP1A.200720.011).
On other MTK devices:

1. Check if `/sys/devices/platform/battery` is labeled
   `sysfs_batteryinfo` in your device's `vendor_sepolicy.cil`:
   ```bash
   adb shell su -c 'grep "platform/battery" /vendor/etc/selinux/vendor_sepolicy.cil'
   ```
   If not, replace `sysfs_batteryinfo_30_0` in rsc.cil with whatever
   label your device uses (or add a new genfscon rule).

2. Check the proc path:
   ```bash
   adb shell ls /proc/mtk_battery_cmd/
   # If empty, your MTK BSP uses a different proc path
   ```

3. The `_30_0` suffix on type names is the platform policy version
   (Android 11 = 30). On Android 12+ it may be `_31_0` or higher.
   Adjust accordingly.

## Rollback

To undo the patch:

```bash
# Find the backup directory created by install.sh
adb shell su -c 'ls /data/local/tmp/selinux-backup-*'

# Pick the latest one, then:
adb shell su -c '
  mount -o rw,remount /vendor
  cp /data/local/tmp/selinux-backup-*/vendor_sepolicy.cil /vendor/etc/selinux/
  cp /data/local/tmp/selinux-backup-*/vendor_file_contexts /vendor/etc/selinux/
  # Restore hash files if they existed
  cp /data/local/tmp/selinux-backup-*/precompiled_sepolicy* /vendor/etc/selinux/ 2>/dev/null
  mount -o ro,remount /vendor
'

adb reboot
```

## How the CIL patch was validated

1. **Parenthesis balance** — verified by `check_cil.py` (Python script
   that counts open/close parens per line).
2. **Type reference integrity** — all types referenced in allow rules
   are either declared in the patch or known to exist in the parent
   `vendor_sepolicy.cil` / `plat_pub_versioned.cil`.
3. **Typeattribute set membership** — only existing attributes
   (`domain`, `file_type`, `data_file_type`, `exec_type`, `proc_type`,
   `mlstrustedsubject`) are referenced.
4. **genfscon syntax** — matches the exact pattern used by the 350+
   existing genfscon rules in vendor_sepolicy.cil.
5. **Pattern compliance** — the rsc domain declaration mirrors the
   `thermal_core` pattern (CIL lines 2531-2534, 8233-8237), which is
   known to compile and load successfully on this device.

Note: full CIL compile verification requires `secilc` (SELinux CIL
compiler). It's not available in this build environment. The patch is
validated by syntax review and pattern matching against existing rules.
If compile fails at boot, the dmesg output will pinpoint the exact
line — see Troubleshooting above.

## Compatibility

| Aspect | Status |
| --- | --- |
| Device | Infinix X695C (Helio G95) |
| Vendor build | RP1A.200720.011 |
| Android | 11 (API 30, plat_sepolicy_vers 30.0) |
| CIL version | Matches vendor_sepolicy.cil structure |
| MTK paths | `/proc/mtk_battery_cmd/`, `/sys/devices/platform/battery/disable_nafg` |

Should also work on:
- Other MTK Helio devices with same paths (verify with `ls`)
- Android 11 vendor CIL structure (plat_sepolicy_vers 30.x)

May need adjustment for:
- Android 12+ (different `_31_0` suffix on type names)
- Non-MTK devices (different battery sysfs paths)
- Devices with Magisk already installed (consider Magisk overlay approach instead)

## License

MIT.
