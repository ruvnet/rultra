//! Spatial awareness: what the room is doing, and what that should drive.
//!
//! Two outputs from one fused state:
//!
//! 1. **Local visuals**, rendered on the 8x8 matrix the box already has. These
//!    work offline, with no provider, no spend and no network.
//! 2. **A steering signal** for generative media, shaped to fit Cognitum Media's
//!    governed dispatch envelope.
//!
//! # What this does not do
//!
//! It does not generate music or video. Cognitum Media's v0 artifact profiles
//! are disabled by its own ADR, its realtime bridge is disabled by default, and
//! Lyria admission requires a verified Google service-account on an explicit
//! allowlist. This crate produces the *input* to that system and renders what it
//! can locally; wiring it to a provider is a separate, authorized act.
//!
//! # Why a steering signal rather than a prompt
//!
//! A prompt built here would bake this box's aesthetic opinions into a sensing
//! crate. A normalized signal — how near, how bright, how settled — lets the
//! consumer decide what that *means*, and keeps the sensing honest and reusable.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod visual;

use rultra_sense::Verification;
use serde::{Deserialize, Serialize};

/// How close the nearest reflector is, in bands.
///
/// Bands rather than a raw distance because the range finder is `Unvalidated`
/// (ADR-0006): its absolute accuracy is unchecked, so a band it can support is
/// honest where a centimetre figure would not be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Proximity {
    /// Nothing within useful range.
    Empty,
    /// Something in the room, but not near.
    Far,
    /// Approaching.
    Near,
    /// Directly in front of the sensor.
    Close,
}

impl Proximity {
    /// Classify a distance in metres.
    ///
    /// Thresholds are wide on purpose. A narrow band would flicker between
    /// states on the sensor's own 0.6cm spread, and a display that flickers
    /// reads as broken rather than responsive.
    pub fn from_metres(m: f64) -> Self {
        match m {
            d if d < 0.15 => Proximity::Close,
            d if d < 0.60 => Proximity::Near,
            d if d < 2.50 => Proximity::Far,
            _ => Proximity::Empty,
        }
    }
}

/// Ambient light, in bands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Light {
    /// Effectively dark.
    Dark,
    /// Dim — evening, a screen-lit room.
    Dim,
    /// Ordinary indoor lighting.
    Lit,
    /// Bright — daylight or direct lamp.
    Bright,
}

impl Light {
    /// Classify an illuminance in lux.
    pub fn from_lux(lux: f64) -> Self {
        match lux {
            l if l < 8.0 => Light::Dark,
            l if l < 60.0 => Light::Dim,
            l if l < 400.0 => Light::Lit,
            _ => Light::Bright,
        }
    }
}

/// The fused state of the room.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoomState {
    /// Nearest reflector, banded.
    pub proximity: Proximity,
    /// Ambient light, banded.
    pub light: Light,
    /// Raw range in metres, when the sensor answered.
    pub range_m: Option<f64>,
    /// Raw illuminance in lux, when the sensor answered.
    pub lux: Option<f64>,
    /// How settled the room is, `0.0..=1.0`: 1.0 means nothing changed between
    /// the last two observations, 0.0 means it changed across a full band.
    pub stillness: f64,
    /// The weakest verification among the sensors that contributed.
    ///
    /// Carried forward rather than discarded: a fused state is only as
    /// trustworthy as its least-trustworthy input, and a consumer deciding
    /// whether to spend money on generation deserves to know that the
    /// proximity term came from an uncalibrated sensor.
    pub verification: Verification,
}

impl RoomState {
    /// Fuse one observation. `previous` is used only for stillness.
    pub fn fuse(
        range_m: Option<f64>,
        lux: Option<f64>,
        range_verification: Verification,
        light_verification: Verification,
        previous: Option<&RoomState>,
    ) -> Self {
        let proximity = range_m
            .map(Proximity::from_metres)
            .unwrap_or(Proximity::Empty);
        let light = lux.map(Light::from_lux).unwrap_or(Light::Dark);

        // Stillness compares bands, not raw values: raw sensor noise is not
        // motion, and treating it as motion is how an ambient display becomes
        // an anxious one.
        let stillness = match previous {
            None => 1.0,
            Some(p) => {
                let moved = u8::from(p.proximity != proximity) + u8::from(p.light != light);
                1.0 - (moved as f64 / 2.0)
            }
        };

        // Only count sensors that actually contributed.
        let mut verification = Verification::Working;
        if range_m.is_some() {
            verification = verification.max(range_verification);
        }
        if lux.is_some() {
            verification = verification.max(light_verification);
        }
        if range_m.is_none() && lux.is_none() {
            verification = Verification::Untested;
        }

        RoomState {
            proximity,
            light,
            range_m,
            lux,
            stillness,
            verification,
        }
    }

    /// Is this state trustworthy enough to spend money generating media from?
    ///
    /// `Working` only. Paying a provider to react to an unvalidated sensor is
    /// spending real money on a number nobody has checked.
    pub fn fit_to_spend_on(&self) -> bool {
        self.verification == Verification::Working
    }
}

