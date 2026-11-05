//! Builds audio that follows the broadcast's measured timing, for tests and
//! the silent capture check. Nothing here plays sound.

use std::f32::consts::TAU;

use crate::decoder::timing::{
    DASH_MS, DASH_SLOT_MS, DIGIT_BREAK_MS, DOT_MS, DOT_SLOT_MS, PAUSE_EXTRA_MS,
};
use crate::decoder::{Symbol, pattern};

/// One keyed element: how long the tone sounds and the silence after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key {
    pub symbol: Symbol,
    pub on_ms: u64,
    pub off_ms: u64,
}

/// The element timing of one block, ending with the long pause. The break
/// between digits 6 and 7 is left out, as the real broadcast does.
pub fn block_keys(digits: &[u8; 10]) -> Vec<Key> {
    let mut keys = Vec::with_capacity(50);
    for (index, digit) in digits.iter().enumerate() {
        for (position, symbol) in pattern(*digit).into_iter().enumerate() {
            let (on_ms, slot_ms) = match symbol {
                Symbol::Dash => (DASH_MS, DASH_SLOT_MS),
                _ => (DOT_MS, DOT_SLOT_MS),
            };
            let mut off_ms = slot_ms - on_ms;
            if position == 4 {
                match index {
                    5 => {}
                    9 => off_ms += PAUSE_EXTRA_MS,
                    _ => off_ms += DIGIT_BREAK_MS,
                }
            }
            keys.push(Key {
                symbol,
                on_ms,
                off_ms,
            });
        }
    }
    keys
}

/// Renders keyed elements as a tone at `amplitude`.
pub fn render(keys: &[Key], sample_rate: usize, hz: f32, amplitude: f32) -> Vec<f32> {
    let mut samples = Vec::new();
    let step = TAU * hz / sample_rate as f32;
    for key in keys {
        let on = sample_rate * key.on_ms as usize / 1000;
        let off = sample_rate * key.off_ms as usize / 1000;
        samples.extend((0..on).map(|n| amplitude * (step * n as f32).sin()));
        samples.resize(samples.len() + off, 0.0);
    }
    samples
}

/// Repeatable white noise.
pub struct Noise {
    state: u32,
    amplitude: f32,
}

impl Noise {
    pub fn new(seed: u32, amplitude: f32) -> Self {
        Self {
            state: seed.wrapping_mul(2_654_435_761) | 1,
            amplitude,
        }
    }

    pub fn add_to(&mut self, samples: &mut [f32]) {
        for sample in samples {
            self.state ^= self.state << 13;
            self.state ^= self.state >> 17;
            self.state ^= self.state << 5;
            *sample += self.amplitude * (self.state as f32 / u32::MAX as f32 * 2.0 - 1.0);
        }
    }
}
