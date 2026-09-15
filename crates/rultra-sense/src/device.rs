//! The device catalog: what the board carries, and how well each part is known.

use serde::{Deserialize, Serialize};

/// Stable identifier for one device on the box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceId {
    /// BH1750 digital ambient light sensor.
    Light,
    /// HT16K33 4-digit 7-segment display.
    SegmentDisplay,
    /// MAX7219 8x8 LED matrix.
    Matrix,
    /// PCF8574-backed character LCD.
    Lcd,
    /// SoC die temperature.
    CpuTemp,
    /// Tactile buttons.
    Buttons,
    /// Tilt / vibration switch.
    Tilt,
    /// HC-SR04 ultrasonic range finder.
    Range,
    /// Passive piezo buzzer.
    Buzzer,
    /// PIR motion detector.
    Motion,
    /// Digital sound/noise detector.
    Sound,
    /// Capacitive touch pad.
    Touch,
    /// Infrared remote receiver.
    InfraRed,
}

/// Whether a device is an input, an output, or both.
///
/// The unified surface must not try to `read()` an output-only device: doing so
/// produced spurious per-tick errors for the displays during bring-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    /// Can be read.
    Sensor,
    /// Can be driven.
    Actuator,
}

/// Which bus a device hangs off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "bus", rename_all = "snake_case")]
pub enum Bus {
    /// I2C with a 7-bit address.
    I2c {
        /// 7-bit address, e.g. `0x5c`.
        addr: u8,
    },
    /// SPI with a chip-select that may be a plain GPIO rather than a hardware CE.
    Spi {
        /// spidev node, e.g. `"/dev/spidev0.0"`.
        dev: &'static str,
        /// BCM line used as chip-select, when CS is software-driven.
        cs_gpio: Option<u32>,
    },
    /// A GPIO line.
    Gpio {
        /// BCM line number.
        line: u32,
    },
    /// A pair of GPIO lines: one driven, one measured. Time-of-flight parts
    /// need both, and collapsing them to a single line loses the distinction
    /// between what we assert and what we observe.
    GpioPair {
        /// Line this end drives.
        trigger: u32,
        /// Line the device drives back.
        echo: u32,
    },
    /// A scanned key matrix: columns are driven, rows are sampled.
    ///
    /// Distinct from a set of `Gpio` lines because a scanned matrix cannot be
    /// read passively at all — with every column at rest, no press changes any
    /// row. Three passive sweeps of this board found zero events before that
    /// was understood, so the bus type now says it outright.
    GpioMatrix {
        /// Sampled lines. Require a pull-up; a floating row reads as every
        /// button in it being held down.
        rows: [u32; 4],
        /// Driven lines. Rest high, pulled low one at a time to scan.
        cols: [u32; 4],
    },
    /// Exposed by the kernel through sysfs rather than a raw bus.
    Sysfs {
        /// Path to read.
        path: &'static str,
    },
}

/// How well a device is known to work **on this physical board**.
///
/// The ordering is deliberate: `Working` is the only variant that licenses a
/// claim of functionality in docs or a README.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verification {
    /// Observed producing correct output. The only variant that means "works".
    Working,
    /// Produces stable, plausible output, but no reading has been checked
    /// against a known reference.
    ///
    /// This is where most real sensors actually live, and conflating it with
    /// `Working` is the most common way an inventory becomes untrue. A device
    /// can return beautifully repeatable numbers that are beautifully wrong:
    /// repeatability is a property of the measurement path, accuracy is a
    /// property of its agreement with the world, and only the second one is
    /// what a reader assumes when told a sensor works.
    Unvalidated,
    /// The chip acknowledges on its bus, but no configuration has yet produced
    /// an observable effect. Strongly suggests the signal is routed elsewhere —
    /// on the CrowPi, through the `UX1`/`UX5` DIP banks.
    AcksButSilent,
    /// Documented on the board, never yet exercised here.
    Untested,
    /// Exercised, and shown to be wrong. Ranks below `Untested` on purpose:
    /// untested means unknown, faulty means known bad, and a reader deciding
    /// what to trust needs those kept apart.
    ///
    /// The distinction matters most for a device that is *stable* while being
    /// wrong, because stability is the property people mistake for correctness.
    Faulty,
}

