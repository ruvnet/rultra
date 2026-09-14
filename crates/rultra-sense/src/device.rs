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
        part: "tactile switches",
        bus: Bus::Gpio { line: 0 },
        kind: DeviceKind::Sensor,
        verification: Verification::Untested,
        evidence: "High/low states were read via libgpiod during bring-up, but the \
                   specific BCM lines were never recorded. The line number here is a \
                   placeholder and must be pinned before this can claim Working.",
    },
    Device {
        id: DeviceId::Tilt,
        part: "tilt switch",
        bus: Bus::Gpio { line: 0 },
        kind: DeviceKind::Sensor,
        verification: Verification::Untested,
        evidence: "State changes observed via libgpiod during bring-up; BCM line not \
                   recorded. Placeholder line number — must be pinned before use.",
    },
    Device {
        id: DeviceId::Range,
        part: "HC-SR04",
        bus: Bus::GpioPair {
            trigger: 23,
            echo: 24,
        },
        kind: DeviceKind::Sensor,
        verification: Verification::Faulty,
        evidence: "ECHO (GPIO24) IS FLOATING — the reading is noise, not distance. \
                   Measured with gpiomon: 29,815 edges in 40s (~745 Hz) continuously, \
                   and 3,736 in a 5s window whose inter-arrival gaps spread across every \
                   timescale (420 under 0.1ms, 1425 at 0.1-1ms, 1699 at 1-10ms, only 191 \
                   over 10ms). A real HC-SR04 driven by the 1Hz poller emits ONE pulse \
                   per trigger with ~1s of silence between, so unstructured chatter at \
                   every scale is a line nothing is driving. That also explains the \
                   4.58cm mean that looked so trustworthy: 4.66cm is a 272us pulse \
                   (2 x 0.0466 / 343), the driver catches the first noise edge just past \
                   its 100us crosstalk guard, and the median-of-5 latches the same noise \
                   floor five times. Repeatability came from the noise being stationary, \
                   not from the sensor working. Same signature as a floating UART RX pin \
                   (see rultra_esp::LineState::Floating). Fix the wiring or power to the \
                   HC-SR04 before this can claim anything; the earlier 'fixed obstruction \
                   ~5cm from the emitter' theory is superseded and was wrong.",
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

    /// The specific regression this guards: the range finder returned a
    /// stable 4.58cm for an entire session and was read as trustworthy
    /// because it was repeatable. Stability is not correctness.
    #[test]
    fn the_range_finder_is_not_counted_among_working_devices() {
        let range = lookup(DeviceId::Range).expect("range is catalogued");
        assert_eq!(range.verification, Verification::Faulty);
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
