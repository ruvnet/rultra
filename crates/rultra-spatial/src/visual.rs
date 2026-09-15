//! Rendering room state onto the 8x8 matrix.
//!
//! A sonar figure: concentric rings that grow as something approaches. Chosen
//! because it is spatially literal — the display gets *bigger* as the thing
//! gets *closer* — so it reads correctly without a legend.

use crate::{Light, Proximity, RoomState};

/// The eight row bytes for a room state. Bit 7 is the leftmost column.
pub fn render(room: &RoomState) -> [u8; 8] {
    match room.proximity {
        // Blank, not ring(0). A centre dot is what an empty room looks like,
        // and a dead sensor must not be able to draw it.
        None => [0u8; 8],
        Some(Proximity::Empty) => ring(0),
        Some(Proximity::Far) => ring(1),
        Some(Proximity::Near) => ring(2),
        Some(Proximity::Close) => ring(3),
    }
}

/// Concentric square rings on an 8x8 grid, `n` from 0 (centre dot) to 3 (edge).
fn ring(n: i32) -> [u8; 8] {
    let mut rows = [0u8; 8];
    for (y, row) in rows.iter_mut().enumerate() {
        for x in 0..8i32 {
            // Chebyshev distance from the centre of the 8x8 grid. The grid is
            // even-sided, so the "centre" is the 2x2 block at 3..4 — measuring
            // to the nearer of the two keeps the rings symmetric instead of
            // biasing one corner.
            let dx = (x - 3).abs().min((x - 4).abs());
            let dy = ((y as i32) - 3).abs().min(((y as i32) - 4).abs());
            if dx.max(dy) == n {
                *row |= 1 << (7 - x);
            }
        }
    }
    rows
}

/// MAX7219 intensity (0..=15) for the ambient light level.
///
/// Inverted on purpose: a bright display in a dark room is glare, and a dim
/// display in daylight is invisible. The display should be *legible*, not
/// *proportional*.
pub fn intensity(room: &RoomState) -> u8 {
    match room.light {
        Light::Dark => 1,
        Light::Dim => 4,
        Light::Lit => 9,
        Light::Bright => 15,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rultra_sense::Verification;

    fn room(p: Proximity, l: Light) -> RoomState {
        RoomState {
            proximity: Some(p),
            light: l,
            range_m: None,
            lux: None,
            stillness: 1.0,
            verification: Verification::Unvalidated,
        }
    }

    fn lit_pixels(rows: &[u8; 8]) -> u32 {
        rows.iter().map(|r| r.count_ones()).sum()
    }

    /// The display must grow as the thing approaches, or the figure is lying
    /// about direction.
    #[test]
    fn closer_means_a_larger_figure() {
        let sizes: Vec<u32> = [
            Proximity::Empty,
            Proximity::Far,
            Proximity::Near,
            Proximity::Close,
        ]
        .iter()
        .map(|p| lit_pixels(&render(&room(*p, Light::Lit))))
        .collect();
        for w in sizes.windows(2) {
            assert!(w[1] > w[0], "figure did not grow: {sizes:?}");
        }
    }

    #[test]
    fn every_ring_is_symmetric_under_horizontal_flip() {
        for n in 0..4 {
            let rows = ring(n);
            for r in rows {
                assert_eq!(r, r.reverse_bits(), "ring {n} is not left-right symmetric");
            }
        }
    }

    #[test]
    fn every_ring_is_symmetric_under_vertical_flip() {
        for n in 0..4 {
            let rows = ring(n);
            let mut flipped = rows;
            flipped.reverse();
            assert_eq!(rows, flipped, "ring {n} is not top-bottom symmetric");
        }
    }

    #[test]
    fn the_outermost_ring_reaches_the_edge() {
        let rows = ring(3);
        assert_eq!(rows[0], 0xFF, "top edge not lit");
        assert_eq!(rows[7], 0xFF, "bottom edge not lit");
    }

    #[test]
    fn rings_do_not_overlap() {
        let (a, b) = (ring(1), ring(2));
        for i in 0..8 {
            assert_eq!(a[i] & b[i], 0, "rings 1 and 2 share pixels in row {i}");
        }
    }

    /// A bright display in a dark room is glare. Legibility, not proportion.
    #[test]
    fn brightness_tracks_ambient_light() {
        assert!(
            intensity(&room(Proximity::Far, Light::Dark))
                < intensity(&room(Proximity::Far, Light::Bright))
        );
        assert!(
            intensity(&room(Proximity::Far, Light::Dark)) >= 1,
            "never fully off"
        );
        assert!(
            intensity(&room(Proximity::Far, Light::Bright)) <= 15,
            "MAX7219 caps at 15"
        );
    }
}

/// A beating heart for the 8x8 matrix.
///
/// The matrix is the one display on this box confirmed working by an observer,
/// so it is where an animation is actually worth building.
///
/// # Why the frames are nested
///
/// Each smaller heart's lit pixels are a strict subset of the next larger one's.
/// That is what makes it read as a single shape contracting rather than as
/// three different glyphs alternating, and [`frames_are_nested`] enforces it so
/// a future edit cannot quietly break the illusion.
///
/// [`frames_are_nested`]: self
pub mod heart {
    /// Resting heart, smallest of the three.
    pub const SMALL: [u8; 8] = [
        0b00000000, 0b00000000, 0b00000000, 0b00100100, 0b00111100, 0b00111100, 0b00011000,
        0b00000000,
    ];

    /// Mid-contraction.
    pub const MEDIUM: [u8; 8] = [
        0b00000000, 0b00000000, 0b01100110, 0b01111110, 0b01111110, 0b00111100, 0b00011000,
        0b00000000,
    ];

    /// Full heart, matching the glyph the box has always drawn.
    pub const LARGE: [u8; 8] = [
        0b00000000, 0b01100110, 0b11111111, 0b11111111, 0b11111111, 0b01111110, 0b00111100,
        0b00011000,
    ];

    /// One frame: what to draw and how long to hold it.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Frame {
        /// The eight row bytes to draw. Bit 7 is the leftmost column.
        pub rows: [u8; 8],
        /// How long to leave this frame on screen before the next.
        pub hold_ms: u64,
    }

    /// Beats per minute a caller may ask for. Below the floor the animation
    /// stops reading as a pulse; above the ceiling the SPI writes and the
    /// eye both give up.
    /// Slowest pulse that still reads as a heartbeat.
    pub const MIN_BPM: u16 = 20;
    /// Fastest the SPI writes and the eye can both keep up with.
    pub const MAX_BPM: u16 = 200;
    /// A resting human rate, which is what makes it read as a heart.
    pub const DEFAULT_BPM: u16 = 72;

    const _: () = assert!(MIN_BPM <= DEFAULT_BPM && DEFAULT_BPM <= MAX_BPM);

    /// One full cardiac cycle: lub, dub, rest.
    ///
    /// Two contractions rather than one — a single pulse reads as a blink,
    /// whereas the doubled beat is what makes it recognisable as a heart. The
    /// proportions are the real thing's: systole is short, diastole is the
    /// majority of the cycle.
    pub fn beat(bpm: u16) -> Vec<Frame> {
        let bpm = bpm.clamp(MIN_BPM, MAX_BPM);
        let cycle_ms = 60_000u64 / bpm as u64;
        // Sixteenths of the cycle, so the split is exact and the sum is the
        // cycle length regardless of bpm.
        let part = |n: u64| cycle_ms * n / 16;
        let mut f = vec![
            Frame {
                rows: LARGE,
                hold_ms: part(2),
            }, // lub
            Frame {
                rows: MEDIUM,
                hold_ms: part(1),
            },
            Frame {
                rows: LARGE,
                hold_ms: part(2),
            }, // dub
            Frame {
                rows: MEDIUM,
                hold_ms: part(2),
            },
            Frame {
                rows: SMALL,
                hold_ms: part(9),
            }, // diastole: the long rest
        ];
        // Absorb the integer-division remainder into the rest, so a cycle is
        // exactly 60000/bpm ms and the animation cannot drift.
        let drawn: u64 = f.iter().map(|x| x.hold_ms).sum();
        if let Some(last) = f.last_mut() {
            last.hold_ms += cycle_ms.saturating_sub(drawn);
        }
        f
    }
}

