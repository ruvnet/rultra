//! Real hardware, via the Linux character devices: `/dev/i2c-*`, `/dev/spidev*`
//! and `/dev/gpiochip*`.
//!
//! Requires the `hardware` feature. Every device is opened lazily, so a box
//! missing one bus still serves the others rather than failing wholesale.

use crate::{device, now, Backend, DeviceId, Presence, Reading, Value, Verification};
use i2cdev::core::I2CDevice;
use i2cdev::linux::LinuxI2CDevice;
use spidev::{SpiModeFlags, Spidev, SpidevOptions};
use std::io::Write as _;

/// I2C bus 1 is the Pi's user bus on every model to date.
const I2C_BUS: &str = "/dev/i2c-1";

/// The 8x8 matrix is on SPI0 **CE1**, with a hardware chip-select.
///
/// The board silkscreen reads "CS: GPIO 26", which means *physical pin 26* —
/// that is BCM GPIO7, i.e. CE1. The vendor manual transposes the BCM numbers
/// between its two SPI rows, and reading it literally leads to driving a button
/// line as if it were a chip-select. Elecrow's own driver uses
/// `spi(port=0, device=1)`.
const MATRIX_DEV: &str = "/dev/spidev0.1";

/// MAX7219 maximum serial clock is 10 MHz. The Raspberry Pi devicetree
/// advertises `spi-max-frequency = 125000000`, and spidev will happily use it
/// if a speed is never set explicitly — 12.5x over the part's limit, which
/// fails silently and identically on every chip-select. Setting this is not
/// optional.
const MATRIX_HZ: u32 = 1_000_000;

/// Hardware backend.
pub struct LinuxBackend {
    bus: String,
}

impl LinuxBackend {
    /// Open the backend. Fails only if the I2C bus node is absent entirely,
    /// which means I2C is not enabled in firmware.
    pub fn open() -> anyhow::Result<Self> {
        if !std::path::Path::new(I2C_BUS).exists() {
            anyhow::bail!("{I2C_BUS} missing — enable I2C (raspi-config / dtparam=i2c_arm=on)");
        }
        Ok(Self {
            bus: I2C_BUS.to_string(),
        })
    }

    /// Does a device acknowledge at `addr`? An ACK proves the chip is powered
    /// and addressed; it proves nothing about whether its *output* is routed.
    fn acks(&self, addr: u8) -> bool {
        match LinuxI2CDevice::new(&self.bus, addr as u16) {
            Ok(mut d) => d.smbus_read_byte().is_ok(),
            Err(_) => false,
        }
    }

    /// BH1750: one-shot high-resolution mode, 1 lux resolution.
    fn read_lux(&self) -> anyhow::Result<f64> {
        let mut d = LinuxI2CDevice::new(&self.bus, 0x5c)?;
        d.smbus_write_byte(0x20)?; // one-time H-resolution mode
        std::thread::sleep(std::time::Duration::from_millis(180));
        let raw = d.smbus_read_word_data(0x00).unwrap_or(0);
        // The BH1750 returns big-endian; smbus word reads are little-endian.
        let be = u16::from_be_bytes(raw.to_le_bytes());
        Ok(be as f64 / 1.2)
    }

    fn read_cpu_temp() -> anyhow::Result<f64> {
        let raw = std::fs::read_to_string("/sys/class/thermal/thermal_zone0/temp")?;
        Ok(raw.trim().parse::<f64>()? / 1000.0)
    }
}

/// MAX7219 register addresses.
mod max7219 {
    pub const DECODE_MODE: u8 = 0x09;
    pub const INTENSITY: u8 = 0x0A;
    pub const SCAN_LIMIT: u8 = 0x0B;
    pub const SHUTDOWN: u8 = 0x0C;
    pub const DISPLAY_TEST: u8 = 0x0F;
}

impl LinuxBackend {
    fn open_matrix() -> anyhow::Result<Spidev> {
        let mut spi = Spidev::open(MATRIX_DEV)?;
        spi.configure(
            &SpidevOptions::new()
                .bits_per_word(8)
                .max_speed_hz(MATRIX_HZ)
                .mode(SpiModeFlags::SPI_MODE_0)
                .build(),
        )?;
        Ok(spi)
    }

