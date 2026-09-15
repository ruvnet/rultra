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

    /// One HC-SR04 round trip, in centimetres.
    ///
    /// The part answers a 10us trigger with an echo pulse whose WIDTH encodes
    /// time of flight. Dividing by 58 converts microseconds to centimetres:
    /// sound travels ~343 m/s, the pulse covers the distance twice, and
    /// 1/(0.0343 cm/us) / 2 == 58.
    fn range_once() -> anyhow::Result<f64> {
        use gpio_cdev::{EventRequestFlags, LineRequestFlags};
        // Pins come from the catalog, never from literals here. When these
        // were hardcoded they said 23/24 — the PIR and the sound sensor — and
        // the catalog could be corrected without the driver noticing, which is
        // exactly how this driver spent the project timing the wrong hardware.
        let (trigger, echo_pin) = match device::lookup(DeviceId::Range).map(|d| d.bus.clone()) {
            Some(crate::Bus::GpioPair { trigger, echo }) => (trigger, echo),
            other => anyhow::bail!("range is catalogued on {other:?}, not a GPIO pair"),
        };
        let mut chip = gpio_cdev::Chip::new("/dev/gpiochip0")?;
        let trig = chip
            .get_line(trigger)?
            .request(LineRequestFlags::OUTPUT, 0, "rultra-range")?;
        let echo_line = chip.get_line(echo_pin)?;
        let echo = echo_line.events(
            LineRequestFlags::INPUT,
            EventRequestFlags::BOTH_EDGES,
            "rultra-range",
        )?;

        // Settle before triggering. Requesting the line can surface a queued
        // edge, and the trigger pulse itself crosstalks onto the echo net; both
        // arrive as a very narrow pulse that reads as a few centimetres. That
        // is what produced a 4.8cm mean with 1.8cm spread against an empty room.
        trig.set_value(0)?;
        std::thread::sleep(std::time::Duration::from_millis(2));

        trig.set_value(1)?;
        std::thread::sleep(std::time::Duration::from_micros(10));
        trig.set_value(0)?;

        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(60);
        let mut rise: Option<u64> = None;
        for ev in echo {
            if std::time::Instant::now() > deadline {
                anyhow::bail!("no echo within 60ms — out of range or nothing reflecting");
            }
            let ev = ev?;
            match ev.event_type() {
                // Always take the LATEST rising edge: crosstalk from the
                // trigger can produce a spurious early one, and the real echo
                // always follows it.
                gpio_cdev::EventType::RisingEdge => rise = Some(ev.timestamp()),
                gpio_cdev::EventType::FallingEdge => {
                    let Some(start) = rise else { continue };
                    let width_ns = ev.timestamp().saturating_sub(start);
                    let us = width_ns as f64 / 1000.0;
                    // Reject the crosstalk pulse rather than reporting it: a
                    // genuine echo from the part's 2cm minimum is at least
                    // ~116us, so anything much shorter is not a measurement.
                    if us < 100.0 {
                        rise = None;
                        continue;
                    }
                    let cm = us / 58.0;
                    if !(1.5..=450.0).contains(&cm) {
                        anyhow::bail!("echo width {us:.0}us is outside the sensor's range");
                    }
                    return Ok(cm);
                }
            }
        }
        anyhow::bail!("echo stream ended without a complete pulse")
    }

    /// Median of several round trips.
    ///
    /// A median rather than a mean: ultrasonic returns occasional wild outliers
    /// from a secondary reflection, and one bad sample must not drag the
    /// reported distance. The HC-SR04 datasheet asks for >60ms between cycles
    /// so the previous burst has decayed.
    fn read_range_cm() -> anyhow::Result<f64> {
        let mut samples = Vec::new();
        let mut last_err = None;
        for i in 0..5 {
            if i > 0 {
                std::thread::sleep(std::time::Duration::from_millis(65));
            }
            match Self::range_once() {
                Ok(v) => samples.push(v),
                Err(e) => last_err = Some(e),
            }
        }
        if samples.is_empty() {
            return Err(last_err.unwrap_or_else(|| anyhow::anyhow!("no ultrasonic samples")));
        }
        samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        Ok(samples[samples.len() / 2])
    }

    /// Read one GPIO line as an input.
    ///
    /// Never drives the line. The PIR spent this project silent because the
    /// range-finder driver held BCM23 as an output, so a read path that could
    /// accidentally become a write is the specific mistake to avoid here.
    fn read_line(line: u32) -> anyhow::Result<bool> {
        let mut chip = gpio_cdev::Chip::new("/dev/gpiochip0")?;
        let h =
            chip.get_line(line)?
                .request(gpio_cdev::LineRequestFlags::INPUT, 0, "rultra-read")?;
        Ok(h.get_value()? != 0)
    }

    fn read_cpu_temp() -> anyhow::Result<f64> {
        let raw = std::fs::read_to_string("/sys/class/thermal/thermal_zone0/temp")?;
        Ok(raw.trim().parse::<f64>()? / 1000.0)
    }
}