/// A normalized steering signal for a generative media provider.
///
/// Deliberately not a prompt. Values are `0.0..=1.0` so a consumer can map them
/// onto tempo, palette, density or anything else without this crate having an
/// opinion about aesthetics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Steering {
    /// How near the nearest presence is. 0 = empty room, 1 = at the sensor.
    pub intensity: f64,
    /// How bright the room is. 0 = dark, 1 = bright.
    pub luminance: f64,
    /// How settled the room is. 1 = unchanging.
    pub calm: f64,
    /// Whether the state behind this signal is trustworthy enough to bill for.
    pub billable: bool,
    /// The weakest verification behind the signal, carried through verbatim.
    pub verification: Verification,
}

impl From<&RoomState> for Steering {
    fn from(r: &RoomState) -> Self {
        Steering {
            intensity: match r.proximity {
                Proximity::Empty => 0.0,
                Proximity::Far => 0.35,
                Proximity::Near => 0.7,
                Proximity::Close => 1.0,
            },
            luminance: match r.light {
                Light::Dark => 0.0,
                Light::Dim => 0.33,
                Light::Lit => 0.66,
                Light::Bright => 1.0,
            },
            calm: r.stillness,
            billable: r.fit_to_spend_on(),
            verification: r.verification,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(range: Option<f64>, lux: Option<f64>, prev: Option<&RoomState>) -> RoomState {
        RoomState::fuse(
            range,
            lux,
            Verification::Unvalidated,
            Verification::Working,
            prev,
        )
    }

    #[test]
    fn proximity_bands_are_ordered_and_wide() {
        assert_eq!(Proximity::from_metres(0.05), Proximity::Close);
        assert_eq!(Proximity::from_metres(0.40), Proximity::Near);
        assert_eq!(Proximity::from_metres(1.50), Proximity::Far);
        assert_eq!(Proximity::from_metres(9.00), Proximity::Empty);
        assert!(Proximity::Close > Proximity::Far);
    }

    /// The sensor's own spread is 0.62cm. A band boundary must not sit inside
    /// that, or the display flickers on noise alone.
    #[test]
    fn sensor_noise_cannot_flip_a_band() {
        for edge in [0.15_f64, 0.60, 2.50] {
            let below = Proximity::from_metres(edge - 0.0062);
            let above = Proximity::from_metres(edge + 0.0062);
            assert_ne!(
                below, above,
                "a boundary sits within sensor noise at {edge}m"
            );
        }
    }

    #[test]
    fn light_bands_cover_dark_to_daylight() {
        assert_eq!(Light::from_lux(3.0), Light::Dark);
        assert_eq!(Light::from_lux(37.0), Light::Dim);
        assert_eq!(Light::from_lux(133.0), Light::Lit);
        assert_eq!(Light::from_lux(900.0), Light::Bright);
    }

    /// The fused state inherits the WEAKEST verification of its inputs.
    #[test]
    fn fusion_carries_the_weakest_verification_forward() {
        let s = state(Some(0.3), Some(100.0), None);
        assert_eq!(
            s.verification,
            Verification::Unvalidated,
            "range is unvalidated"
        );
        let light_only = RoomState::fuse(
            None,
            Some(100.0),
            Verification::Unvalidated,
            Verification::Working,
            None,
        );
        assert_eq!(
            light_only.verification,
            Verification::Working,
            "range did not contribute"
        );
    }

    #[test]
    fn a_state_with_no_sensors_is_untested() {
        let s = RoomState::fuse(
            None,
            None,
            Verification::Working,
            Verification::Working,
            None,
        );
        assert_eq!(s.verification, Verification::Untested);
        assert!(!s.fit_to_spend_on());
    }

    /// Spending money on an unvalidated reading is spending money on a number
    /// nobody has checked.
    #[test]
    fn an_unvalidated_state_is_not_billable() {
        let s = state(Some(0.3), Some(100.0), None);
        assert!(!s.fit_to_spend_on());
        assert!(!Steering::from(&s).billable);
    }

    #[test]
    fn stillness_is_one_when_nothing_changed() {
        let a = state(Some(1.0), Some(100.0), None);
        let b = state(Some(1.1), Some(105.0), Some(&a));
        assert_eq!(b.stillness, 1.0, "same bands means settled");
    }

    #[test]
    fn stillness_falls_when_a_band_changes() {
        let a = state(Some(3.0), Some(100.0), None);
        let b = state(Some(0.1), Some(100.0), Some(&a));
        assert!(b.stillness < 1.0);
    }

    #[test]
    fn steering_is_normalized() {
        for (r, l) in [(0.05, 900.0), (9.0, 1.0), (0.4, 100.0)] {
            let s = Steering::from(&state(Some(r), Some(l), None));
            assert!((0.0..=1.0).contains(&s.intensity));
            assert!((0.0..=1.0).contains(&s.luminance));
            assert!((0.0..=1.0).contains(&s.calm));
        }
    }

    #[test]
    fn an_empty_dark_room_steers_to_zero() {
        let s = Steering::from(&state(Some(9.0), Some(0.0), None));
        assert_eq!(s.intensity, 0.0);
        assert_eq!(s.luminance, 0.0);
    }
}
