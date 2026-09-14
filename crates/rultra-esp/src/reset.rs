//! Hardware reset over the modem control lines.
//!
//! An ESP32 dev board wires the bridge's handshake lines to the chip's boot
//! straps — RTS to `EN` (reset) and DTR to `IO0` (boot mode select). Toggling
//! them in the right order is the only way to reset the chip from the host.
//!
//! This is worth a module because the obvious approaches do not work:
//!
//! - Opening and closing the port does **not** reliably reset the board. The
//!   lines are asserted on open and released on close only when `HUPCL` is
//!   set, the transition edges land in the wrong order, and the result is a
//!   port that stays silent while looking correctly configured.
//! - `stty` cannot drive DTR/RTS directly, so a shell-only approach cannot
//!   express this sequence at all.
//!
//! The timing below is the sequence `esptool` uses, and it is load-bearing:
//! the chip samples `IO0` at the moment `EN` is released, so the two edges
//! must be ordered, not merely set.

use std::io;
use std::path::Path;

/// Which mode to leave the chip in after reset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetMode {
    /// Release with `IO0` high: boot the application normally.
    Run,
    /// Release with `IO0` low: enter the ROM serial bootloader. This is the
    /// mode that makes a board flashable, and it is also the most reliable
    /// proof-of-life there is — the ROM prints `waiting for download` even
    /// when the application firmware is absent or crashing.
    Download,
}

/// Hold time with `EN` asserted. The chip needs a real low period; a few
/// microseconds of line toggle is not a reset.
pub const RESET_HOLD_MS: u64 = 120;
/// Settle time after releasing `EN` before restoring `IO0`, so the strap is
/// still valid when the chip samples it.
pub const STRAP_SETTLE_MS: u64 = 60;

// Enforced at compile time rather than in a test: sub-millisecond toggles are
// exactly why opening and closing the port is not a reset, so a future edit
// that shortens this should fail the build, not a test run.
const _: () = assert!(RESET_HOLD_MS >= 50);

#[cfg(all(target_os = "linux", feature = "hardware"))]
mod imp {
    use super::*;
    use std::os::unix::ffi::OsStrExt;

    // Not re-exported by the libc crate on every target, and the values are
    // stable Linux ABI, so they are spelled out rather than depended upon.
    const TIOCMBIS: libc::c_ulong = 0x5416;
    const TIOCMBIC: libc::c_ulong = 0x5417;
    const TIOCM_DTR: libc::c_int = 0x002;
    const TIOCM_RTS: libc::c_int = 0x004;

    struct Fd(libc::c_int);
    impl Drop for Fd {
        fn drop(&mut self) {
            unsafe { libc::close(self.0) };
        }
    }

    fn set(fd: &Fd, bit: libc::c_int, on: bool) -> io::Result<()> {
        let req = if on { TIOCMBIS } else { TIOCMBIC };
        let rc = unsafe { libc::ioctl(fd.0, req, &bit as *const libc::c_int) };
        if rc < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub fn reset(port: &Path, mode: ResetMode) -> io::Result<()> {
        let mut c = port.as_os_str().as_bytes().to_vec();
        c.push(0);
        let fd = unsafe {
            libc::open(
                c.as_ptr() as *const libc::c_char,
                libc::O_RDWR | libc::O_NOCTTY | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = Fd(fd);

        // IO0 high, EN low: the chip is held in reset with the run-mode strap.
        set(&fd, TIOCM_DTR, false)?;
        set(&fd, TIOCM_RTS, true)?;
        std::thread::sleep(std::time::Duration::from_millis(RESET_HOLD_MS));

        match mode {
            ResetMode::Run => set(&fd, TIOCM_RTS, false)?,
            ResetMode::Download => {
                // Pull IO0 low *before* releasing EN, so the strap is already
                // valid at the sampling edge, then restore it.
                set(&fd, TIOCM_DTR, true)?;
                set(&fd, TIOCM_RTS, false)?;
                std::thread::sleep(std::time::Duration::from_millis(STRAP_SETTLE_MS));
                set(&fd, TIOCM_DTR, false)?;
            }
        }
        Ok(())
    }
}

#[cfg(not(all(target_os = "linux", feature = "hardware")))]
mod imp {
    use super::*;
    pub fn reset(_port: &Path, _mode: ResetMode) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "serial reset needs the `hardware` feature on Linux",
        ))
    }
}

/// Reset the chip attached to `port`.
pub fn reset(port: &Path, mode: ResetMode) -> io::Result<()> {
    imp::reset(port, mode)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_mode_is_distinct_from_run_mode() {
        assert_ne!(ResetMode::Run, ResetMode::Download);
    }

    #[cfg(not(feature = "hardware"))]
    #[test]
    fn without_the_hardware_feature_reset_refuses_rather_than_pretending() {
        let e = reset(Path::new("/dev/null"), ResetMode::Run).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::Unsupported);
    }
}