/// The CrowPi LCD is an **MCP23008** I2C expander driving an HD44780 — not the
/// far more common PCF8574 "1602 I2C" backpack. PCF8574-style writes land in
/// the MCP23008's IODIR/GPIO registers, so the panel acknowledges and appears
/// to invert on readback while displaying nothing at all.
mod lcd {
    /// I2C address of the expander.
    pub const ADDR: u8 = 0x21;
    /// Pin direction register. 0 = output.
    pub const IODIR: u8 = 0x00;
    /// Output latch register.
    pub const GPIO: u8 = 0x09;

    // Pin map, from Elecrow's own lcd.py.
    /// Register select: low = command, high = data.
    pub const RS: u8 = 1 << 1;
    /// Enable strobe; the HD44780 latches on its falling edge.
    pub const EN: u8 = 1 << 2;
    /// Backlight, independent of the display controller entirely — which is
    /// why it makes a good liveness test.
    pub const BACKLIGHT: u8 = 1 << 7;
    /// Data nibble occupies GP3..GP6.
    pub const DATA_SHIFT: u8 = 3;
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

    /// Convert an 8-column window (column-major, bit 0 = top) into the eight
    /// row bytes the MAX7219 wants (bit 7 = leftmost column).
    fn columns_to_rows(window: &[u8]) -> [u8; 8] {
        let mut rows = [0u8; 8];
        for (x, col) in window.iter().take(8).enumerate() {
            for (y, row) in rows.iter_mut().enumerate() {
                if col & (1 << y) != 0 {
                    *row |= 1 << (7 - x);
                }
            }
        }
        rows
    }

    /// Scroll text across the panel, right to left.
    pub fn matrix_scroll(text: &str, frame_ms: u64) -> anyhow::Result<()> {
        let cols = crate::font::columns(text);
        let mut spi = Self::matrix_init()?;
        for start in 0..cols.len().saturating_sub(7) {
            let rows = Self::columns_to_rows(&cols[start..]);
            for (i, b) in rows.iter().enumerate() {
                Self::word(&mut spi, i as u8 + 1, *b)?;
            }
            std::thread::sleep(std::time::Duration::from_millis(frame_ms));
        }
        Ok(())
    }

    /// Play a sequence of frames on one open SPI handle.
    ///
    /// `matrix_draw` re-runs `matrix_init` on every call, and init blanks all
    /// eight rows before drawing. That is correct for a single static frame and
    /// wrong for an animation: each frame becomes blank-then-draw, so the panel
    /// spends part of every frame dark and the motion reads as flicker rather
    /// than movement. `matrix_scroll` already opens the bus once for exactly
    /// this reason; animation does the same.
    pub fn matrix_animate(frames: &[([u8; 8], u64)]) -> anyhow::Result<()> {
        let mut spi = Self::matrix_init()?;
        for (rows, hold_ms) in frames {
            for (i, b) in rows.iter().enumerate() {
                Self::word(&mut spi, i as u8 + 1, *b)?;
            }
            std::thread::sleep(std::time::Duration::from_millis(*hold_ms));
        }
        Ok(())
    }

