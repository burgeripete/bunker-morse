//! Finds the broadcast's beep in mixed game audio.
//!
//! Each hop measures a bank of frequencies across the band the tone can sit
//! in. The tone's level is compared with the bank's median rather than with a
//! fixed level, so game volume does not matter and a gunshot, which raises the
//! whole band at once, does not look like a beep.

use std::f32::consts::TAU;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

/// The tone heard in recordings, used until a stronger one is locked.
pub const DEFAULT_TONE_HZ: f32 = 988.0;

const BANK_LOW_HZ: f32 = 600.0;
const BANK_HIGH_HZ: f32 = 1_500.0;
const BANK_STEP_HZ: f32 = 20.0;
const WINDOW_MS: usize = 40;
pub const HOP_MS: u64 = 10;
/// Keeps logarithms finite on digital silence: -120 dBFS.
const POWER_FLOOR: f32 = 1e-12;

/// A bank bin must stand this far above the median to count as a candidate.
const LOCK_CONTRAST_DB: f32 = 20.0;
/// Candidate votes fade over about half a minute, longer than any pause.
const LOCK_DECAY_PER_HOP: f32 = 0.9995;
/// A rival must out-vote the locked bin by this much to take over.
const LOCK_SWITCH_RATIO: f32 = 1.5;
/// How fast the displayed frequency follows each reading during a beep.
const TONE_SMOOTHING: f32 = 0.1;

/// The automatic on threshold is this share, in dB, of the way from the floor
/// to the loudest recent beep, held inside the range below. In game a beep
/// reads about 43 dB, and ambient sound near its pitch can reach about 23 dB.
const AUTO_THRESHOLD_SHARE: f32 = 0.7;
const MIN_ON_THRESHOLD_DB: f32 = 12.0;
const MAX_ON_THRESHOLD_DB: f32 = 36.0;
const HYSTERESIS_DB: f32 = 4.0;
const PEAK_DECAY_DB_PER_HOP: f32 = 0.005;

/// Gaps this short inside a beep are detector flicker, and pulses this short
/// are not beeps. The shortest real gap is about 800 ms.
const MERGE_GAP_MS: u64 = 40;
const MIN_TONE_MS: u64 = 40;

/// One detector measurement. `time_ms` counts stream samples, never the wall
/// clock, so a file decoded faster than real time gives identical results.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub time_ms: u64,
    pub tone_hz: f32,
    /// Tone level above the bank median.
    pub snr_db: f32,
    pub threshold_db: f32,
    /// Bank median, in dBFS.
    pub floor_db: f32,
    /// Overall level of the audio, in dBFS.
    pub level_db: f32,
    pub on: bool,
}

/// The on threshold setting, shared so it can change while audio is running.
#[derive(Clone, Debug, Default)]
pub struct Threshold(Arc<AtomicU32>);

impl Threshold {
    /// Sets a fixed level in dB, or `None` to follow the beeps automatically.
    pub fn set(&self, manual_db: Option<f32>) {
        let bits = manual_db.map_or(0, f32::to_bits);
        self.0.store(bits, Ordering::Relaxed);
    }

    pub fn manual_db(&self) -> Option<f32> {
        match self.0.load(Ordering::Relaxed) {
            0 => None,
            bits => Some(f32::from_bits(bits)),
        }
    }
}

/// A debounced beep.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tone {
    pub start_ms: u64,
    pub end_ms: u64,
}

impl Tone {
    pub fn duration_ms(&self) -> u64 {
        self.end_ms - self.start_ms
    }
}

pub struct Detector {
    sample_rate: usize,
    window: usize,
    hop: usize,
    /// Hann-weighted cosine and sine rows, one per bank bin.
    cosines: Vec<Vec<f32>>,
    sines: Vec<Vec<f32>>,
    power: Vec<f32>,
    votes: Vec<f32>,
    locked: usize,
    /// Only measured during a beep: between beeps the bank holds just noise.
    tone_hz: f32,
    peak_snr_db: f32,
    threshold: Threshold,
    on: bool,
    buffer: Vec<f32>,
    consumed: u64,
}

