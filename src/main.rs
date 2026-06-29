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
    /// Post-resume health check state.
    /// When a resume is applied, we record cap_at_resume + time_of_resume.
    /// On each subsequent tick for up to 10 minutes, we compare cap
    /// trajectory to detect silent resume failure (FET stuck off
    /// despite sysfs reporting Charging).
    resume_health: Option<ResumeHealth>,
}

/// Post-resume health check tracker.
///
/// After `resume_charging()` succeeds, the daemon records the cap and
/// timestamp. On each subsequent tick, it checks:
///
///   - Within 5 min: if cap drops 2+% below resume_cap → WARN
///     (result=degrading). This is the silent-failure signature —
///     cap went 80→56 in 2h38min after "successful"
///     resume, because `current_cmd=0 0` wrote OK but the FET stayed
///     off.
///
///   - At 5 min: if cap has risen 1+% → INFO (result=confirmed), clear
///     tracker. Resume was successful.
///
///   - At 10 min: if cap is still flat or dropping → ERROR
///     (result=failed), clear tracker, suggest manual intervention.
///     User may need to physically replug the charger.
#[derive(Debug, Clone, Copy)]
struct ResumeHealth {
    /// Capacity (%) at the moment resume was applied.
    cap_at_resume: u8,
    /// Timestamp when resume was applied.
    time: Instant,
}

