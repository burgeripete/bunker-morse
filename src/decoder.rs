//! Turns beeps into bunker codes.
//!
//! The broadcast loops through its bunkers. Each block is ten digits of five
//! Morse elements, followed by a pause of about seven seconds.
//!
//! Digits are found by counting elements from the pause, not from gaps. A gap
//! inside a digit after a dash (1.4 s) is longer than a gap between digits
//! after a dot (1.2 s), and the break between digits 6 and 7 is missing
//! altogether. Gaps are only used to notice beeps that were drowned out, so a
//! gunshot costs one digit rather than shifting the rest of the block.

use crate::detector::Tone;

pub const ELEMENTS_PER_DIGIT: usize = 5;
pub const DIGITS_PER_BLOCK: usize = 10;
pub const ELEMENTS_PER_BLOCK: usize = ELEMENTS_PER_DIGIT * DIGITS_PER_BLOCK;

/// Measured from a recording of the broadcast. A slot runs from the start of
/// one element to the start of the next.
pub mod timing {
    pub const DOT_MS: u64 = 120;
    pub const DASH_MS: u64 = 500;
    pub const DOT_SLOT_MS: u64 = 900;
    pub const DASH_SLOT_MS: u64 = 1_900;
    /// Added to a slot between digits.
    pub const DIGIT_BREAK_MS: u64 = 400;
    /// Added to a slot between blocks.
    pub const PAUSE_EXTRA_MS: u64 = 6_000;
}
use timing::{DASH_SLOT_MS, DIGIT_BREAK_MS, DOT_SLOT_MS};

/// Measured lengths come out a little short of the true ones, and noise only
/// ever clips a beep, so anything outside these bands is settled by its slot.
const MIN_DOT_MS: u64 = 45;
const MAX_DOT_MS: u64 = 150;
const MIN_DASH_MS: u64 = 400;
const MAX_DASH_MS: u64 = 620;
/// Longer than this is some other sound.
const MAX_TONE_MS: u64 = 800;
/// Silence that ends a block. The shortest real pause is 6.8 s, and two or
/// three drowned elements can leave a gap almost as long as this.
const PAUSE_MS: u64 = 6_000;
/// How far a measured slot may stray from its nominal length.
const SLOT_TOLERANCE_MS: i64 = 120;
/// A beep this much earlier than its slot allows cannot be part of the code.
const EARLY_MS: i64 = 200;
/// Residual slot time above this means the gap held more than a digit break.
const MAX_BREAK_MS: i64 = 650;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Symbol {
    Dot,
    Dash,
    /// Drowned out, or too damaged to tell apart.
    Unknown,
}

impl Symbol {
    pub fn as_char(self) -> char {
        match self {
            Symbol::Dot => '.',
            Symbol::Dash => '-',
            Symbol::Unknown => '?',
        }
    }

    fn slot_ms(self) -> u64 {
        match self {
            Symbol::Dash => DASH_SLOT_MS,
            _ => DOT_SLOT_MS,
        }
    }
}

/// International Morse for a digit.
pub fn pattern(digit: u8) -> [Symbol; ELEMENTS_PER_DIGIT] {
    let (first, second, leading) = match digit {
        0 => (Symbol::Dash, Symbol::Dot, 5),
        1..=5 => (Symbol::Dot, Symbol::Dash, digit as usize),
        _ => (Symbol::Dash, Symbol::Dot, digit as usize - 5),
    };
    std::array::from_fn(|index| if index < leading { first } else { second })
}

/// The one digit fitting `symbols`, where `Unknown` matches anything.
pub fn match_digit(symbols: &[Symbol]) -> Option<u8> {
    let mut matches = (0..10).filter(|digit| {
        pattern(*digit)
            .iter()
            .zip(symbols)
            .all(|(expected, seen)| *seen == Symbol::Unknown || seen == expected)
    });
    let digit = matches.next()?;
    matches.next().is_none().then_some(digit)
}

