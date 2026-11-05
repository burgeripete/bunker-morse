//! The decoder as a plain WebAssembly module.
//!
//! The page writes audio into a buffer owned by the session, calls
//! `session_feed`, and reads back a JSON array of what happened. This keeps the
//! build to `cargo` alone, with no JavaScript glue generator.

use std::fmt::Write as _;

use bunker_morse::bunkers::{Bunker, Bunkers, DigitState};
use bunker_morse::decoder::{Block, BlockKind, DIGITS_PER_BLOCK, Element, Lead, format_code};
use bunker_morse::detector::{Threshold, Tone, bank_hz};
use bunker_morse::listener::{Listener, Signal, Update};

pub struct Session {
    listener: Listener,
    threshold: Threshold,
    bunkers: Bunkers,
    input: Vec<f32>,
    output: String,
}

impl Session {
    fn new(sample_rate: u32) -> Self {
        let threshold = Threshold::default();
        Self {
            listener: Listener::new(sample_rate as usize, threshold.clone()),
            threshold,
            bunkers: Bunkers::default(),
            input: Vec::new(),
            output: String::new(),
        }
    }

    fn apply(output: &mut String, bunkers: &mut Bunkers, update: Update) {
        if !output.is_empty() {
            output.push(',');
        }
        match update {
            Update::Signal(signal) => write_signal(output, &signal),
            Update::Element(element) => write_element(output, &element),
            Update::Rejected(tone) => write_rejected(output, tone),
            Update::Recognised(elements) => write_recognised(output, &elements),
            Update::Block(block) => {
                write_block(output, &block);
                if let Some(code) = block.code() {
                    bunkers.record(&code);
                }
                output.push(',');
                write_bunkers(output, bunkers);
            }
        }
    }

    fn take_output(&mut self) -> *const u8 {
        self.output.insert(0, '[');
        self.output.push(']');
        self.output.as_ptr()
    }
}

fn write_signal(out: &mut String, signal: &Signal) {
    let frame = &signal.frame;
    let _ = write!(
        out,
        r#"{{"type":"signal","time":{},"hz":{:.1},"snr":{:.1},"peak":{:.1},"threshold":{:.1},"floor":{:.1},"level":{:.1},"on":{},"aligned":{},"spectrum":["#,
        frame.time_ms,
        frame.tone_hz,
        frame.snr_db,
        signal.peak_snr_db,
        frame.threshold_db,
        frame.floor_db,
        frame.level_db,
        signal.on,
        signal.aligned,
    );
    for (index, db) in signal.spectrum.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let _ = write!(out, "{db:.1}");
    }
    out.push_str("]}");
}

fn lead_name(lead: Lead) -> &'static str {
    match lead {
        Lead::Start => "start",
        Lead::Slot => "slot",
        Lead::DigitBreak => "break",
        Lead::StrayBreak => "stray",
        Lead::Drowned => "drowned",
        Lead::Known => "known",
    }
}

fn write_element(out: &mut String, element: &Element) {
    let _ = write!(
        out,
        r#"{{"type":"element","symbol":"{}","start":{},"duration":{},"gap":{},"lead":"{}"}}"#,
        element.symbol.as_char(),
        element.start_ms,
        element.duration_ms,
        element.gap_ms,
        lead_name(element.lead),
    );
}

fn write_recognised(out: &mut String, elements: &[Element]) {
    out.push_str(r#"{"type":"recognised","elements":["#);
    for (index, element) in elements.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        write_element(out, element);
    }
    out.push_str("]}");
}

fn write_rejected(out: &mut String, tone: Tone) {
    let _ = write!(
        out,
        r#"{{"type":"rejected","start":{},"duration":{}}}"#,
        tone.start_ms,
        tone.duration_ms(),
    );
}

fn write_block(out: &mut String, block: &Block) {
    let kind = match block.kind {
        BlockKind::Complete => "complete",
        BlockKind::Tail => "tail",
        BlockKind::Damaged => "damaged",
    };
    let flagged = block
        .elements
        .iter()
        .filter(|e| matches!(e.lead, Lead::StrayBreak | Lead::Drowned))
        .count();
    let known = block
        .elements
        .iter()
        .filter(|e| e.lead == Lead::Known)
        .count();
    let _ = write!(
        out,
        r#"{{"type":"block","kind":"{kind}","digits":"{}","elements":{},"flagged":{flagged},"known":{known},"start":{}}}"#,
        format_code(&block.digits()),
        block.elements.len(),
        block.elements.first().map_or(0, |e| e.start_ms),
    );
}

