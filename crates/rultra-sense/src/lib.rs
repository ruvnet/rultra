//! # rultra-sense — one sensor to rule them all
//!
//! A single trait surface over every sensor and actuator on the box, plus the
//! thing most hardware crates leave out: **how well each device is actually
//! known to work**.
//!
//! Hardware inventories rot because they record intent ("the board has an LCD")
//! rather than evidence ("the LCD acknowledges on I2C 0x21 but no configuration
//! has ever produced visible output"). [`Verification`] makes that distinction
//! a type, so an unproven device cannot quietly read as a working one.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod backend;
pub mod device;
pub mod font;

pub use device::{Bus, Device, DeviceId, DeviceKind, Verification};

use serde::{Deserialize, Serialize};

/// A single observation from one device.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Reading {
    /// Which device produced it.
    pub device: DeviceId,
    /// Unix epoch seconds. Seconds, not milliseconds, and not ISO-8601 —
    /// consistent with the rest of the ruvnet wire contracts.
    pub at: u64,
    /// The measured value.
    pub value: Value,
    /// How much the reading can be trusted, inherited from the device.
    pub verification: Verification,
}

/// The value space a sensor can report.
///
/// Deliberately small: a unified surface is only useful if consumers can switch
/// exhaustively over it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Value {
    /// A scalar with a unit, e.g. lux, degrees Celsius, centimetres.
    Scalar {
        /// Magnitude.
        n: f64,
        /// SI-ish unit string, e.g. `"lux"`.
        unit: String,
    },
    /// A digital line or logical on/off device.
    Bool(bool),
    /// A discrete count, e.g. a keypad scan code.
    Count(u64),
}

/// Anything that can be read.
pub trait Sensor {
    /// Stable identity of this device.
    fn id(&self) -> DeviceId;
    /// Take one reading. Errors are transport failures, not absent hardware —
    /// absence is reported by [`Backend::probe`] instead.
    fn read(&mut self) -> anyhow::Result<Reading>;
}

/// Anything that can be driven.
pub trait Actuator {
    /// Stable identity of this device.
    fn id(&self) -> DeviceId;
    /// Apply a value. Returns the value actually committed.
    fn write(&mut self, v: &Value) -> anyhow::Result<()>;
}

/// A source of devices — real hardware, or a mock for tests and CI.
pub trait Backend {
    /// Which devices are present *right now*, physically interrogated.
    ///
    /// This is the honest half of the inventory: [`device::CATALOG`] says what
    /// the board is documented to carry, `probe` says what answered today.
    fn probe(&mut self) -> anyhow::Result<Vec<Presence>>;
    /// Read one device by id.
    fn read(&mut self, id: DeviceId) -> anyhow::Result<Reading>;
}

/// The result of physically interrogating one device.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Presence {
    /// Which device.
    pub device: DeviceId,
    /// Did it answer on its bus at all?
    pub responding: bool,
    /// What the catalog claims about it, unchanged by this probe.
    pub verification: Verification,
    /// Human-readable detail, e.g. `"ACK at 0x21"`.
    pub detail: String,
}

/// Seconds since the Unix epoch.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