/// Thresholds for the post-resume health check.
const RESUME_HEALTH_WARN_DROP_PCT: u8 = 2;
const RESUME_HEALTH_CONFIRM_RISE_PCT: u8 = 1;
const RESUME_HEALTH_CONFIRM_WINDOW: Duration = Duration::from_secs(5 * 60);
const RESUME_HEALTH_FAIL_WINDOW: Duration = Duration::from_secs(10 * 60);

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
            resume_health: None,
        }
    }

    /// Log a message with event type + stats counters appended.
    ///
    /// This is the event-driven replacement for the periodic stats dump.
    /// Instead of waking the daemon every N seconds to dump stats, we
    /// append the current stats counters to significant event log lines
    /// (cutoff, resume, thermal toggle, health check verdicts, shutdown).
    /// This gives the same diagnostic visibility with ZERO interval-based
    /// logic — the daemon stays 100% pure-uevent-driven.
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
        if charging && !self.thermal_on {
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
        } else if !charging && self.thermal_on {
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

        // --- Auto-cut with hysteresis ---
        //
        // Cut-off: fire when charging AND cap >= cutoff AND not already cut.
        // Resume: fire when cut off AND cap <= resume, REGARDLESS of charging
        // status (after cut-off, status is NotCharging/Discharging — expected).
        //
        // NOTE: No top-up cycle. Pure uevent-driven — daemon only wakes on
        // kernel uevents. After cut-off, battery stays at cutoff level while
        // charger is plugged in (MTK cut-off makes device run on charger power).
        // Battery will resume charging naturally when it drops to resume
        // threshold (uevent fires on capacity change).

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
            // --- Resume charging (primary + fallback strategy) ---
            //
            // The naive resume sequence (en_power_path=1 + current_cmd=0 0)
            // silently failed on Infinix X695C: sysfs write succeeded but
            // hardware FET stayed off, causing battery to keep draining
            // despite kernel reporting POWER_SUPPLY_STATUS=Charging.
            //
            // Strategy:
            //   1. Try primary sequence (reset+re-apply en_power_path).
            //   2. Verify sysfs readback shows current_cmd=0 0.
            //   3. If verification fails, try fallback toggle sequence.
            //   4. Either way, set up post-resume health check that
            //      monitors cap trajectory for 5-10 minutes to detect
            //      the silent failure mode (FET stuck off).
            self.log.log_kv(
                "INFO",
                "RESUMING charging",
                &[
                    ("event", "resume"),
                    ("cap", &format!("{}%", cap)),
                    ("resume", &format!("{}%", self.cfg.resume)),
                ],
            );

            let mut applied = false;
            let mut strategy_used = "none";

            // Strategy A: primary reset+re-apply sequence.
            match mtk::resume_charging() {
                Ok(_) => {
                    // Verify sysfs readback.
                    match mtk::verify_resume_applied() {
                        Ok(true) => {
                            applied = true;
                            strategy_used = "primary";
                        }
                        Ok(false) => {
                            // Sysfs readback mismatch — try fallback.
                            self.log.log_kv(
                                "WARN",
                                "resume primary strategy: sysfs readback mismatch",
                                &[
                                    ("event", "resume"),
                                    ("strategy", "primary"),
                                    ("verified", "false"),
                                    ("fallback", "trying_toggle"),
                                ],
                            );
                        }
                        Err(e) => {
                            self.log.log_kv(
                                "WARN",
                                "resume primary strategy: verify failed",
                                &[
                                    ("event", "resume"),
                                    ("strategy", "primary"),
                                    ("err", &e.to_string()),
                                    ("fallback", "trying_toggle"),
                                ],
                            );
                        }
                    }
                }
                Err(e) => {
                    self.log.log_kv(
                        "WARN",
                        "resume primary strategy failed",
                        &[
                            ("event", "resume"),
                            ("strategy", "primary"),
                            ("err", &e.to_string()),
                            ("fallback", "trying_toggle"),
                        ],
                    );
                }
            }

            // Strategy B: fallback toggle sequence (only if A didn't verify).
            if !applied {
                match mtk::resume_charging_with_toggle() {
                    Ok(_) => match mtk::verify_resume_applied() {
                        Ok(true) => {
                            applied = true;
                            strategy_used = "toggle";
                        }
                        Ok(false) => {
                            self.log.log_kv(
                                "ERROR",
                                "resume toggle strategy: sysfs readback mismatch",
                                &[
                                    ("event", "resume"),
                                    ("strategy", "toggle"),
                                    ("verified", "false"),
                                ],
                            );
                        }
                        Err(e) => {
                            self.log.log_kv(
                                "ERROR",
                                "resume toggle strategy: verify failed",
                                &[
                                    ("event", "resume"),
                                    ("strategy", "toggle"),
                                    ("err", &e.to_string()),
                                ],
                            );
                        }
                    },
                    Err(e) => {
                        self.log.log_kv(
                            "ERROR",
                            "resume toggle strategy failed",
                            &[
                                ("event", "resume"),
                                ("strategy", "toggle"),
                                ("err", &e.to_string()),
                            ],
                        );
                    }
                }
            }

            if applied {
                self.cut = CutState::Charging;
                self.stats.resume_events.fetch_add(1, Ordering::Relaxed);
                self.log_event_with_stats(
                    "INFO",
                    "resume applied",
                    "resume",
                    &[
                        ("result", "ok"),
                        ("strategy", strategy_used),
                        ("verified", "true"),
                    ],
                );
                // Arm the post-resume health check — monitor cap
                // trajectory for the next 5-10 minutes to detect the
                // silent failure mode (FET stuck off).
                self.resume_health = Some(ResumeHealth {
                    cap_at_resume: cap,
                    time: Instant::now(),
                });
                self.log.log_kv(
                    "INFO",
                    "resume health check armed",
                    &[
                        ("event", "resume_health"),
                        ("phase", "armed"),
                        ("cap_at_resume", &format!("{}%", cap)),
                        (
                            "warn_drop_threshold",
                            &format!("{}%", RESUME_HEALTH_WARN_DROP_PCT),
                        ),
                        (
                            "confirm_window_secs",
                            &RESUME_HEALTH_CONFIRM_WINDOW.as_secs().to_string(),
                        ),
                        (
                            "fail_window_secs",
                            &RESUME_HEALTH_FAIL_WINDOW.as_secs().to_string(),
                        ),
                    ],
                );
            } else {
                self.stats.errors.fetch_add(1, Ordering::Relaxed);
                self.log.log_kv(
                    "ERROR",
                    "resume failed — all strategies exhausted",
                    &[
                        ("event", "resume"),
                        ("result", "failed"),
                        ("hint", "manual charger replug may be required"),
                    ],
                );
                // Leave cut=CutOff so we retry on next tick. The
                // post-resume health check is NOT armed.
            }
        }

        // --- Post-resume health check ---
        //
        // If we recently applied a resume, monitor cap trajectory to
        // detect the silent failure mode where sysfs reports Charging
        // but the FET is actually still off (battery keeps draining).
        // See ResumeHealth struct docs for the full state machine.
        if let Some(rh) = self.resume_health {
            let elapsed = rh.time.elapsed();
            let cap_delta = cap as i16 - rh.cap_at_resume as i16;

            if elapsed >= RESUME_HEALTH_FAIL_WINDOW {
                // 10-minute window expired — make final verdict.
                if cap_delta < RESUME_HEALTH_CONFIRM_RISE_PCT as i16 {
                    // Cap didn't rise enough — silent failure.
                    self.log.log_kv(
                        "ERROR",
                        "resume health check FAILED — cap not rising",
                        &[
                            ("event", "resume_health"),
                            ("result", "failed"),
                            ("cap_at_resume", &format!("{}%", rh.cap_at_resume)),
                            ("cap_now", &format!("{}%", cap)),
                            ("delta", &format!("{}%", cap_delta)),
                            ("elapsed_secs", &elapsed.as_secs().to_string()),
                            (
                                "hint",
                                "FET likely stuck off — manual charger replug required",
                            ),
                        ],
                    );
                } else {
                    // Cap rose eventually — late confirmation.
                    self.log.log_kv(
                        "INFO",
                        "resume health check confirmed (late)",
                        &[
                            ("event", "resume_health"),
                            ("result", "confirmed_late"),
                            ("cap_at_resume", &format!("{}%", rh.cap_at_resume)),
                            ("cap_now", &format!("{}%", cap)),
                            ("delta", &format!("{}%", cap_delta)),
                            ("elapsed_secs", &elapsed.as_secs().to_string()),
                        ],
                    );
                }
                self.resume_health = None;
            } else if elapsed >= RESUME_HEALTH_CONFIRM_WINDOW {
                // 5-minute window — check for confirmation.
                if cap_delta >= RESUME_HEALTH_CONFIRM_RISE_PCT as i16 {
                    // Cap rose 1+% — resume confirmed successful.
                    self.log.log_kv(
                        "INFO",
                        "resume health check confirmed",
                        &[
                            ("event", "resume_health"),
                            ("result", "confirmed"),
                            ("cap_at_resume", &format!("{}%", rh.cap_at_resume)),
                            ("cap_now", &format!("{}%", cap)),
                            ("delta", &format!("{}%", cap_delta)),
                            ("elapsed_secs", &elapsed.as_secs().to_string()),
                        ],
                    );
                    self.resume_health = None;
                } else if cap_delta <= -(RESUME_HEALTH_WARN_DROP_PCT as i16) {
                    // Cap dropped 2+% — silent failure signature.
                    self.log.log_kv(
                        "WARN",
                        "resume health check DEGRADING — cap dropping despite charging=true",
                        &[
                            ("event", "resume_health"),
                            ("result", "degrading"),
                            ("cap_at_resume", &format!("{}%", rh.cap_at_resume)),
                            ("cap_now", &format!("{}%", cap)),
                            ("delta", &format!("{}%", cap_delta)),
                            ("elapsed_secs", &elapsed.as_secs().to_string()),
                            (
                                "hint",
                                "FET may be stuck off — will keep monitoring until 10min window",
                            ),
                        ],
                    );
                }
                // else: cap flat — keep monitoring until 10min window.
            } else {
                // Within first 5 minutes — only warn on significant drop.
                if cap_delta <= -(RESUME_HEALTH_WARN_DROP_PCT as i16) {
                    self.log.log_kv(
                        "WARN",
                        "resume health check DEGRADING — cap dropping early",
                        &[
                            ("event", "resume_health"),
                            ("result", "degrading_early"),
                            ("cap_at_resume", &format!("{}%", rh.cap_at_resume)),
                            ("cap_now", &format!("{}%", cap)),
                            ("delta", &format!("{}%", cap_delta)),
                            ("elapsed_secs", &elapsed.as_secs().to_string()),
                        ],
                    );
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
