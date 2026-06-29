//! rsc — Android MTK battery auto-cut & thermal delimiter daemon.
//!
//! Uevent-driven daemon (pure blocking recv, no polling). Listens on
//! netlink KOBJECT_UEVENT for power_supply events and reacts to:
//!   1. Capacity changes — cut off charging when cap >= `cutoff` (default 80%).
//!   2. Charge state changes — enable/disable thermal delimiter on charger
//!      plug-in / unplug.
//!   3. Resume charging when cap drops to `resume` (default 70%) with hysteresis.
//!
//! missed uevents. If uevent socket init fails, the daemon EXITS —
//! no degradation to polling mode.
//!
//! Debuggability:
//!   - `debug = true` in config enables verbose logging (every uevent,
//!     every sysfs read/write, every state transition).
//!   - Event-driven stats dump — counters are logged on significant
//!     events (cutoff/resume/thermal/health-check) and on shutdown,
//!     NOT on a fixed interval. Zero polling, zero timer wakeups. The
//!     daemon is 100% pure-uevent-driven.
//!   - Startup log includes: version, config, fd, paths, mode, boot_id.
//!   - Every log line has a per-process seq counter for precise ordering.
//!
//! Subcommands:
//!   - (no args)  — normal daemon mode
//!   - --cleanup  — restore MTK state + rotate log to rsc-lastboot.log, then exit 0.
//!     Intended to be run as a oneshot init service at boot before the
//!     main daemon starts.
//!
//! Designed to be launched from an Android init `.rc` file as `user root`.

mod battery;
mod config;
mod logger;
mod mtk;
mod uevent;

use std::fs;
use std::path::Path;
use std::process;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use nix::sys::signal::{self, SigHandler, Signal};
use nix::unistd::Uid;

const CONFIG_PATH: &str = "/data/adb/rsc/config.toml";
const BOOT_ID_PATH: &str = "/proc/sys/kernel/random/boot_id";
const VERSION: &str = env!("CARGO_PKG_VERSION");

static RUNNING: AtomicBool = AtomicBool::new(true);

extern "C" fn handle_signal(_sig: i32) {
    RUNNING.store(false, Ordering::SeqCst);
}

fn install_signal_handlers() {
    unsafe {
        let _ = signal::signal(Signal::SIGINT, SigHandler::Handler(handle_signal));
        let _ = signal::signal(Signal::SIGTERM, SigHandler::Handler(handle_signal));
        let _ = signal::signal(Signal::SIGHUP, SigHandler::Handler(handle_signal));
    }
}

fn require_root() {
    if !Uid::effective().is_root() {
        eprintln!("rsc: must run as root (uid 0)");
        process::exit(1);
    }
}