impl Detector {
    pub fn new(sample_rate: usize, threshold: Threshold) -> Self {
        let window = sample_rate * WINDOW_MS / 1000;
        let hop = sample_rate * HOP_MS as usize / 1000;
        let bins = bank_len();
        let hann: Vec<f32> = (0..window)
            .map(|n| 0.5 - 0.5 * (TAU * n as f32 / (window - 1) as f32).cos())
            .collect();
        // Scales a full-scale sine to a power of 1.
        let gain = 2.0 / hann.iter().sum::<f32>();
        let row = |bin: usize, wave: fn(f32) -> f32| -> Vec<f32> {
            let step = TAU * bank_hz(bin) / sample_rate as f32;
            hann.iter()
                .enumerate()
                .map(|(n, w)| w * gain * wave(step * n as f32))
                .collect()
        };
        Self {
            sample_rate,
            window,
            hop,
            cosines: (0..bins).map(|bin| row(bin, f32::cos)).collect(),
            sines: (0..bins).map(|bin| row(bin, f32::sin)).collect(),
            power: vec![POWER_FLOOR; bins],
            votes: vec![0.0; bins],
            locked: nearest_bin(DEFAULT_TONE_HZ),
            tone_hz: DEFAULT_TONE_HZ,
            peak_snr_db: 0.0,
            threshold,
            on: false,
            buffer: Vec::new(),
            consumed: 0,
        }
    }

    pub fn push(&mut self, samples: &[f32], frames: &mut Vec<Frame>) {
        self.buffer.extend_from_slice(samples);
        let mut position = 0;
        while position + self.window <= self.buffer.len() {
            let frame = self.analyze(position);
            frames.push(frame);
            position += self.hop;
        }
        self.buffer.drain(..position);
        self.consumed += position as u64;
    }

    /// The latest bank, in dB above its median, for the spectrum display.
    pub fn spectrum(&self) -> Vec<f32> {
        let floor = median(&self.power);
        self.power.iter().map(|p| db(p / floor)).collect()
    }

    fn analyze(&mut self, position: usize) -> Frame {
        let samples = &self.buffer[position..position + self.window];
        for bin in 0..self.power.len() {
            let re: f32 = samples
                .iter()
                .zip(&self.cosines[bin])
                .map(|(s, c)| s * c)
                .sum();
            let im: f32 = samples
                .iter()
                .zip(&self.sines[bin])
                .map(|(s, c)| s * c)
                .sum();
            self.power[bin] = re * re + im * im + POWER_FLOOR;
        }
        let energy: f32 = samples.iter().map(|s| s * s).sum();
        let level_db = db(energy / samples.len() as f32 + POWER_FLOOR);

        let floor = median(&self.power);
        self.update_lock(floor);

        // Neighbours too, since the tone may fall between two bins.
        let low = self.locked.saturating_sub(1);
        let high = (self.locked + 1).min(self.power.len() - 1);
        let tone = self.power[low..=high].iter().copied().fold(0.0, f32::max);
        let snr_db = db(tone / floor);

        self.peak_snr_db = (self.peak_snr_db - PEAK_DECAY_DB_PER_HOP).max(snr_db);
        let threshold_db = self.threshold.manual_db().unwrap_or_else(|| {
            (self.peak_snr_db * AUTO_THRESHOLD_SHARE)
                .clamp(MIN_ON_THRESHOLD_DB, MAX_ON_THRESHOLD_DB)
        });
        self.on = if self.on {
            snr_db >= threshold_db - HYSTERESIS_DB
        } else {
            snr_db >= threshold_db
        };
        if self.on {
            self.tone_hz += TONE_SMOOTHING * (self.refined_hz() - self.tone_hz);
        }

        Frame {
            time_ms: (self.consumed + position as u64 + self.window as u64 / 2) * 1000
                / self.sample_rate as u64,
            tone_hz: self.tone_hz,
            snr_db,
            threshold_db,
            floor_db: db(floor),
            level_db,
            on: self.on,
        }
    }

    fn update_lock(&mut self, floor: f32) {
        self.votes.iter_mut().for_each(|v| *v *= LOCK_DECAY_PER_HOP);
        let (peak, power) = self
            .power
            .iter()
            .copied()
            .enumerate()
            .fold(
                (0, 0.0),
                |best, item| {
                    if item.1 > best.1 { item } else { best }
                },
            );
        if db(power / floor) < LOCK_CONTRAST_DB {
            return;
        }
        self.votes[peak] += 1.0;
        if self.votes[peak] > self.votes[self.locked] * LOCK_SWITCH_RATIO {
            self.locked = peak;
        }
    }

