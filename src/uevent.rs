//! Netlink KOBJECT_UEVENT listener.
//!
//! Listens on AF_NETLINK / NETLINK_KOBJECT_UEVENT for power_supply events
//! (battery capacity change, charger plug/unplug). Replaces the polling
//! loop with event-driven wake-ups — daemon stays asleep until the kernel
//! emits a uevent for /sys/class/power_supply/battery/*.
//!
//! Socket setup:
//!   - AF_NETLINK, SOCK_RAW, NETLINK_KOBJECT_UEVENT (15)
//!   - bind to sockaddr_nl { nl_pid=0, nl_groups=1 } (kobject multicast)
//!   - SO_RCVTIMEO for bounded wait (fallback poll safety net)
//!
//! Message format (NULL-separated strings, first line is "ACTION@DEVPATH"):
//!   change@/devices/.../power_supply/battery\0
//!   ACTION=change\0
//!   DEVPATH=/devices/.../power_supply/battery\0
//!   SUBSYSTEM=power_supply\0
//!   POWER_SUPPLY_NAME=battery\0
//!   POWER_SUPPLY_STATUS=Charging\0
//!   POWER_SUPPLY_CAPACITY=85\0
//!   SEQNUM=12345\0
//!
//! We only care about ACTION=="change" AND DEVPATH contains
//! "/power_supply/battery". Other events (input, sound, usb) are filtered.

use std::io;
use std::os::unix::io::RawFd;

/// Netlink protocol number for kobject uevents (from <linux/netlink.h>).
const NETLINK_KOBJECT_UEVENT: i32 = 15;

/// Multicast group mask for kobject events (from <linux/kobject.h>).
/// Group 1 = current kernel format (text-based KEY=VALUE pairs).
const KOBJECT_UEVENT_MULTICAST_GROUP: u32 = 1;

/// Buffer for a single uevent message. Uevents are typically <1KB but
/// can be larger if the driver emits many POWER_SUPPLY_* attributes.
/// 8KB matches what udev uses.
const RECV_BUFFER_SIZE: usize = 8192;

/// Filter keywords — only events matching ALL of these are "relevant".
/// power_supply subsystem events fire when:
///   - Battery capacity changes (typically every 1% drop)
///   - Charger plugs in / unplugs (status change)
///   - Charging rate changes (current_now updates)
const RELEVANT_DEVPATH_FRAGMENT: &str = "/power_supply/battery";
const RELEVANT_SUBSYSTEM: &str = "power_supply";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Uevent {
    pub action: String,
    pub devpath: String,
    pub subsystem: String,
    /// Optional payload fields we extract for diagnostics. Currently
    /// we capture POWER_SUPPLY_CAPACITY and POWER_SUPPLY_STATUS if
    /// present in the uevent — useful for debug logging.
    pub capacity: Option<String>,
    pub status: Option<String>,
    pub seqnum: Option<String>,
}

impl Uevent {
    /// True if this event is relevant to rsc — must be a power_supply
    /// event on the battery device. We accept any ACTION (add/remove/change)
    /// so we don't miss plug-in/out of alternate power supplies.
    pub fn is_relevant(&self) -> bool {
        self.subsystem == RELEVANT_SUBSYSTEM && self.devpath.contains(RELEVANT_DEVPATH_FRAGMENT)
    }

    /// Compact one-line summary for debug logs.
    pub fn summary(&self) -> String {
        let cap = self.capacity.as_deref().unwrap_or("-");
        let st = self.status.as_deref().unwrap_or("-");
        let seq = self.seqnum.as_deref().unwrap_or("-");
        format!(
            "action={} subsystem={} devpath={} cap={} status={} seq={}",
            self.action, self.subsystem, self.devpath, cap, st, seq
        )
    }
}

pub struct UeventListener {
    fd: RawFd,
}

impl UeventListener {
    /// Open and bind the netlink socket. Returns Err if socket creation
    /// fails — caller MUST exit (no polling fallback).
    pub fn new() -> Result<Self, io::Error> {
        // SAFETY: socket(2) is safe to call; returns -1 on error.
        let fd = unsafe {
            libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_RAW | libc::SOCK_CLOEXEC,
                NETLINK_KOBJECT_UEVENT,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }

        // Bind to kobject multicast group. nl_pid=0 lets kernel assign.
        // nl_pad is private in libc's Android bindings (Padding<c_ushort>),
        // so we init zeroed then set public fields.
        let mut addr: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        addr.nl_family = libc::AF_NETLINK as u16;
        addr.nl_pid = 0;
        addr.nl_groups = KOBJECT_UEVENT_MULTICAST_GROUP;

        // SAFETY: bind(2) with valid fd and properly-initialized sockaddr.
        let rc = unsafe {
            libc::bind(
                fd,
                &addr as *const _ as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
            )
        };
        if rc < 0 {
            let err = io::Error::last_os_error();
            // SAFETY: close(2) on a valid fd; ignore errors during cleanup.
            unsafe { libc::close(fd) };
            return Err(err);
        }

        Ok(Self { fd })
    }