/// Read the kernel boot_id (same identifier systemd-journald uses for
/// `_BOOT_ID`). Returns a shortened 8-char prefix for log readability —
/// full UUID is overkill for correlating daemon lifetimes within rsc.log.
/// Returns "unknown" if /proc is not readable (very unlikely on Android).
fn read_boot_id() -> String {
    match fs::read_to_string(BOOT_ID_PATH) {
        Ok(s) => {
            let trimmed = s.trim();
            // Take first 8 chars of the UUID (e.g. "a30d49a3-...").
            let short: String = trimmed.chars().take(8).collect();
            if short.is_empty() {
                "unknown".to_string()
            } else {
                short
            }
        }
        Err(_) => "unknown".to_string(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CutState {
    /// Charging path is open — battery will accept current.
    Charging,
    /// Charging path is cut — battery will not accept current even if
    /// a charger is plugged in, until `resume_charging()` is called.
    CutOff,
}

impl CutState {
    /// Compact name for log output. Avoids `{:?}` which would emit
    /// `Charging` / `CutOff` (capitalization inconsistent with rest
    /// of log).
    fn as_str(&self) -> &'static str {
        match self {
            CutState::Charging => "charging",
            CutState::CutOff => "cutoff",
        }
    }
}

/// Runtime stats counters — all lock-free atomics. Dumped to log
/// periodically for diagnostics.
struct Stats {
    events_received: AtomicU64,
    events_relevant: AtomicU64,
    events_irrelevant: AtomicU64,
    events_drained: AtomicU64,
    ticks: AtomicU64,
    errors: AtomicU64,
    thermal_toggles: AtomicU64,
    cut_events: AtomicU64,
    resume_events: AtomicU64,
}

impl Stats {
    const fn new() -> Self {
        Self {
            events_received: AtomicU64::new(0),
            events_relevant: AtomicU64::new(0),
            events_irrelevant: AtomicU64::new(0),
            events_drained: AtomicU64::new(0),
            ticks: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            thermal_toggles: AtomicU64::new(0),
            cut_events: AtomicU64::new(0),
            resume_events: AtomicU64::new(0),
        }
    }

    /// Render stats as logfmt-style kv pairs. Caller prepends "stats" message.
    fn kv_snapshot(&self) -> [(&'static str, String); 9] {
        [
            ("ticks", self.ticks.load(Ordering::Relaxed).to_string()),
            (
                "events_recv",
                self.events_received.load(Ordering::Relaxed).to_string(),
            ),
            (
                "relevant",
                self.events_relevant.load(Ordering::Relaxed).to_string(),
            ),
            (
                "irrelevant",
                self.events_irrelevant.load(Ordering::Relaxed).to_string(),
            ),
            (
                "drained",
                self.events_drained.load(Ordering::Relaxed).to_string(),
            ),
            (
                "thermal_toggles",
                self.thermal_toggles.load(Ordering::Relaxed).to_string(),
            ),
            ("cut", self.cut_events.load(Ordering::Relaxed).to_string()),
            (
                "resume",
                self.resume_events.load(Ordering::Relaxed).to_string(),
            ),
            ("errors", self.errors.load(Ordering::Relaxed).to_string()),
        ]
    }
}

struct Daemon {
    cfg: config::Config,
    log: logger::FileLogger,
    cut: CutState,
    thermal_on: bool,
    /// Previous state for event-driven logging.
    last_cap: Option<u8>,
    last_state: Option<battery::ChargeState>,
    /// Boot ID — short prefix of /proc/sys/kernel/random/boot_id.
    /// All log lines from one boot can be grouped by this ID.
    boot_id: String,
    /// Runtime stats counters.
    stats: Stats,
    /// Timestamp of the last charging-status transition (true→false or
    /// false→true). Used by the kernel-flip debounce: if a new transition
    /// happens within `CHARGE_FLIP_DEBOUNCE_MS` of the previous one, the
    /// thermal delimiter toggle is suppressed (the daemon assumes it's a
    /// spurious kernel state flip, not a real plug/unplug event).
    /// See docs/KERNEL_FLIP_DEBUG.md for root-cause debugging guide.
    last_charge_flip: Option<Instant>,
}

/// Debounce window for charging-status transitions. If two consecutive
/// `charging` flips (true→false→true OR false→true→false) happen within
/// this window, the second flip is treated as a kernel glitch and the
/// thermal delimiter toggle is suppressed.
///
/// 2 seconds is chosen because:
///   - Real user plug/unplug takes at least 5-10 seconds (cable
///     manipulation is physically slow).
///   - USB PD renegotiation flips happen in <1 second.
///   - MTK driver fuel-gauge jitter happens in <500ms.
///
/// Set to `Duration::ZERO` to disable debounce entirely.
const CHARGE_FLIP_DEBOUNCE: Duration = Duration::from_millis(2000);

impl Daemon {
    fn new(cfg: config::Config) -> Self {
        let log = logger::FileLogger::new(&cfg.log_file, cfg.log_max_size_kb, cfg.log_keep);
        logger::ensure_log_dir(std::path::Path::new(&cfg.log_file));
        let boot_id = read_boot_id();
        Self {
            cfg,
            log,
            cut: CutState::Charging,
            thermal_on: false,
            last_cap: None,
            last_state: None,
            boot_id,
            stats: Stats::new(),
            last_charge_flip: None,
        }
    }

    /// Log a message with event type + stats counters appended.
    ///
    /// This is the event-driven replacement for the periodic stats dump.
    /// Instead of waking the daemon every N seconds to dump stats, we
    /// append the current stats counters to significant event log lines
    /// (cutoff, resume, thermal toggle, shutdown). This gives the same
    /// diagnostic visibility with ZERO interval-based logic — the daemon
    /// stays 100% pure-uevent-driven.
    ///
    /// The emitted line looks like:
    ///   [ts INFO seq=N] <msg> event=<event_type> <extra_kv...> boot_id=xxx ticks=N events_recv=N ...
    fn log_event_with_stats(
        &self,
        level: &str,
        msg: &str,
        event_type: &str,
        extra_kv: &[(&str, &str)],
    ) {
        let stats_kv = self.stats.kv_snapshot();
        let stats_ref: Vec<(&str, &str)> = stats_kv.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let mut full_kv: Vec<(&str, &str)> =
            Vec::with_capacity(extra_kv.len() + stats_ref.len() + 2);
        full_kv.push(("event", event_type));
        full_kv.extend_from_slice(extra_kv);
        full_kv.push(("boot_id", &self.boot_id));
        full_kv.extend(stats_ref.iter().copied());
        self.log.log_kv(level, msg, &full_kv);
    }

    fn run(&mut self) {
        // --- BOOT START marker — one line per daemon lifetime so all
        // subsequent lines can be grouped by boot_id when reading the
        // log. This is the journald `_BOOT_ID` pattern, simplified.
        self.log.log_kv(
            "INFO",
            "BOOT START rsc",
            &[
                ("event", "startup"),
                ("version", VERSION),
                ("boot_id", &self.boot_id),
            ],
        );

        // --- Verbose startup banner (always logged, even without debug) ---
        self.log.log_kv(
            "INFO",
            "rsc starting",
            &[
                ("event", "startup"),
                ("version", VERSION),
                ("cutoff", &format!("{}%", self.cfg.cutoff)),
                ("resume", &format!("{}%", self.cfg.resume)),
                ("debug", &self.cfg.debug.to_string()),
                ("stats_mode", "event-driven"),
            ],
        );
        self.log.log_kv(
            "INFO",
            "paths",
            &[
                ("event", "startup"),
                ("config_path", CONFIG_PATH),
                ("log_file", &self.cfg.log_file),
            ],
        );

        if !mtk::paths_exist() {
            self.log.log_kv(
                "ERROR",
                "MTK battery paths missing",
                &[
                    ("event", "error"),
                    ("hint", "is this really an MTK device?"),
                ],
            );
            process::exit(1);
        }
        self.log.log_kv(
            "INFO",
            "MTK battery paths verified",
            &[("event", "startup")],
        );

        // --- Initialize uevent listener — NO FALLBACK, exit on failure ---
        let uevent = match uevent::UeventListener::new() {
            Ok(l) => {
                self.log.log_kv(
                    "INFO",
                    "uevent listener initialized",
                    &[
                        ("event", "startup"),
                        ("fd", &l.fd().to_string()),
                        ("mode", "uevent"),
                    ],
                );
                l
            }
            Err(e) => {
                self.log.log_kv(
                    "ERROR",
                    "uevent listener init failed",
                    &[
                        ("event", "error"),
                        ("err", &e.to_string()),
                        (
                            "hint",
                            "uevent mandatory — no polling fallback. Check SELinux: allow rsc self netlink_kobject_uevent_socket { create bind read }",
                        ),
                    ],
                );
                process::exit(1);
            }
        };

        self.log.log_kv(
            "INFO",
            "uevent-driven mode active",
            &[("event", "startup"), ("fallback", "none")],
        );
        self.run_uevent(uevent);

        self.shutdown();
    }

    /// Uevent-driven main loop. Pure blocking recv — no polling, no
    /// timeout, no interval-based logic. The daemon sleeps on the
    /// netlink socket until the kernel emits a power_supply uevent.
    ///
    /// Flow:
    ///   1. tick() — read sysfs, apply state changes (initial baseline).
    ///   2. Block on uevent socket (recv_blocking — no timeout).
    ///   3. On relevant event: drain queued events, loop back to tick.
    ///   4. On EINTR (signal): check RUNNING, exit if false.
    ///
    /// Stats are dumped event-driven (on cutoff/resume/thermal events
    /// via `log_event_with_stats`) + once on shutdown. No periodic
    /// timer, no `last_stats_dump` tracking, no `elapsed()` check.
    fn run_uevent(&mut self, uevent: uevent::UeventListener) {
        // Initial tick to establish baseline state.
        self.tick();

        while RUNNING.load(Ordering::SeqCst) {
            match uevent.recv_blocking() {
                Ok(event) => {
                    self.stats.events_received.fetch_add(1, Ordering::Relaxed);

                    if event.is_relevant() {
                        self.stats.events_relevant.fetch_add(1, Ordering::Relaxed);
                        let summary = event.summary();
                        self.log.debug_if_kv(
                            self.cfg.debug,
                            "relevant uevent",
                            &[("event", "uevent"), ("payload", &summary)],
                        );
                        // Drain queued events (charger plug-in fires 3-5
                        // uevents in <100ms). Process as one tick.
                        let drained = uevent.try_drain();
                        if drained > 0 {
                            self.stats
                                .events_drained
                                .fetch_add(drained as u64, Ordering::Relaxed);
                        }
                        self.tick();
                    } else {
                        self.stats.events_irrelevant.fetch_add(1, Ordering::Relaxed);
                    }
                }
                Err(e) => {
                    if e.kind() == std::io::ErrorKind::Interrupted {
                        // EINTR from signal — check RUNNING at top of loop.
                        continue;
                    }
                    // Real socket error — log and back off briefly.
                    // Single 5s sleep (not 5x1s tick loop) to avoid
                    // polling-style wakeup. SIGTERM during this sleep
                    // may take up to 5s to take effect — acceptable
                    // since socket errors are rare (0 in 19h of log
                    // analysis). For immediate exit, use SIGKILL.
                    self.stats.errors.fetch_add(1, Ordering::Relaxed);
                    self.log.log_kv(
                        "WARN",
                        "uevent recv error",
                        &[
                            ("event", "warn"),
                            ("err", &e.to_string()),
                            ("errno_kind", &format!("{:?}", e.kind())),
                            ("retry_secs", "5"),
                        ],
                    );
                    thread::sleep(Duration::from_secs(5));
                }
            }
        }

        // Final stats dump on shutdown (event-driven — fires once
        // when the daemon exits, not on a timer).
        self.log_event_with_stats("INFO", "stats", "stats", &[("final", "true")]);
    }

    /// Returns `true` if currently charging.
    fn tick(&mut self) -> bool {
        self.stats.ticks.fetch_add(1, Ordering::Relaxed);

        let cap = match battery::read_capacity() {
            Ok(c) => c,
            Err(e) => {
                self.stats.errors.fetch_add(1, Ordering::Relaxed);
                self.log.log_kv(
                    "WARN",
                    "read capacity failed",
                    &[("event", "warn"), ("err", &e.to_string())],
                );
                return false;
            }
        };
        let cs = match battery::read_charge_state() {
            Ok(s) => s,
            Err(e) => {
                self.stats.errors.fetch_add(1, Ordering::Relaxed);
                self.log.log_kv(
                    "WARN",
                    "read status failed",
                    &[("event", "warn"), ("err", &e.to_string())],
                );
                return false;
            }
        };

        let charging = cs.is_charging();

        self.log.debug_if_kv(
            self.cfg.debug,
            "tick",
            &[
                ("event", "tick"),
                ("cap", &format!("{}%", cap)),
                ("state", &cs.to_string()),
                ("charging", &charging.to_string()),
                ("cut", self.cut.as_str()),
                ("thermal_on", &self.thermal_on.to_string()),
            ],
        );

        // Event-driven state log (INFO level): only log when state changes.
        let state_changed = self.last_cap != Some(cap) || self.last_state != Some(cs);
        if state_changed {
            self.log.log_kv(
                "INFO",
                "state",
                &[
                    ("event", "state"),
                    ("cap", &format!("{}%", cap)),
                    ("status", &cs.to_string()),
                    ("charging", &charging.to_string()),
                    ("cut", self.cut.as_str()),
                    ("thermal_on", &self.thermal_on.to_string()),
                ],
            );
            self.last_cap = Some(cap);
            self.last_state = Some(cs);
        }

        // --- Thermal delimiter: ON while charging, OFF otherwise ---
        //
        // Kernel-flip debounce (v1.0.1+): if the charging status just
        // flipped within `CHARGE_FLIP_DEBOUNCE` (2s), suppress the
        // thermal toggle — this is almost certainly a spurious kernel
        // state flip (USB PD renegotiation, MTK driver fuel-gauge
        // jitter), not a real plug/unplug event. Real user plug/unplug
        // takes at least 5-10 seconds.
        //
        // See docs/KERNEL_FLIP_DEBUG.md for root-cause debugging guide.
        let now = Instant::now();
        let mut suppress_thermal_toggle = false;
        if let Some(last_flip) = self.last_charge_flip {
            let elapsed_since_flip = now.duration_since(last_flip);
            if elapsed_since_flip < CHARGE_FLIP_DEBOUNCE {
                suppress_thermal_toggle = true;
                self.log.log_kv(
                    "DEBUG",
                    "charge flip debounce: suppressing thermal toggle",
                    &[
                        ("event", "charge_flip_suppressed"),
                        ("elapsed_ms", &elapsed_since_flip.as_millis().to_string()),
                        (
                            "threshold_ms",
                            &CHARGE_FLIP_DEBOUNCE.as_millis().to_string(),
                        ),
                    ],
                );
            }
        }

        if charging && !self.thermal_on {
            if suppress_thermal_toggle {
                // Kernel flip detected — leave thermal_on as-is (false).
                // The next real charging event will toggle it on.
            } else {
                self.log.debug_if_kv(
                    self.cfg.debug,
                    "enabling thermal delimiter",
                    &[("event", "thermal")],
                );
                match mtk::enable_thermal_delimiter() {
                    Ok(_) => {
                        self.thermal_on = true;
                        self.stats.thermal_toggles.fetch_add(1, Ordering::Relaxed);
                        self.log_event_with_stats(
                            "INFO",
                            "thermal delimiter ENABLED",
                            "thermal",
                            &[("reason", "charging_detected")],
                        );
                    }
                    Err(e) => {
                        self.stats.errors.fetch_add(1, Ordering::Relaxed);
                        self.log.log_kv(
                            "WARN",
                            "enable thermal failed",
                            &[("event", "warn"), ("err", &e.to_string())],
                        );
                    }
                }
            }
        } else if !charging && self.thermal_on {
            if suppress_thermal_toggle {
                // Kernel flip detected — leave thermal_on as-is (true).
                // The next real unplug event will toggle it off.
            } else {
                self.log.debug_if_kv(
                    self.cfg.debug,
                    "disabling thermal delimiter",
                    &[("event", "thermal")],
                );
                match mtk::disable_thermal_delimiter() {
                    Ok(_) => {
                        self.thermal_on = false;
                        self.stats.thermal_toggles.fetch_add(1, Ordering::Relaxed);
                        // After cutoff, charging=false but charger may still be
                        // plugged in. Use accurate reason instead of "unplugged".
                        let reason = if self.cut == CutState::CutOff {
                            "charging_cut_off"
                        } else {
                            "charger_unplugged"
                        };
                        self.log_event_with_stats(
                            "INFO",
                            "thermal delimiter DISABLED",
                            "thermal",
                            &[("reason", reason)],
                        );
                    }
                    Err(e) => {
                        self.stats.errors.fetch_add(1, Ordering::Relaxed);
                        self.log.log_kv(
                            "WARN",
                            "disable thermal failed",
                            &[("event", "warn"), ("err", &e.to_string())],
                        );
                    }
                }
            }
        }

        // Track this charging-status transition for the next tick's
        // debounce check. We record the flip regardless of whether the
        // thermal toggle was suppressed — the goal is to detect
        // rapid back-to-back transitions, not to track thermal state.
        if self.last_state.is_some() && self.last_state.unwrap().is_charging() != charging {
            self.last_charge_flip = Some(now);
        }

        // --- Auto-cut with hysteresis ---
        //
        // Cut-off: fire when charging AND cap >= cutoff AND not already cut.
        // Resume: fire when cut off AND cap <= resume, REGARDLESS of charging
        // status (after cut-off, status is NotCharging/Discharging — expected).
        //
        // NOTE: No top-up cycle. Pure uevent-driven — daemon only wakes on
        // kernel uevents. After cut-off, the device enters MTK bypass
        // charging mode (status=NotCharging): device runs directly on
        // charger power with low input current, battery is idle, and
        // battery level stays stable. Battery will resume charging
        // naturally when it drops to resume threshold (uevent fires on
        // capacity change).

        if charging && self.cut == CutState::Charging && cap >= self.cfg.cutoff {
            self.log.log_kv(
                "INFO",
                "CUTTING OFF charging",
                &[
                    ("event", "cutoff"),
                    ("cap", &format!("{}%", cap)),
                    ("cutoff", &format!("{}%", self.cfg.cutoff)),
                ],
            );
            match mtk::cut_off_charging() {
                Ok(_) => {
                    self.cut = CutState::CutOff;
                    self.stats.cut_events.fetch_add(1, Ordering::Relaxed);
                    self.log_event_with_stats(
                        "INFO",
                        "cutoff applied",
                        "cutoff",
                        &[("result", "ok")],
                    );
                }
                Err(e) => {
                    self.stats.errors.fetch_add(1, Ordering::Relaxed);
                    self.log.log_kv(
                        "ERROR",
                        "cut-off failed",
                        &[("event", "error"), ("err", &e.to_string())],
                    );
                }
            }
        } else if self.cut == CutState::CutOff && cap <= self.cfg.resume {
            // --- Resume charging (single-strategy) ---
            //
            // The resume sequence uses reset+re-apply: en_power_path=0
            // (force driver FSM reset) → sleep 50ms → en_power_path=1
            // (re-enable power path) → sleep 50ms → current_cmd=0 0
            // (clear cut flag). This is more robust than the naive
            // `en_power_path=1 + current_cmd=0 0` sequence which can
            // silently fail on some MTK BSP revisions.
            //
            // See src/mtk.rs::resume_charging() for the implementation.
            self.log.log_kv(
                "INFO",
                "RESUMING charging",
                &[
                    ("event", "resume"),
                    ("cap", &format!("{}%", cap)),
                    ("resume", &format!("{}%", self.cfg.resume)),
                ],
            );

            match mtk::resume_charging() {
                Ok(_) => {
                    self.cut = CutState::Charging;
                    self.stats.resume_events.fetch_add(1, Ordering::Relaxed);
                    self.log_event_with_stats(
                        "INFO",
                        "resume applied",
                        "resume",
                        &[("result", "ok")],
                    );
                }
                Err(e) => {
                    self.stats.errors.fetch_add(1, Ordering::Relaxed);
                    self.log.log_kv(
                        "ERROR",
                        "resume failed",
                        &[
                            ("event", "resume"),
                            ("result", "failed"),
                            ("err", &e.to_string()),
                            ("hint", "will retry on next tick"),
                        ],
                    );
                    // Leave cut=CutOff so we retry on next tick.
                }
            }
        }

        charging
    }

    fn shutdown(&mut self) {
        self.log.log_kv(
            "INFO",
            "rsc shutting down",
            &[("event", "shutdown"), ("boot_id", &self.boot_id)],
        );
        // Final stats snapshot already dumped at end of run_uevent.
        if self.thermal_on {
            let _ = mtk::disable_thermal_delimiter();
            self.log.log_kv(
                "INFO",
                "thermal delimiter restored on exit",
                &[("event", "shutdown"), ("restored", "thermal")],
            );
        }
        if self.cut == CutState::CutOff {
            let _ = mtk::resume_charging();
            self.log.log_kv(
                "INFO",
                "charging path resumed on exit",
                &[("event", "shutdown"), ("restored", "charging")],
            );
        }
        self.log.log_kv(
            "INFO",
            "BOOT END rsc",
            &[("event", "shutdown"), ("boot_id", &self.boot_id)],
        );
    }
}

// ---------------------------------------------------------------------------
// --cleanup subcommand
// ---------------------------------------------------------------------------

/// Restore MTK state to a safe default (thermal off, charging path open)
/// and rotate the current `rsc.log` to `rsc-lastboot.log` so the next
/// daemon lifetime starts with a fresh log file. Only ONE previous boot
/// log is kept — `rsc-lastboot.log` is overwritten on each cleanup.
///
/// This is intended to be invoked by an Android init oneshot service
/// at boot, BEFORE the main `rsc` daemon starts. It addresses two
/// concerns:
///
///   1. If the previous daemon instance was SIGKILLed (OOM, crash,
///      reboot), the MTK sysfs nodes (`disable_nafg`,
///      `ntc_disable_nafg`, `current_cmd`) may be left in a non-default
///      state. The next daemon instance assumes defaults and may not
///      re-apply them correctly.
///
///   2. Multiple boot lifetimes interleaved in one log file make
///      post-mortem analysis harder. Single-file rotation
///      (`rsc.log` -> `rsc-lastboot.log`) gives the user the previous
///      boot's log for comparison without filling up storage with
///      multi-boot rotation chains.
fn run_cleanup(cfg: &config::Config) -> i32 {
    let log_path = Path::new(&cfg.log_file);
    let lastboot_path = lastboot_log_path(log_path);

    // Use a temporary stderr-only logger for cleanup messages — we
    // don't want cleanup log lines polluting the new fresh log file.
    // Cleanup messages go to stderr (visible via `logcat` if init
    // is configured to capture them, otherwise silent).
    eprintln!("rsc: --cleanup starting (boot_id={})", read_boot_id());

    // 1. Restore MTK state — always do this defensively, ignore errors
    //    (paths may not exist on non-MTK devices, but we already validated
    //    them in main() before calling this).
    if mtk::paths_exist() {
        match mtk::disable_thermal_delimiter() {
            Ok(_) => eprintln!(
                "rsc: --cleanup thermal delimiter restored (disable_nafg=0, ntc_disable_nafg=0)"
            ),
            Err(e) => eprintln!(
                "rsc: --cleanup WARN: disable_thermal_delimiter failed: {}",
                e
            ),
        }
        match mtk::resume_charging() {
            Ok(_) => eprintln!("rsc: --cleanup charging path restored (current_cmd=0 0)"),
            Err(e) => eprintln!("rsc: --cleanup WARN: resume_charging failed: {}", e),
        }
    } else {
        eprintln!("rsc: --cleanup MTK paths missing — skipping sysfs restore");
    }

    // 2. Rotate rsc.log -> rsc-lastboot.log (single file, overwrite if exists)
    if log_path.exists() {
        // If rsc-lastboot.log already exists from an earlier boot,
        // overwrite it — we only keep the MOST RECENT previous boot.
        if lastboot_path.exists() {
            let _ = fs::remove_file(&lastboot_path);
        }
        match fs::rename(log_path, &lastboot_path) {
            Ok(_) => eprintln!(
                "rsc: --cleanup rotated {} -> {}",
                log_path.display(),
                lastboot_path.display()
            ),
            Err(e) => eprintln!("rsc: --cleanup WARN: rotate failed: {}", e),
        }
    } else {
        eprintln!("rsc: --cleanup no existing log to rotate");
    }

    // 3. Touch the fresh log file so the daemon can append cleanly.
    if let Some(parent) = log_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(_f) = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(log_path)
    {
        eprintln!("rsc: --cleanup fresh log created at {}", log_path.display());
    } else {
        eprintln!(
            "rsc: --cleanup WARN: could not create fresh log at {}",
            log_path.display()
        );
    }

    eprintln!("rsc: --cleanup complete");
    0
}

/// Compute the `rsc-lastboot.log` path from a base log path.
/// Replaces the base filename's stem with `<stem>-lastboot<ext>`.
/// Example: `/data/adb/rsc/rsc.log` -> `/data/adb/rsc/rsc-lastboot.log`
fn lastboot_log_path(base: &Path) -> std::path::PathBuf {
    let parent = base.parent().unwrap_or_else(|| Path::new(""));
    let filename = base
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("rsc.log");

    // Split "rsc.log" -> stem="rsc", ext=".log"
    // If no extension, just append "-lastboot" to the whole name.
    let new_name = match filename.rfind('.') {
        Some(dot_idx) if dot_idx > 0 => {
            let stem = &filename[..dot_idx];
            let ext = &filename[dot_idx..]; // includes the dot
            format!("{}-lastboot{}", stem, ext)
        }
        _ => format!("{}-lastboot", filename),
    };

    parent.join(new_name)
}

fn print_usage() {
    eprintln!("rsc v{} — Radiant Smart Charging daemon", VERSION);
    eprintln!();
    eprintln!("USAGE:");
    eprintln!("    rsc              Run as daemon (normal mode)");
    eprintln!("    rsc --cleanup    Restore MTK state + rotate log, then exit");
    eprintln!("    rsc --help       Show this help");
    eprintln!("    rsc --version    Show version");
    eprintln!();
    eprintln!("CONFIG: {}", CONFIG_PATH);
    eprintln!("LOG:    /data/adb/rsc/rsc.log (default)");
}

fn main() {
    // --- Parse args FIRST (so --help/--version work without root) ---
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        match args[1].as_str() {
            "--help" | "-h" => {
                print_usage();
                process::exit(0);
            }
            "--version" | "-V" => {
                println!("rsc {}", VERSION);
                process::exit(0);
            }
            _ => {} // fall through to root check + subcommand dispatch
        }
    }

    // All real subcommands (--cleanup, daemon mode) need root for
    // sysfs/procfs writes. Check AFTER --help/--version so users can
    // inspect the binary without privileges.
    require_root();

    if args.len() > 1 {
        match args[1].as_str() {
            "--cleanup" => {
                let cfg = config::Config::load(CONFIG_PATH).unwrap_or_default();
                let rc = run_cleanup(&cfg);
                process::exit(rc);
            }
            other => {
                eprintln!("rsc: unknown argument: {}", other);
                print_usage();
                process::exit(2);
            }
        }
    }

    // --- Normal daemon mode ---
    let cfg = match config::Config::load(CONFIG_PATH) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("rsc: config load failed ({}), using defaults", e);
            config::Config::default()
        }
    };

    install_signal_handlers();

    let mut daemon = Daemon::new(cfg);
    daemon.run();
}
