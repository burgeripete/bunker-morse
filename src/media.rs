//! Reads the audio out of a recording, such as an OBS `.mkv`, for debugging
//! the decoder without a game. Nothing here plays sound.

use std::fs::File;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context as _, Result, anyhow};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
use symphonia::core::errors::Error;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// Decodes the first audio track, handing `sink` mono chunks and their rate.
pub fn read_audio(
    path: &Path,
    stop: &AtomicBool,
    mut sink: impl FnMut(&[f32], usize),
) -> Result<()> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(extension);
    }
    let mut format = symphonia::default::get_probe()
        .format(
            &hint,
            stream,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .with_context(|| format!("reading {}", path.display()))?
        .format;

    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL && t.codec_params.sample_rate.is_some())
        .ok_or_else(|| anyhow!("{} has no audio track", path.display()))?;
    let track_id = track.id;
    let rate = track.codec_params.sample_rate.unwrap_or_default() as usize;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .context("opening the audio decoder")?;

    let mut interleaved: Option<SampleBuffer<f32>> = None;
    let mut mono = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(Error::IoError(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                break;
            }
            Err(error) => return Err(error).context("reading the recording"),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let audio = match decoder.decode(&packet) {
            Ok(audio) => audio,
            Err(Error::DecodeError(error)) => {
                log::warn!("skipping a damaged audio packet: {error}");
                continue;
            }
            Err(error) => return Err(error).context("decoding audio"),
        };
        let spec = *audio.spec();
        let buffer =
            interleaved.get_or_insert_with(|| SampleBuffer::new(audio.capacity() as u64, spec));
        buffer.copy_interleaved_ref(audio);

        let channels = spec.channels.count().max(1);
        mono.clear();
        mono.extend(
            buffer
                .samples()
                .chunks_exact(channels)
                .map(|frame| frame.iter().sum::<f32>() / channels as f32),
        );
        sink(&mono, rate);
    }
    Ok(())
}
