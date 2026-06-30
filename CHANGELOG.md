# Changelog

## [1.0.2] — 2026-06-29

### Changed — Use device local timezone (not hardcoded WITA)

Previous versions hardcoded GMT+8 (Asia/Makassar / WITA) timezone for
all log timestamps. This was a developer-specific choice — it doesn't
work for users in other timezones (WIB GMT+7, WIT GMT+9, or non-Indonesia
users).

v1.0.2 switches to `chrono::Local`, which reads the device's system
timezone from `/etc/localtime` or `TZ` environment variable. The daemon
now adapts to whatever timezone the Android device is configured to use.

#### What changed

- **`src/logger.rs`**: replaced `chrono::{FixedOffset, Utc}` import with
  `chrono::Local`. Removed `WITA_OFFSET_SECS` constant + `wita_tz()`
  helper function.
- **`log_kv()`**: replaced `Utc::now().with_timezone(&wita_tz())` with
  `Local::now()`. Same format string `%Y-%m-%dT%H:%M:%S%:z` produces the
  same ISO 8601 with offset suffix (e.g. `+08:00`, `+07:00`, `-05:00`).
- **Doc comments + README + INSTALL + config.example.toml**: updated to
  say "device's local timezone" instead of "GMT+8 WITA".

#### Behavior

- On a device configured for Asia/Makassar (WITA): timestamps will be
  `+08:00` (same as before).
- On a device configured for Asia/Jakarta (WIB): timestamps will be
  `+07:00`.
- On a device configured for America/New_York: timestamps will be
  `-05:00` (EST) or `-04:00` (EDT during DST).
- On a device with no timezone config: defaults to UTC (`+00:00`).

The UTC offset suffix is always included in the log, so the timezone is
unambiguous regardless of device locale — no mental conversion needed
when reading logs from devices in different timezones.

#### Compatibility

- No config changes.
- No breaking changes to log format (still ISO 8601 with offset suffix).
- No SELinux policy changes.
- Existing log analysis scripts using `grep` patterns on timestamps
  still work (the offset suffix is at the end of the timestamp, after
  the seconds field).

## [1.0.1] — 2026-06-29

### Added — Kernel flip debounce + debugging guide

The v0.0.4 device log revealed a "kernel flip" pattern: charging status
flips `Charging → Discharging → Charging` within the same second (0s
gap), which is physically impossible for a real plug/unplug event.
This is caused by USB PD renegotiation, MTK driver fuel-gauge jitter,
or loose cable/port — not a daemon bug, but the daemon was wasting
2 thermal toggles per flip.

#### Code-level mitigation

- **`CHARGE_FLIP_DEBOUNCE` (2 seconds)** — new constant in `src/main.rs`.
  If the charging status flips within 2s of the previous flip, the
  thermal delimiter toggle is suppressed. The daemon logs a DEBUG-level
  `event=charge_flip_suppressed` line with `elapsed_ms` + `threshold_ms`
  for diagnostics.
- **`last_charge_flip: Option<Instant>`** — new field on `Daemon` struct
  to track the timestamp of the most recent charging-status transition.
- 2s threshold chosen because real user plug/unplug takes ≥5s (physical
  cable manipulation), while kernel flips happen in <1s.

#### Debugging guide

- **`docs/KERNEL_FLIP_DEBUG.md`** — new comprehensive guide with 7-step
  debugging procedure: confirm pattern, capture raw uevent stream, check
  USB PD logs, check MTK battery driver logs, isolate hardware vs
  software, check charger type detection, long-term monitoring. Includes
  exact `adb shell` commands for each step.

### Changed — NotCharging = MTK bypass charging (doc correction)

User clarified: `status=NotCharging` on MTK devices is **bypass charging
mode** — device runs directly on charger power with low input current,
battery is idle, battery level stays stable (does NOT drop). This is
NOT the same as "charger unplugged".

Updated doc comments in:
- `src/battery.rs` — added "Status semantics on MTK devices" section
  explaining all 5 states (Charging, Discharging, NotCharging, Full,
  Unknown) with MTK-specific behavior.
- `src/main.rs` — cutoff comment updated from "device runs on charger
  power" to "device enters MTK bypass charging mode (status=NotCharging):
  device runs directly on charger power with low input current, battery
  is idle, and battery level stays stable".
- `src/battery.rs::is_charging()` — doc updated to explain why
  `NotCharging` is excluded (battery is idle in bypass mode, not
  receiving current).

### Removed — Strip unused resume fallback (per device log analysis)

Based on v0.0.4 device log analysis: **0 resume events** occurred during
9 hours of monitoring (cap never dropped to 80% threshold). The
following fallback/detection code was never exercised and is stripped
to reduce code complexity:

#### Removed from `src/mtk.rs`

- **`resume_charging_with_toggle()`** — Strategy B fallback toggle
  sequence (re-assert cut `1 1` then release `0 0`). Never called
  because primary `resume_charging()` never failed.
- **`verify_resume_applied()`** — sysfs readback verification. Only
  used to decide whether to invoke the toggle fallback.
- **`read_current_cmd()`** — read back `/proc/mtk_battery_cmd/current_cmd`.
  Only used by `verify_resume_applied()`.
- **`read_sysctl()`** — internal helper for reading sysfs values. Only
  used by `read_current_cmd()`.
- **`RESUME_TOGGLE_DELAY_MS`** constant — only used by toggle fallback.

#### Removed from `src/main.rs`

- **`ResumeHealth` struct** — post-resume cap trajectory tracker.
- **`RESUME_HEALTH_WARN_DROP_PCT`, `RESUME_HEALTH_CONFIRM_RISE_PCT`,
  `RESUME_HEALTH_CONFIRM_WINDOW`, `RESUME_HEALTH_FAIL_WINDOW`** —
  health check threshold constants.
- **`resume_health: Option<ResumeHealth>`** field on `Daemon` struct.
- **Post-resume health check logic** (~100 lines) — the 5min/10min
  cap trajectory monitoring state machine that detected silent resume
  failure (FET stuck off).
- **Multi-strategy resume orchestration** (~120 lines) — primary +
  verify + fallback + verify again + log which strategy succeeded.
  Replaced with single-strategy: call `resume_charging()`, log result.

#### Net code reduction

- `src/mtk.rs`: 231 → 161 lines (-70 lines, -30%)
- `src/main.rs`: ~1146 → ~830 lines (-316 lines, -28%)

#### Risk acknowledgment

Stripping the resume fallback + health check re-introduces the silent
resume failure risk documented in the v0.0.1 log analysis (Anomali #1:
cap dropped 80→56% in 2h38min after "successful" resume because FET
stayed off). Mitigations:

1. **Primary `resume_charging()` is still robust** — uses reset+re-apply
   sequence (`en_power_path=0` → 50ms → `en_power_path=1` → 50ms →
   `current_cmd=0 0`) which addresses the root cause better than the
   naive sequence.
2. **Retry on failure** — if `resume_charging()` returns Err, daemon
   leaves `cut=CutOff` and retries on next tick (when cap drops
   further). No silent acceptance of failure.
3. **User informed decision** — based on 9h device log showing 0
   resume events, user decided the fallback complexity is not worth
   the defensive value. If silent failure recurs, the health check
   can be re-added in a future version.

### Compatibility

- No config changes.
- No log format changes (new `event=charge_flip_suppressed` is
  DEBUG-level, only visible with `debug=true`).
- No SELinux policy changes.
- Resume behavior unchanged when `resume_charging()` succeeds. Only
  the fallback path (which never fired) is removed.

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
  Robust reset+re-apply sequence.
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