/// How an element relates to the one before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lead {
    /// First element since the stream started or a block ended.
    Start,
    Slot,
    DigitBreak,
    /// A digit break where counting says no digit ended: expect damage.
    StrayBreak,
    /// A placeholder for a beep the gap says was there but was not heard.
    Drowned,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Element {
    pub symbol: Symbol,
    pub start_ms: u64,
    pub duration_ms: u64,
    /// Silence since the previous element.
    pub gap_ms: u64,
    pub lead: Lead,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockKind {
    /// Fifty elements between two pauses.
    Complete,
    /// Listening began partway through, so only the end was heard.
    Tail,
    /// Started on time but ended short or overran.
    Damaged,
}

pub type Code = [Option<u8>; DIGITS_PER_BLOCK];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub kind: BlockKind,
    pub elements: Vec<Element>,
}

impl Block {
    /// The block's ten digits, placed by what is known about alignment.
    pub fn digits(&self) -> Code {
        let mut code = [None; DIGITS_PER_BLOCK];
        let symbols: Vec<Symbol> = self.elements.iter().map(|e| e.symbol).collect();
        if self.kind == BlockKind::Tail {
            let groups = symbols.rchunks_exact(ELEMENTS_PER_DIGIT);
            for (slot, group) in code.iter_mut().rev().zip(groups) {
                *slot = match_digit(group);
            }
        } else {
            let groups = symbols.as_chunks::<ELEMENTS_PER_DIGIT>().0;
            for (slot, group) in code.iter_mut().zip(groups) {
                *slot = match_digit(group);
            }
        }
        code
    }

    /// A code that can be trusted to belong to one bunker, digit for digit.
    pub fn code(&self) -> Option<Code> {
        (self.kind == BlockKind::Complete).then(|| self.digits())
    }
}

