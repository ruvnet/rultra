//! The CrowPi 4x4 button matrix.
//!
//! Wiring is taken from Elecrow's own `Examples/button_matrix.py`, converted
//! from the BOARD (physical) numbering that example uses into BCM, which is
//! what every other driver here speaks. That conversion is the single most
//! error-prone step on this board — the vendor's own examples mix the two
//! schemes, and the same trap already cost this project the MAX7219's
//! chip-select.
//!
//! | | physical | BCM |
//! |---|---|---|
//! | rows (inputs, pull-up) | 13, 15, 29, 31 | 27, 22, 5, 6 |
//! | columns (outputs, rest **high**) | 33, 35, 37, 22 | 13, 19, 26, 25 |
//!
//! # A scanned matrix cannot be watched passively
//!
//! A press shorts one row to one column. With every column resting high,
//! nothing changes on any row no matter how many buttons are held — so
//! `gpiomon` on the row lines reports *nothing, ever*. Reading the matrix
//! means driving one column low at a time and sampling the rows during that
//! window. Three separate passive sweeps of this board found zero events
//! before that was understood.
//!
//! # The rows must be biased, and this crate cannot do it
//!
//! `gpio-cdev` 0.6 exposes only INPUT/OUTPUT/ACTIVE_LOW/OPEN_DRAIN/OPEN_SOURCE
//! — bias arrived with the v2 kernel uAPI and is not available here. Without a
//! pull-up the rows float, and a floating row reads low *constantly*, which
//! decodes as "every button in that row is held down forever". That is not a
//! hypothetical: it produced 572 phantom presses in one 40-second run.
//!
//! So the bias must be applied out of band (`pinctrl set 5,6,22,27 ip pu`, or
//! `gpio=5,6,22,27=ip,pu` in config.txt), and [`MatrixReading::decode`] refuses
//! to report anything at all when the baseline shows it was not.

use serde::{Deserialize, Serialize};

/// Row lines in BCM numbering. Inputs; must be pulled up.
pub const ROWS: [u32; 4] = [27, 22, 5, 6];
/// Column lines in BCM numbering. Outputs; rest high, driven low to scan.
pub const COLS: [u32; 4] = [13, 19, 26, 25];

/// Button ids are 1..=16, row-major, matching the vendor's `buttonIDs` table.
pub fn button_id(row: usize, col: usize) -> u8 {
    (row * COLS.len() + col + 1) as u8
}

/// The rows were floating when this reading was taken, so it says nothing.
///
/// Carries which rows were bad so the operator can fix the right pins rather
/// than re-running the scan and getting the same confident nonsense.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BiasFault {
    /// BCM lines that read low with no column driven.
    pub floating_rows: Vec<u32>,
}

impl std::fmt::Display for BiasFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "row(s) {:?} read low with no column driven, so they are floating, \
             not pressed — apply pull-ups (`pinctrl set {} ip pu`) before trusting \
             any scan; without them a floating row decodes as four buttons held \
             down forever",
            self.floating_rows,
            ROWS.map(|r| r.to_string()).join(",")
        )
    }
}

impl std::error::Error for BiasFault {}

/// Which buttons are down, as a bitmask over ids 1..=16.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pressed(pub u16);

impl Pressed {
    /// Whether button `id` (1..=16) is down.
    pub fn is_down(&self, id: u8) -> bool {
        (1..=16).contains(&id) && self.0 & (1 << (id - 1)) != 0
    }
    /// Mark button `id` down. Ids outside 1..=16 are ignored.
    pub fn set(&mut self, id: u8) {
        if (1..=16).contains(&id) {
            self.0 |= 1 << (id - 1);
        }
    }
    /// How many buttons are down.
    pub fn count(&self) -> u32 {
        self.0.count_ones()
    }
    /// Whether no button is down.
    pub fn is_empty(&self) -> bool {
        self.0 == 0
    }
    /// Ids that are down, ascending.
    pub fn ids(&self) -> Vec<u8> {
        (1..=16u8).filter(|i| self.is_down(*i)).collect()
    }

    /// Whether this reading *could* be corrupted by matrix ghosting.
    ///
    /// A matrix without per-key diodes cannot distinguish some three-key
    /// combinations from four: pressing three corners of a rectangle makes the
    /// fourth appear pressed too. This is a property of the hardware, not of
    /// the scan, so the honest move is to flag the reading rather than to
    /// silently pick one interpretation.
    pub fn may_be_ghosted(&self) -> bool {
        for r1 in 0..4 {
            for r2 in (r1 + 1)..4 {
                let mut shared = 0;
                for c in 0..4 {
                    if self.is_down(button_id(r1, c)) && self.is_down(button_id(r2, c)) {
                        shared += 1;
                    }
                }
                if shared >= 2 {
                    return true;
                }
            }
        }
        false
    }
}