/// A documented device.
#[derive(Debug, Clone)]
pub struct Device {
    /// Identity.
    pub id: DeviceId,
    /// Part number or module name.
    pub part: &'static str,
    /// Where it lives.
    pub bus: Bus,
    /// Input or output.
    pub kind: DeviceKind,
    /// Current verification state.
    pub verification: Verification,
    /// What is actually known, in one sentence. Evidence, not intent.
    pub evidence: &'static str,
}

/// Everything the board is documented to carry.
///
/// `verification` here is a claim about *this* board and is updated only when
/// an observation justifies it — never to make the table look better.
pub const CATALOG: &[Device] = &[
    Device {
        id: DeviceId::Light,
        part: "BH1750",
        bus: Bus::I2c { addr: 0x5c },
        kind: DeviceKind::Sensor,
        verification: Verification::Working,
        evidence: "Read 58.3 lux, stable across repeated samples.",
    },
    Device {
        id: DeviceId::CpuTemp,
        part: "BCM2712 die sensor",
        bus: Bus::Sysfs {
            path: "/sys/class/thermal/thermal_zone0/temp",
        },
        kind: DeviceKind::Sensor,
        verification: Verification::Working,
        evidence: "Kernel thermal zone; also the signal behind get_throttled bit 19.",
    },
    Device {
        id: DeviceId::SegmentDisplay,
        part: "HT16K33",
        bus: Bus::I2c { addr: 0x70 },
        kind: DeviceKind::Actuator,
        verification: Verification::Working,
        evidence: "Digits observed changing on the physical display.",
    },
    Device {
        id: DeviceId::Matrix,
        part: "MAX7219",
        bus: Bus::Spi {
            dev: "/dev/spidev0.1",
            cs_gpio: None,
        },
        kind: DeviceKind::Actuator,
        verification: Verification::Working,
        evidence: "Confirmed lit by an observer. SPI0 CE1, hardware chip-select — NOT a software CS on GPIO26. The \
                   board silkscreen \"CS: GPIO 26\" means PHYSICAL pin 26, which is BCM \
                   GPIO7 = CE1; the vendor manual transposes the GPIO numbers for its \
                   two SPI rows. Elecrow's own driver uses spi(port=0, device=1). \
                   Root cause of earlier silence: the bus defaults to the devicetree \
                   125 MHz, 12.5x over the MAX7219's 10 MHz limit, which fails silently \
                   and identically on every chip-select. The speed MUST be set \
                   explicitly; it is not optional.",
    },
    Device {
        id: DeviceId::Lcd,
        part: "MCP23008 + HD44780",
        bus: Bus::I2c { addr: 0x21 },
        kind: DeviceKind::Actuator,
        verification: Verification::AcksButSilent,
        evidence: "MCP23008 expander, NOT the common PCF8574 backpack — backlight is \
                   GP7, RS=GP1, E=GP2, D4-D7=GP3..GP6. PCF8574-style writes land in \
                   IODIR/GPIO registers, which is why it ACKs and appears to invert \
                   on readback while displaying nothing. Not yet confirmed lit.",
    },
    Device {
        id: DeviceId::Buttons,
        part: "4x4 tactile matrix",
        bus: Bus::GpioMatrix {
            rows: crate::buttons::ROWS,
            cols: crate::buttons::COLS,
        },
        kind: DeviceKind::Sensor,
        verification: Verification::Working,
        evidence: "Ten distinct buttons observed across 76 presses, with a clean \
                   all-high baseline. Pins are Elecrow Examples/button_matrix.py \
                   converted from BOARD to BCM: rows 27,22,5,6 in, cols 13,19,26,25 \
                   out. TWO TRAPS: (1) a scanned matrix is invisible to passive \
                   gpiomon - three sweeps found zero events before a column was \
                   driven; (2) rows need a pull-up, and gpio-cdev 0.6 cannot set \
                   bias, so without `pinctrl set 5,6,22,27 ip pu` a floating row \
                   decoded as four buttons held forever - 572 phantom presses in \
                   40s. Columns MUST be returned to high; leaving them low lights \
                   board LEDs.",
    },
    Device {
        id: DeviceId::Tilt,
        part: "tilt switch",
        bus: Bus::Gpio { line: 22 },
        kind: DeviceKind::Sensor,
        verification: Verification::Untested,
        evidence: "Line taken from Elecrow Examples/tilt.py (BCM 22), replacing a \
                   placeholder of 0. Not yet exercised here. NOTE: BCM22 is also \
                   button-matrix row 1, so the two cannot be read independently \
                   without care - a tilt read during a column scan is ambiguous.",
    },
    Device {
        id: DeviceId::Range,
        part: "HC-SR04",
        bus: Bus::GpioPair {
            trigger: 16,
            echo: 12,
        },
        kind: DeviceKind::Sensor,
        verification: Verification::Untested,
        evidence: "REMAPPED 2026-09-15 to the vendor pinout: Elecrow Examples/distance.py \
                   uses TRIG=BCM16, ECHO=BCM12. This entry previously claimed 23/24, \
                   which are the PIR (23) and the sound sensor (24). Every distance \
                   reading this project ever produced came from pulsing the PIR output \
                   and timing the sound sensor - including a stable 4.58cm that was \
                   believed for a session, and a later diagnosis of a floating ECHO that \
                   was ALSO wrong: GPIO24 chattering at ~745Hz was the sound sensor \
                   responding to room noise, a real signal on the wrong pin. Back to \
                   Untested because 16/12 has never been exercised; the previous Faulty \
                   verdict described hardware that was never the range finder.",
    },
    Device {
        id: DeviceId::Motion,
        part: "PIR motion detector",
        bus: Bus::Gpio { line: 23 },
        kind: DeviceKind::Sensor,
        verification: Verification::Working,
        evidence: "Observed idle-low for 35 consecutive samples then high for 5 on a \
                   hand wave - the retrigger-hold signature of a PIR. Line 23 from \
                   Elecrow Examples/motion.py. It read nothing for the whole project \
                   until now because the range-finder driver had claimed 23 as its \
                   TRIGGER and left it as an output driving LOW, shorting the PIR\x27s \
                   own output to ground. Never drive this line.",
    },
    Device {
        id: DeviceId::Sound,
        part: "digital sound detector",
        bus: Bus::Gpio { line: 24 },
        kind: DeviceKind::Sensor,
        verification: Verification::Working,
        evidence: "Live transitions observed while idle (0 nine times, then 1). Line 24 \
                   from Elecrow Examples/sound.py, which biases it pull-up and treats \
                   LOW as detection. This is the pin whose ~745Hz activity was \
                   previously mistaken for a floating ultrasonic ECHO; it was the sound \
                   sensor hearing the room the entire time.",
    },
    Device {
        id: DeviceId::Touch,
        part: "capacitive touch pad",
        bus: Bus::Gpio { line: 17 },
        kind: DeviceKind::Sensor,
        verification: Verification::Untested,
        evidence: "Line 17 from Elecrow Examples/touch.py (input, pull-up). Present on \
                   the board and absent from this catalog until 2026-09-15; never \
                   exercised here.",
    },
    Device {
        id: DeviceId::InfraRed,
        part: "IR remote receiver",
        bus: Bus::Gpio { line: 20 },
        kind: DeviceKind::Sensor,
        verification: Verification::Untested,
        evidence: "Line 20 from Elecrow Examples/IR.py (input, pull-up; NEC codes). \
                   Present on the board and absent from this catalog until 2026-09-15.",
    },
    Device {
        id: DeviceId::Buzzer,
        part: "passive piezo",
        bus: Bus::Gpio { line: 18 },
        kind: DeviceKind::Actuator,
        verification: Verification::Working,
        evidence: "Confirmed audible by an observer: a 120ms pulse train on BCM GPIO18 \
                   produced a high-frequency tone they heard and asked to be stopped. \
                   DRIVE IT LOW WHEN DONE — a timed hold that simply expires can leave \
                   the line floating and the buzzer sounding.",
    },
];

