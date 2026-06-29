# Changelog

## [1.0.0] — 2026-06-29

### Initial stable release

RSC (Radiant Smart Charging) — Android MTK battery auto-cut & thermal
delimiter daemon. Pure uevent-driven, zero polling, 100% event-driven.

#### Features

- **Uevent-driven** via AF_NETLINK / KOBJECT_UEVENT (blocking recv, no
  timeout, no polling fallback). Daemon sleeps until kernel emits a
  power_supply event. Zero CPU when idle.
- **Auto-cut charging** at configurable percentage (default 80%).
- **Resume charging** at lower percentage with hysteresis (default 70%).
  Robust multi-strategy sequence (reset+re-apply primary, toggle
  fallback) with sysfs readback verification + post-resume cap
  trajectory health check (detects silent FET-stuck-off failure).
- **NTC thermal delimiter** toggle on charger plug/unplug.
- **Structured logfmt-style log lines** — `event=TYPE` field on every
  line for fast filtering (`grep "event=cutoff" rsc.log`). Per-line
  `seq=N` counter for precise ordering. `boot_id` groups lines per
  daemon lifetime.
- **GMT+8 (WITA) timestamps** — Asia/Makassar timezone, format
  `2026-06-28T11:40:46+08:00`. No mental UTC conversion needed.
- **Single-file boot log rotation** — `rsc.log` → `rsc-lastboot.log`
  on each boot via `rsc --cleanup` oneshot service. Only the MOST
  RECENT previous boot is kept (no multi-boot rotation chain).
- **Cleanup oneshot service** — `rsc_cleanup` runs at boot BEFORE main
  daemon, restores MTK sysfs state to safe defaults (mitigates SIGKILL
  leaving thermal/cut state dangling) + rotates log.
- **Event-driven stats** — counters logged on cutoff/resume/thermal/
  shutdown events, NOT on a fixed interval. Zero interval-based logic
  in the main loop.
- **Tight SELinux confinement** — custom `rsc` domain with dedicated
  types (`rsc_exec`, `rsc_data_file`, `rsc_mtk_battery_proc`).
- **Small footprint** — ~480 KB stripped binary, deps: libc + libdl +
  serde + toml + nix + chrono.

#### Subcommands

- `rsc` — normal daemon mode
- `rsc --cleanup` — restore MTK state + rotate log to `rsc-lastboot.log`,
  then exit 0. Intended for init oneshot service at boot.
- `rsc --help` / `rsc --version` — work without root

#### Configuration

TOML config at `/data/adb/rsc/config.toml`. Partial files supported.

| Key | Default | Description |
|-----|---------|-------------|
| `cutoff` | `80` | % at which charging is cut off |
| `resume` | `70` | % at which charging resumes (hysteresis) |
| `debug` | `false` | Verbose logging of every event/tick |
| `log_file` | `/data/adb/rsc/rsc.log` | Log file path |
| `log_max_size_kb` | `512` | Max log size before size-based rotation |
| `log_keep` | `3` | Number of size-rotated logs to keep |

#### SELinux policy

- Custom `rsc` domain with dedicated types (`rsc_exec`, `rsc_data_file`,
  `rsc_mtk_battery_proc`).
- Fallback `vendor_file` exec + entrypoint rules (for cil-only install).
- `seclabel u:r:rsc:s0` in `rsc.rc` for resilient domain transition.
- All permission fixes: sigkill, entrypoint, setopt, dir create, file
  create, sysfs_mm dontaudit.

#### CI/CD

- **ci.yml**: cargo fmt + clippy (warnings as errors) + check + test +
  cross-compile to aarch64-linux-android + SELinux CIL validation.
- **release.yml**: Build + ZIP bundle + GitHub Release (triggers on
  `v*` tag push).

#### Verified on

- Infinix X695C (Helio G95, Android 11, RP1A.200720.011)
