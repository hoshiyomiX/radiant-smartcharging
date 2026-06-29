---
name: Device Compatibility Report
about: Report rsc working (or not working) on a specific device
title: "[COMPAT] "
labels: compatibility
assignees: ''
---

## Device information

- **Device model**:
- **SoC**:
- **Android version**:
- **Vendor build**:
- **Root method**:

## Path verification

```
$ adb shell ls /proc/mtk_battery_cmd/
<output>

$ adb shell ls /sys/devices/platform/battery/disable_nafg
<output>

$ adb shell cat /vendor/etc/selinux/plat_sepolicy_vers.txt
<output>
```

## SELinux policy

- Did the stock vendor_sepolicy.cil need patching? (yes/no)
- Did you use the pre-patched files from this repo, or did you adapt
  them? If adapted, what changed?

## Test results

| Test | Result |
| --- | --- |
| Daemon starts (init.svc.rsc = running) | |
| Runs in rsc domain (ps -A -Z shows u:r:rsc:s0) | |
| Uevent mode active (log shows "uevent-driven mode active") | |
| Charging cut-off fires at cutoff % | |
| Charging resumes at resume % | |
| Thermal delimiter toggles on charger plug/unplug | |
| No SELinux denials in logcat | |

## Modifications needed

<!-- If you had to change anything from the stock repo (paths, CIL
     types, .rc seclabel, etc.), describe here. PRs welcome! -->

## Logs

<!-- Attach the first ~20 lines of /data/adb/rsc/rsc.log after
     startup, and one stats dump line. -->
