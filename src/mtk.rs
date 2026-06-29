//! MediaTek battery command interface.
//!
//! Implements the MTK-specific sysfs/procfs paths the user supplied:
//!
//! Cut-off charging:
//!   echo "1"   > /proc/mtk_battery_cmd/en_power_path
//!   echo "1 1" > /proc/mtk_battery_cmd/current_cmd
//!
//! Resume charging (robust sequence):
//!   Strategy A (primary): reset+re-apply
//!     echo "0"   > /proc/mtk_battery_cmd/en_power_path
//!     (sleep 50ms)
//!     echo "1"   > /proc/mtk_battery_cmd/en_power_path
//!     (sleep 50ms)
//!     echo "0 0" > /proc/mtk_battery_cmd/current_cmd
//!   Strategy B (fallback): toggle pattern
//!     echo "1"   > /proc/mtk_battery_cmd/en_power_path
//!     echo "1 1" > /proc/mtk_battery_cmd/current_cmd   (re-assert cut)
//!     (sleep 100ms)
//!     echo "0 0" > /proc/mtk_battery_cmd/current_cmd   (release)
//!
//! Enable battery thermal delimiter:
//!   echo 1 > /sys/devices/platform/battery/disable_nafg
//!   echo 1 > /sys/devices/platform/battery/ntc_disable_nafg
//!
//! Restore thermal (disable delimiter):
//!   echo 0 > /sys/devices/platform/battery/disable_nafg
//!   echo 0 > /sys/devices/platform/battery/ntc_disable_nafg
//!
//! ## Why the resume sequence changed
//!
//! The naive resume sequence (`en_power_path=1` then
//! `current_cmd=0 0`) succeeds at the sysfs write level but can FAIL
//! to actually re-enable current flow on Infinix X695C (Helio G95,
//! Android 11). Battery capacity keeps dropping despite kernel
//! reporting `POWER_SUPPLY_STATUS=Charging`.
//!
//! The fix: reset `en_power_path` to 0 first (force power-path
//! driver FSM reset), then re-apply 1, then clear current_cmd.
//! If that still doesn't work (device-specific BSP quirk), the
//! fallback toggles `current_cmd` from `1 1` to `0 0` to force
//! the cut-flag state machine to re-evaluate.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::Path;
use std::thread;
use std::time::Duration;

const EN_POWER_PATH: &str = "/proc/mtk_battery_cmd/en_power_path";
const CURRENT_CMD_PATH: &str = "/proc/mtk_battery_cmd/current_cmd";
const DISABLE_NAFG_PATH: &str = "/sys/devices/platform/battery/disable_nafg";
const NTC_DISABLE_NAFG_PATH: &str = "/sys/devices/platform/battery/ntc_disable_nafg";

/// Sleep between reset and re-apply of en_power_path. 50ms is enough
/// for the MTK battery driver FSM to register the transition without
/// being so long that it delays daemon ticks noticeably.
const RESUME_RESET_DELAY_MS: u64 = 50;

/// Sleep between re-asserting cut (1 1) and releasing (0 0) in the
/// toggle fallback. 100ms gives the driver time to fully enter the
/// cut state before being asked to exit it.
const RESUME_TOGGLE_DELAY_MS: u64 = 100;

#[derive(Debug)]
pub enum MtkError {
    Io(std::io::Error),
    PathMissing(String),
}

impl From<std::io::Error> for MtkError {
    fn from(e: std::io::Error) -> Self {
        MtkError::Io(e)
    }
}

impl std::fmt::Display for MtkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MtkError::Io(e) => write!(f, "io: {}", e),
            MtkError::PathMissing(p) => write!(f, "path missing: {}", p),
        }
    }
}

impl std::error::Error for MtkError {}

fn write_sysctl(path: &str, value: &str) -> Result<(), MtkError> {
    if !Path::new(path).exists() {
        return Err(MtkError::PathMissing(path.to_string()));
    }
    let mut f = OpenOptions::new().write(true).open(path)?;
    f.write_all(value.as_bytes())?;
    f.flush()?;
    Ok(())
}

/// Read back a sysfs/procfs value. Useful for post-write verification.
/// Returns the trimmed string content of the file.
fn read_sysctl(path: &str) -> Result<String, MtkError> {
    if !Path::new(path).exists() {
        return Err(MtkError::PathMissing(path.to_string()));
    }
    let mut f = OpenOptions::new().read(true).open(path)?;
    let mut buf = String::new();
    f.read_to_string(&mut buf)?;
    Ok(buf.trim().to_string())
}

/// Cut off charging via MTK battery_cmd. The `en_power_path` write is kept
/// for parity with the user's snippet, even though it is a no-op against
/// the same value — some MTK BSP revisions key off the write event itself.
pub fn cut_off_charging() -> Result<(), MtkError> {
    write_sysctl(EN_POWER_PATH, "1")?;
    write_sysctl(CURRENT_CMD_PATH, "1 1")?;
    Ok(())
}

