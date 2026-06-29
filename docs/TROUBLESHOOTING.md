# Troubleshooting Guide — rsc on Infinix X695C

Real-world issues encountered during installation and their solutions.
Based on actual debugging session.

## Issue 1: `avc: denied { execute } for comm="init" name="rsc"`

```
avc: denied { execute } for comm="init" name="rsc" dev="dm-3" ino=48289
  scontext=u:r:init:s0 tcontext=u:object_r:vendor_file:s0 tclass=file permissive=0
```

### Cause

Binary `/vendor/bin/rsc` dilabeli `vendor_file:s0` (default generic) karena `vendor_file_contexts.patched` belum di-push atau belum di-restorecon.

### Fix

Push `vendor_file_contexts.patched` ke `/vendor/etc/selinux/vendor_file_contexts`, hapus `precompiled_sepolicy*` hash files, reboot.

## Issue 2: Service `stopped` + no AVC denials + no log file

```
getprop init.svc.rsc = stopped
dmesg | grep "avc.*rsc" = empty
tail /data/adb/rsc/rsc.log = No such file
```

### Cause

**CIL patch had invalid `sigterm` permission** — compile failed silently at boot, init fell back to old policy without `rsc` type. `.rc` with `seclabel u:r:rsc:s0` was silently skipped because type didn't exist.

SELinux `process` class only supports: `sigchld, sigkill, sigstop, signull, signal` (generic). `sigterm` is NOT a valid SELinux permission — SIGTERM is covered by the generic `signal` permission.

### Fix

Replace `(allow init_30_0 rsc (process (signal sigkill sigterm)))` with `(allow init_30_0 rsc (process (signal sigkill)))` in `rsc.cil`.

### Lesson

Always run `secilc` to compile-test CIL locally before pushing to device. Silent compile failures are very hard to debug — no error appears in dmesg/logcat.

## Issue 3: `avc: denied { entrypoint } for comm="init" path="/vendor/bin/rsc"`

```
avc: denied { entrypoint } for comm="init" path="/vendor/bin/rsc"
  scontext=u:r:rsc:s0 tcontext=u:object_r:vendor_file:s0 tclass=file permissive=0
```

### Cause

SELinux checks `entrypoint` permission on the **NEW** domain (rsc), not on the OLD domain (init). The fallback rule had wrong subject:
- ❌ Wrong: `(allow init_30_0 vendor_file_30_0 (file (... entrypoint)))`
- ✅ Correct: `(allow rsc vendor_file_30_0 (file (entrypoint)))`

### Fix

Added `(allow rsc vendor_file_30_0 (file (entrypoint)))` in `rsc.cil` (commit `ccd2ad6`).

## Issue 4: `restorecon` fails with "Read-only file system"

```
SELinux: Could not set context for /vendor/bin/rsc: Read-only file system
restorecon: restorecon failed: /vendor/bin/rsc: Read-only file system
```

### Cause

`/vendor` partition is mounted read-only. `restorecon` needs write access to modify xattr.

### Fix attempts

1. ❌ `mount -o rw,remount /vendor` + restorecon — failed even with mount showing rw (some devices still block xattr writes due to dm-verity or AVB)
2. ❌ `chcon` directly — same "Read-only" error
3. ✅ **Delete + recreate binary** (force label at creation time):

```bash
adb shell su -c '
  mount -o rw,remount /vendor
  cp /vendor/bin/rsc /data/local/tmp/rsc.bak
  rm -f /vendor/bin/rsc              # removes corrupted xattr
  cp /data/local/tmp/rsc.bak /vendor/bin/rsc  # creates fresh file
  chmod 755 /vendor/bin/rsc
  chcon u:object_r:rsc_exec:s0 /vendor/bin/rsc  # set correct label at creation
  ls -Z /vendor/bin/rsc              # verify
  mount -o ro,remount /vendor
  setprop ctl.restart rsc
'
```

### Why delete+recreate works

- `rm` clears the inode (and its xattr)
- `cp` creates a new inode with fresh xattr
- `chcon` immediately after creation sets the correct label
- The new file has no "history" of corrupted xattr

