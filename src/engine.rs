//! Connects an audio source to the decoder on a background thread.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use flume::{Receiver, RecvTimeoutError, Sender};

use crate::capture::{self, CAPTURE_RATE};
use crate::detector::Threshold;
pub use crate::listener::{Listener, SIGNAL_INTERVAL_MS, Signal, Update};
use crate::media;

const RETRY_DELAY: Duration = Duration::from_secs(2);
const POLL_DELAY: Duration = Duration::from_millis(200);

#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    /// Captures a running game, waiting for it to start if necessary.
    Process(String),
    /// Reads a recording silently. `speed` is a multiple of real time, and
    /// zero means as fast as possible.
    File { path: PathBuf, speed: f32 },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// The named process is not running yet.
    Waiting(String),
    Listening(String),
    Update(Update),
    Warning(String),
    Stopped,
}

pub struct Engine {
    stop: Arc<AtomicBool>,
}

impl Engine {
    pub fn start(source: Source, threshold: Threshold) -> (Self, Receiver<Event>) {
        let (events, receiver) = flume::unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        std::thread::spawn(move || {
            let result = match source {
                Source::Process(name) => {
                    listen_to_process(&name, &threshold, &worker_stop, &events)
                }
                Source::File { path, speed } => {
                    read_file(path, speed, &threshold, &worker_stop, &events)
                }
            };
            if let Err(error) = result {
                events.send(Event::Warning(format!("{error:#}"))).ok();
            }
            events.send(Event::Stopped).ok();
        });
        (Self { stop }, receiver)
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop();
    }
}

fn listen_to_process(
    name: &str,
    threshold: &Threshold,
    stop: &Arc<AtomicBool>,
    events: &Sender<Event>,
) -> Result<()> {
    let mut waiting = false;
    while !stop.load(Ordering::Relaxed) {
        let Some(pid) = capture::find_process(name) else {
            if !waiting {
                events.send(Event::Waiting(name.to_owned())).ok();
                waiting = true;
            }
            std::thread::sleep(POLL_DELAY);
            continue;
        };
        waiting = false;
        events.send(Event::Listening(name.to_owned())).ok();
        if let Err(error) = listen_to_pid(pid, threshold, stop, events) {
            log::warn!("capture ended: {error:#}");
            events.send(Event::Warning(format!("{error:#}"))).ok();
            std::thread::sleep(RETRY_DELAY);
        }
    }
    Ok(())
}

/// Runs one capture session. Each starts its own stream clock, so a block cut
/// off by a game restart is reported rather than carried across the gap.
fn listen_to_pid(
    pid: u32,
    threshold: &Threshold,
    stop: &Arc<AtomicBool>,
    events: &Sender<Event>,
) -> Result<()> {
    let (tx, rx) = flume::unbounded();
    let worker_stop = stop.clone();
    let worker = std::thread::spawn(move || capture::capture_process(pid, &worker_stop, &tx));

    let mut listener = Listener::new(CAPTURE_RATE, threshold.clone());
    let mut emit = |update| {
        events.send(Event::Update(update)).ok();
    };
    loop {
        match rx.recv_timeout(POLL_DELAY) {
            Ok(samples) => listener.feed(&samples, &mut emit),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    listener.finish(&mut emit);
    worker
        .join()
        .map_err(|_| anyhow!("the capture thread panicked"))?
}

fn read_file(
    path: PathBuf,
    speed: f32,
    threshold: &Threshold,
    stop: &Arc<AtomicBool>,
    events: &Sender<Event>,
) -> Result<()> {
    events
        .send(Event::Listening(path.display().to_string()))
        .ok();
    let mut listener = None;
    let mut emit = |update| {
        events.send(Event::Update(update)).ok();
    };
    let started = Instant::now();
    let mut samples_read = 0usize;
    media::read_audio(&path, stop, |samples, rate| {
        listener
            .get_or_insert_with(|| Listener::new(rate, threshold.clone()))
            .feed(samples, &mut emit);
        samples_read += samples.len();
        if speed > 0.0 {
            let due = Duration::from_secs_f32(samples_read as f32 / rate as f32 / speed);
            if let Some(wait) = due.checked_sub(started.elapsed()) {
                std::thread::sleep(wait);
            }
        }
    })?;
    if let Some(listener) = &mut listener {
        listener.finish(&mut emit);
    }
    Ok(())
}