/// Resume charging — primary strategy.
///
/// Resets `en_power_path` to 0 first (forcing the MTK power-path driver
/// FSM to release any latched cutoff state), then re-applies 1, then
/// clears `current_cmd` to `0 0`.
///
/// This addresses the silent resume failure mode where the naive
/// sequence (`en_power_path=1` + `current_cmd=0 0`) succeeded at the
/// sysfs write level but failed to actually re-enable current flow on
/// Infinix X695C (Helio G95, Android 11) — battery capacity kept
/// dropping despite kernel reporting `POWER_SUPPLY_STATUS=Charging`.
///
/// Returns Ok(()) if all writes succeed. Caller should call
/// `verify_resume_applied()` afterwards for sysfs-level verification,
/// AND monitor battery capacity trajectory for 5-10 minutes to detect
/// the silent failure mode (FET stuck off despite sysfs reporting
/// charging).
pub fn resume_charging() -> Result<(), MtkError> {
    // 1. Reset en_power_path to 0 — force driver FSM reset.
    write_sysctl(EN_POWER_PATH, "0")?;
    thread::sleep(Duration::from_millis(RESUME_RESET_DELAY_MS));

    // 2. Re-apply en_power_path=1 — re-enable power path.
    write_sysctl(EN_POWER_PATH, "1")?;
    thread::sleep(Duration::from_millis(RESUME_RESET_DELAY_MS));

    // 3. Clear current_cmd cut flag.
    write_sysctl(CURRENT_CMD_PATH, "0 0")?;
    Ok(())
}

/// Resume charging — fallback toggle strategy.
///
/// Re-asserts the cut command (`current_cmd=1 1`) first, waits briefly,
/// then releases it (`current_cmd=0 0`). This forces the cut-flag state
/// machine to re-evaluate from a known state, which is necessary on
/// some MTK BSP revisions where the driver doesn't properly process
/// a `0 0` write after a `1 1` write without an intermediate reset.
///
/// Use this as a fallback when the primary `resume_charging()` strategy
/// fails verification (sysfs readback shows wrong value) or when the
/// post-resume health check detects continued capacity drop.
pub fn resume_charging_with_toggle() -> Result<(), MtkError> {
    // 1. Ensure en_power_path is 1.
    write_sysctl(EN_POWER_PATH, "1")?;

    // 2. Re-assert cut — forces driver to register a fresh transition.
    write_sysctl(CURRENT_CMD_PATH, "1 1")?;
    thread::sleep(Duration::from_millis(RESUME_TOGGLE_DELAY_MS));

    // 3. Release cut — driver should now process the 0 0 cleanly.
    write_sysctl(CURRENT_CMD_PATH, "0 0")?;
    Ok(())
}

/// Read back the current_cmd value. Useful for post-resume verification.
/// Returns the trimmed string (typically "0 0" after successful resume
/// or "1 1" after cut).
pub fn read_current_cmd() -> Result<String, MtkError> {
    read_sysctl(CURRENT_CMD_PATH)
}

/// Verify that the resume command actually took effect at the sysfs
/// level. Returns true if `current_cmd` reads back as "0 0" (or any
/// "0 ..." variant — some MTK BSPs use "0 1" for resume).
///
/// NOTE: This only verifies the sysfs value was written correctly.
/// It CANNOT verify that the hardware FET actually re-enabled current
/// flow. The only reliable hardware-level verification is monitoring
/// battery capacity trajectory after resume — see Daemon's
/// `resume_health_check` logic in main.rs.
pub fn verify_resume_applied() -> Result<bool, MtkError> {
    let cmd = read_current_cmd()?;
    // Accept "0 0" (standard) and "0 1" (some MTK variants).
    // Reject "1 1" (cut), "1 0", empty, etc.
    Ok(cmd == "0 0" || cmd == "0 1" || cmd == "0")
}

/// Enable the NTC/NAFG thermal delimiter. Per the user's snippet, this is
/// only applied during charging events.
pub fn enable_thermal_delimiter() -> Result<(), MtkError> {
    write_sysctl(DISABLE_NAFG_PATH, "1")?;
    write_sysctl(NTC_DISABLE_NAFG_PATH, "1")?;
    Ok(())
}

/// Restore normal thermal behaviour by clearing both delimiter knobs.
pub fn disable_thermal_delimiter() -> Result<(), MtkError> {
    write_sysctl(DISABLE_NAFG_PATH, "0")?;
    write_sysctl(NTC_DISABLE_NAFG_PATH, "0")?;
    Ok(())
}

/// True if the MTK battery command paths exist on this device. Used as a
/// startup sanity check — non-MTK devices will exit early instead of
/// spamming logs with `PathMissing` errors.
pub fn paths_exist() -> bool {
    Path::new(EN_POWER_PATH).exists()
        && Path::new(CURRENT_CMD_PATH).exists()
        && Path::new(DISABLE_NAFG_PATH).exists()
        && Path::new(NTC_DISABLE_NAFG_PATH).exists()
}

#[cfg(test)]
mod tests {
    // No unit tests for the MTK functions because they all hit real
    // /proc and /sys paths that don't exist in the CI/test environment.
    // Integration testing requires a real MTK Android device.
    //
    // The logic that IS testable (string parsing, state transitions)
    // lives in battery.rs and uevent.rs — see those modules for tests.
}
