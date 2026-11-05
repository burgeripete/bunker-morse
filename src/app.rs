//! The main window: the three bunkers, the live signal and the debug panel.

use std::collections::VecDeque;
use std::path::PathBuf;

use bunker_morse::bunkers::{BUNKER_COUNT, Bunker, Bunkers, DigitState};
use bunker_morse::capture::{self, AudioApp};
use bunker_morse::decoder::{
    Block, BlockKind, DIGITS_PER_BLOCK, ELEMENTS_PER_BLOCK, ELEMENTS_PER_DIGIT, Element, Lead,
    Symbol, format_code, match_digit,
};
use bunker_morse::detector::{self, DEFAULT_TONE_HZ, Threshold};
use bunker_morse::engine::{Engine, Event, Signal, Source, Update};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::select::{Select, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, IndexPath, Sizable as _};
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, Window, div, prelude::FluentBuilder as _, px, relative,
};

use crate::scope::{self, Reading, SCOPE_READINGS};

const DEFAULT_PROCESS: &str = "DayZ_x64.exe";
const MONOSPACE: &str = "Consolas";
const MAX_LOG_ROWS: usize = 8;
const RECENT_ELEMENTS: usize = 8;
/// Meters span this range of dBFS.
const METER_FLOOR_DB: f32 = -80.0;
const TONE_METER_DB: f32 = 60.0;
/// The slider's range, inside the plot's 0 to 60 dB scale.
const MIN_THRESHOLD_DB: f32 = 10.0;
const MAX_THRESHOLD_DB: f32 = 55.0;
const INITIAL_THRESHOLD_DB: f32 = 30.0;
/// Reads this recording instead of capturing a process and starts at launch,
/// to exercise the whole UI with no game running. Nothing is played.
const TEST_FILE_VARIABLE: &str = "BUNKER_MORSE_TEST_FILE";
/// Multiple of real time for the test recording. Defaults to 1.
const TEST_SPEED_VARIABLE: &str = "BUNKER_MORSE_TEST_SPEED";

#[derive(Clone, PartialEq)]
struct Choice(String);

impl SelectItem for Choice {
    type Value = String;

    fn title(&self) -> SharedString {
        self.0.clone().into()
    }

    fn value(&self) -> &Self::Value {
        &self.0
    }
}

type ChoiceState = Entity<SelectState<Vec<Choice>>>;

fn process_choices(apps: &[AudioApp], selected: &str) -> Vec<Choice> {
    let mut names = vec![selected, DEFAULT_PROCESS];
    names.extend(apps.iter().map(|app| app.name.as_str()));

    let mut seen: Vec<String> = Vec::new();
    names
        .into_iter()
        .filter(|name| {
            let key = name.to_lowercase();
            let fresh = !seen.contains(&key);
            seen.push(key);
            fresh
        })
        .map(|name| Choice(name.to_owned()))
        .collect()
}

#[derive(Clone, PartialEq)]
enum RunState {
    Idle,
    Waiting(String),
    Listening(String),
}

struct LogRow {
    time_ms: u64,
    kind: BlockKind,
    digits: String,
    elements: usize,
    flagged: usize,
}

pub struct BunkerMorse {
    process: ChoiceState,
    process_name: String,
    engine: Option<Engine>,
    state: RunState,
    notice: Option<SharedString>,
    bunkers: Bunkers,
    readings: VecDeque<Reading>,
    latest: Option<Signal>,
    /// Elements of the block being received.
    received: Vec<Element>,
    recent: VecDeque<Element>,
    log: VecDeque<LogRow>,
    rejected: u32,
    show_debug: bool,
    threshold: Threshold,
    threshold_slider: Entity<SliderState>,
    _pump: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl BunkerMorse {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        window.on_window_should_close(cx, |_, cx| {
            cx.quit();
            true
        });

        let process_name = DEFAULT_PROCESS.to_owned();
        let selected = Some(IndexPath::default().row(0));
        let process = cx
            .new(|cx| SelectState::new(process_choices(&[], &process_name), selected, window, cx));
        let threshold_slider = cx.new(|_| {
            SliderState::new()
                .min(MIN_THRESHOLD_DB)
                .max(MAX_THRESHOLD_DB)
                .step(1.0)
                .default_value(INITIAL_THRESHOLD_DB)
        });
        let threshold = Threshold::default();
        let slider_target = threshold.clone();
        let subscriptions = vec![
            cx.subscribe_in(
                &process,
                window,
                |this, _, event: &SelectEvent<Vec<Choice>>, _, cx| {
                    if let SelectEvent::Confirm(Some(value)) = event {
                        this.process_name = value.clone();
                        cx.notify();
                    }
                },
            ),
            cx.subscribe(&threshold_slider, move |_, _, event: &SliderEvent, cx| {
                if let SliderEvent::Change(value) = event
                    && slider_target.manual_db().is_some()
                {
                    slider_target.set(Some(value.end()));
                    cx.notify();
                }
            }),
        ];

        let mut app = Self {
            process,
            process_name,
            engine: None,
            state: RunState::Idle,
            notice: None,
            bunkers: Bunkers::default(),
            readings: VecDeque::with_capacity(SCOPE_READINGS),
            latest: None,
            received: Vec::new(),
            recent: VecDeque::with_capacity(RECENT_ELEMENTS),
            log: VecDeque::with_capacity(MAX_LOG_ROWS),
            rejected: 0,
            show_debug: true,
            threshold,
            threshold_slider,
            _pump: None,
            _subscriptions: subscriptions,
        };
        app.refresh_apps(window, cx);
        if std::env::var_os(TEST_FILE_VARIABLE).is_some() {
            app.start(cx);
        }
        app
    }

