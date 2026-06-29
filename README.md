# rsc — Radiant Smart Charging

[![Version](https://img.shields.io/badge/version-v1.0.0-blue.svg)](https://github.com/hoshiyomiX/radiant-smartcharging/releases/tag/v1.0.0)
[![License: MIT](https://img.shields.io/badge/license-MIT-green.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-orange.svg)](https://www.rust-lang.org/)
[![CI](https://github.com/hoshiyomiX/radiant-smartcharging/actions/workflows/ci.yml/badge.svg)](https://github.com/hoshiyomiX/radiant-smartcharging/actions/workflows/ci.yml)
[![Release](https://github.com/hoshiyomiX/radiant-smartcharging/actions/workflows/release.yml/badge.svg)](https://github.com/hoshiyomiX/radiant-smartcharging/actions/workflows/release.yml)
[![Downloads](https://img.shields.io/github/downloads/hoshiyomiX/radiant-smartcharging/total.svg)](https://github.com/hoshiyomiX/radiant-smartcharging/releases)
[![Platform](https://img.shields.io/badge/platform-Android%20aarch64-lightgrey.svg)](https://github.com/hoshiyomiX/radiant-smartcharging/releases)
[![SoC](https://img.shields.io/badge/SoC-MediaTek-red.svg)](https://github.com/hoshiyomiX/radiant-smartcharging)

**RSC** (Radiant Smart Charging) is a Rust init daemon for MediaTek-powered
Android devices (Transsion: Infinix / Tecno / Itel). It monitors battery
state via **netlink uevents** (pure event-driven, zero polling) and:

1. **Auto-cuts charging** at a configurable percentage (default 80%).
2. **Resumes charging** at a lower percentage (default 70%) with hysteresis.
3. **Toggles the MTK NTC thermal delimiter** on charger plug/unplug.

## Key features

- **Pure uevent-driven**: Blocking `recv()` on AF_NETLINK / KOBJECT_UEVENT.
  Zero CPU when idle. No polling, no timeout, no fallback. If the socket
  cannot be opened, the daemon exits.
- **Debuggable**: `debug = true` in config enables verbose logging. Stats
  are logged event-driven (on cutoff/resume/thermal events + shutdown)
  — zero polling overhead.
- **GMT+8 (WITA) timestamps**: All log lines use Asia/Makassar timezone
  for easy reading without mental UTC conversion.
- **Tight SELinux confinement**: Custom `rsc` domain with dedicated types
  (`rsc_exec`, `rsc_data_file`, `rsc_mtk_battery_proc`).
- **Small footprint**: ~480 KB stripped binary, only deps: libc + libdl.

## Repository structure

```
radiant-smartcharging/
├── Cargo.toml                # Rust manifest (release: opt-z, LTO, strip)
├── config.example.toml       # Example config (partial TOML supported)
├── INSTALL.md                # Step-by-step installation guide
├── CHANGELOG.md              # Version history
├── LICENSE                   # MIT
├── src/
│   ├── main.rs               # Entry point, daemon loop, stats, signals
│   ├── config.rs             # TOML config loader (partial file support)
│   ├── battery.rs            # Sysfs reader (capacity, charge state)
│   ├── mtk.rs                # MTK /proc & /sys writers (cut/resume/thermal)
│   ├── logger.rs             # File logger with size-based rotation
│   └── uevent.rs             # AF_NETLINK KOBJECT_UEVENT listener + parser
├── tests/
│   └── config_test.rs        # Config validation tests
├── android/
│   ├── rsc.rc                # Android init service definition
│   ├── build.sh              # Cross-compile script (aarch64-linux-android)
│   └── install.sh            # On-device installer
├── selinux/
│   ├── rsc.cil               # CIL policy patch
│   ├── file_contexts.patch   # File label entries
│   ├── vendor_sepolicy.cil.patched    # Pre-patched CIL (Infinix X695C)
│   ├── vendor_file_contexts.patched   # Pre-patched file_contexts
│   ├── check_cil.py          # CIL syntax validator
│   ├── install.sh            # SELinux patch installer
│   └── README.md             # SELinux docs + troubleshooting
├── docs/
│   ├── AUDIT.md              # CPU/battery efficiency audit
│   ├── ANALYSIS.md           # SELinux architecture analysis
│   └── TROUBLESHOOTING.md    # Real-world debugging guide
└── .github/
    ├── workflows/
    │   ├── ci.yml            # Lint + syntax check + compile test
    │   └── release.yml       # Build + ZIP bundle + GitHub Release
    ├── CONTRIBUTING.md
    └── ISSUE_TEMPLATE/
```

## How it works

The daemon opens an AF_NETLINK socket bound to KOBJECT_UEVENT at startup.
It then blocks on `recv()` — the kernel schedules the process only when a
uevent arrives. No timer, no polling, no CPU usage when idle.

On each relevant uevent (`SUBSYSTEM=power_supply` + `DEVPATH` contains
`/power_supply/battery`), the daemon reads sysfs, applies state changes
(cut-off, resume, thermal toggle), and logs only on state change.

## Configuration

Config file: `/data/adb/rsc/config.toml`. Partial files supported —
specify only the fields you want to override.

| Key | Default | Description |
| --- | --- | --- |
| `cutoff` | `80` | % at which charging is cut off |
| `resume` | `70` | % at which charging resumes (hysteresis) |
| `debug` | `false` | Verbose logging of every event/tick |
| `log_file` | `/data/adb/rsc/rsc.log` | Log file path |
| `log_max_size_kb` | `512` | Max log size before rotation |
| `log_keep` | `3` | Number of rotated logs to keep |

Log timestamps are in GMT+8 (Asia/Makassar / WITA). Previous-boot log
is saved as `rsc-lastboot.log` by `rsc --cleanup` at boot.

## Installation

See [INSTALL.md](INSTALL.md) for complete step-by-step guide.

Quick summary:
1. Download `rsc-v1.0.0-bundle.zip` from [Releases](https://github.com/hoshiyomiX/radiant-smartcharging/releases)
2. Push files to `/vendor/bin/`, `/vendor/etc/init/`, `/vendor/etc/selinux/`
3. Delete `precompiled_sepolicy*` hash files
4. Reboot

## CI/CD

| Workflow | Trigger | Purpose |
| --- | --- | --- |
| `ci.yml` | Push to main + PRs | cargo fmt + clippy + check + test + cross-compile + CIL validation |
| `release.yml` | Tag push `v*` | Build binary + assemble ZIP bundle + upload to GitHub Release |

## Compatibility

- **Verified**: Infinix X695C (Helio G95, Android 11, RP1A.200720.011)
- **Should work**: Other Transsion MTK devices with same paths
- **Not supported**: Non-MTK devices (Snapdragon, Exynos)

## License

MIT. See [LICENSE](LICENSE).