/// One complete pass over the matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatrixReading {
    /// Row levels with **no** column driven. All four must be high.
    pub baseline: [bool; 4],
    /// `scanned[col][row]` — the level on each row while that column was low.
    pub scanned: [[bool; 4]; 4],
}

impl MatrixReading {
    /// Decode a pass, or refuse if the rows were not biased.
    pub fn decode(&self) -> Result<Pressed, BiasFault> {
        let floating: Vec<u32> = self
            .baseline
            .iter()
            .enumerate()
            .filter(|(_, high)| !**high)
            .map(|(i, _)| ROWS[i])
            .collect();
        if !floating.is_empty() {
            return Err(BiasFault {
                floating_rows: floating,
            });
        }
        let mut p = Pressed::default();
        for (c, rows) in self.scanned.iter().enumerate() {
            for (r, high) in rows.iter().enumerate() {
                // Low while this column is driven low: the button bridges them.
                if !*high {
                    p.set(button_id(r, c));
                }
            }
        }
        Ok(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean() -> MatrixReading {
        MatrixReading {
            baseline: [true; 4],
            scanned: [[true; 4]; 4],
        }
    }

    #[test]
    fn the_pin_map_matches_the_vendor_example_converted_to_bcm() {
        // physical 13,15,29,31 -> BCM 27,22,5,6 ; physical 33,35,37,22 -> 13,19,26,25
        assert_eq!(ROWS, [27, 22, 5, 6]);
        assert_eq!(COLS, [13, 19, 26, 25]);
    }

    #[test]
    fn button_ids_are_row_major_one_to_sixteen() {
        assert_eq!(button_id(0, 0), 1);
        assert_eq!(button_id(0, 3), 4);
        assert_eq!(button_id(2, 0), 9);
        assert_eq!(button_id(3, 3), 16);
    }

    #[test]
    fn nothing_pressed_decodes_to_nothing() {
        assert_eq!(clean().decode().unwrap(), Pressed::default());
        assert!(clean().decode().unwrap().is_empty());
    }

    #[test]
    fn a_real_press_is_located_by_row_and_column() {
        // The observed press: button 9, row BCM5 (index 2), column BCM13 (index 0).
        let mut r = clean();
        r.scanned[0][2] = false;
        let p = r.decode().unwrap();
        assert_eq!(p.ids(), vec![9]);
        assert!(p.is_down(9) && !p.is_down(10));
    }

    #[test]
    fn the_other_observed_presses_decode_correctly() {
        // button 12 = row BCM5 (2), col BCM25 (3); button 7 = row BCM22 (1), col BCM26 (2)
        let mut r = clean();
        r.scanned[3][2] = false;
        assert_eq!(r.decode().unwrap().ids(), vec![12]);
        let mut r = clean();
        r.scanned[2][1] = false;
        assert_eq!(r.decode().unwrap().ids(), vec![7]);
    }

    #[test]
    fn a_floating_row_is_refused_rather_than_reported_as_four_presses() {
        // The exact failure: BCM27 floated low, and the scan reported buttons
        // 1-4 held down on every pass — 572 of them in 40 seconds.
        let mut r = clean();
        r.baseline[0] = false;
        for c in 0..4 {
            r.scanned[c][0] = false; // what a floating row looks like
        }
        let e = r.decode().unwrap_err();
        assert_eq!(e.floating_rows, vec![27]);
        assert!(e.to_string().contains("floating"));
        assert!(e.to_string().contains("pinctrl"), "must say how to fix it");
    }

    #[test]
    fn the_fault_names_every_bad_row_not_just_the_first() {
        let mut r = clean();
        r.baseline[1] = false;
        r.baseline[3] = false;
        assert_eq!(r.decode().unwrap_err().floating_rows, vec![22, 6]);
    }

    #[test]
    fn two_buttons_in_different_rows_and_columns_are_both_seen() {
        let mut r = clean();
        r.scanned[0][0] = false; // button 1
        r.scanned[2][2] = false; // button 11
        let p = r.decode().unwrap();
        assert_eq!(p.ids(), vec![1, 11]);
        assert_eq!(p.count(), 2);
        assert!(!p.may_be_ghosted(), "a diagonal pair cannot ghost");
    }

    #[test]
    fn a_rectangle_of_presses_is_flagged_as_possibly_ghosted() {
        // Without per-key diodes, three corners of a rectangle make the fourth
        // appear pressed. The reading is reported, but marked unreliable.
        let mut r = clean();
        for (c, row) in [(0usize, 0usize), (1, 0), (0, 1), (1, 1)] {
            r.scanned[c][row] = false;
        }
        let p = r.decode().unwrap();
        assert!(p.may_be_ghosted(), "shared rows and columns can ghost");
    }

    #[test]
    fn out_of_range_ids_are_ignored_rather_than_shifting_the_mask() {
        let mut p = Pressed::default();
        p.set(0);
        p.set(17);
        assert!(p.is_empty(), "invalid ids must not corrupt the bitmask");
        assert!(!p.is_down(0) && !p.is_down(17));
    }
}
