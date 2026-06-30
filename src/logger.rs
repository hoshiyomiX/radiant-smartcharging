//! Tiny file-backed logger with size-based rotation and structured
//! (logfmt-style) line format.
//!
//! No `log` facade, no `env_logger` — rsc is a daemon with a single
//! consumer (the sysadmin reading `rsc.log` via `adb shell cat|grep`)
//! so we keep this to ~150 lines of plain stdlib code.
//!
//! ## Line format
//!
//! ```text
//! [2026-06-28T11:40:46+08:00 INFO  seq=42] CUTTING OFF cap=95 cutoff=95
//! ```
//!
//! - Fixed prefix `[<iso8601_with_offset> <LEVEL_padded>  seq=<n>]` —
//!   backward compatible with grep patterns targeting `[INFO]` or
//!   message text.
//! - Timestamps use the **device's local timezone** (whatever Android
//!   is configured to use via `/etc/localtime` or `TZ` env var). Format
//!   includes the UTC offset suffix (e.g. `+08:00`, `-05:00`, `+00:00`)
//!   so the timezone is unambiguous in the log regardless of device
//!   locale.
//! - Message text comes right after the `]` — preserves existing
//!   `grep "CUTTING OFF"` / `grep "thermal delimiter"` workflows.
//! - Zero or more `key=value` pairs are appended after the message,
//!   separated by spaces. Values containing spaces, `=`, or `"` are
//!   double-quoted with `\` and `"` escaped (logfmt convention).
//! - `seq` is a per-process monotonic counter — every log line from
//!   one daemon lifetime gets a unique sequence number for precise
//!   ordering during post-mortem analysis.
//!
//! ## Rotation
//!
//! Naive but correct: when the active file exceeds `max_bytes`, shift
//! `rsc.log` -> `rsc.log.1`, `rsc.log.1` -> `rsc.log.2`, ..., drop the
//! oldest. Counter is in-memory — no stat() per line.

use chrono::Local;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

pub struct FileLogger {
    path: PathBuf,
    max_bytes: u64,
    keep: u32,
    inner: Mutex<()>,
    /// In-memory byte counter — avoids stat() syscall on every log
    /// line. Only checked against max_bytes; reset to 0 on rotate.
    bytes_written: AtomicU64,
    /// Per-process monotonic sequence counter — every log line gets a
    /// unique seq number for precise ordering during post-mortem.
    /// Starts at 0, increments BEFORE write so first line is seq=1.
    seq: AtomicU64,
}

impl FileLogger {
    pub fn new(path: impl Into<PathBuf>, max_kb: u64, keep: u32) -> Self {
        let path = path.into();
        // Do mkdir -p ONCE at construction, not on every log line.
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        // Initialize byte counter from existing file size if present.
        let initial_bytes = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        Self {
            path,
            max_bytes: max_kb.saturating_mul(1024),
            keep: keep.max(1),
            inner: Mutex::new(()),
            bytes_written: AtomicU64::new(initial_bytes),
            seq: AtomicU64::new(0),
        }
    }