    /// Enumerates audio apps off the UI thread, then updates the picker.
    fn refresh_apps(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let list = cx.background_spawn(async { capture::list_audio_apps() });
        cx.spawn_in(window, async move |this, cx| {
            let apps = list.await.unwrap_or_else(|error| {
                log::warn!("listing audio applications: {error:#}");
                Vec::new()
            });
            this.update_in(cx, |this, window, cx| {
                let selected = this.process_name.clone();
                let items = process_choices(&apps, &selected);
                this.process.update(cx, |state, cx| {
                    state.set_items(items, window, cx);
                    state.set_selected_value(&selected, window, cx);
                });
            })
            .ok();
        })
        .detach();
    }

    fn source(&self) -> Source {
        match std::env::var_os(TEST_FILE_VARIABLE) {
            Some(path) => Source::File {
                path: PathBuf::from(path),
                speed: std::env::var(TEST_SPEED_VARIABLE)
                    .ok()
                    .and_then(|speed| speed.parse().ok())
                    .unwrap_or(1.0),
            },
            None => Source::Process(self.process_name.clone()),
        }
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        self.notice = None;
        self.received.clear();
        self.readings.clear();
        self.latest = None;
        let (engine, events) = Engine::start(self.source(), self.threshold.clone());
        self.engine = Some(engine);
        self._pump = Some(cx.spawn(async move |this, cx| {
            while let Ok(first) = events.recv_async().await {
                // Handled in batches so a redraw covers everything waiting.
                let mut batch = vec![first];
                batch.extend(events.try_iter());
                let updated = this.update(cx, |this, cx| {
                    batch.into_iter().for_each(|event| this.on_event(event));
                    cx.notify();
                });
                if updated.is_err() {
                    break;
                }
            }
        }));
        cx.notify();
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some(engine) = &self.engine {
            engine.stop();
        }
        cx.notify();
    }

    /// Forgets the decoded codes, for a server restart or a new server.
    fn reset(&mut self, cx: &mut Context<Self>) {
        self.bunkers = Bunkers::default();
        self.received.clear();
        self.recent.clear();
        self.log.clear();
        self.rejected = 0;
        self.notice = None;
        cx.notify();
    }

    fn on_event(&mut self, event: Event) {
        match event {
            Event::Waiting(name) => self.state = RunState::Waiting(name),
            Event::Listening(name) => {
                self.notice = None;
                self.state = RunState::Listening(name);
            }
            Event::Update(update) => self.on_update(update),
            Event::Warning(message) => self.notice = Some(message.into()),
            Event::Stopped => {
                self.engine = None;
                self.state = RunState::Idle;
            }
        }
    }

    fn on_update(&mut self, update: Update) {
        match update {
            Update::Signal(signal) => {
                if self.readings.len() == SCOPE_READINGS {
                    self.readings.pop_front();
                }
                self.readings.push_back(Reading {
                    snr_db: signal.peak_snr_db,
                    threshold_db: signal.frame.threshold_db,
                    on: signal.on,
                });
                self.latest = Some(signal);
            }
            Update::Element(element) => {
                self.received.push(element);
                if self.recent.len() == RECENT_ELEMENTS {
                    self.recent.pop_front();
                }
                self.recent.push_back(element);
            }
            Update::Rejected(_) => self.rejected += 1,
            Update::Block(block) => self.on_block(block),
        }
    }