    /// Return the underlying socket fd (for debug logging only).
    pub fn fd(&self) -> RawFd {
        self.fd
    }

    /// Block until a uevent arrives. No timeout — blocks forever until
    /// an event is received or a signal interrupts (EINTR).
    ///
    /// Returns:
    ///   - `Ok(Some(event))` — event received
    ///   - `Err(EINTR)` — signal interrupted recv, caller checks RUNNING
    ///   - `Err(other)` — real socket error
    pub fn recv_blocking(&self) -> Result<Uevent, io::Error> {
        let mut buf = [0u8; RECV_BUFFER_SIZE];
        loop {
            // SAFETY: recv(2) with valid fd and buffer of known size.
            let n =
                unsafe { libc::recv(self.fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0) };
            if n < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    return Err(err);
                }
                // EAGAIN/EWOULDBLOCK shouldn't happen on blocking socket,
                // but handle defensively.
                if err.kind() == io::ErrorKind::WouldBlock {
                    continue;
                }
                return Err(err);
            }
            if n == 0 {
                continue;
            }
            return Ok(parse_uevent(&buf[..n as usize]).unwrap_or(Uevent {
                action: String::new(),
                devpath: String::new(),
                subsystem: String::new(),
                capacity: None,
                status: None,
                seqnum: None,
            }));
        }
    }

    /// Non-blocking drain of any queued events. Returns the number of
    /// events drained (regardless of relevance). Used to coalesce
    /// multiple rapid-fire uevents (e.g. on charger plug-in, kernel
    /// often emits 3-5 events in <100ms).
    pub fn try_drain(&self) -> usize {
        let mut count = 0;
        let mut buf = [0u8; RECV_BUFFER_SIZE];
        loop {
            // SAFETY: recv(2) with MSG_DONTWAIT for non-blocking.
            let n = unsafe {
                libc::recv(
                    self.fd,
                    buf.as_mut_ptr() as *mut libc::c_void,
                    buf.len(),
                    libc::MSG_DONTWAIT,
                )
            };
            if n <= 0 {
                break; // EAGAIN or error — drain complete
            }
            count += 1;
        }
        count
    }
}

impl Drop for UeventListener {
    fn drop(&mut self) {
        // SAFETY: close(2) on a valid fd; ignore errors during cleanup.
        unsafe { libc::close(self.fd) };
    }
}

