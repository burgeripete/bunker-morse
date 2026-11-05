//! Debug tool: drives the decoder without the UI. Nothing here plays sound
//! you can hear; `loopback-test` renders far below audibility.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use bunker_morse::bunkers::Bunkers;
use bunker_morse::capture::{self, CAPTURE_RATE};
use bunker_morse::decoder::{Block, BlockKind, Element, Lead, format_code};
use bunker_morse::detector::{DEFAULT_TONE_HZ, Threshold};
use bunker_morse::engine::{Engine, Event, Listener, Source, Update};
use bunker_morse::server;
use bunker_morse::synth;

const USAGE: &str = "\
usage: bunker-morse-cli <command>

  apps                     list applications with audio sessions
  server ADDRESS:PORT [--watch]
                           ask a server for its game time, using its query port;
                           --watch runs the clock and logs each correction
  decode FILE [--elements] [--threshold DB]
                           decode a recording (mkv, mp4, wav) as fast as
                           possible, printing blocks; --elements also prints
                           every beep with its timing; --threshold fixes the
                           beep threshold instead of following the signal
  listen NAME [--elements] [--threshold DB]
                           decode a running process, for example DayZ_x64.exe
  loopback-test            silently check capture: render one synthetic block
                           at -70 dB in this process, record it back through
                           the real capture path and decode it (about 90 s)
";

/// -70 dB: decodable after the gain is undone, far below audibility.
const SILENT_TEST_GAIN: f32 = 0.000_316;
const TEST_CODE: [u8; 10] = [0, 5, 7, 0, 7, 4, 6, 8, 7, 0];

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        print!("{USAGE}");
        return Ok(());
    }
    let command = args.remove(0);
    let elements = args.iter().any(|a| a == "--elements");
    let threshold = Threshold::default();
    if let Some(index) = args.iter().position(|a| a == "--threshold") {
        let value = args.get(index + 1).context("--threshold needs a value")?;
        threshold.set(Some(value.parse().context("bad --threshold")?));
        args.drain(index..=index + 1);
    }
    let target = args.iter().find(|a| !a.starts_with("--")).cloned();
    match command.as_str() {
        "apps" => apps(),
        "server" => {
            let address = target.context("a server address is required")?;
            if args.iter().any(|a| a == "--watch") {
                watch_clock(address);
                Ok(())
            } else {
                server_info(address)
            }
        }
        "decode" => run(
            Source::File {
                path: target.context("a file is required")?.into(),
                speed: 0.0,
            },
            threshold,
            elements,
        ),
        "listen" => run(
            Source::Process(target.context("a process name is required")?),
            threshold,
            elements,
        ),
        "loopback-test" => loopback_test(),
        other => bail!("unknown command {other}\n\n{USAGE}"),
    }
}

fn server_info(address: String) -> Result<()> {
    let info = server::query(&address)?;
    println!("{}", info.name);
    println!("map      {}", info.map);
    println!("version  {}", info.version);
    println!("players  {}/{}", info.players, info.max_players);
    match info.time {
        Some(time) => println!("time     {time}"),
        None => println!("time     not reported"),
    }
    if let Some(speed) = info.day_speed {
        println!("day      {speed}x");
    }
    if let Some(speed) = info.night_speed {
        println!("night    {speed}x");
    }
    Ok(())
}

fn watch_clock(address: String) {
    let (_watch, results) = server::ServerWatch::start(address);
    let mut clock: Option<server::GameClock> = None;
    while let Ok(result) = results.recv() {
        let now = Instant::now();
        let info = match result {
            Ok(info) => info,
            Err(error) => {
                eprintln!("warning: {error}");
                continue;
            }
        };
        let reported = info
            .time
            .map_or("--:--".to_owned(), |time| time.to_string());
        match &mut clock {
            Some(clock) => {
                let before = clock.text(now);
                let moved = clock.correct(now, &info);
                println!(
                    "server {reported}  clock {before}  moved {moved:+.0} game s  speed {:.2}x",
                    clock.speed()
                );
            }
            None => {
                clock = server::GameClock::start(now, &info);
                println!("server {reported}  clock started");
            }
        }
    }
}