    /// The locked frequency, interpolated between bins for display.
    fn refined_hz(&self) -> f32 {
        let bin = self.locked;
        if bin == 0 || bin + 1 == self.power.len() {
            return bank_hz(bin);
        }
        let (left, middle, right) = (
            db(self.power[bin - 1]),
            db(self.power[bin]),
            db(self.power[bin + 1]),
        );
        let curve = left - 2.0 * middle + right;
        if curve >= -0.01 {
            return bank_hz(bin);
        }
        let offset = (0.5 * (left - right) / curve).clamp(-1.0, 1.0);
        bank_hz(bin) + offset * BANK_STEP_HZ
    }
}

/// Turns per-hop on/off decisions into whole beeps.
#[derive(Default)]
pub struct Keyer {
    current: Option<Tone>,
}

impl Keyer {
    pub fn frame(&mut self, frame: &Frame) -> Option<Tone> {
        if frame.on {
            let end_ms = frame.time_ms + HOP_MS;
            match &mut self.current {
                Some(tone) => tone.end_ms = end_ms,
                None => {
                    self.current = Some(Tone {
                        start_ms: frame.time_ms,
                        end_ms,
                    });
                }
            }
            return None;
        }
        let tone = self.current?;
        if frame.time_ms < tone.end_ms + MERGE_GAP_MS {
            return None;
        }
        self.current = None;
        (tone.duration_ms() >= MIN_TONE_MS).then_some(tone)
    }
}

pub fn bank_len() -> usize {
    ((BANK_HIGH_HZ - BANK_LOW_HZ) / BANK_STEP_HZ) as usize + 1
}

pub fn bank_hz(bin: usize) -> f32 {
    BANK_LOW_HZ + bin as f32 * BANK_STEP_HZ
}

fn nearest_bin(hz: f32) -> usize {
    ((hz - BANK_LOW_HZ) / BANK_STEP_HZ).round() as usize
}

fn db(power: f32) -> f32 {
    10.0 * power.log10()
}

