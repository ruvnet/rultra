//! ESP32 fleet management for the rultra box.
//!
//! This crate exists because three separate failures during bring-up each
//! looked like a dead board, and none of them were:
//!
//! 1. A **charge-only USB cable**. The board powered up and looked alive while
//!    being completely invisible to the host. The tell was not an error — it
//!    was the *absence* of one. A board that is present but misbehaving logs
//!    `device descriptor read error`; a board whose D+/D- are not connected
//!    logs nothing at all. [`Attachment`] makes that distinction explicit so
//!    the diagnosis is not re-derived by reading dmesg by eye.
//!
//! 2. A **silent UART**. `cat /dev/ttyUSB0` returns zero bytes at every baud
//!    rate until the chip is actually reset, and opening/closing the port is
//!    not a reset. See [`reset`].
//!
//! 3. A **floating RX line**, which is the trap that looks most like success.
//!    A disconnected receive pin produces a steady trickle of bytes, and it is
//!    tempting to read that as "the board is talking, wrong baud". It is not.
//!    See [`LineState`].

pub mod board;
pub mod flash;
pub mod reset;

pub use board::{Board, Bridge};
pub use flash::{Backup, FlashPlan};
pub use reset::ResetMode;

use serde::{Deserialize, Serialize};

/// Whether a board is electrically present on the USB bus.
///
/// The middle variant is the one that matters. It is the difference between
/// "this board is broken" and "this cable has no data wires", and those call
/// for opposite responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Attachment {
    /// Enumerated, driver bound, a tty exists.
    Enumerated,
    /// Nothing on the bus and *no enumeration errors either*. The host never
    /// saw an attach attempt, so the data lines are not connected: a
    /// charge-only cable, a power-only port, or a module with no USB bridge.
    /// Do not debug the board for this — debug the cable.
    SilentNoErrors,
    /// The host saw the device and failed to talk to it (descriptor read
    /// errors, enumeration retries, over-current). This one *is* the board,
    /// the port power budget, or a marginal cable.
    EnumerationFailed,
}

impl Attachment {
    /// Classify from the two counts a caller can cheaply get out of dmesg.
    pub fn classify(enumerations: usize, usb_errors: usize) -> Self {
        match (enumerations, usb_errors) {
            (0, 0) => Attachment::SilentNoErrors,
            (0, _) => Attachment::EnumerationFailed,
            _ => Attachment::Enumerated,
        }
    }

    /// The single most useful next action, in words a human can act on.
    pub fn advice(self) -> &'static str {
        match self {
            Attachment::Enumerated => "Board is on the bus; drive it over its tty.",
            Attachment::SilentNoErrors => {
                "No attach attempt reached the host. Try a known-data USB cable \
                 before suspecting the board — a charge-only cable is the single \
                 commonest cause and is indistinguishable from a dead board."
            }
            Attachment::EnumerationFailed => {
                "The host saw the device and could not enumerate it. Suspect port \
                 power, a marginal cable, or the bridge chip."
            }
        }
    }
}

/// What a UART line is actually doing, which is not the same as how many bytes
/// arrived.
///
/// A disconnected RX pin floats, and the receiver frames the noise into bytes.
/// Those bytes look like data at a glance. Two properties separate them from a
/// real transmitter, and both are checkable without knowing the protocol:
///
/// - Real output is **baud-selective**: exactly one rate yields clean framing
///   and the others yield little or nothing. Noise yields garbage at *every*
///   rate, and more of it the faster you sample, because framing errors scale
///   with sampling rate.
/// - Real output contains **printable structure**. Noise is high-entropy.
///
/// A correctly connected but quiet line yields *zero* bytes, not a trickle.
/// That is the state that follows a good connection, and it is easy to mistake
/// for a regression after seeing noise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineState {
    /// Zero bytes. The line is driven and idle — a healthy connected UART with
    /// nothing to say. Reset the chip to make it talk.
    Idle,
    /// Bytes arriving at every baud, scaling with baud, no printable structure.
    /// Nothing is driving the receive pin.
    Floating,
    /// Framed, printable output.
    Talking,
}

/// One observation of a line at a given baud rate.
#[derive(Debug, Clone, Copy)]
pub struct BaudSample {
    pub baud: u32,
    pub bytes: usize,
    pub printable: usize,
}

/// Classify a line from samples taken across several baud rates.
///
/// Requires at least two samples: a single sample cannot distinguish noise
/// from data, which is precisely the mistake this function exists to prevent.
pub fn classify_line(samples: &[BaudSample]) -> LineState {
    if samples.len() < 2 {
        // Refuse to guess. One sample of 7 garbage bytes and one sample of 7
        // bytes of a real protocol are indistinguishable.
        return LineState::Floating;
    }
    let total: usize = samples.iter().map(|s| s.bytes).sum();
    if total == 0 {
        return LineState::Idle;
    }
    // Any single rate that yields mostly-printable output of non-trivial
    // length is a real transmitter.
    let talking = samples
        .iter()
        .any(|s| s.bytes >= 16 && s.printable * 2 >= s.bytes);
    if talking {
        return LineState::Talking;
    }
    LineState::Floating
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cable_with_no_data_lines_is_not_a_dead_board() {
        // The 2.8-day case: zero enumerations AND zero errors.
        assert_eq!(Attachment::classify(0, 0), Attachment::SilentNoErrors);
        assert!(Attachment::SilentNoErrors.advice().contains("cable"));
    }

    #[test]
    fn errors_without_enumeration_blame_the_board_not_the_cable() {
        assert_eq!(Attachment::classify(0, 3), Attachment::EnumerationFailed);
        assert!(!Attachment::EnumerationFailed
            .advice()
            .contains("charge-only"));
    }

    #[test]
    fn a_quiet_connected_line_reads_as_idle_not_broken() {
        let s = [
            BaudSample {
                baud: 9600,
                bytes: 0,
                printable: 0,
            },
            BaudSample {
                baud: 115_200,
                bytes: 0,
                printable: 0,
            },
        ];
        assert_eq!(classify_line(&s), LineState::Idle);
    }

    #[test]
    fn a_floating_pin_is_not_mistaken_for_a_transmitter() {
        // Real numbers from the PL2303 with nothing wired to its RX.
        let s = [
            BaudSample {
                baud: 9600,
                bytes: 1,
                printable: 0,
            },
            BaudSample {
                baud: 115_200,
                bytes: 7,
                printable: 0,
            },
            BaudSample {
                baud: 921_600,
                bytes: 42,
                printable: 2,
            },
        ];
        assert_eq!(classify_line(&s), LineState::Floating);
    }

    #[test]
    fn a_real_boot_log_is_recognised() {
        // Real numbers from the ESP32 after a proper reset.
        let s = [
            BaudSample {
                baud: 9600,
                bytes: 0,
                printable: 0,
            },
            BaudSample {
                baud: 115_200,
                bytes: 695,
                printable: 660,
            },
        ];
        assert_eq!(classify_line(&s), LineState::Talking);
    }

    #[test]
    fn one_sample_is_never_enough_to_claim_a_line_is_talking() {
        let s = [BaudSample {
            baud: 115_200,
            bytes: 4096,
            printable: 4000,
        }];
        assert_ne!(classify_line(&s), LineState::Talking);
    }
}
