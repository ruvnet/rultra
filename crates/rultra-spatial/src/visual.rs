//! Rendering room state onto the 8x8 matrix.
//!
//! A sonar figure: concentric rings that grow as something approaches. Chosen
//! because it is spatially literal — the display gets *bigger* as the thing
//! gets *closer* — so it reads correctly without a legend.

use crate::{Light, Proximity, RoomState};

/// The eight row bytes for a room state. Bit 7 is the leftmost column.
pub fn render(room: &RoomState) -> [u8; 8] {
    match room.proximity {
        Proximity::Empty => ring(0),
        Proximity::Far => ring(1),
        Proximity::Near => ring(2),
        Proximity::Close => ring(3),
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
            proximity: p,
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