    fn on_block(&mut self, block: Block) {
        self.received.clear();
        if let Some(code) = block.code() {
            self.bunkers.record(&code);
        }
        if self.log.len() == MAX_LOG_ROWS {
            self.log.pop_back();
        }
        self.log.push_front(LogRow {
            time_ms: block.elements.first().map_or(0, |e| e.start_ms),
            kind: block.kind,
            digits: format_code(&block.digits()),
            elements: block.elements.len(),
            flagged: flagged(&block.elements),
        });
        if self.bunkers.unplaced > 0 {
            self.notice = Some(
                "Heard a code that fits no bunker. If the server restarted, press Reset.".into(),
            );
        }
    }

    /// Switches between following the beeps and a fixed threshold. A fixed one
    /// starts where the line already is, so nothing jumps.
    fn set_auto_threshold(&mut self, auto: bool, window: &mut Window, cx: &mut Context<Self>) {
        if auto {
            self.threshold.set(None);
        } else {
            let current = self
                .latest
                .as_ref()
                .map_or(INITIAL_THRESHOLD_DB, |s| s.frame.threshold_db.round())
                .clamp(MIN_THRESHOLD_DB, MAX_THRESHOLD_DB);
            self.threshold.set(Some(current));
            self.threshold_slider
                .update(cx, |slider, cx| slider.set_value(current, window, cx));
        }
        cx.notify();
    }