fn apps() -> Result<()> {
    for app in capture::list_audio_apps()? {
        let state = if app.active { "active" } else { "idle" };
        println!("{:>7}  {state:<6}  {}", app.pid, app.name);
    }
    Ok(())
}

#[derive(Default)]
struct Report {
    bunkers: Bunkers,
    elements: bool,
    tone_hz: f32,
    peak_snr_db: f32,
    floor_db: f32,
    rejected: u32,
}

impl Report {
    fn update(&mut self, update: Update) {
        match update {
            Update::Signal(signal) => {
                self.tone_hz = signal.frame.tone_hz;
                self.floor_db = signal.frame.floor_db;
                self.peak_snr_db = self.peak_snr_db.max(signal.peak_snr_db);
            }
            Update::Element(element) if self.elements => print_element(&element),
            Update::Element(_) => {}
            Update::Rejected(tone) => {
                self.rejected += 1;
                if self.elements {
                    println!(
                        "  {:9.3}s  rejected  {} ms",
                        tone.start_ms as f32 / 1000.0,
                        tone.duration_ms()
                    );
                }
            }
            Update::Recognised(elements) => {
                if self.elements {
                    let known = elements.iter().filter(|e| e.lead == Lead::Known).count();
                    println!("  recognised from the list, {known} missed elements filled in");
                }
            }
            Update::Block(block) => self.block(&block),
        }
    }

    fn block(&mut self, block: &Block) {
        let start = block.elements.first().map_or(0, |e| e.start_ms);
        let flags = block
            .elements
            .iter()
            .filter(|e| matches!(e.lead, Lead::StrayBreak | Lead::Drowned))
            .count();
        let placed = block.code().and_then(|code| self.bunkers.record(&code));
        let placed = placed.map_or(String::new(), |slot| format!(" -> bunker {}", slot + 1));
        let known = block
            .elements
            .iter()
            .filter(|e| e.lead == Lead::Known)
            .count();
        let placed = match known {
            0 => placed,
            known => format!("{placed}, {known} from the list"),
        };
        println!(
            "{:8.1}s  {:<9} {}  ({}/50 elements, {flags} flagged){placed}",
            start as f32 / 1000.0,
            kind_label(block.kind),
            format_code(&block.digits()),
            block.elements.len(),
        );
    }

    fn summary(&self) {
        println!(
            "\ntone {:.0} Hz, peak {:.0} dB over a {:.0} dBFS floor, {} rejected beeps",
            self.tone_hz, self.peak_snr_db, self.floor_db, self.rejected
        );
        for (index, bunker) in self.bunkers.slots.iter().enumerate() {
            let code = std::array::from_fn(|i| bunker.digit(i));
            let state = if bunker.is_confirmed() {
                "confirmed"
            } else {
                "heard once"
            };
            println!(
                "bunker {}: {}  {}  ({state}, {} passes)",
                index + 1,
                format_code(&code),
                bunker.gps(),
                bunker.passes
            );
            if let Some(url) = bunker.map_url() {
                println!("          {url}");
            }
            if bunker.is_off_list() {
                println!("          not on the bunker list");
            }
        }
    }
}

fn kind_label(kind: BlockKind) -> &'static str {
    match kind {
        BlockKind::Complete => "complete",
        BlockKind::Tail => "tail",
        BlockKind::Damaged => "damaged",
    }
}

fn print_element(element: &Element) {
    let note = match element.lead {
        Lead::Start => "block start",
        Lead::Slot => "",
        Lead::DigitBreak => "digit break",
        Lead::StrayBreak => "STRAY BREAK",
        Lead::Drowned => "DROWNED",
        Lead::Known => "from list",
    };
    println!(
        "  {:9.3}s  {}  on {:4} ms  gap {:5} ms  {note}",
        element.start_ms as f32 / 1000.0,
        element.symbol.as_char(),
        element.duration_ms,
        element.gap_ms,
    );
}