    /// Draw rows at a given brightness (0..=15).
    ///
    /// Intensity is set before the rows so the panel never flashes at the old
    /// brightness for a frame — visible as a stutter when tracking ambient light.
    pub fn matrix_draw_with_intensity(rows: &[u8; 8], intensity: u8) -> anyhow::Result<()> {
        let mut spi = Self::matrix_init()?;
        Self::word(&mut spi, max7219::INTENSITY, intensity.min(15))?;
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

impl LinuxBackend {
    fn lcd_dev(&self) -> anyhow::Result<LinuxI2CDevice> {
        Ok(LinuxI2CDevice::new(&self.bus, lcd::ADDR as u16)?)
    }

    /// Clock one nibble in. The HD44780 latches on the *falling* edge of EN,
    /// so the sequence is set-up, raise, drop — never a single write.
    fn lcd_nibble(d: &mut LinuxI2CDevice, nibble: u8, rs: u8, backlight: u8) -> anyhow::Result<()> {
        let base = ((nibble & 0x0F) << lcd::DATA_SHIFT) | rs | backlight;
        d.smbus_write_byte_data(lcd::GPIO, base)?;
        d.smbus_write_byte_data(lcd::GPIO, base | lcd::EN)?;
        // 1us is the datasheet minimum for the enable pulse width; I2C is far
        // slower than that, so the bus transaction itself satisfies it.
        d.smbus_write_byte_data(lcd::GPIO, base)?;
        Ok(())
    }

    fn lcd_byte(d: &mut LinuxI2CDevice, byte: u8, rs: u8, backlight: u8) -> anyhow::Result<()> {
        Self::lcd_nibble(d, byte >> 4, rs, backlight)?;
        Self::lcd_nibble(d, byte & 0x0F, rs, backlight)?;
        Ok(())
    }

    /// Turn the backlight on or off. Touches only GP7, so it works even if the
    /// display controller is unresponsive — the cleanest liveness check there is.
    pub fn lcd_backlight(&self, on: bool) -> anyhow::Result<()> {
        let mut d = self.lcd_dev()?;
        d.smbus_write_byte_data(lcd::IODIR, 0x00)?;
        d.smbus_write_byte_data(lcd::GPIO, if on { lcd::BACKLIGHT } else { 0 })?;
        Ok(())
    }

    /// Bring the HD44780 up in 4-bit mode.
    ///
    /// The wake-up sequence is not optional: the controller powers on in an
    /// 8-bit state, and 0x03 must be sent three times with delays before 0x02
    /// switches it to 4-bit. Skipping it leaves the panel in whatever mode it
    /// happened to boot in.
    pub fn lcd_init(&self) -> anyhow::Result<LinuxI2CDevice> {
        let mut d = self.lcd_dev()?;
        d.smbus_write_byte_data(lcd::IODIR, 0x00)?;
        std::thread::sleep(std::time::Duration::from_millis(50));
        let bl = lcd::BACKLIGHT;
        for wait in [5u64, 5, 1] {
            Self::lcd_nibble(&mut d, 0x03, 0, bl)?;
            std::thread::sleep(std::time::Duration::from_millis(wait));
        }
        Self::lcd_nibble(&mut d, 0x02, 0, bl)?; // enter 4-bit mode
        std::thread::sleep(std::time::Duration::from_millis(1));
        Self::lcd_byte(&mut d, 0x28, 0, bl)?; // 4-bit, 2 lines, 5x8 font
        Self::lcd_byte(&mut d, 0x08, 0, bl)?; // display off
        Self::lcd_byte(&mut d, 0x01, 0, bl)?; // clear
        std::thread::sleep(std::time::Duration::from_millis(2)); // clear is slow
        Self::lcd_byte(&mut d, 0x06, 0, bl)?; // entry mode: increment, no shift
        Self::lcd_byte(&mut d, 0x0C, 0, bl)?; // display on, cursor off
        Ok(d)
    }

    /// Write up to two 16-character lines.
    pub fn lcd_write(&self, line1: &str, line2: &str) -> anyhow::Result<()> {
        let mut d = self.lcd_init()?;
        let bl = lcd::BACKLIGHT;
        for (addr, text) in [(0x80u8, line1), (0xC0u8, line2)] {
            Self::lcd_byte(&mut d, addr, 0, bl)?;
            for c in text.chars().take(16) {
                Self::lcd_byte(&mut d, c as u8, lcd::RS, bl)?;
            }
        }
        Ok(())
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
                    crate::Bus::GpioPair { .. } => (
                        std::path::Path::new("/dev/gpiochip0").exists(),
                        // An echo line that is idle proves nothing on its own —
                        // only a triggered round trip does, which is a read.
                        "gpiochip0 (round trip required to confirm)".to_string(),
                    ),
                    crate::Bus::GpioMatrix { rows, cols } => (
                        std::path::Path::new("/dev/gpiochip0").exists(),
                        // Presence of the chip says nothing about the matrix:
                        // it can only be read by driving a column, and the rows
                        // must be biased first or they decode as all-pressed.
                        format!(
                            "gpiochip0 rows {rows:?} cols {cols:?} (scan required; rows need pull-up)"
                        ),
                    ),
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
            DeviceId::Range => Value::Scalar {
                n: Self::read_range_cm()?,
                unit: "centimetre".into(),
            },
            // Digital sensors: one line, one level. `active` is what the
            // device asserts, which is not always high - the sound and touch
            // parts pull LOW on detection, per the vendor examples.
            DeviceId::Motion => Value::Bool {
                on: Self::read_line(23)?,
            },
            DeviceId::Sound => Value::Bool {
                on: !Self::read_line(24)?,
            },
            DeviceId::Touch => Value::Bool {
                on: !Self::read_line(17)?,
            },
            DeviceId::InfraRed => Value::Bool {
                on: !Self::read_line(20)?,
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
