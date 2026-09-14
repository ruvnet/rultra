//! Board discovery and stable identity.
//!
//! The identity problem is real and bites as soon as there is more than one
//! board: `/dev/ttyUSB0` is assigned in attach order, so the name a board had
//! yesterday is not the name it has today. Flashing the wrong board is a
//! silent, destructive mistake.
//!
//! Bridges differ in whether they can be identified at all:
//!
//! | Bridge  | Serial number in USB descriptor | Stable identity from |
//! |---------|---------------------------------|----------------------|
//! | CP210x  | yes                             | the serial number    |
//! | FTDI    | yes                             | the serial number    |
//! | native  | yes (S3/C3/C6 USB-serial-JTAG)  | the serial number    |
//! | CH340   | **no**                          | the physical port    |
//! | PL2303  | yes, but not unique per board   | the physical port    |
//!
//! A CH340 therefore cannot be told apart from another CH340 except by which
//! socket it is plugged into. That is a property of the hardware, not a gap in
//! this code, and [`Bridge::has_stable_serial`] states it so callers do not
//! build features that assume otherwise.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// The USB-to-UART bridge in front of the microcontroller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Bridge {
    Ch340,
    Cp210x,
    Ftdi,
    /// ESP32-S3/C3/C6 native USB-serial-JTAG — no separate bridge chip.
    Native,
    /// A standalone USB-TTL adapter, used to reach bare modules that have no
    /// onboard bridge at all (castellated-pad parts such as the RTL8721DAF).
    Pl2303,
    Unknown,
}

impl Bridge {
    pub fn from_ids(vid: &str, pid: &str) -> Self {
        match (
            vid.to_ascii_lowercase().as_str(),
            pid.to_ascii_lowercase().as_str(),
        ) {
            ("1a86", "7523") => Bridge::Ch340,
            ("10c4", "ea60") => Bridge::Cp210x,
            ("0403", _) => Bridge::Ftdi,
            ("303a", _) => Bridge::Native,
            ("067b", "23a3") => Bridge::Pl2303,
            _ => Bridge::Unknown,
        }
    }

    /// Whether the USB descriptor carries a serial number unique to the board.
    ///
    /// When this is false the caller must fall back to the physical port path,
    /// and must not assume the same board comes back on the same tty.
    pub fn has_stable_serial(self) -> bool {
        matches!(self, Bridge::Cp210x | Bridge::Ftdi | Bridge::Native)
    }

    /// Whether this bridge is part of a dev board, or a loose adapter that
    /// needs hand-wiring to a target.
    ///
    /// This matters for interpreting silence: a loose adapter with nothing
    /// wired to it *should* read as floating, and that is not a fault.
    pub fn is_onboard(self) -> bool {
        !matches!(self, Bridge::Pl2303)
    }
}

/// One attached board.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Board {
    /// Kernel device node, e.g. `/dev/ttyUSB0`. Not stable across reboots.
    pub tty: PathBuf,
    pub bridge: Bridge,
    pub vid: String,
    pub pid: String,
    /// USB descriptor serial number, when the bridge provides one.
    pub serial: Option<String>,
    /// Physical topology, e.g. `1-1.2`. Stable as long as the board stays in
    /// the same socket, which is the only identity a CH340 has.
    pub usb_path: Option<String>,
}

impl Board {
    /// A name that survives reattachment, for use in logs, claims and UI.
    ///
    /// Prefers the descriptor serial; falls back to the socket. Never falls
    /// back to the tty name, which is the thing that moves.
    pub fn stable_id(&self) -> String {
        match (&self.serial, &self.usb_path) {
            (Some(s), _) if self.bridge.has_stable_serial() && !s.is_empty() => {
                format!("{:?}-{}", self.bridge, s).to_ascii_lowercase()
            }
            (_, Some(p)) => format!("{:?}-port-{}", self.bridge, p).to_ascii_lowercase(),
            _ => format!("{:?}-unknown", self.bridge).to_ascii_lowercase(),
        }
    }
}

