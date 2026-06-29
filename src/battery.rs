//! Battery sysfs reader.
//!
//! Reads from the standard Android power_supply sysfs nodes:
//!   - capacity: `/sys/class/power_supply/battery/capacity`
//!   - status:   `/sys/class/power_supply/battery/status`
//!
//! The `status` field is one of: Charging, Discharging, Full, Not charging,
//! or Unknown. We map it to a small enum for clarity.

use std::fs;
use std::io::Read;

const CAPACITY_PATH: &str = "/sys/class/power_supply/battery/capacity";
const STATUS_PATH: &str = "/sys/class/power_supply/battery/status";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChargeState {
    Charging,
    Discharging,
    Full,
    NotCharging,
    Unknown,
}

impl ChargeState {
    /// True when an external power source is actively pushing current into
    /// the battery. `Full` is excluded because once the battery is full,
    /// MTK has already cut off the path and re-applying the cut command is
    /// a no-op but a delimiter toggle would be wasteful.
    pub fn is_charging(&self) -> bool {
        matches!(self, ChargeState::Charging)
    }
}

impl std::fmt::Display for ChargeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChargeState::Charging => write!(f, "Charging"),
            ChargeState::Discharging => write!(f, "Discharging"),
            ChargeState::Full => write!(f, "Full"),
            ChargeState::NotCharging => write!(f, "NotCharging"),
            ChargeState::Unknown => write!(f, "Unknown"),
        }
    }
}

pub fn read_capacity() -> Result<u8, std::io::Error> {
    // Stack-allocated buffer — capacity is always 1-3 digits + newline.
    // Avoids heap String allocation per tick (Issue #7).
    let mut buf = [0u8; 16];
    let mut f = fs::File::open(CAPACITY_PATH)?;
    let n = f.read(&mut buf)?;
    let s = std::str::from_utf8(&buf[..n])
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?
        .trim();
    s.parse::<u8>()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

pub fn read_charge_state() -> Result<ChargeState, std::io::Error> {
    // Stack-allocated buffer — status is at most "Discharging\n" = 11 bytes.
    // Avoids heap String allocation per tick (Issue #7).
    let mut buf = [0u8; 16];
    let mut f = fs::File::open(STATUS_PATH)?;
    let n = f.read(&mut buf)?;
    let s = std::str::from_utf8(&buf[..n])
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?
        .trim();
    let state = match s {
        "Charging" => ChargeState::Charging,
        "Discharging" => ChargeState::Discharging,
        "Full" => ChargeState::Full,
        "Not charging" => ChargeState::NotCharging,
        _ => ChargeState::Unknown,
    };
    Ok(state)
}
