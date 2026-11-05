//! Per-process audio capture via WASAPI process loopback.
//!
//! This uses the documented Windows loopback API and never opens, reads or
//! injects into the target process.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use flume::Sender;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};
use wasapi::{
    AudioClient, DeviceEnumerator, Direction, SampleType, SessionState, StreamMode, WaveFormat,
    initialize_mta,
};

/// Rate of the mono audio `capture_process` delivers.
pub const CAPTURE_RATE: usize = 48_000;
const CAPTURE_CHANNELS: usize = 2;
/// How long a target may stay silent before we synthesize silence.
const SILENCE_TICK: Duration = Duration::from_millis(100);
const LIVENESS_CHECK: Duration = Duration::from_secs(2);

/// An application with an audio session, as shown in the source picker.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioApp {
    pub pid: u32,
    pub name: String,
    pub active: bool,
}

/// Lists applications with an audio session, deduplicated by process tree.
///
/// Runs on its own thread because it needs a COM MTA and callers may be on an
/// STA UI thread.
pub fn list_audio_apps() -> Result<Vec<AudioApp>> {
    std::thread::spawn(list_audio_apps_mta)
        .join()
        .map_err(|_| anyhow!("audio session enumeration panicked"))?
}

fn list_audio_apps_mta() -> Result<Vec<AudioApp>> {
    initialize_mta().ok().context("initializing COM")?;
    let enumerator = DeviceEnumerator::new()?;
    let system = process_snapshot();

    let mut apps: HashMap<u32, AudioApp> = HashMap::new();
    for device in &enumerator.get_device_collection(&Direction::Render)? {
        let device = device?;
        let sessions = device
            .get_iaudiosessionmanager()?
            .get_audiosessionenumerator()?;
        for i in 0..sessions.get_count()? {
            let session = sessions.get_session(i)?;
            let state = session.get_state()?;
            let pid = session.get_process_id()?;
            if pid == 0 || state == SessionState::Expired {
                continue;
            }
            let Some(name) = process_name(&system, pid) else {
                continue;
            };
            let root = capture_root(&system, pid);
            let app = apps.entry(root).or_insert(AudioApp {
                pid: root,
                name,
                active: false,
            });
            app.active |= state == SessionState::Active;
        }
    }

    let mut apps: Vec<_> = apps.into_values().collect();
    apps.sort_by(|a, b| {
        b.active
            .cmp(&a.active)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(apps)
}

fn process_snapshot() -> System {
    System::new_with_specifics(RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing()))
}

fn process_name(system: &System, pid: u32) -> Option<String> {
    system
        .process(Pid::from_u32(pid))
        .map(|p| p.name().to_string_lossy().into_owned())
}

/// Walks up through ancestors with the same executable name, because loopback's
/// include-tree mode must target the root. Stopping at the executable boundary
/// keeps us from climbing into a launcher and capturing what else it started.
fn capture_root(system: &System, pid: u32) -> u32 {
    let Some(name) = process_name(system, pid) else {
        return pid;
    };
    let mut current = pid;
    while let Some(parent) = system
        .process(Pid::from_u32(current))
        .and_then(|p| p.parent())
        .map(Pid::as_u32)
    {
        if process_name(system, parent).as_deref() != Some(name.as_str()) {
            break;
        }
        current = parent;
    }
    current
}

/// Finds a running process by executable name, case-insensitively.
pub fn find_process(name: &str) -> Option<u32> {
    let system = process_snapshot();
    let pid = system
        .processes()
        .values()
        .find(|p| p.name().to_string_lossy().eq_ignore_ascii_case(name))?
        .pid()
        .as_u32();
    Some(capture_root(&system, pid))
}

/// Streams mono audio from `pid` until `stop` is set or the process exits.
pub fn capture_process(pid: u32, stop: &Arc<AtomicBool>, tx: &Sender<Vec<f32>>) -> Result<()> {
    initialize_mta().ok().context("initializing COM")?;

    let format = WaveFormat::new(
        32,
        32,
        &SampleType::Float,
        CAPTURE_RATE,
        CAPTURE_CHANNELS,
        None,
    );
    let block_align = format.get_blockalign() as usize;

    let mut client = AudioClient::new_application_loopback_client(pid, true)
        .with_context(|| format!("opening loopback for process {pid}"))?;
    client.initialize_client(
        &format,
        &Direction::Capture,
        &StreamMode::EventsShared {
            autoconvert: true,
            buffer_duration_hns: 0,
        },
    )?;
    let event = client.set_get_eventhandle()?;
    let capture = client.get_audiocaptureclient()?;
    client.start_stream()?;

    let mut bytes: VecDeque<u8> = VecDeque::new();
    let mut system = System::new();
    let mut last_audio = Instant::now();
    let mut last_liveness = Instant::now();

    while !stop.load(Ordering::Relaxed) {
        // A timeout only means the target is quiet, never a failure.
        let _ = event.wait_for_event(SILENCE_TICK.as_millis() as u32);

        while capture.get_next_packet_size()?.unwrap_or(0) > 0 {
            capture.read_from_device_to_deque(&mut bytes)?;
        }

        if bytes.len() >= block_align {
            let frames = bytes.len() / block_align;
            let mono = downmix(&mut bytes, frames);
            last_audio = Instant::now();
            if tx.send(mono).is_err() {
                break;
            }
        } else if last_audio.elapsed() >= SILENCE_TICK {
            // Process loopback delivers nothing while the target writes
            // nothing. The stream's clock times the beeps, so it must not
            // stop along with the audio.
            let samples = CAPTURE_RATE * SILENCE_TICK.as_millis() as usize / 1000;
            last_audio = Instant::now();
            if tx.send(vec![0.0; samples]).is_err() {
                break;
            }
        }

        if last_liveness.elapsed() >= LIVENESS_CHECK {
            last_liveness = Instant::now();
            let pid = Pid::from_u32(pid);
            system.refresh_processes_specifics(
                ProcessesToUpdate::Some(&[pid]),
                true,
                ProcessRefreshKind::nothing(),
            );
            if system.process(pid).is_none() {
                client.stop_stream().ok();
                return Err(anyhow!("the captured application exited"));
            }
        }
    }

    client.stop_stream().ok();
    Ok(())
}

/// Pops `frames` interleaved f32 stereo frames and averages them to mono.
fn downmix(bytes: &mut VecDeque<u8>, frames: usize) -> Vec<f32> {
    let mut mono = Vec::with_capacity(frames);
    let mut sample = [0u8; 4];
    for _ in 0..frames {
        let mut sum = 0.0;
        for _ in 0..CAPTURE_CHANNELS {
            for byte in &mut sample {
                *byte = bytes.pop_front().unwrap_or(0);
            }
            sum += f32::from_le_bytes(sample);
        }
        mono.push(sum / CAPTURE_CHANNELS as f32);
    }
    mono
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downmix_averages_channels() {
        let mut bytes = VecDeque::new();
        for sample in [1.0f32, 0.0, 0.5, 0.5] {
            bytes.extend(sample.to_le_bytes());
        }
        assert_eq!(downmix(&mut bytes, 2), vec![0.5, 0.5]);
        assert!(bytes.is_empty());
    }
}