#[cfg(test)]
mod heart_tests {
    use super::heart::*;

    fn lit(rows: &[u8; 8]) -> u32 {
        rows.iter().map(|r| r.count_ones()).sum()
    }

    #[test]
    fn frames_are_nested_so_the_heart_grows_rather_than_flickers() {
        for (small, big) in [(SMALL, MEDIUM), (MEDIUM, LARGE)] {
            for (y, (s, b)) in small.iter().zip(big.iter()).enumerate() {
                assert_eq!(
                    s & !b,
                    0,
                    "row {y}: smaller frame lights a pixel the larger one does not"
                );
            }
        }
    }

    #[test]
    fn the_three_sizes_are_strictly_ordered() {
        assert!(
            lit(&SMALL) < lit(&MEDIUM),
            "small must be smaller than medium"
        );
        assert!(
            lit(&MEDIUM) < lit(&LARGE),
            "medium must be smaller than large"
        );
    }

    #[test]
    fn the_large_frame_is_still_the_heart_the_box_has_always_drawn() {
        // The glyph an observer confirmed lit. Changing it would invalidate
        // that observation, so it is pinned.
        assert_eq!(LARGE, [0x00, 0x66, 0xff, 0xff, 0xff, 0x7e, 0x3c, 0x18]);
    }

    #[test]
    fn a_cycle_lasts_exactly_the_requested_bpm() {
        for bpm in [20u16, 45, 72, 110, 200] {
            let total: u64 = beat(bpm).iter().map(|f| f.hold_ms).sum();
            assert_eq!(total, 60_000 / bpm as u64, "bpm {bpm} drifted");
        }
    }

    #[test]
    fn the_rest_is_the_longest_phase_as_in_a_real_cycle() {
        let f = beat(DEFAULT_BPM);
        let rest = f.last().unwrap();
        assert_eq!(rest.rows, SMALL);
        let beats: u64 = f[..f.len() - 1].iter().map(|x| x.hold_ms).sum();
        assert!(rest.hold_ms > beats, "diastole must dominate the cycle");
    }

    #[test]
    fn it_is_a_double_beat_not_a_single_blink() {
        let f = beat(DEFAULT_BPM);
        let peaks = f.iter().filter(|x| x.rows == LARGE).count();
        assert_eq!(peaks, 2, "lub-dub needs two contractions");
    }

    #[test]
    fn an_absurd_bpm_is_clamped_rather_than_producing_a_zero_length_frame() {
        for bpm in [0u16, 1, 5000] {
            let f = beat(bpm);
            assert!(
                f.iter().all(|x| x.hold_ms > 0),
                "bpm {bpm} produced a zero hold"
            );
        }
    }
}