fn write_bunker(out: &mut String, bunker: &Bunker) {
    out.push_str(r#"{"digits":["#);
    for index in 0..DIGITS_PER_BLOCK {
        if index > 0 {
            out.push(',');
        }
        let state = match bunker.state(index) {
            DigitState::Missing => "missing",
            DigitState::Heard => "heard",
            DigitState::Confirmed => "confirmed",
            DigitState::Disputed => "disputed",
        };
        let digit = bunker.digit(index).map_or(-1, i32::from);
        let _ = write!(out, r#"{{"digit":{digit},"state":"{state}"}}"#);
    }
    let _ = write!(
        out,
        r#"],"raw":"{}","gps":"{}","passes":{},"confirmed":{},"offList":{},"url":"#,
        bunker.raw_coordinates(),
        bunker.gps(),
        bunker.passes,
        bunker.is_confirmed(),
        bunker.is_off_list(),
    );
    match bunker.map_url() {
        Some(url) => {
            let _ = write!(out, r#""{url}""#);
        }
        None => out.push_str("null"),
    }
    out.push('}');
}

fn write_bunkers(out: &mut String, bunkers: &Bunkers) {
    out.push_str(r#"{"type":"bunkers","slots":["#);
    for (index, bunker) in bunkers.slots.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        write_bunker(out, bunker);
    }
    let _ = write!(out, r#"],"unplaced":{}}}"#, bunkers.unplaced);
}

/// Starts a session for audio at `sample_rate`.
#[unsafe(no_mangle)]
pub extern "C" fn session_new(sample_rate: u32) -> *mut Session {
    Box::into_raw(Box::new(Session::new(sample_rate)))
}

/// # Safety
/// `session` must come from `session_new` and not be used afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn session_free(session: *mut Session) {
    drop(unsafe { Box::from_raw(session) });
}

/// Returns room for `length` samples, for the page to write audio into.
///
/// # Safety
/// `session` must come from `session_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn session_input(session: *mut Session, length: usize) -> *mut f32 {
    let session = unsafe { &mut *session };
    session.input.resize(length, 0.0);
    session.input.as_mut_ptr()
}

/// Decodes the samples in the input buffer. Returns a JSON array whose length
/// is given by `session_output_length`.
///
/// # Safety
/// `session` must come from `session_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn session_feed(session: *mut Session) -> *const u8 {
    let session = unsafe { &mut *session };
    let Session {
        listener,
        bunkers,
        input,
        output,
        ..
    } = session;
    output.clear();
    listener.feed(input, &mut |update| {
        Session::apply(output, bunkers, update);
    });
    session.take_output()
}

/// Ends the stream, reporting any block in progress. Returns JSON as
/// `session_feed` does.
///
/// # Safety
/// `session` must come from `session_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn session_finish(session: *mut Session) -> *const u8 {
    let session = unsafe { &mut *session };
    let Session {
        listener,
        bunkers,
        output,
        ..
    } = session;
    output.clear();
    listener.finish(&mut |update| {
        Session::apply(output, bunkers, update);
    });
    session.take_output()
}

/// # Safety
/// `session` must come from `session_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn session_output_length(session: *const Session) -> usize {
    unsafe { &*session }.output.len()
}

/// Sets a fixed beep threshold in dB. Zero or less follows the beeps instead.
///
/// # Safety
/// `session` must come from `session_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn session_set_threshold(session: *mut Session, db: f32) {
    let manual = (db > 0.0).then_some(db);
    unsafe { &*session }.threshold.set(manual);
}

/// Forgets the decoded codes, for a server restart.
///
/// # Safety
/// `session` must come from `session_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn session_reset(session: *mut Session) {
    unsafe { &mut *session }.bunkers = Bunkers::default();
}

/// The frequency of a spectrum bin, for labelling the display.
#[unsafe(no_mangle)]
pub extern "C" fn spectrum_hz(bin: usize) -> f32 {
    bank_hz(bin)
}