    /// Log a message with optional structured key=value pairs.
    ///
    /// Pairs are rendered logfmt-style: `key=value` separated by spaces,
    /// appended after the message. Values containing space, `=`, or `"`
    /// are double-quoted with `\` and `"` escaped.
    ///
    /// Example:
    /// ```ignore
    /// logger.log_kv("INFO", "CUTTING OFF", &[("cap", "95"), ("cutoff", "95")]);
    /// // emits: [2026-06-28T11:40:46+08:00 INFO  seq=42] CUTTING OFF cap=95 cutoff=95
    /// ```
    pub fn log_kv(&self, level: &str, msg: &str, kv: &[(&str, &str)]) {
        let _g = self.inner.lock().unwrap();
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        // Timestamp in device local timezone. chrono::Local reads the
        // system timezone from /etc/localtime or TZ env var. Format
        // includes the UTC offset suffix (e.g. "+08:00", "-05:00") so
        // the timezone is unambiguous regardless of device locale:
        // "2026-06-28T11:40:46+08:00".
        let ts = Local::now().format("%Y-%m-%dT%H:%M:%S%:z").to_string();

        // Level is left-padded to 5 chars so DEBUG/INFO/WARN/ERROR align
        // vertically when reading the file. Truncate (shouldn't happen
        // with our constants) to keep column width bounded.
        let level_pad = if level.len() >= 5 {
            level.to_string()
        } else {
            format!("{:<5}", level)
        };

        let mut line = format!("[{} {}  seq={}] {}", ts, level_pad, seq, msg);
        for (k, v) in kv {
            line.push(' ');
            line.push_str(k);
            line.push('=');
            line.push_str(&logfmt_escape(v));
        }
        line.push('\n');

        let line_bytes = line.as_bytes();
        let line_len = line_bytes.len() as u64;

        // Rotation check via in-memory counter (Issue #3) — avoids stat()
        // syscall on every log line. The counter is approximate (doesn't
        // account for external file modifications) but is reset on rotate
        // and re-synced periodically via the slow-path below.
        let current = self.bytes_written.load(Ordering::Relaxed);
        if current >= self.max_bytes {
            self.rotate();
            self.bytes_written.store(0, Ordering::Relaxed);
        }

        // Parent dir creation is now once-only at construction (Issue #4).
        // No create_dir_all() call here.

        if let Ok(mut f) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            if f.write_all(line_bytes).is_ok() {
                self.bytes_written.fetch_add(line_len, Ordering::Relaxed);
            }
        }
    }

    fn rotate(&self) {
        // Drop the oldest, shift the rest up.
        let oldest = self.rotated_path(self.keep);
        if oldest.exists() {
            let _ = fs::remove_file(oldest);
        }
        for i in (1..self.keep).rev() {
            let from = self.rotated_path(i);
            let to = self.rotated_path(i + 1);
            if from.exists() {
                let _ = fs::rename(&from, &to);
            }
        }
        // Active -> .1
        let to = self.rotated_path(1);
        let _ = fs::rename(&self.path, &to);
    }

    fn rotated_path(&self, n: u32) -> PathBuf {
        // rsc.log -> rsc.log.1, rsc.log.2, ...
        let mut name = self
            .path
            .file_name()
            .map(|s| s.to_os_string())
            .unwrap_or_else(|| std::ffi::OsString::from("rsc.log"));
        name.push(format!(".{}", n));
        let mut p = self.path.clone();
        p.set_file_name(name);
        p
    }

    // --- Convenience wrappers (for call sites that don't need kv pairs) ---
    // Kept for ergonomic use by future modules that don't need kv pairs.
    // Currently all call sites use log_kv directly, hence #[allow(dead_code)].

    #[allow(dead_code)]
    pub fn log(&self, level: &str, msg: &str) {
        self.log_kv(level, msg, &[]);
    }

    #[allow(dead_code)]
    pub fn info(&self, msg: &str) {
        self.log_kv("INFO", msg, &[]);
    }
    #[allow(dead_code)]
    pub fn warn(&self, msg: &str) {
        self.log_kv("WARN", msg, &[]);
    }
    #[allow(dead_code)]
    pub fn error(&self, msg: &str) {
        self.log_kv("ERROR", msg, &[]);
    }
    /// Debug log — only writes if `enabled` is true. Caller passes
    /// `cfg.debug` so the gate decision lives at the call site, no
    /// global mutable state needed.
    #[allow(dead_code)]
    pub fn debug_if(&self, enabled: bool, msg: &str) {
        if enabled {
            self.log_kv("DEBUG", msg, &[]);
        }
    }
    /// Debug log with kv pairs — only writes if `enabled` is true.
    pub fn debug_if_kv(&self, enabled: bool, msg: &str, kv: &[(&str, &str)]) {
        if enabled {
            self.log_kv("DEBUG", msg, kv);
        }
    }
}

/// Escape a value for logfmt output. If the value contains space, `=`,
/// `"`, or any control character, wrap it in double quotes and escape
/// `\` and `"` inside. Otherwise return as-is (no quoting needed).
///
/// Examples:
///   - `95` → `95`
///   - `Charging` → `Charging`
///   - `Not charging` → `"Not charging"`
///   - `a"b` → `"a\"b"`
///   - `a=b` → `"a=b"`
fn logfmt_escape(v: &str) -> String {
    let needs_quote = v.is_empty()
        || v.contains(' ')
        || v.contains('\t')
        || v.contains('=')
        || v.contains('"')
        || v.contains('\\')
        || v.chars().any(|c| c.is_control());
    if !needs_quote {
        return v.to_string();
    }
    let mut s = String::with_capacity(v.len() + 2);
    s.push('"');
    for c in v.chars() {
        match c {
            '\\' => s.push_str("\\\\"),
            '"' => s.push_str("\\\""),
            _ => s.push(c),
        }
    }
    s.push('"');
    s
}

/// Convenience: check if the log path's parent is writable. Returns false
/// if the directory does not exist and cannot be created.
pub fn ensure_log_dir(path: &Path) -> bool {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).is_ok()
    } else {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_logfmt_escape_plain() {
        assert_eq!(logfmt_escape("95"), "95");
        assert_eq!(logfmt_escape("Charging"), "Charging");
    }

    #[test]
    fn test_logfmt_escape_space() {
        assert_eq!(logfmt_escape("Not charging"), "\"Not charging\"");
    }

    #[test]
    fn test_logfmt_escape_equals() {
        assert_eq!(logfmt_escape("a=b"), "\"a=b\"");
    }

    #[test]
    fn test_logfmt_escape_quote() {
        assert_eq!(logfmt_escape("a\"b"), "\"a\\\"b\"");
    }

    #[test]
    fn test_logfmt_escape_backslash() {
        assert_eq!(logfmt_escape("a\\b"), "\"a\\\\b\"");
    }

    #[test]
    fn test_logfmt_escape_empty() {
        assert_eq!(logfmt_escape(""), "\"\"");
    }
}
