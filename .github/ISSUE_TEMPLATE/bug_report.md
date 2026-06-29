---
name: Bug Report
about: Report a bug in rsc
title: "[BUG] "
labels: bug
assignees: ''
---

## Device information

- **Device model**: (e.g. Infinix X695C)
- **SoC**: (e.g. MTK Helio G95)
- **Android version**: (e.g. 11, RP1A.200720.011)
- **Root method**: (e.g. Magisk, KernelSU, vendor rw)
- **rsc version**: (e.g. v0.3.0 — check startup log `rsc v0.X.X starting`)

## Symptom

<!-- What did you expect? What happened instead? -->

## Steps to reproduce

1.
2.
3.

## Log excerpt

<!-- Enable debug=true in config, restart service, reproduce, then attach
     relevant lines from /data/adb/rsc/rsc.log -->

```
<paste log here>
```

## SELinux denials

<!-- Output of: adb logcat -d | grep avc.*rsc -->

```
<paste denials here, or "none" if empty>
```

## Stats dump

<!-- Last "stats:" line from /data/adb/rsc/rsc.log -->

```
<paste here>
```

## Configuration

<!-- Contents of /data/adb/rsc/config.toml, or "default" if missing -->

```toml
<paste here>
```

## Additional context

<!-- Anything else relevant? Custom ROM? Modified kernel? Other battery
     management apps running? -->