/// Look one device up in the catalog.
pub fn lookup(id: DeviceId) -> Option<&'static Device> {
    CATALOG.iter().find(|d| d.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_device_is_in_the_catalog_exactly_once() {
        for d in CATALOG {
            let n = CATALOG.iter().filter(|x| x.id == d.id).count();
            assert_eq!(n, 1, "{:?} appears {n} times", d.id);
        }
    }

    #[test]
    fn lookup_finds_each_catalog_entry() {
        for d in CATALOG {
            assert_eq!(lookup(d.id).map(|x| x.part), Some(d.part));
        }
    }

    /// An `Unvalidated` device must never be presented as working — that
    /// conflation is the whole reason the variant exists.
    #[test]
    fn unvalidated_is_ranked_below_working() {
        assert!(Verification::Working < Verification::Unvalidated);
        assert!(Verification::Unvalidated < Verification::AcksButSilent);
    }

    /// Known-bad must rank below never-tried. Ordering them the other way
    /// would let a device we have *disproved* outrank one we simply have not
    /// reached yet.
    #[test]
    fn faulty_ranks_below_untested_because_known_bad_beats_unknown() {
        assert!(Verification::Untested < Verification::Faulty);
        assert!(Verification::Working < Verification::Faulty);
    }

    /// A fault claim is an assertion about the world just as much as a Working
    /// claim is, so it carries the same evidence burden.
    #[test]
    fn faulty_devices_cite_the_measurement_that_condemned_them() {
        for d in CATALOG
            .iter()
            .filter(|d| d.verification == Verification::Faulty)
        {
            assert!(
                d.evidence.len() > 20,
                "{:?} claims Faulty without substantive evidence",
                d.id
            );
        }
    }

    /// The regression this guards: the range finder returned a stable 4.58cm
    /// for an entire session and was read as trustworthy because it was
    /// repeatable. Stability is not correctness.
    ///
    /// It was later found to be reading the wrong pins entirely, so the state
    /// is now `Untested` on the corrected 16/12 pair rather than `Faulty` —
    /// the fault verdict described hardware that was never the range finder.
    /// What must hold either way is that it is not counted as Working.
    #[test]
    fn the_range_finder_is_not_counted_among_working_devices() {
        let range = lookup(DeviceId::Range).expect("range is catalogued");
        assert_ne!(range.verification, Verification::Working);
        assert!(
            !CATALOG
                .iter()
                .filter(|d| d.verification == Verification::Working)
                .any(|d| d.id == DeviceId::Range),
            "a sensor measuring a floating pin must never be reported as working"
        );
    }

    /// A `Working` claim is the only one that may be read as "this works", so
    /// it must always carry evidence describing an observation.
    #[test]
    fn working_devices_cite_evidence() {
        for d in CATALOG
            .iter()
            .filter(|d| d.verification == Verification::Working)
        {
            assert!(
                d.evidence.len() > 20,
                "{:?} claims Working without substantive evidence",
                d.id
            );
        }
    }
}