    /// One 16-bit command. Exactly one transfer per word: the MAX7219 latches
    /// on the chip-select's rising edge and keeps only the last 16 bits shifted
    /// in, so batching several commands into a single write would latch only
    /// the final one.
    fn word(spi: &mut Spidev, addr: u8, data: u8) -> anyhow::Result<()> {
        spi.write_all(&[addr, data])?;
        Ok(())
    }

    /// Bring the matrix up. Order follows luma.led_matrix: leave shutdown
    /// **last**, after the digit registers have been cleared, so the panel
    /// never displays whatever happened to be in its RAM at power-on.
    pub fn matrix_init() -> anyhow::Result<Spidev> {
        let mut spi = Self::open_matrix()?;
        Self::word(&mut spi, max7219::SCAN_LIMIT, 0x07)?;
        Self::word(&mut spi, max7219::DECODE_MODE, 0x00)?;
        Self::word(&mut spi, max7219::DISPLAY_TEST, 0x00)?;
        Self::word(&mut spi, max7219::INTENSITY, 0x07)?;
        for row in 1..=8u8 {
            Self::word(&mut spi, row, 0x00)?;
        }
        Self::word(&mut spi, max7219::SHUTDOWN, 0x01)?;
        Ok(spi)
    }

    /// Draw eight rows, MSB leftmost.
    pub fn matrix_draw(rows: &[u8; 8]) -> anyhow::Result<()> {
        let mut spi = Self::matrix_init()?;
        for (i, b) in rows.iter().enumerate() {
            Self::word(&mut spi, i as u8 + 1, *b)?;
        }
        Ok(())
    }

    /// Light every LED from the chip's own oscillator, bypassing row RAM.
    /// Per the datasheet this overrides shutdown, so if display-test produces
    /// nothing the words are not reaching the chip at all.
    pub fn matrix_display_test(on: bool) -> anyhow::Result<()> {
        let mut spi = Self::open_matrix()?;
        Self::word(&mut spi, max7219::DISPLAY_TEST, u8::from(on))
    }
}

impl Backend for LinuxBackend {
    fn probe(&mut self) -> anyhow::Result<Vec<Presence>> {
        Ok(device::CATALOG
            .iter()
            .map(|d| {
                let (responding, detail) = match &d.bus {
                    crate::Bus::I2c { addr } => {
                        let a = self.acks(*addr);
                        (
                            a,
                            if a {
                                format!("ACK at {addr:#04x}")
                            } else {
                                format!("no ACK at {addr:#04x}")
                            },
                        )
                    }
                    crate::Bus::Sysfs { path } => {
                        let e = std::path::Path::new(path).exists();
                        (
                            e,
                            format!("{path} {}", if e { "present" } else { "missing" }),
                        )
                    }
                    crate::Bus::Spi { dev, .. } => {
                        let e = std::path::Path::new(dev).exists();
                        // A spidev node proves the bus exists, never that a
                        // chip is on the other end — SPI has no ACK.
                        (
                            e,
                            format!(
                                "{dev} {} (SPI cannot confirm a peer)",
                                if e { "present" } else { "missing" }
                            ),
                        )
                    }
                    crate::Bus::Gpio { .. } => (
                        std::path::Path::new("/dev/gpiochip0").exists(),
                        "gpiochip0".to_string(),
                    ),
                };
                Presence {
                    device: d.id,
                    responding,
                    verification: d.verification,
                    detail,
                }
            })
            .collect())
    }

    fn read(&mut self, id: DeviceId) -> anyhow::Result<Reading> {
        let value = match id {
            DeviceId::Light => Value::Scalar {
                n: self.read_lux()?,
                unit: "lux".into(),
            },
            DeviceId::CpuTemp => Value::Scalar {
                n: Self::read_cpu_temp()?,
                unit: "celsius".into(),
            },
            other => anyhow::bail!("{other:?} has no read path on the hardware backend yet"),
        };
        Ok(Reading {
            device: id,
            at: now(),
            value,
            verification: device::lookup(id)
                .map(|d| d.verification)
                .unwrap_or(Verification::Untested),
        })
    }
}