/// Parse a uevent message buffer into a Uevent struct.
///
/// Format: NULL-separated strings. First string is "ACTION@DEVPATH",
/// subsequent strings are "KEY=VALUE" pairs. We extract the keys we
/// care about (ACTION, DEVPATH, SUBSYSTEM, POWER_SUPPLY_CAPACITY,
/// POWER_SUPPLY_STATUS, SEQNUM) and ignore the rest.
fn parse_uevent(buf: &[u8]) -> Option<Uevent> {
    // Split on NUL bytes; collect into Vec<&str> (filter out empty trailing).
    let parts: Vec<&str> = buf
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .filter_map(|s| std::str::from_utf8(s).ok())
        .collect();

    if parts.is_empty() {
        return None;
    }

    // First part is "ACTION@DEVPATH" — split on '@'.
    let mut action = String::new();
    let mut devpath = String::new();
    if let Some(at_pos) = parts[0].find('@') {
        action = parts[0][..at_pos].to_string();
        devpath = parts[0][at_pos + 1..].to_string();
    }

    // Look for KEY=VALUE pairs in the rest.
    let mut subsystem = String::new();
    let mut capacity: Option<String> = None;
    let mut status: Option<String> = None;
    let mut seqnum: Option<String> = None;

    for part in &parts[1..] {
        // Prefer explicit ACTION= / DEVPATH= over the first-line parse
        // (more reliable across kernel versions).
        if action.is_empty() {
            if let Some(rest) = part.strip_prefix("ACTION=") {
                action = rest.to_string();
                continue;
            }
        }
        if devpath.is_empty() {
            if let Some(rest) = part.strip_prefix("DEVPATH=") {
                devpath = rest.to_string();
                continue;
            }
        }
        if subsystem.is_empty() {
            if let Some(rest) = part.strip_prefix("SUBSYSTEM=") {
                subsystem = rest.to_string();
                continue;
            }
        }
        if capacity.is_none() {
            if let Some(rest) = part.strip_prefix("POWER_SUPPLY_CAPACITY=") {
                capacity = Some(rest.to_string());
                continue;
            }
        }
        if status.is_none() {
            if let Some(rest) = part.strip_prefix("POWER_SUPPLY_STATUS=") {
                status = Some(rest.to_string());
                continue;
            }
        }
        if seqnum.is_none() {
            if let Some(rest) = part.strip_prefix("SEQNUM=") {
                seqnum = Some(rest.to_string());
                continue;
            }
        }
    }

    Some(Uevent {
        action,
        devpath,
        subsystem,
        capacity,
        status,
        seqnum,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_buf(parts: &[&str]) -> Vec<u8> {
        let mut buf = Vec::new();
        for p in parts {
            buf.extend_from_slice(p.as_bytes());
            buf.push(0);
        }
        buf
    }

    #[test]
    fn test_parse_battery_change_event() {
        let buf = build_buf(&[
            "change@/devices/platform/battery/power_supply/battery",
            "ACTION=change",
            "DEVPATH=/devices/platform/battery/power_supply/battery",
            "SUBSYSTEM=power_supply",
            "POWER_SUPPLY_CAPACITY=85",
            "POWER_SUPPLY_STATUS=Charging",
            "SEQNUM=12345",
        ]);
        let ev = parse_uevent(&buf).expect("parse failed");
        assert_eq!(ev.action, "change");
        assert_eq!(ev.subsystem, "power_supply");
        assert!(ev.devpath.contains("/power_supply/battery"));
        assert_eq!(ev.capacity.as_deref(), Some("85"));
        assert_eq!(ev.status.as_deref(), Some("Charging"));
        assert_eq!(ev.seqnum.as_deref(), Some("12345"));
        assert!(ev.is_relevant());
        // Verify summary doesn't panic and includes key fields.
        let s = ev.summary();
        assert!(s.contains("action=change"));
        assert!(s.contains("cap=85"));
    }

    #[test]
    fn test_parse_irrelevant_input_event() {
        let buf = build_buf(&[
            "add@/devices/platform/input0",
            "ACTION=add",
            "DEVPATH=/devices/platform/input0",
            "SUBSYSTEM=input",
        ]);
        let ev = parse_uevent(&buf).expect("parse failed");
        assert_eq!(ev.subsystem, "input");
        assert!(ev.capacity.is_none());
        assert!(ev.status.is_none());
        assert!(!ev.is_relevant());
    }

    #[test]
    fn test_parse_charger_plug_event() {
        // When charger plugs in, kernel emits uevent for the charger
        // device AND the battery device. We care about the battery one.
        let buf = build_buf(&[
            "change@/devices/platform/mt_charger/power_supply/mt_charger",
            "ACTION=change",
            "DEVPATH=/devices/platform/mt_charger/power_supply/mt_charger",
            "SUBSYSTEM=power_supply",
        ]);
        let ev = parse_uevent(&buf).expect("parse failed");
        // Charger event — not /battery, so NOT relevant for us.
        assert!(!ev.is_relevant());
    }

    #[test]
    fn test_parse_empty_buffer() {
        assert!(parse_uevent(&[]).is_none());
    }

    #[test]
    fn test_parse_garbage_buffer() {
        // No NUL-separated strings — should not panic.
        let buf = b"just some random bytes without nulls";
        let ev = parse_uevent(buf).expect("should not return None");
        assert!(!ev.is_relevant());
        assert!(ev.capacity.is_none());
    }

    #[test]
    fn test_parse_event_without_payload_fields() {
        // Some kernels emit uevents without POWER_SUPPLY_CAPACITY etc.
        // Parser must handle missing fields gracefully (Option::None).
        let buf = build_buf(&[
            "change@/devices/platform/battery/power_supply/battery",
            "ACTION=change",
            "DEVPATH=/devices/platform/battery/power_supply/battery",
            "SUBSYSTEM=power_supply",
            // No POWER_SUPPLY_* fields — just a generic change notification.
        ]);
        let ev = parse_uevent(&buf).expect("parse failed");
        assert!(ev.is_relevant());
        assert!(ev.capacity.is_none());
        assert!(ev.status.is_none());
        assert!(ev.seqnum.is_none());
    }
}