#[cfg(test)]
mod kind_tests {
    use super::*;

    #[test]
    fn displays_are_actuators_not_sensors() {
        for id in [DeviceId::SegmentDisplay, DeviceId::Matrix, DeviceId::Lcd] {
            assert_eq!(lookup(id).unwrap().kind, DeviceKind::Actuator, "{id:?}");
        }
    }

    #[test]
    fn every_catalog_entry_declares_a_kind() {
        assert_eq!(
            CATALOG.len(),
            CATALOG
                .iter()
                .filter(|d| matches!(d.kind, DeviceKind::Sensor | DeviceKind::Actuator))
                .count()
        );
    }
}

#[cfg(test)]
mod placeholder_tests {
    use super::*;

    /// GPIO line 0 is used as a placeholder for "we never wrote down which line
    /// this is". A device may carry a placeholder, but it may not simultaneously
    /// claim to work — that combination is how an unverified device launders
    /// itself into a verified one, which ADR-0002 exists to prevent.
    #[test]
    fn no_working_device_relies_on_a_placeholder_gpio_line() {
        for d in CATALOG {
            if let Bus::Gpio { line: 0 } = d.bus {
                assert_ne!(
                    d.verification,
                    Verification::Working,
                    "{:?} claims Working but its GPIO line is an unrecorded placeholder",
                    d.id
                );
            }
        }
    }
}

#[cfg(test)]
mod actuator_safety_tests {
    use super::*;

    /// Devices that emit sound or motion must never be listed as sensors: a
    /// streaming loop reads every sensor on a timer, and reading a buzzer would
    /// mean sounding it on a timer.
    #[test]
    fn emitters_are_actuators_so_the_stream_never_drives_them() {
        for id in [
            DeviceId::Buzzer,
            DeviceId::Matrix,
            DeviceId::Lcd,
            DeviceId::SegmentDisplay,
        ] {
            assert_eq!(
                lookup(id).unwrap().kind,
                DeviceKind::Actuator,
                "{id:?} must not be readable, or the telemetry loop will drive it"
            );
        }
    }
}

