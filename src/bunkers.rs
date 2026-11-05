//! Collects decoded blocks into bunkers.
//!
//! The broadcast repeats, so each bunker is heard again every few minutes. A
//! digit damaged on one pass is settled by the others.

use crate::decoder::{Code, DIGITS_PER_BLOCK};

pub const BUNKER_COUNT: usize = 3;
/// A block joins a bunker when enough digits are known on both sides and at
/// most a quarter of those differ, up to `MAX_CONFLICTS`. Random codes agree on
/// about one digit in ten, so this only merges passes of the same bunker.
const MIN_OVERLAP: usize = 4;
const MAX_CONFLICTS: usize = 2;
/// A block needs this many digits to take a slot or to count as a new code, so
/// a badly damaged pass cannot use up one of the three bunkers.
const MIN_DIGITS_FOR_NEW: usize = 8;

const MAP_URL: &str = "https://www.izurvive.com/nasdara/";
const MAP_ZOOM: u8 = 6;
/// Nasdara's edge length in game units.
const MAP_SIZE: i32 = 16_384;
/// Coordinate digits count 100 m squares: `057` is GPS 0.57, or 5700 units.
const UNITS_PER_STEP: i32 = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DigitState {
    Missing,
    /// Heard on one pass only.
    Heard,
    /// Heard the same on at least two passes.
    Confirmed,
    /// Passes disagree, and this is the most common value.
    Disputed,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bunker {
    votes: [[u16; 10]; DIGITS_PER_BLOCK],
    pub passes: u32,
}

impl Bunker {
    pub fn digit(&self, index: usize) -> Option<u8> {
        let (digit, count) = self.votes[index]
            .iter()
            .enumerate()
            .max_by_key(|(digit, count)| (**count, std::cmp::Reverse(*digit)))?;
        (*count > 0).then_some(digit as u8)
    }

    pub fn state(&self, index: usize) -> DigitState {
        let votes = &self.votes[index];
        let total: u16 = votes.iter().sum();
        let top = votes.iter().copied().max().unwrap_or(0);
        match (total, top) {
            (0, _) => DigitState::Missing,
            (_, top) if top < total => DigitState::Disputed,
            (_, 1) => DigitState::Heard,
            _ => DigitState::Confirmed,
        }
    }

    /// The six coordinate digits, `057074`, with `-` for those not known.
    pub fn raw_coordinates(&self) -> String {
        (0..6).map(|index| self.digit_char(index)).collect()
    }

    /// The coordinates as a GPS reads them, `X: 0.57  Y: 0.74`.
    pub fn gps(&self) -> String {
        let axis = |start: usize| {
            format!(
                "{}.{}{}",
                self.digit_char(start),
                self.digit_char(start + 1),
                self.digit_char(start + 2)
            )
        };
        format!("X: {}  Y: {}", axis(0), axis(3))
    }

    /// The location on the iZurvive Nasdara map, once all six coordinate
    /// digits are known and fall on the map. Nasdara's Y axis is flipped.
    pub fn map_url(&self) -> Option<String> {
        let axis = |start: usize| -> Option<i32> {
            (start..start + 3).try_fold(0, |value, index| {
                Some(value * 10 + i32::from(self.digit(index)?))
            })
        };
        let game_x = axis(0)? * UNITS_PER_STEP;
        let game_y = MAP_SIZE - axis(3)? * UNITS_PER_STEP;
        let on_map = (0..=MAP_SIZE).contains(&game_x) && (0..=MAP_SIZE).contains(&game_y);
        on_map.then(|| format!("{MAP_URL}#location={game_x};{game_y};{MAP_ZOOM}"))
    }

    fn digit_char(&self, index: usize) -> char {
        self.digit(index).map_or('-', |d| char::from(b'0' + d))
    }

    pub fn is_confirmed(&self) -> bool {
        (0..DIGITS_PER_BLOCK).all(|i| self.state(i) == DigitState::Confirmed)
    }

    fn record(&mut self, code: &Code) {
        for (votes, digit) in self.votes.iter_mut().zip(code) {
            if let Some(digit) = digit {
                votes[*digit as usize] += 1;
            }
        }
        self.passes += 1;
    }

    fn compare(&self, code: &Code) -> (usize, usize) {
        let mut matches = 0;
        let mut conflicts = 0;
        for (index, digit) in code.iter().enumerate() {
            if let (Some(seen), Some(known)) = (*digit, self.digit(index)) {
                if seen == known {
                    matches += 1;
                } else {
                    conflicts += 1;
                }
            }
        }
        (matches, conflicts)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bunkers {
    pub slots: Vec<Bunker>,
    /// Blocks that fit no bunker while all slots were taken: usually a server
    /// restart, which changes the codes.
    pub unplaced: u32,
}

impl Bunkers {
    /// Files a block's digits and returns the bunker they went to.
    pub fn record(&mut self, code: &Code) -> Option<usize> {
        let best = self
            .slots
            .iter()
            .enumerate()
            .map(|(index, bunker)| (index, bunker.compare(code)))
            .filter(|(_, (matches, conflicts))| {
                let overlap = matches + conflicts;
                overlap >= MIN_OVERLAP && *conflicts <= MAX_CONFLICTS && conflicts * 4 <= overlap
            })
            .max_by_key(|(_, (matches, _))| *matches)
            .map(|(index, _)| index);

        if let Some(index) = best {
            self.slots[index].record(code);
            return Some(index);
        }
        if code.iter().flatten().count() < MIN_DIGITS_FOR_NEW {
            return None;
        }
        if self.slots.len() < BUNKER_COUNT {
            let mut bunker = Bunker::default();
            bunker.record(code);
            self.slots.push(bunker);
            return Some(self.slots.len() - 1);
        }
        self.unplaced += 1;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: [u8; 10] = [0, 5, 7, 0, 7, 4, 6, 8, 7, 0];
    const B: [u8; 10] = [1, 1, 2, 0, 6, 7, 3, 6, 3, 6];
    const C: [u8; 10] = [1, 0, 7, 1, 2, 3, 0, 0, 3, 4];
    const D: [u8; 10] = [9, 9, 9, 4, 4, 4, 2, 2, 2, 2];

    fn code(digits: [u8; 10]) -> Code {
        digits.map(Some)
    }

    #[test]
    fn distinct_codes_take_their_own_slots() {
        let mut bunkers = Bunkers::default();
        let placed = [A, B, C].map(|d| bunkers.record(&code(d)));
        assert_eq!(placed, [Some(0), Some(1), Some(2)]);
    }

    #[test]
    fn a_repeat_confirms_its_bunker() {
        let mut bunkers = Bunkers::default();
        bunkers.record(&code(A));
        assert!(!bunkers.slots[0].is_confirmed());
        assert_eq!(bunkers.record(&code(A)), Some(0));
        assert!(bunkers.slots[0].is_confirmed());
        assert_eq!(bunkers.slots[0].passes, 2);
    }

    #[test]
    fn a_later_pass_fills_a_damaged_digit() {
        let mut damaged = code(A);
        damaged[3] = None;
        damaged[7] = None;
        let mut bunkers = Bunkers::default();
        bunkers.record(&damaged);
        assert_eq!(bunkers.slots[0].state(3), DigitState::Missing);
        assert_eq!(bunkers.record(&code(A)), Some(0));
        assert_eq!(bunkers.slots[0].digit(3), Some(0));
        assert_eq!(bunkers.slots[0].state(3), DigitState::Heard);
        assert_eq!(bunkers.slots[0].state(4), DigitState::Confirmed);
    }

    #[test]
    fn a_misread_digit_is_outvoted_and_marked_disputed() {
        let mut wrong = code(A);
        wrong[2] = Some(1);
        let mut bunkers = Bunkers::default();
        bunkers.record(&wrong);
        bunkers.record(&code(A));
        bunkers.record(&code(A));
        assert_eq!(bunkers.slots.len(), 1);
        assert_eq!(bunkers.slots[0].digit(2), Some(7));
        assert_eq!(bunkers.slots[0].state(2), DigitState::Disputed);
    }

    #[test]
    fn coordinates_read_both_raw_and_as_gps() {
        let mut bunkers = Bunkers::default();
        bunkers.record(&code(A));
        bunkers.record(&code(C));
        assert_eq!(bunkers.slots[0].raw_coordinates(), "057074");
        assert_eq!(bunkers.slots[0].gps(), "X: 0.57  Y: 0.74");
        assert_eq!(bunkers.slots[1].gps(), "X: 1.07  Y: 1.23");
    }

    fn map_url(digits: [u8; 10]) -> Option<String> {
        let mut bunkers = Bunkers::default();
        bunkers.record(&code(digits));
        bunkers.slots[0].map_url()
    }

    #[test]
    fn coordinates_link_to_the_nasdara_map() {
        let example = [0, 1, 0, 0, 2, 0, 0, 0, 0, 0];
        assert_eq!(
            map_url(example).as_deref(),
            Some("https://www.izurvive.com/nasdara/#location=1000;14384;6")
        );
        assert!(map_url(A).unwrap().ends_with("#location=5700;8984;6"));
        assert!(map_url(B).unwrap().ends_with("#location=11200;9684;6"));
        assert!(map_url(C).unwrap().ends_with("#location=10700;4084;6"));
    }

    #[test]
    fn the_map_edges_are_on_the_map_and_past_them_is_not() {
        assert!(map_url([1, 6, 3, 1, 6, 3, 0, 0, 0, 0]).is_some());
        assert_eq!(map_url([1, 6, 4, 0, 0, 0, 0, 0, 0, 0]), None);
        assert_eq!(map_url([0, 0, 0, 1, 6, 4, 0, 0, 0, 0]), None);
    }

    #[test]
    fn no_link_until_every_coordinate_digit_is_known() {
        let mut damaged = code(B);
        damaged[4] = None;
        let mut bunkers = Bunkers::default();
        bunkers.record(&damaged);
        assert_eq!(bunkers.slots[0].map_url(), None);
        bunkers.record(&code(B));
        assert!(bunkers.slots[0].map_url().is_some());
    }

    #[test]
    fn unknown_coordinate_digits_stay_visible() {
        let mut damaged = code(B);
        damaged[1] = None;
        let mut bunkers = Bunkers::default();
        bunkers.record(&damaged);
        assert_eq!(bunkers.slots[0].raw_coordinates(), "1-2067");
        assert_eq!(bunkers.slots[0].gps(), "X: 1.-2  Y: 0.67");
    }

    #[test]
    fn a_badly_damaged_pass_still_joins_its_bunker() {
        let mut damaged = code(A);
        damaged[3..8].fill(None);
        let mut bunkers = Bunkers::default();
        for digits in [A, B, C] {
            bunkers.record(&code(digits));
        }
        assert_eq!(bunkers.record(&damaged), Some(0));
        assert_eq!(bunkers.slots[0].passes, 2);
        assert_eq!(bunkers.unplaced, 0);
    }

    #[test]
    fn a_badly_damaged_pass_cannot_take_a_slot_or_raise_the_restart_notice() {
        let mut damaged = code(D);
        damaged[2..7].fill(None);
        let mut bunkers = Bunkers::default();
        assert_eq!(bunkers.record(&damaged), None);
        assert!(bunkers.slots.is_empty());
        for digits in [A, B, C] {
            bunkers.record(&code(digits));
        }
        assert_eq!(bunkers.record(&damaged), None);
        assert_eq!(bunkers.unplaced, 0);
    }

    #[test]
    fn a_fourth_code_is_counted_not_stored() {
        let mut bunkers = Bunkers::default();
        for digits in [A, B, C] {
            bunkers.record(&code(digits));
        }
        assert_eq!(bunkers.record(&code(D)), None);
        assert_eq!(bunkers.slots.len(), 3);
        assert_eq!(bunkers.unplaced, 1);
    }
}
