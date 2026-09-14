//! The `ruv://lab/*` capability namespace.
//!
//! Every physical capability on the box gets one address, and the address
//! carries its *plane* — sensor or actuator. That split is not cosmetic. A
//! sensor read is idempotent and safe to retry; an actuator write moves
//! something in the physical world and may not be undoable. Encoding the plane
//! in the address means a policy can default-deny an entire class without
//! enumerating members, and means code cannot accidentally treat a relay like
//! a thermometer.
//!
//! This is the same separation `rultra_sense::DeviceKind` already makes at the
//! device layer; here it is lifted into the address so agents and policies can
//! reason about it without touching hardware.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Whether an address observes the world or changes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Plane {
    /// Reading is safe, repeatable, and has no physical effect.
    Sensor,
    /// Writing moves something. Default-denied by policy; see `policy`.
    Actuator,
}

impl Plane {
    /// Whether an address in this plane may be exercised without an explicit
    /// grant. Sensors yes, actuators never.
    pub fn safe_by_default(self) -> bool {
        matches!(self, Plane::Sensor)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UriError {
    NotLabScheme,
    MissingPlane,
    UnknownPlane(String),
    MissingKind,
    EmptySegment,
    IllegalSegment(String),
    TooManySegments,
}

impl fmt::Display for UriError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UriError::NotLabScheme => write!(f, "not a ruv://lab/ address"),
            UriError::MissingPlane => write!(f, "address has no plane (sensor/actuator)"),
            UriError::UnknownPlane(p) => write!(f, "unknown plane '{p}'"),
            UriError::MissingKind => write!(f, "address has no capability kind"),
            UriError::EmptySegment => write!(f, "address has an empty path segment"),
            UriError::IllegalSegment(s) => write!(f, "illegal path segment '{s}'"),
            UriError::TooManySegments => write!(f, "address has too many segments"),
        }
    }
}

impl std::error::Error for UriError {}

/// One addressable physical capability, e.g. `ruv://lab/actuator/relay/1`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LabUri {
    pub plane: Plane,
    /// The capability family: `light`, `motion`, `distance`, `relay`, ...
    pub kind: String,
    /// Which one, when there is more than one. `relay/1` vs `relay/2`.
    pub instance: Option<String>,
}

const PREFIX: &str = "ruv://lab/";

impl LabUri {
    pub fn sensor(kind: &str) -> LabUri {
        LabUri {
            plane: Plane::Sensor,
            kind: kind.to_string(),
            instance: None,
        }
    }

    pub fn actuator(kind: &str, instance: Option<&str>) -> LabUri {
        LabUri {
            plane: Plane::Actuator,
            kind: kind.to_string(),
            instance: instance.map(str::to_string),
        }
    }

    /// Parse an address, rejecting anything that could escape the namespace.
    ///
    /// Segments are restricted to `[a-z0-9_-]` deliberately. These addresses
    /// end up in log lines, file paths and policy keys, so `..`, slashes and
    /// whitespace are refused at the boundary rather than sanitised later.
    pub fn parse(s: &str) -> Result<LabUri, UriError> {
        let rest = s.strip_prefix(PREFIX).ok_or(UriError::NotLabScheme)?;
        let mut segs = rest.split('/');

        let plane_s = segs
            .next()
            .filter(|s| !s.is_empty())
            .ok_or(UriError::MissingPlane)?;
        let plane = match plane_s {
            "sensor" => Plane::Sensor,
            "actuator" => Plane::Actuator,
            other => return Err(UriError::UnknownPlane(other.to_string())),
        };

        let kind = segs
            .next()
            .filter(|s| !s.is_empty())
            .ok_or(UriError::MissingKind)?;
        check_segment(kind)?;

        let instance = match segs.next() {
            None => None,
            Some("") => return Err(UriError::EmptySegment),
            Some(i) => {
                check_segment(i)?;
                Some(i.to_string())
            }
        };

        if segs.next().is_some() {
            return Err(UriError::TooManySegments);
        }

        Ok(LabUri {
            plane,
            kind: kind.to_string(),
            instance,
        })
    }
}

fn check_segment(s: &str) -> Result<(), UriError> {
    if s.is_empty() {
        return Err(UriError::EmptySegment);
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err(UriError::IllegalSegment(s.to_string()));
    }
    Ok(())
}

impl fmt::Display for LabUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let plane = match self.plane {
            Plane::Sensor => "sensor",
            Plane::Actuator => "actuator",
        };
        write!(f, "{PREFIX}{plane}/{}", self.kind)?;
        if let Some(i) = &self.instance {
            write!(f, "/{i}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_documented_addresses_all_parse() {
        for s in [
            "ruv://lab/sensor/light",
            "ruv://lab/sensor/motion",
            "ruv://lab/sensor/distance",
            "ruv://lab/sensor/camera",
            "ruv://lab/sensor/rf",
            "ruv://lab/actuator/relay/1",
            "ruv://lab/actuator/display",
            "ruv://lab/actuator/led-matrix",
            "ruv://lab/actuator/buzzer",
        ] {
            let u = LabUri::parse(s).unwrap_or_else(|e| panic!("{s} failed: {e}"));
            assert_eq!(u.to_string(), s, "round trip changed the address");
        }
    }

    #[test]
    fn the_plane_decides_what_is_safe_without_a_grant() {
        assert!(LabUri::parse("ruv://lab/sensor/light")
            .unwrap()
            .plane
            .safe_by_default());
        assert!(!LabUri::parse("ruv://lab/actuator/relay/1")
            .unwrap()
            .plane
            .safe_by_default());
    }

    #[test]
    fn traversal_and_injection_are_refused_at_the_boundary() {
        for bad in [
            "ruv://lab/sensor/../../etc/passwd",
            "ruv://lab/actuator/relay/../buzzer",
            "ruv://lab/sensor/Light",  // uppercase
            "ruv://lab/sensor/li ght", // whitespace
            "ruv://lab/sensor/",       // empty kind
            "ruv://lab//light",        // empty plane
            "ruv://lab/valve/1",       // unknown plane
            "file:///etc/passwd",
            "ruv://other/sensor/light",
        ] {
            assert!(LabUri::parse(bad).is_err(), "{bad} should not parse");
        }
    }

    #[test]
    fn extra_segments_are_refused_rather_than_silently_dropped() {
        // Silently ignoring a trailing segment would let relay/1/on and
        // relay/1/off collapse to the same capability.
        assert_eq!(
            LabUri::parse("ruv://lab/actuator/relay/1/on"),
            Err(UriError::TooManySegments)
        );
    }

    #[test]
    fn instances_distinguish_addresses() {
        let a = LabUri::parse("ruv://lab/actuator/relay/1").unwrap();
        let b = LabUri::parse("ruv://lab/actuator/relay/2").unwrap();
        assert_ne!(a, b);
    }
}