    fn render_threshold(&self, cx: &Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let manual = self.threshold.manual_db();
        let shown = manual.or(self.latest.as_ref().map(|s| s.frame.threshold_db));
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .w(px(84.))
                    .flex_none()
                    .text_xs()
                    .text_color(muted)
                    .child("Threshold"),
            )
            .child(
                div()
                    .flex_1()
                    .child(Slider::new(&self.threshold_slider).disabled(manual.is_none())),
            )
            .child(
                div()
                    .w(px(48.))
                    .flex_none()
                    .text_xs()
                    .font_family(MONOSPACE)
                    .text_right()
                    .child(shown.map_or("-".to_owned(), |db| format!("{db:.0} dB"))),
            )
            .child(
                Switch::new("auto-threshold")
                    .label("Auto")
                    .checked(manual.is_none())
                    .on_click(cx.listener(|this, checked: &bool, window, cx| {
                        this.set_auto_threshold(*checked, window, cx);
                    })),
            )
    }

    fn status_text(&self) -> SharedString {
        match &self.state {
            RunState::Idle => "Stopped".into(),
            RunState::Waiting(name) => format!("Waiting for {name} to start").into(),
            RunState::Listening(name) => format!("Listening to {name}").into(),
        }
    }

    fn scope_colors(&self, cx: &Context<Self>) -> scope::Colors {
        let theme = cx.theme();
        scope::Colors {
            background: theme.secondary,
            grid: theme.border,
            idle: theme.muted_foreground,
            active: theme.success,
            threshold: theme.warning,
        }
    }

    fn render_header(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let running = self.engine.is_some();
        let lamp = if self.latest.as_ref().is_some_and(|s| s.on) {
            cx.theme().success
        } else {
            cx.theme().secondary
        };
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;

        div()
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .text_size(px(20.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Bunker Morse"),
            )
            .child(
                div()
                    .id("tone-lamp")
                    .size(px(14.))
                    .rounded_full()
                    .border_1()
                    .border_color(border)
                    .bg(lamp),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .text_color(muted)
                    .child(self.status_text()),
            )
            .child(
                Switch::new("debug")
                    .label("Debug")
                    .checked(self.show_debug)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.show_debug = *checked;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("reset")
                    .label("Reset")
                    .on_click(cx.listener(|this, _, _, cx| this.reset(cx))),
            )
            .child(if running {
                Button::new("stop")
                    .danger()
                    .label("Stop")
                    .on_click(cx.listener(|this, _, _, cx| this.stop(cx)))
            } else {
                Button::new("start")
                    .primary()
                    .label("Start")
                    .on_click(cx.listener(|this, _, _, cx| this.start(cx)))
            })
    }

    fn render_source(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let running = self.engine.is_some();
        let muted = cx.theme().muted_foreground;
        div()
            .flex()
            .items_center()
            .gap_3()
            .child(div().text_sm().text_color(muted).child("Listen to"))
            .child(
                div().w(px(280.)).child(
                    Select::new(&self.process)
                        .disabled(running)
                        .into_any_element(),
                ),
            )
            .child(
                Button::new("refresh")
                    .label("Refresh")
                    .disabled(running)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.refresh_apps(window, cx);
                    })),
            )
    }

    fn render_bunkers(&self, cx: &Context<Self>) -> impl IntoElement {
        let palette = DigitPalette {
            muted: cx.theme().muted_foreground,
            heard: cx.theme().foreground,
            confirmed: cx.theme().success,
            disputed: cx.theme().warning,
            hover: cx.theme().secondary,
        };
        let border = cx.theme().border;
        let cards = (0..BUNKER_COUNT)
            .map(|index| bunker_card(index, self.bunkers.slots.get(index), palette, border));
        div().flex().gap_3().children(cards)
    }

    fn render_receiving(&self, cx: &Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let aligned = self.latest.as_ref().is_some_and(|s| s.aligned);
        let symbols = group_symbols(&self.received);
        let headline: SharedString = if self.received.is_empty() {
            if aligned {
                "Waiting for the next block".into()
            } else {
                "Waiting for a pause to line up on a block".into()
            }
        } else if aligned {
            format!(
                "Receiving {}/{ELEMENTS_PER_BLOCK}: {}",
                self.received.len(),
                digits_so_far(&self.received)
            )
            .into()
        } else {
            format!(
                "Joined mid-block, {} beeps heard. Digits start after the next pause.",
                self.received.len()
            )
            .into()
        };
        div()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(div().text_sm().child(headline))
            .child(
                div()
                    .h(px(18.))
                    .font_family(MONOSPACE)
                    .text_sm()
                    .text_color(muted)
                    .overflow_hidden()
                    .child(symbols),
            )
    }

    fn render_signal(&self, cx: &Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let (level, tone) = self.latest.as_ref().map_or((0.0, 0.0), |signal| {
            (
                (signal.frame.level_db - METER_FLOOR_DB) / -METER_FLOOR_DB,
                signal.peak_snr_db / TONE_METER_DB,
            )
        });
        let meter = move |label: &'static str, value: f32, color: Hsla, track: Hsla| {
            div()
                .flex()
                .flex_1()
                .items_center()
                .gap_2()
                .child(div().w(px(84.)).text_xs().text_color(muted).child(label))
                .child(
                    div().flex_1().h(px(8.)).rounded_full().bg(track).child(
                        div()
                            .h_full()
                            .rounded_full()
                            .bg(color)
                            .w(relative(value.clamp(0.0, 1.0))),
                    ),
                )
        };
        let (primary, success, track) =
            (cx.theme().primary, cx.theme().success, cx.theme().secondary);
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .gap_4()
                    .child(meter("Audio level", level, primary, track))
                    .child(meter("Beep tone", tone, success, track)),
            )
            .child(self.render_threshold(cx))
            .child(scope::scope(
                self.readings.iter().copied().collect(),
                self.scope_colors(cx),
            ))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .text_xs()
                    .text_color(muted)
                    .child("30 s ago")
                    .child("tone level, threshold line, keyed beeps along the bottom")
                    .child("now"),
            )
    }

    fn render_debug(&self, cx: &Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        let border = cx.theme().border;
        let colors = self.scope_colors(cx);

        let frame = self.latest.as_ref().map(|signal| signal.frame);
        let tone_hz = frame.map_or(DEFAULT_TONE_HZ, |f| f.tone_hz);
        let reading = |text: String| -> AnyElement { div().child(text).into_any_element() };
        let stats = [
            ("Tone", format!("{tone_hz:.0} Hz")),
            (
                "Tone level",
                frame.map_or("-".into(), |f| format!("{:.1} dB", f.snr_db)),
            ),
            (
                "Threshold",
                frame.map_or("-".into(), |f| format!("{:.1} dB", f.threshold_db)),
            ),
            (
                "Noise floor",
                frame.map_or("-".into(), |f| format!("{:.1} dBFS", f.floor_db)),
            ),
            (
                "Audio level",
                frame.map_or("-".into(), |f| format!("{:.1} dBFS", f.level_db)),
            ),
            (
                "Stream time",
                frame.map_or("-".into(), |f| format_time(f.time_ms)),
            ),
            ("Rejected", self.rejected.to_string()),
        ];
        let stat_rows = stats.into_iter().map(|(label, value)| {
            div()
                .flex()
                .justify_between()
                .child(div().text_color(muted).child(label))
                .child(reading(value))
        });

        let bins = self
            .latest
            .as_ref()
            .map_or(Vec::new(), |s| s.spectrum.clone());
        let locked = (0..detector::bank_len())
            .min_by_key(|bin| (detector::bank_hz(*bin) - tone_hz).abs() as u32);

        let elements = self.recent.iter().rev().map(|element| {
            let warn = matches!(element.lead, Lead::StrayBreak | Lead::Drowned);
            div()
                .flex()
                .gap_3()
                .when(warn, |row| row.text_color(danger))
                .child(div().w(px(70.)).child(format_time(element.start_ms)))
                .child(div().w(px(14.)).child(element.symbol.as_char().to_string()))
                .child(
                    div()
                        .w(px(56.))
                        .child(format!("{} ms", element.duration_ms)),
                )
                .child(div().w(px(64.)).child(format!("{} ms", element.gap_ms)))
                .child(div().child(lead_label(element.lead)))
        });

        let blocks = self.log.iter().map(|row| {
            let warn = row.kind == BlockKind::Damaged || row.flagged > 0;
            div()
                .flex()
                .gap_3()
                .when(warn, |line| line.text_color(danger))
                .child(div().w(px(64.)).child(format_time(row.time_ms)))
                .child(div().w(px(84.)).child(row.digits.clone()))
                .child(div().child(block_note(row)))
        });

        let heading = move |text: &'static str| {
            div()
                .text_xs()
                .text_color(muted)
                .font_family(".SystemUIFont")
                .child(text)
        };

        div()
            .flex()
            .gap_4()
            .pt_3()
            .border_t_1()
            .border_color(border)
            .font_family(MONOSPACE)
            .text_xs()
            .child(
                div()
                    .w(px(210.))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(stat_rows)
                    .child(heading("Spectrum, 600 to 1500 Hz"))
                    .child(scope::spectrum(bins, locked, colors)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(heading(
                        "Latest beeps: time, symbol, length, silence before",
                    ))
                    .children(elements),
            )
            .child(
                div()
                    .w(px(310.))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(heading("Blocks"))
                    .children(blocks),
            )
    }
}

