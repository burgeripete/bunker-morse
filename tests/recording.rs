//! Runs the whole audio chain over a clip of the real broadcast: the end of
//! one block, a pause, one full block and the start of the next.

use std::path::Path;
use std::sync::atomic::AtomicBool;

use bunker_morse::decoder::{Block, BlockKind, format_code};
use bunker_morse::detector::Threshold;
use bunker_morse::engine::{Listener, Update};
use bunker_morse::media::read_audio;
use bunker_morse::synth::Noise;

const CLIP: &str = "assets/broadcast-clip.wav";
const EXPECTED: &str = "057074 6870";

fn clip() -> (Vec<f32>, usize) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(CLIP);
    let mut samples = Vec::new();
    let mut rate = 0;
    read_audio(&path, &AtomicBool::new(false), |chunk, chunk_rate| {
        samples.extend_from_slice(chunk);
        rate = chunk_rate;
    })
    .expect("reading the clip");
    (samples, rate)
}

fn blocks(samples: &[f32], rate: usize) -> Vec<Block> {
    let mut listener = Listener::new(rate, Threshold::default());
    let mut found = Vec::new();
    let mut emit = |update| {
        if let Update::Block(block) = update {
            found.push(block);
        }
    };
    for chunk in samples.chunks(4_096) {
        listener.feed(chunk, &mut emit);
    }
    listener.finish(&mut emit);
    found
}

fn complete_codes(blocks: &[Block]) -> Vec<String> {
    blocks
        .iter()
        .filter_map(Block::code)
        .map(|code| format_code(&code))
        .collect()
}

#[test]
fn decodes_the_real_broadcast() {
    let (samples, rate) = clip();
    let blocks = blocks(&samples, rate);
    assert_eq!(complete_codes(&blocks), [EXPECTED]);
    assert_eq!(blocks[0].kind, BlockKind::Tail);
}

#[test]
fn a_block_joined_partway_through_is_completed_from_the_list() {
    let (samples, rate) = clip();
    // The block starts about 10 s into the clip, and each digit takes 4 to 9 s.
    // Some of these land in the middle of a beep.
    for join_seconds in (11..=41).step_by(3) {
        let blocks = blocks(&samples[join_seconds * rate..], rate);
        assert_eq!(
            complete_codes(&blocks),
            [EXPECTED],
            "joined at {join_seconds} s"
        );
    }
}

#[test]
fn a_block_joined_too_late_keeps_only_what_was_heard() {
    let (samples, rate) = clip();
    let blocks = blocks(&samples[50 * rate..], rate);
    assert!(complete_codes(&blocks).is_empty());
    let digits = |b: &Block| format_code(&b.digits());
    assert!(blocks.iter().any(|b| digits(b) == "?????? 6870"));
}

#[test]
fn does_not_depend_on_game_volume() {
    let (samples, rate) = clip();
    let quiet: Vec<f32> = samples.iter().map(|s| s * 0.01).collect();
    assert_eq!(complete_codes(&blocks(&quiet, rate)), [EXPECTED]);
}

#[test]
fn decodes_under_steady_noise_louder_than_the_beeps() {
    let (mut samples, rate) = clip();
    Noise::new(5, 0.16).add_to(&mut samples);
    assert_eq!(complete_codes(&blocks(&samples, rate)), [EXPECTED]);
}

#[test]
fn hopeless_noise_never_produces_a_trusted_code() {
    let (mut samples, rate) = clip();
    Noise::new(5, 0.3).add_to(&mut samples);
    assert!(complete_codes(&blocks(&samples, rate)).is_empty());
}

#[test]
fn survives_broadband_bursts_over_the_beeps() {
    let (mut samples, rate) = clip();
    // Half-second bursts far louder than the beeps, every seven seconds.
    let mut noise = Noise::new(11, 0.5);
    for second in (10..95).step_by(7) {
        let start = second * rate;
        noise.add_to(&mut samples[start..start + rate / 2]);
    }
    let blocks = blocks(&samples, rate);
    let code = blocks
        .iter()
        .find_map(Block::code)
        .expect("the full block is still counted out");
    let text = format_code(&code);
    let wrong = text
        .chars()
        .zip(EXPECTED.chars())
        .filter(|(heard, truth)| *heard != '?' && heard != truth)
        .count();
    let known = text.chars().filter(char::is_ascii_digit).count();
    assert_eq!(wrong, 0, "{text}");
    assert!(known >= 7, "{text}");
}
