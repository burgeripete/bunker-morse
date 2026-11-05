//! The audio-to-code chain, independent of where the audio comes from. The
//! desktop app feeds it from WASAPI and the web page from the browser.

use crate::decoder::{Block, Decoder, Element, Output};
use crate::detector::{Detector, Frame, HOP_MS, Keyer, Threshold, Tone};

/// How often the signal displays get a new reading, in stream time.
pub const SIGNAL_INTERVAL_MS: u64 = 50;
const FRAMES_PER_SIGNAL: usize = (SIGNAL_INTERVAL_MS / HOP_MS) as usize;

/// What the detector saw over one signal interval.
#[derive(Clone, Debug, PartialEq)]
pub struct Signal {
    /// The latest frame.
    pub frame: Frame,
    /// Strongest tone reading in the interval, so short blips stay visible.
    pub peak_snr_db: f32,
    pub on: bool,
    /// Bank levels in dB above their median.
    pub spectrum: Vec<f32>,
    /// Whether the elements being heard can be counted into digits yet.
    pub aligned: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Update {
    Signal(Signal),
    Element(Element),
    Rejected(Tone),
    Block(Block),
}

/// The audio-to-code chain, independent of where the audio comes from.
pub struct Listener {
    detector: Detector,
    keyer: Keyer,
    decoder: Decoder,
    frames: Vec<Frame>,
    output: Vec<Output>,
    interval: Vec<Frame>,
}

impl Listener {
    pub fn new(sample_rate: usize, threshold: Threshold) -> Self {
        Self {
            detector: Detector::new(sample_rate, threshold),
            keyer: Keyer::default(),
            decoder: Decoder::default(),
            frames: Vec::new(),
            output: Vec::new(),
            interval: Vec::with_capacity(FRAMES_PER_SIGNAL),
        }
    }

    pub fn feed(&mut self, samples: &[f32], emit: &mut impl FnMut(Update)) {
        self.frames.clear();
        self.detector.push(samples, &mut self.frames);
        for frame in std::mem::take(&mut self.frames) {
            if let Some(tone) = self.keyer.frame(&frame) {
                self.decoder.tone(tone, &mut self.output);
            }
            self.decoder.tick(frame.time_ms, &mut self.output);
            self.flush(emit);

            self.interval.push(frame);
            if self.interval.len() == FRAMES_PER_SIGNAL {
                emit(Update::Signal(Signal {
                    frame,
                    peak_snr_db: self
                        .interval
                        .iter()
                        .map(|f| f.snr_db)
                        .fold(f32::MIN, f32::max),
                    on: self.interval.iter().any(|f| f.on),
                    spectrum: self.detector.spectrum(),
                    aligned: self.decoder.aligned(),
                }));
                self.interval.clear();
            }
        }
    }

    /// Ends the stream, reporting whatever block was in progress.
    pub fn finish(&mut self, emit: &mut impl FnMut(Update)) {
        self.decoder.finish(&mut self.output);
        self.flush(emit);
    }

    fn flush(&mut self, emit: &mut impl FnMut(Update)) {
        for output in self.output.drain(..) {
            emit(match output {
                Output::Element(element) => Update::Element(element),
                Output::Rejected(tone) => Update::Rejected(tone),
                Output::Block(block) => Update::Block(block),
            });
        }
    }
}