fn flagged(elements: &[Element]) -> usize {
    elements
        .iter()
        .filter(|e| matches!(e.lead, Lead::StrayBreak | Lead::Drowned))
        .count()
}

fn lead_label(lead: Lead) -> &'static str {
    match lead {
        Lead::Start => "block start",
        Lead::Slot => "",
        Lead::DigitBreak => "digit break",
        Lead::StrayBreak => "stray break",
        Lead::Drowned => "drowned",
    }
}

fn block_note(row: &LogRow) -> String {
    let kind = match row.kind {
        BlockKind::Complete => "ok",
        BlockKind::Tail => "tail",
        BlockKind::Damaged => "damaged",
    };
    let count = format!("{kind} {}/{ELEMENTS_PER_BLOCK}", row.elements);
    match row.flagged {
        0 => count,
        flagged => format!("{count}, {flagged} flagged"),
    }
}

fn format_time(ms: u64) -> String {
    format!("{}:{:06.3}", ms / 60_000, (ms % 60_000) as f32 / 1000.0)
}

/// `----- ..... --.` for the display of a block in progress.
fn group_symbols(elements: &[Element]) -> String {
    let symbols: Vec<char> = elements.iter().map(|e| e.symbol.as_char()).collect();
    let groups: Vec<String> = symbols
        .chunks(ELEMENTS_PER_DIGIT)
        .map(|group| group.iter().collect())
        .collect();
    groups.join(" ")
}

fn digits_so_far(elements: &[Element]) -> String {
    let symbols: Vec<Symbol> = elements.iter().map(|e| e.symbol).collect();
    let mut digits: Vec<char> = symbols
        .as_chunks::<ELEMENTS_PER_DIGIT>()
        .0
        .iter()
        .map(|group| match_digit(group).map_or('?', |d| char::from(b'0' + d)))
        .collect();
    digits.resize(DIGITS_PER_BLOCK, '_');
    digits.insert(6, ' ');
    digits.into_iter().collect()
}

#[derive(Clone, Copy)]
struct DigitPalette {
    muted: Hsla,
    heard: Hsla,
    confirmed: Hsla,
    disputed: Hsla,
    hover: Hsla,
}