fn median(values: &[f32]) -> f32 {
    let mut sorted = values.to_vec();
    let middle = sorted.len() / 2;
    *sorted.select_nth_unstable_by(middle, f32::total_cmp).1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth::Noise;

    const RATE: usize = 48_000;

    fn tones(samples: &[f32]) -> (Vec<Tone>, Vec<Frame>) {
        tones_with(samples, Threshold::default())
    }

    fn tones_with(samples: &[f32], threshold: Threshold) -> (Vec<Tone>, Vec<Frame>) {
        let mut detector = Detector::new(RATE, threshold);
        let mut frames = Vec::new();
        for chunk in samples.chunks(731) {
            detector.push(chunk, &mut frames);
        }
        let mut keyer = Keyer::default();
        let tones = frames.iter().filter_map(|f| keyer.frame(f)).collect();
        (tones, frames)
    }

    fn sine(hz: f32, ms: usize, amplitude: f32) -> Vec<f32> {
        (0..RATE * ms / 1000)
            .map(|n| amplitude * (TAU * hz * n as f32 / RATE as f32).sin())
            .collect()
    }

    fn silence(ms: usize) -> Vec<f32> {
        vec![0.0; RATE * ms / 1000]
    }

    #[test]
    fn measures_beep_lengths_regardless_of_volume() {
        for amplitude in [0.5, 0.05, 0.0005] {
            let mut signal = silence(300);
            signal.extend(sine(988.0, 120, amplitude));
            signal.extend(silence(800));
            signal.extend(sine(988.0, 500, amplitude));
            signal.extend(silence(800));
            Noise::new(7, amplitude / 300.0).add_to(&mut signal);

            let (tones, _) = tones(&signal);
            assert_eq!(tones.len(), 2, "amplitude {amplitude}: {tones:?}");
            assert!(tones[0].duration_ms().abs_diff(120) <= 50, "{tones:?}");
            assert!(tones[1].duration_ms().abs_diff(500) <= 50, "{tones:?}");
        }
    }

    #[test]
    fn locks_onto_a_tone_at_another_pitch() {
        let mut signal = Vec::new();
        for _ in 0..8 {
            signal.extend(sine(760.0, 500, 0.1));
            signal.extend(silence(400));
        }
        Noise::new(3, 0.0005).add_to(&mut signal);
        let (tones, frames) = tones(&signal);
        assert!(tones.len() >= 7, "{tones:?}");
        assert!((frames.last().unwrap().tone_hz - 760.0).abs() < 15.0);
    }

    #[test]
    fn broadband_noise_bursts_are_not_beeps() {
        let mut signal = silence(2_000);
        Noise::new(1, 0.0002).add_to(&mut signal);
        let burst = RATE..RATE + RATE / 2;
        Noise::new(2, 0.3).add_to(&mut signal[burst]);
        let (tones, _) = tones(&signal);
        assert!(tones.is_empty(), "{tones:?}");
    }

    /// Levels from a live session: beeps about 20 dB above an ambient sound at
    /// the same pitch, with the ambient running right up to a beep.
    fn beeps_after_ambient() -> Vec<f32> {
        let beep = |signal: &mut Vec<f32>| {
            signal.extend(sine(988.0, 500, 0.05));
            signal.extend(silence(1_400));
        };
        let mut signal = silence(300);
        (0..3).for_each(|_| beep(&mut signal));
        signal.extend(sine(988.0, 2_000, 0.005));
        beep(&mut signal);
        Noise::new(9, 0.01).add_to(&mut signal);
        signal
    }

    #[test]
    fn ambient_sound_at_the_beep_pitch_stays_under_the_auto_threshold() {
        let (tones, frames) = tones(&beeps_after_ambient());
        assert_eq!(tones.len(), 4, "{tones:?}");
        assert!(tones.iter().all(|t| t.duration_ms().abs_diff(500) <= 50));
        assert!(frames.iter().any(|f| f.threshold_db > 28.0));
    }

    #[test]
    fn a_manual_threshold_replaces_the_automatic_one() {
        let threshold = Threshold::default();
        threshold.set(Some(14.0));
        let (tones, frames) = tones_with(&beeps_after_ambient(), threshold.clone());
        assert!(frames.iter().all(|f| f.threshold_db == 14.0));
        assert!(tones.iter().any(|t| t.duration_ms() > 1_000), "{tones:?}");

        threshold.set(None);
        assert_eq!(threshold.manual_db(), None);
        let (tones, _) = tones_with(&beeps_after_ambient(), threshold);
        assert_eq!(tones.len(), 4, "{tones:?}");
    }

    #[test]
    fn a_threshold_change_applies_while_audio_is_running() {
        let threshold = Threshold::default();
        let mut detector = Detector::new(RATE, threshold.clone());
        let mut frames = Vec::new();
        detector.push(&silence(100), &mut frames);
        threshold.set(Some(40.0));
        frames.clear();
        detector.push(&silence(100), &mut frames);
        assert!(frames.iter().all(|f| f.threshold_db == 40.0));
    }

    #[test]
    fn a_flicker_inside_a_beep_is_merged() {
        let frame = |time_ms, on| Frame {
            time_ms,
            tone_hz: 988.0,
            snr_db: 0.0,
            threshold_db: 0.0,
            floor_db: 0.0,
            level_db: 0.0,
            on,
        };
        let mut keyer = Keyer::default();
        let mut out = Vec::new();
        for t in 0..60u64 {
            let on = t < 50 && t != 20;
            out.extend(keyer.frame(&frame(t * HOP_MS, on)));
        }
        assert_eq!(
            out,
            [Tone {
                start_ms: 0,
                end_ms: 500
            }]
        );
    }

    #[test]
    fn chunk_size_does_not_change_the_frames() {
        let mut signal = sine(988.0, 300, 0.2);
        signal.extend(silence(300));
        let run = |size: usize| {
            let mut detector = Detector::new(RATE, Threshold::default());
            let mut frames = Vec::new();
            for chunk in signal.chunks(size) {
                detector.push(chunk, &mut frames);
            }
            frames
        };
        assert_eq!(run(97), run(10_000));
    }
}
