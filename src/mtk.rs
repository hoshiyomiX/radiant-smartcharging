//! MediaTek battery command interface.
//!
//! Implements the MTK-specific sysfs/procfs paths the user supplied:
//!
//! Cut-off charging:
//!   echo "1"   > /proc/mtk_battery_cmd/en_power_path
//!   echo "1 1" > /proc/mtk_battery_cmd/current_cmd
//!
//! Resume charging (reset+re-apply sequence):
//!   echo "0"   > /proc/mtk_battery_cmd/en_power_path
//!   (sleep 50ms)
//!   echo "1"   > /proc/mtk_battery_cmd/en_power_path
//!   (sleep 50ms)
//!   echo "0 0" > /proc/mtk_battery_cmd/current_cmd
//!
//! Enable battery thermal delimiter:
//!   echo 1 > /sys/devices/platform/battery/disable_nafg
//!   echo 1 > /sys/devices/platform/battery/ntc_disable_nafg
//!
//! Restore thermal (disable delimiter):
//!   echo 0 > /sys/devices/platform/battery/disable_nafg
//!   echo 0 > /sys/devices/platform/battery/ntc_disable_nafg
//!
//! ## Why the resume sequence is reset+re-apply
//!
//! The naive resume sequence (`en_power_path=1` then
//! `current_cmd=0 0`) succeeds at the sysfs write level but can FAIL
//! to actually re-enable current flow on Infinix X695C (Helio G95,
//! Android 11). Battery capacity keeps dropping despite kernel
//! reporting `POWER_SUPPLY_STATUS=Charging`.
//!
//! The fix: reset `en_power_path` to 0 first (force power-path
//! driver FSM reset), then re-apply 1, then clear current_cmd.

use std::fs::OpenOptions;
use std::io::Write;
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

/// Cut off charging via MTK battery_cmd. The `en_power_path` write is kept
/// for parity with the user's snippet, even though it is a no-op against
/// the same value — some MTK BSP revisions key off the write event itself.
pub fn cut_off_charging() -> Result<(), MtkError> {
    write_sysctl(EN_POWER_PATH, "1")?;
    write_sysctl(CURRENT_CMD_PATH, "1 1")?;
    Ok(())
}

/// Resume charging via reset+re-apply sequence.
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