fn digit_run(
    bunker: Option<&Bunker>,
    range: std::ops::Range<usize>,
    palette: DigitPalette,
) -> impl IntoElement {
    let digits = range.map(move |index| {
        let (text, color) = match bunker.map(|b| (b.digit(index), b.state(index))) {
            Some((Some(digit), DigitState::Confirmed)) => (digit.to_string(), palette.confirmed),
            Some((Some(digit), DigitState::Disputed)) => (digit.to_string(), palette.disputed),
            Some((Some(digit), _)) => (digit.to_string(), palette.heard),
            _ => ("-".to_owned(), palette.muted),
        };
        div().text_color(color).child(text)
    });
    div()
        .flex()
        .font_family(MONOSPACE)
        .text_size(px(30.))
        .line_height(px(32.))
        .font_weight(FontWeight::SEMIBOLD)
        .children(digits.collect::<Vec<_>>())
}

fn bunker_card(
    index: usize,
    bunker: Option<&Bunker>,
    palette: DigitPalette,
    border: Hsla,
) -> impl IntoElement {
    let tag = match bunker {
        None => Tag::secondary().small().child("Not heard yet"),
        Some(bunker) if bunker.is_confirmed() => Tag::success()
            .small()
            .child(format!("Confirmed, {} passes", bunker.passes)),
        Some(_) => Tag::warning().small().child("Heard once"),
    };
    let label = |text: &'static str| div().text_xs().text_color(palette.muted).child(text);
    let map_url = bunker.and_then(Bunker::map_url);
    let has_map = map_url.is_some();
    let coordinates = div()
        .id(("coordinates", index))
        .flex()
        .flex_col()
        .gap_1()
        .p_1()
        .mx(px(-4.))
        .rounded_sm()
        .child(
            div()
                .flex()
                .justify_between()
                .child(label("Coordinates"))
                .when(has_map, |row| {
                    row.child(
                        div()
                            .text_xs()
                            .underline()
                            .text_color(palette.heard)
                            .child("Open in iZurvive"),
                    )
                }),
        )
        .child(digit_run(bunker, 0..6, palette))
        .child(
            div()
                .font_family(MONOSPACE)
                .text_size(px(16.))
                .text_color(palette.muted)
                .child(bunker.map_or_else(|| Bunker::default().gps(), Bunker::gps)),
        )
        .when_some(map_url, |area, url| {
            area.cursor_pointer()
                .hover(move |style| style.bg(palette.hover))
                .on_click(move |_, _, cx| cx.open_url(&url))
        });
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_1()
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(border)
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(format!("Bunker {}", index + 1)),
                )
                .child(tag),
        )
        .child(coordinates)
        .child(label("Keypad code"))
        .child(digit_run(bunker, 6..10, palette))
}

impl Render for BunkerMorse {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let background = cx.theme().background;
        let foreground = cx.theme().foreground;
        let danger = cx.theme().danger;
        let notice = self.notice.clone();

        div()
            .id("root")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_4()
            .p_4()
            .bg(background)
            .text_color(foreground)
            .child(self.render_header(cx))
            .child(self.render_source(cx))
            .when_some(notice, |root, notice| {
                root.child(div().text_sm().text_color(danger).child(notice))
            })
            .child(self.render_bunkers(cx))
            .child(self.render_receiving(cx))
            .child(self.render_signal(cx))
            .when(self.show_debug, |root| root.child(self.render_debug(cx)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn element(symbol: Symbol) -> Element {
        Element {
            symbol,
            start_ms: 0,
            duration_ms: 0,
            gap_ms: 0,
            lead: Lead::Slot,
        }
    }

    #[test]
    fn progress_groups_symbols_into_digits() {
        let elements: Vec<_> = [Symbol::Dash; 6].map(element).to_vec();
        assert_eq!(group_symbols(&elements), "----- -");
        assert_eq!(digits_so_far(&elements), "0_____ ____");
    }

    #[test]
    fn process_choices_keep_the_selection_first_and_drop_duplicates() {
        let app = |name: &str| AudioApp {
            pid: 1,
            name: name.into(),
            active: true,
        };
        let choices = process_choices(&[app("DAYZ_X64.EXE"), app("spotify.exe")], "DayZ_x64.exe");
        let values: Vec<_> = choices.iter().map(|c| c.0.as_str()).collect();
        assert_eq!(values, ["DayZ_x64.exe", "spotify.exe"]);
    }
}