fn run(source: Source, threshold: Threshold, elements: bool) -> Result<()> {
    let (engine, events) = Engine::start(source, threshold);
    let mut report = Report {
        elements,
        ..Report::default()
    };
    while let Ok(event) = events.recv() {
        match event {
            Event::Waiting(name) => println!("waiting for {name}"),
            Event::Listening(name) => println!("listening to {name}"),
            Event::Update(update) => report.update(update),
            Event::Warning(message) => eprintln!("warning: {message}"),
            Event::Stopped => break,
        }
    }
    drop(engine);
    report.summary();
    Ok(())
}

fn loopback_test() -> Result<()> {
    let mut keys = synth::block_keys(&TEST_CODE);
    let mut lead_in = keys[keys.len() - 1];
    lead_in.on_ms = 0;
    lead_in.off_ms = 6_500;
    keys.insert(0, lead_in);
    let samples = synth::render(&keys, CAPTURE_RATE, DEFAULT_TONE_HZ, 0.5);

    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = flume::unbounded();
    let worker_stop = stop.clone();
    let pid = std::process::id();
    let capture = std::thread::spawn(move || capture::capture_process(pid, &worker_stop, &tx));
    std::thread::sleep(Duration::from_millis(300));

    let render = std::thread::spawn(move || render_quietly(&samples));
    let mut listener = Listener::new(CAPTURE_RATE, Threshold::default());
    let mut report = Report::default();
    let mut emit = |update| report.update(update);
    while !render.is_finished() {
        if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(100)) {
            let chunk: Vec<f32> = chunk.iter().map(|s| s / SILENT_TEST_GAIN).collect();
            listener.feed(&chunk, &mut emit);
        }
    }
    render
        .join()
        .map_err(|_| anyhow!("render thread panicked"))??;
    stop.store(true, Ordering::Relaxed);
    capture
        .join()
        .map_err(|_| anyhow!("capture thread panicked"))??;
    for chunk in rx.drain() {
        let chunk: Vec<f32> = chunk.iter().map(|s| s / SILENT_TEST_GAIN).collect();
        listener.feed(&chunk, &mut emit);
    }
    listener.finish(&mut emit);
    report.summary();

    let expected = format_code(&TEST_CODE.map(Some));
    let heard = report.bunkers.slots.first().map(|bunker| {
        let code = std::array::from_fn(|i| bunker.digit(i));
        format_code(&code)
    });
    if heard.as_deref() != Some(expected.as_str()) {
        bail!("expected {expected}, heard {heard:?}");
    }
    println!("capture path decoded {expected} correctly");
    Ok(())
}

fn render_quietly(samples: &[f32]) -> Result<()> {
    use wasapi::{DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat, initialize_mta};

    initialize_mta().ok().context("initializing COM")?;
    let device = DeviceEnumerator::new()?.get_default_device(&Direction::Render)?;
    let mut client = device.get_iaudioclient()?;
    let format = WaveFormat::new(32, 32, &SampleType::Float, CAPTURE_RATE, 1, None);
    client.initialize_client(
        &format,
        &Direction::Render,
        &StreamMode::EventsShared {
            autoconvert: true,
            buffer_duration_hns: 0,
        },
    )?;
    let event = client.set_get_eventhandle()?;
    let renderer = client.get_audiorenderclient()?;
    client.start_stream()?;

    let mut remaining = samples;
    while !remaining.is_empty() {
        let space = client.get_available_space_in_frames()? as usize;
        let (now, later) = remaining.split_at(space.min(remaining.len()));
        if !now.is_empty() {
            let bytes: Vec<u8> = now
                .iter()
                .flat_map(|s| (s * SILENT_TEST_GAIN).to_le_bytes())
                .collect();
            renderer.write_to_device(now.len(), &bytes, None)?;
        }
        remaining = later;
        event.wait_for_event(1000)?;
    }
    std::thread::sleep(Duration::from_millis(300));
    client.stop_stream()?;
    Ok(())
}