/// Elecrow's own pin assignments, transcribed from the vendor examples.
///
/// This table exists because a wrong pin map survived in this catalog long
/// enough to produce two confident, published, wrong diagnoses: a stable
/// "4.58cm" distance reading that was actually the sound sensor, and a
/// "floating ECHO pin" that was actually that sensor hearing the room. Both
/// were argued from real measurements. Neither was checkable against anything,
/// because nothing tied the catalog to the hardware's documentation.
///
/// Source files are named per entry so a future reader can re-derive them
/// rather than trust this transcription. Where a vendor example uses
/// `GPIO.setmode(GPIO.BOARD)` the numbers here are already converted to BCM —
/// that conversion is itself a trap, and has now cost this project twice
/// (the MAX7219 chip-select and the button matrix).
pub const VENDOR_PINS: &[(DeviceId, &str, &[u32])] = &[
    (DeviceId::Motion, "Examples/motion.py", &[23]),
    (DeviceId::Sound, "Examples/sound.py", &[24]),
    (DeviceId::Touch, "Examples/touch.py", &[17]),
    (DeviceId::InfraRed, "Examples/IR.py", &[20]),
    (DeviceId::Tilt, "Examples/tilt.py", &[22]),
    (DeviceId::Buzzer, "Examples/button_buzzer.py", &[18]),
    (DeviceId::Range, "Examples/distance.py", &[16, 12]),
    (
        DeviceId::Buttons,
        "Examples/button_matrix.py (BOARD->BCM)",
        &[27, 22, 5, 6, 13, 19, 26, 25],
    ),
];

#[cfg(test)]
mod vendor_pin_tests {
    use super::*;

    /// Every GPIO-backed device must match the vendor's documented pins.
    ///
    /// This is the check whose absence allowed the range finder to spend the
    /// project pointed at two unrelated sensors.
    #[test]
    fn the_catalog_agrees_with_the_vendor_pinout() {
        for (id, source, want) in VENDOR_PINS {
            let d = lookup(*id).unwrap_or_else(|| panic!("{id:?} is not catalogued"));
            let got: Vec<u32> = match &d.bus {
                Bus::Gpio { line } => vec![*line],
                Bus::GpioPair { trigger, echo } => vec![*trigger, *echo],
                Bus::GpioMatrix { rows, cols } => rows.iter().chain(cols.iter()).copied().collect(),
                other => panic!("{id:?} is on {other:?}, not GPIO — cannot check pins"),
            };
            assert_eq!(
                got,
                want.to_vec(),
                "{id:?} disagrees with {source}: catalog says {got:?}, vendor says {want:?}"
            );
        }
    }

    /// No two devices may claim the same line for incompatible roles.
    ///
    /// The range finder claimed 23 as an output TRIGGER while the PIR needs 23
    /// as an input, so the driver drove the sensor's own output to ground. A
    /// shared line is not always wrong — BCM22 is both the tilt switch and a
    /// button row — but a line driven by one device and read by another is.
    #[test]
    fn no_device_drives_a_line_another_device_reads() {
        let mut driven: Vec<(DeviceId, u32)> = Vec::new();
        let mut read: Vec<(DeviceId, u32)> = Vec::new();
        for d in CATALOG {
            match &d.bus {
                Bus::Gpio { line } => match d.kind {
                    DeviceKind::Sensor => read.push((d.id, *line)),
                    DeviceKind::Actuator => driven.push((d.id, *line)),
                },
                Bus::GpioPair { trigger, echo } => {
                    driven.push((d.id, *trigger));
                    read.push((d.id, *echo));
                }
                Bus::GpioMatrix { rows, cols } => {
                    for c in cols.iter() {
                        driven.push((d.id, *c));
                    }
                    for r in rows.iter() {
                        read.push((d.id, *r));
                    }
                }
                _ => {}
            }
        }
        for (writer, line) in &driven {
            for (reader, rline) in &read {
                assert!(
                    !(line == rline && writer != reader),
                    "{writer:?} drives BCM{line} which {reader:?} reads — \
                     that is two outputs fighting, and it silenced the PIR for \
                     this project's whole life"
                );
            }
        }
    }

    #[test]
    fn the_sensors_found_on_2026_09_15_are_all_catalogued() {
        for id in [
            DeviceId::Motion,
            DeviceId::Sound,
            DeviceId::Touch,
            DeviceId::InfraRed,
        ] {
            assert!(lookup(id).is_some(), "{id:?} missing from the catalog");
        }
    }
}
