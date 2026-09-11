//! Real hardware, via the Linux character devices: `/dev/i2c-*`, `/dev/spidev*`
//! and `/dev/gpiochip*`.
//!
//! Requires the `hardware` feature. Every device is opened lazily, so a box
//! missing one bus still serves the others rather than failing wholesale.

use crate::{device, now, Backend, DeviceId, Presence, Reading, Value, Verification};
use i2cdev::core::I2CDevice;
use i2cdev::linux::LinuxI2CDevice;

/// I2C bus 1 is the Pi's user bus on every model to date.
const I2C_BUS: &str = "/dev/i2c-1";

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