## Issue 5: Service `stopped` after CIL fix + reboot

After fixing `sigterm` and `entrypoint` rules, service still `stopped`. Binary still `vendor_file:s0`.

### Cause

`restorecon` failed (Issue 4), so binary kept old label `vendor_file:s0`. Fallback rule should have worked but xattr was somehow stuck.

### Fix

Use the **delete + recreate** trick from Issue 4. After this, binary got `rsc_exec:s0` label and primary path fired.

## Summary — Installation Checklist

For future installs on similar devices:

1. ✅ Disable dm-verity: `adb disable-verity && adb reboot`
2. ✅ Push `vendor_sepolicy.cil.patched` to `/vendor/etc/selinux/vendor_sepolicy.cil`
3. ✅ Push `vendor_file_contexts.patched` to `/vendor/etc/selinux/vendor_file_contexts`
4. ✅ Push `rsc.rc` to `/vendor/etc/init/rsc.rc`
5. ✅ Push binary to `/vendor/bin/rsc` (use **delete + recreate** method, not direct cp overwrite)
6. ✅ Delete `precompiled_sepolicy*` hash files (forces CIL recompile at boot)
7. ✅ Delete `selinux_denial_metadata` (some Android 11 builds need this)
8. ✅ `chcon u:object_r:rsc_exec:s0 /vendor/bin/rsc` (set label at creation time)
9. ✅ `chmod 755 /vendor/bin/rsc`
10. ✅ Reboot
11. ✅ Verify: `getprop init.svc.rsc` = `running`
12. ✅ Verify: `ls -Z /vendor/bin/rsc` = `u:object_r:rsc_exec:s0`
13. ✅ Verify: `ps -A -Z | grep rsc` = `u:r:rsc:s0`
14. ✅ Verify: `dmesg | grep "avc.*rsc"` = empty

## Common Pitfalls

### Pitfall 1: Forgetting to delete precompiled_sepolicy hash files

**Symptom**: CIL patch is in `/vendor/etc/selinux/vendor_sepolicy.cil`, but policy doesn't change after reboot.

**Cause**: Init checks `precompiled_sepolicy.*.sha256` files. If they exist and match, init uses the OLD precompiled binary policy (ignores CIL source).

**Fix**: Always delete:
```bash
rm -f /vendor/etc/selinux/precompiled_sepolicy
rm -f /vendor/etc/selinux/precompiled_sepolicy.*.sha256
```

### Pitfall 2: Not running secilc compile test

**Symptom**: CIL patch looks correct in source, but daemon doesn't start after install.

**Cause**: Syntax error in CIL (invalid permission name, typo, etc.) causes silent compile failure at boot.

**Fix**: Always compile-test CIL locally:
```bash
secilc plat_sepolicy.cil plat_pub_versioned.cil 30.0.cil \
       vendor_sepolicy.cil rsc.cil -m -N -o /tmp/test.bin
# If compile fails, fix before pushing to device
```

### Pitfall 3: Restorecon fails on read-only /vendor

**Symptom**: `restorecon: Read-only file system` even after `mount -o rw,remount /vendor`.

**Cause**: dm-verity or AVB still blocks xattr writes despite mount showing rw.

**Fix**: Use delete + recreate trick (see Issue 4 above).

### Pitfall 4: cp preserves old xattr

**Symptom**: Pushing new binary via `cp /data/local/tmp/rsc /vendor/bin/rsc` keeps old label.

**Cause**: `cp` preserves xattr from source file (which has `vendor_file` label from /data/local/tmp default).

**Fix**: Use `cp` then immediately `chcon`, OR use `cp --no-preserve=xattr` (not all Android cp supports this).

Best: use the **delete + recreate** method that creates fresh inode.

## Compatibility Notes

This troubleshooting guide is based on real debugging session on:
- Device: Infinix X695C
- SoC: MTK Helio G95
- Android: 11 (RP1A.200720.011)
- Root: Magisk + DFE (Disable Force Encryption)
- rsc version: v1.0.0

Other MTK devices may have slightly different behavior, but the general troubleshooting steps should apply.