/// `057074 6870`, with `?` for digits not known.
pub fn format_code(code: &Code) -> String {
    let mut text = String::with_capacity(DIGITS_PER_BLOCK + 1);
    for (index, digit) in code.iter().enumerate() {
        if index == 6 {
            text.push(' ');
        }
        text.push(digit.map_or('?', |d| char::from(b'0' + d)));
    }
    text
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Output {
    Element(Element),
    /// A beep that does not fit the code, kept visible for debugging.
    Rejected(Tone),
    Block(Block),
}

#[derive(Default)]
pub struct Decoder {
    elements: Vec<Element>,
    /// Whether the current block began right after a pause.
    aligned: bool,
    last_end_ms: u64,
}

impl Decoder {
    pub fn tone(&mut self, tone: Tone, output: &mut Vec<Output>) {
        self.tick(tone.start_ms, output);

        let duration_ms = tone.duration_ms();
        if !(MIN_DOT_MS..=MAX_TONE_MS).contains(&duration_ms) {
            output.push(Output::Rejected(tone));
            return;
        }
        let symbol = match duration_ms {
            ..=MAX_DOT_MS => Symbol::Dot,
            MIN_DASH_MS..=MAX_DASH_MS => Symbol::Dash,
            _ => Symbol::Unknown,
        };

        let mut lead = Lead::Start;
        let silence = tone.start_ms.saturating_sub(self.last_end_ms) as i64;
        if let Some(last) = self.elements.last_mut() {
            let period = (tone.start_ms - last.start_ms) as i64;
            if last.symbol == Symbol::Unknown {
                last.symbol = settle_symbol(period, silence).unwrap_or(Symbol::Unknown);
            }
            let last_start_ms = last.start_ms;
            let residual = period - last.symbol.slot_ms() as i64;
            if residual < -EARLY_MS {
                output.push(Output::Rejected(tone));
                return;
            }
            lead = if residual <= EARLY_MS {
                Lead::Slot
            } else if residual <= MAX_BREAK_MS {
                if self.elements.len().is_multiple_of(ELEMENTS_PER_DIGIT) || !self.aligned {
                    Lead::DigitBreak
                } else {
                    Lead::StrayBreak
                }
            } else {
                self.drowned(residual, last_start_ms, output);
                Lead::Slot
            };
        }

        let element = Element {
            symbol,
            start_ms: tone.start_ms,
            duration_ms,
            gap_ms: tone.start_ms.saturating_sub(self.last_end_ms),
            lead,
        };
        self.elements.push(element);
        self.last_end_ms = tone.end_ms;
        output.push(Output::Element(element));
    }

    /// Advances time through silence, so a block ends as soon as its pause is
    /// long enough rather than when the next one starts.
    pub fn tick(&mut self, now_ms: u64, output: &mut Vec<Output>) {
        if now_ms.saturating_sub(self.last_end_ms) < PAUSE_MS {
            return;
        }
        self.finish(output);
        self.aligned = true;
    }

    /// Ends the current block, for a stream that stops.
    pub fn finish(&mut self, output: &mut Vec<Output>) {
        if self.elements.is_empty() {
            return;
        }
        // Exactly a block's worth is a whole block even with no pause before
        // it, as when listening starts during a pause shorter than `PAUSE_MS`.
        let kind = if self.elements.len() == ELEMENTS_PER_BLOCK {
            BlockKind::Complete
        } else if !self.aligned {
            BlockKind::Tail
        } else {
            BlockKind::Damaged
        };
        output.push(Output::Block(Block {
            kind,
            elements: std::mem::take(&mut self.elements),
        }));
    }

    /// Whether the block being received began after a pause, which is when
    /// counting its elements gives digits.
    pub fn aligned(&self) -> bool {
        self.aligned
    }

    fn drowned(&mut self, residual: i64, after_ms: u64, output: &mut Vec<Output>) {
        for (index, symbol) in drowned_symbols(residual).into_iter().enumerate() {
            let element = Element {
                symbol,
                start_ms: after_ms + index as u64 + 1,
                duration_ms: 0,
                gap_ms: 0,
                lead: Lead::Drowned,
            };
            self.elements.push(element);
            output.push(Output::Element(element));
        }
    }
}

/// Works out what a clipped beep was from the timing around it. Noise can clip
/// either end of a beep, so one of the time from its start and the silence
/// after its end will still be true to the slot.
fn settle_symbol(period: i64, silence: i64) -> Option<Symbol> {
    let error = |slot: u64, tone: u64| {
        let fit = |measured: i64, nominal: u64| {
            let without_break = (measured - nominal as i64).abs();
            let with_break = (measured - (nominal + DIGIT_BREAK_MS) as i64).abs();
            without_break.min(with_break)
        };
        fit(period, slot).min(fit(silence, slot - tone))
    };
    let dot = error(DOT_SLOT_MS, timing::DOT_MS);
    let dash = error(DASH_SLOT_MS, timing::DASH_MS);
    (dot.min(dash) <= SLOT_TOLERANCE_MS).then_some(if dot < dash {
        Symbol::Dot
    } else {
        Symbol::Dash
    })
}

/// Explains leftover slot time as the fewest elements that fit it. When the
/// fit says what they were, they are filled in, so a single drowned beep often
/// costs nothing at all.
fn drowned_symbols(residual: i64) -> Vec<Symbol> {
    for count in 1..=ELEMENTS_PER_DIGIT as u64 {
        let mut fits = (0..=count).flat_map(|dashes| {
            let slots = dashes * DASH_SLOT_MS + (count - dashes) * DOT_SLOT_MS;
            [slots, slots + DIGIT_BREAK_MS]
                .into_iter()
                .filter(move |total| (*total as i64 - residual).abs() <= SLOT_TOLERANCE_MS)
                .map(move |_| dashes)
        });
        if let Some(dashes) = fits.next() {
            let certain = fits.all(|other| other == dashes);
            let symbol = match dashes {
                _ if !certain => Symbol::Unknown,
                0 => Symbol::Dot,
                d if d == count => Symbol::Dash,
                _ => Symbol::Unknown,
            };
            return vec![symbol; count as usize];
        }
    }
    let slots = (residual as f32 / DASH_SLOT_MS as f32).round().max(1.0) as usize;
    vec![Symbol::Unknown; slots]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth::{Key, block_keys};

    const FIRST: [u8; 10] = [0, 5, 7, 0, 7, 4, 6, 8, 7, 0];
    const SECOND: [u8; 10] = [1, 1, 2, 0, 6, 7, 3, 6, 3, 6];

    fn play(decoder: &mut Decoder, keys: &[Key], mut time: u64, output: &mut Vec<Output>) {
        for key in keys {
            let tone = Tone {
                start_ms: time,
                end_ms: time + key.on_ms,
            };
            decoder.tone(tone, output);
            time += key.on_ms + key.off_ms;
        }
        decoder.tick(time, output);
    }

    /// Plays keys as if the stream opened in the pause before them.
    fn decode(keys: &[Key]) -> (Vec<Block>, Vec<Output>) {
        let mut decoder = Decoder::default();
        let mut output = Vec::new();
        decoder.tick(7_000, &mut output);
        play(&mut decoder, keys, 7_000, &mut output);
        let blocks = output
            .iter()
            .filter_map(|o| match o {
                Output::Block(block) => Some(block.clone()),
                _ => None,
            })
            .collect();
        (blocks, output)
    }

    fn digits(code: [u8; 10]) -> Code {
        code.map(Some)
    }

    #[test]
    fn every_digit_has_its_international_pattern() {
        let expected = [
            "-----", ".----", "..---", "...--", "....-", ".....", "-....", "--...", "---..",
            "----.",
        ];
        for (digit, morse) in expected.iter().enumerate() {
            let text: String = pattern(digit as u8).map(Symbol::as_char).iter().collect();
            assert_eq!(text, *morse);
        }
    }

    #[test]
    fn digits_are_counted_without_relying_on_gaps() {
        let (blocks, _) = decode(&block_keys(&FIRST));
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, BlockKind::Complete);
        assert_eq!(blocks[0].code(), Some(digits(FIRST)));
    }

    #[test]
    fn consecutive_blocks_end_at_their_pauses() {
        let mut keys = block_keys(&FIRST);
        keys.extend(block_keys(&SECOND));
        let (blocks, _) = decode(&keys);
        let codes: Vec<_> = blocks.iter().map(|b| b.code().unwrap()).collect();
        assert_eq!(codes, [digits(FIRST), digits(SECOND)]);
    }

    #[test]
    fn a_pause_after_a_dot_also_ends_a_block() {
        let code = [1, 2, 3, 4, 5, 6, 7, 8, 9, 5];
        let (blocks, _) = decode(&block_keys(&code));
        assert_eq!(blocks[0].code(), Some(digits(code)));
    }

    #[test]
    fn listening_from_mid_block_aligns_on_the_block_end() {
        let keys = block_keys(&FIRST);
        let mut decoder = Decoder::default();
        let mut output = Vec::new();
        play(&mut decoder, &keys[29..], 1_000, &mut output);
        let Some(Output::Block(block)) = output.last() else {
            panic!("no block: {output:?}");
        };
        assert_eq!(block.kind, BlockKind::Tail);
        assert_eq!(block.code(), None);
        assert_eq!(format_code(&block.digits()), "?????? 6870");
    }

    #[test]
    fn starting_late_in_a_pause_still_gets_the_next_full_block() {
        let mut decoder = Decoder::default();
        let mut output = Vec::new();
        play(&mut decoder, &block_keys(&FIRST), 3_000, &mut output);
        let Some(Output::Block(block)) = output.last() else {
            panic!("no block: {output:?}");
        };
        assert_eq!(block.kind, BlockKind::Complete);
        assert_eq!(block.code(), Some(digits(FIRST)));
    }

    #[test]
    fn a_drowned_dot_or_dash_is_recovered_from_its_gap() {
        for lost in [12, 20, 34] {
            let mut keys = block_keys(&FIRST);
            let removed = keys.remove(lost);
            keys[lost - 1].off_ms += removed.on_ms + removed.off_ms;
            let (blocks, _) = decode(&keys);
            assert_eq!(blocks[0].kind, BlockKind::Complete, "lost {lost}");
            assert_eq!(blocks[0].code(), Some(digits(FIRST)), "lost {lost}");
        }
    }

    #[test]
    fn two_drowned_elements_cost_at_most_their_digits() {
        let mut keys = block_keys(&FIRST);
        // The last dash of the first digit and the first dot of the second.
        let (dash, dot) = (keys.remove(4), keys.remove(4));
        keys[3].off_ms += dash.on_ms + dash.off_ms + dot.on_ms + dot.off_ms;
        let (blocks, _) = decode(&keys);
        assert_eq!(blocks[0].kind, BlockKind::Complete);
        let code = blocks[0].code().unwrap();
        assert_eq!(&code[2..], &digits(FIRST)[2..]);
    }

    #[test]
    fn a_blip_between_elements_is_rejected() {
        let mut keys = block_keys(&FIRST);
        // A blip 180 ms after the first dash, well before its slot ends.
        keys[0].off_ms = 180;
        let blip = Key {
            symbol: Symbol::Dot,
            on_ms: 100,
            off_ms: 1_120,
        };
        keys.insert(1, blip);
        let (blocks, output) = decode(&keys);
        assert_eq!(blocks[0].code(), Some(digits(FIRST)));
        assert!(output.iter().any(|o| matches!(o, Output::Rejected(_))));
    }

    #[test]
    fn beeps_outside_the_code_lengths_are_rejected() {
        let mut keys = block_keys(&FIRST);
        keys[3].on_ms = 1_100;
        keys[3].off_ms = 800;
        let (_, output) = decode(&keys);
        assert!(output.iter().any(|o| matches!(o, Output::Rejected(_))));
    }

    #[test]
    fn an_ambiguous_length_is_settled_by_the_next_slot() {
        let mut keys = block_keys(&FIRST);
        keys[0].on_ms = 280;
        keys[0].off_ms = 1_620;
        let (blocks, _) = decode(&keys);
        assert_eq!(blocks[0].code(), Some(digits(FIRST)));
    }

    #[test]
    fn a_dash_clipped_at_the_end_is_recognized_by_its_slot() {
        let mut keys = block_keys(&FIRST);
        keys[1].on_ms = 190;
        keys[1].off_ms = 1_710;
        let (blocks, _) = decode(&keys);
        assert_eq!(blocks[0].code(), Some(digits(FIRST)));
    }

    #[test]
    fn a_dash_clipped_at_the_start_is_recognized_by_the_silence_after_it() {
        let mut keys = block_keys(&FIRST);
        // The second dash starts 170 ms late, so 330 ms of it survives.
        keys[0].off_ms += 170;
        keys[1].on_ms = 330;
        keys[1].off_ms = 1_400;
        let (blocks, _) = decode(&keys);
        assert_eq!(blocks[0].code(), Some(digits(FIRST)));
    }

    #[test]
    fn a_short_block_is_damaged_not_trusted() {
        let mut keys = block_keys(&FIRST)[..30].to_vec();
        keys[29].off_ms = 7_000;
        let (blocks, _) = decode(&keys);
        assert_eq!(blocks[0].kind, BlockKind::Damaged);
        assert_eq!(blocks[0].code(), None);
    }

    #[test]
    fn a_stray_digit_break_is_flagged() {
        let mut keys = block_keys(&FIRST);
        keys[2].off_ms += DIGIT_BREAK_MS;
        let (blocks, _) = decode(&keys);
        let leads = blocks[0].elements.iter().map(|e| e.lead);
        assert!(leads.into_iter().any(|lead| lead == Lead::StrayBreak));
    }

    #[test]
    fn wildcards_only_resolve_when_one_digit_fits() {
        use Symbol::{Dash, Dot, Unknown};
        assert_eq!(match_digit(&[Dash, Dash, Dot, Unknown, Dot]), Some(7));
        assert_eq!(match_digit(&[Dash, Dash, Unknown, Dot, Dot]), None);
        assert_eq!(match_digit(&[Unknown, Dot, Dot, Dot, Dot]), None);
        assert_eq!(match_digit(&[Dot, Dash, Dot, Dash, Dot]), None);
    }
}