/// Read a sysfs attribute file, trimming the trailing newline.
fn attr(dir: &Path, name: &str) -> Option<String> {
    fs::read_to_string(dir.join(name))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Walk up from a tty's device link until a directory carrying `idVendor` is
/// found — that is the USB device node, however deep the hub chain is.
fn usb_device_dir(start: &Path, ceiling: &Path) -> Option<PathBuf> {
    let mut cur = start.to_path_buf();
    for _ in 0..12 {
        if cur.join("idVendor").is_file() {
            return Some(cur);
        }
        match cur.parent() {
            Some(p) if p.starts_with(ceiling) => cur = p.to_path_buf(),
            _ => return None,
        }
    }
    None
}

/// Enumerate attached boards by reading sysfs under `sysfs_root`.
///
/// `sysfs_root` is a parameter rather than a hardcoded `/sys` so this is
/// testable on a machine with no boards attached — which is the machine the
/// tests actually run on.
pub fn discover_in(sysfs_root: &Path, dev_root: &Path) -> Vec<Board> {
    let tty_dir = sysfs_root.join("class/tty");
    let Ok(entries) = fs::read_dir(&tty_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !(name.starts_with("ttyUSB") || name.starts_with("ttyACM")) {
            continue;
        }
        let dev_link = e.path().join("device");
        let Ok(real) = fs::canonicalize(&dev_link) else {
            continue;
        };
        let Some(usb) = usb_device_dir(&real, sysfs_root) else {
            continue;
        };
        let vid = attr(&usb, "idVendor").unwrap_or_default();
        let pid = attr(&usb, "idProduct").unwrap_or_default();
        out.push(Board {
            tty: dev_root.join(&name),
            bridge: Bridge::from_ids(&vid, &pid),
            vid,
            pid,
            serial: attr(&usb, "serial"),
            usb_path: usb.file_name().map(|s| s.to_string_lossy().to_string()),
        });
    }
    out.sort_by(|a, b| a.tty.cmp(&b.tty));
    out
}

/// Enumerate attached boards on this host.
pub fn discover() -> Vec<Board> {
    discover_in(Path::new("/sys"), Path::new("/dev"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ch340_is_identified_by_its_socket_because_it_has_no_serial() {
        let b = Board {
            tty: "/dev/ttyUSB0".into(),
            bridge: Bridge::Ch340,
            vid: "1a86".into(),
            pid: "7523".into(),
            serial: None,
            usb_path: Some("3-2".into()),
        };
        assert!(!Bridge::Ch340.has_stable_serial());
        assert_eq!(b.stable_id(), "ch340-port-3-2");
    }

    #[test]
    fn a_descriptor_serial_wins_when_the_bridge_actually_has_one() {
        let b = Board {
            tty: "/dev/ttyUSB1".into(),
            bridge: Bridge::Cp210x,
            vid: "10c4".into(),
            pid: "ea60".into(),
            serial: Some("0001".into()),
            usb_path: Some("3-3".into()),
        };
        assert_eq!(b.stable_id(), "cp210x-0001");
    }

    #[test]
    fn a_stable_id_never_falls_back_to_the_tty_name() {
        // The tty is exactly the thing that moves between reboots, so it must
        // not leak into an identity that is supposed to be stable.
        let b = Board {
            tty: "/dev/ttyUSB7".into(),
            bridge: Bridge::Ch340,
            vid: "1a86".into(),
            pid: "7523".into(),
            serial: None,
            usb_path: None,
        };
        assert!(!b.stable_id().contains("ttyUSB7"));
    }

    #[test]
    fn known_bridges_map_from_their_usb_ids() {
        assert_eq!(Bridge::from_ids("1a86", "7523"), Bridge::Ch340);
        assert_eq!(Bridge::from_ids("067b", "23a3"), Bridge::Pl2303);
        assert_eq!(Bridge::from_ids("303a", "1001"), Bridge::Native);
        assert_eq!(Bridge::from_ids("dead", "beef"), Bridge::Unknown);
    }

    #[test]
    fn a_loose_ttl_adapter_is_not_treated_as_a_dev_board() {
        // Silence on a PL2303 with nothing wired is expected, not a fault.
        assert!(!Bridge::Pl2303.is_onboard());
        assert!(Bridge::Ch340.is_onboard());
    }

    #[test]
    fn discovery_on_a_host_with_no_sysfs_returns_empty_not_panic() {
        let v = discover_in(Path::new("/nonexistent-sysfs"), Path::new("/dev"));
        assert!(v.is_empty());
    }
}
