import { Session, loadDecoder } from "./decoder.js";

const BUNKER_COUNT = 3;
const SCOPE_READINGS = 600;
const FULL_SCALE_DB = 60;
const GRID_STEP_DB = 20;
const KEY_STRIP_HEIGHT = 8;
const METER_FLOOR_DB = -80;
const RECENT_BEEPS = 8;
const LOG_ROWS = 8;
const ELEMENTS_PER_DIGIT = 5;
const ELEMENTS_PER_BLOCK = 50;
/// Below this the shared audio is digital silence, not a quiet game.
const SILENCE_DB = -100;
/// How long silence lasts before the page says something, in stream time.
const SILENCE_HINT_MS = 5000;
const FILE_RATE = 48000;
const FILE_CHUNK_SECONDS = 5;

const element = (id) => document.getElementById(id);
const ui = {
  start: element("start"),
  reset: element("reset"),
  status: element("status"),
  notice: element("notice"),
  lamp: element("lamp"),
  mastLight: element("mast-light"),
  help: element("help"),
  bunkers: element("bunkers"),
  receiving: element("receiving"),
  symbols: element("symbols"),
  level: element("level"),
  tone: element("tone"),
  threshold: element("threshold"),
  thresholdValue: element("threshold-value"),
  thresholdAuto: element("threshold-auto"),
  scope: element("scope"),
  spectrum: element("spectrum"),
  debug: element("debug"),
  debugToggle: element("debug-toggle"),
  stats: element("stats"),
  beeps: element("beeps"),
  blocks: element("blocks"),
  file: element("file"),
};

const style = getComputedStyle(document.documentElement);
const color = (name) => style.getPropertyValue(name).trim();
const colors = {
  background: color("--surface"),
  grid: color("--border"),
  idle: color("--muted"),
  active: color("--accent"),
  threshold: color("--warning"),
};

const wasm = await loadDecoder(new URL("bunker_morse_web.wasm", import.meta.url));

const state = {
  session: null,
  capture: null,
  readingFile: false,
  bunkers: { slots: [], unplaced: 0 },
  readings: [],
  latest: null,
  received: [],
  recent: [],
  log: [],
  rejected: 0,
  silentSince: null,
  drawPending: false,
};

// Blocks and repeats build up over minutes, so a session outlives Stop and
// Start, and only Reset or a new sample rate replaces it.
function sessionFor(sampleRate) {
  if (state.session?.sampleRate !== sampleRate) {
    state.session?.free();
    state.session = new Session(wasm, sampleRate);
    applyThreshold();
  }
  return state.session;
}

function applyThreshold() {
  const manual = ui.thresholdAuto.checked ? null : Number(ui.threshold.value);
  state.session?.setThreshold(manual);
}

function clearResults() {
  state.bunkers = { slots: [], unplaced: 0 };
  state.received = [];
  state.recent = [];
  state.log = [];
  state.rejected = 0;
  showNotice(null);
  renderBunkers();
}

function showNotice(message) {
  ui.notice.textContent = message ?? "";
  ui.notice.hidden = !message;
}

function formatTime(ms) {
  const minutes = Math.floor(ms / 60000);
  const seconds = ((ms % 60000) / 1000).toFixed(3).padStart(6, "0");
  return `${minutes}:${seconds}`;
}

function formatGroups(symbols) {
  const groups = [];
  for (let i = 0; i < symbols.length; i += ELEMENTS_PER_DIGIT) {
    groups.push(symbols.slice(i, i + ELEMENTS_PER_DIGIT).join(""));
  }
  return groups.join(" ");
}

const DIGIT_PATTERNS = [
  "-----", ".----", "..---", "...--", "....-",
  ".....", "-....", "--...", "---..", "----.",
];

function digitsSoFar(symbols) {
  let text = "";
  for (let digit = 0; digit < 10; digit++) {
    const group = symbols.slice(digit * 5, digit * 5 + 5).join("");
    if (digit === 6) text += " ";
    if (group.length < 5) {
      text += "_";
      continue;
    }
    const found = DIGIT_PATTERNS.filter((pattern) =>
      [...pattern].every((mark, i) => group[i] === "?" || group[i] === mark),
    );
    text += found.length === 1 ? DIGIT_PATTERNS.indexOf(found[0]) : "?";
  }
  return text;
}

function handle(events) {
  for (const event of events) {
    switch (event.type) {
      case "signal":
        state.latest = event;
        state.readings.push({
          snr: event.peak,
          threshold: event.threshold,
          on: event.on,
        });
        if (state.readings.length > SCOPE_READINGS) state.readings.shift();
        if (event.level > SILENCE_DB) {
          state.silentSince = null;
        } else {
          state.silentSince ??= event.time;
        }
        break;
      case "element":
        state.received.push(event);
        state.recent.push(event);
        if (state.recent.length > RECENT_BEEPS) state.recent.shift();
        break;
      case "rejected":
        state.rejected += 1;
        break;
      case "recognised":
        state.received = event.elements;
        break;
      case "block":
        state.received = [];
        state.log.unshift(event);
        if (state.log.length > LOG_ROWS) state.log.pop();
        break;
      case "bunkers":
        state.bunkers = event;
        renderBunkers();
        if (event.unplaced > 0) {
          showNotice(
            "Heard a code that fits no bunker. If the server restarted, press Reset.",
          );
        }
        break;
    }
  }
  requestDraw();
}

function requestDraw() {
  if (state.drawPending) return;
  state.drawPending = true;
  requestAnimationFrame(() => {
    state.drawPending = false;
    draw();
  });
}

function renderBunkers() {
  ui.bunkers.replaceChildren(
    ...Array.from({ length: BUNKER_COUNT }, (_, index) =>
      bunkerCard(index, state.bunkers.slots[index]),
    ),
  );
}

function span(className, text) {
  const node = document.createElement("span");
  node.className = className;
  node.textContent = text;
  return node;
}

function digitRun(bunker, from, to, className = "digits") {
  const run = document.createElement("div");
  run.className = className;
  for (let index = from; index < to; index++) {
    const digit = bunker?.digits[index];
    const known = digit && digit.digit >= 0;
    run.append(
      span(`digit-${known ? digit.state : "missing"}`, known ? digit.digit : "-"),
    );
  }
  return run;
}

function bunkerCard(index, bunker) {
  const card = document.createElement("article");
  card.className = "card";

  const title = document.createElement("div");
  title.className = "card-title";
  title.append(span("", `Bunker ${index + 1}`));
  if (!bunker) {
    title.append(span("tag", "Not heard yet"));
  } else if (bunker.confirmed) {
    title.append(span("tag confirmed", `Confirmed, ${bunker.passes} passes`));
  } else {
    title.append(span("tag heard", "Heard once"));
  }

  const coordinates = document.createElement(bunker?.url ? "a" : "div");
  coordinates.className = "coordinates";
  const head = document.createElement("div");
  head.className = "coordinates-head";
  head.append(span("card-label", "Coordinates"));
  if (bunker?.url) {
    coordinates.href = bunker.url;
    coordinates.target = "_blank";
    coordinates.rel = "noopener noreferrer";
    head.append(span("map-link", "Open in iZurvive"));
  }
  const gps = document.createElement("div");
  gps.className = "gps";
  gps.textContent = bunker?.gps ?? "X: -.--  Y: -.--";
  coordinates.append(head, digitRun(bunker, 0, 6), gps);
  if (bunker?.offList) {
    coordinates.append(span("off-list", "Not on list, wait for a repeat"));
  }

  const keypad = span("card-label", "Keypad code");
  card.append(title, coordinates, keypad, digitRun(bunker, 6, 10, "digits keypad"));
  return card;
}

function fitCanvas(canvas) {
  const scale = window.devicePixelRatio || 1;
  const width = Math.round(canvas.clientWidth * scale);
  const height = Math.round(canvas.clientHeight * scale);
  if (canvas.width !== width || canvas.height !== height) {
    canvas.width = width;
    canvas.height = height;
  }
  const context = canvas.getContext("2d");
  context.setTransform(scale, 0, 0, scale, 0, 0);
  return { context, width: canvas.clientWidth, height: canvas.clientHeight };
}

const heightOf = (db, scale) =>
  (Math.min(Math.max(db, 0), FULL_SCALE_DB) / FULL_SCALE_DB) * scale;

function drawScope() {
  const { context, width, height } = fitCanvas(ui.scope);
  context.fillStyle = colors.background;
  context.fillRect(0, 0, width, height);

  const traceHeight = height - KEY_STRIP_HEIGHT - 2;
  context.fillStyle = colors.grid;
  for (let db = GRID_STEP_DB; db < FULL_SCALE_DB; db += GRID_STEP_DB) {
    context.fillRect(0, traceHeight - heightOf(db, traceHeight), width, 1);
  }

  const column = width / SCOPE_READINGS;
  const barWidth = Math.max(column, 1);
  const firstX = width - state.readings.length * column;
  state.readings.forEach((reading, index) => {
    const x = firstX + index * column;
    const bar = heightOf(reading.snr, traceHeight);
    context.fillStyle = reading.on ? colors.active : colors.idle;
    context.fillRect(x, traceHeight - bar, barWidth, bar);
    context.fillStyle = colors.threshold;
    context.fillRect(
      x,
      traceHeight - heightOf(reading.threshold, traceHeight),
      barWidth,
      1,
    );
    if (reading.on) {
      context.fillStyle = colors.active;
      context.fillRect(x, height - KEY_STRIP_HEIGHT, barWidth, KEY_STRIP_HEIGHT);
    }
  });
}

function drawSpectrum() {
  const { context, width, height } = fitCanvas(ui.spectrum);
  context.fillStyle = colors.background;
  context.fillRect(0, 0, width, height);
  const signal = state.latest;
  if (!signal) return;

  const bins = signal.spectrum;
  const column = width / bins.length;
  let locked = 0;
  let closest = Infinity;
  bins.forEach((_, bin) => {
    const distance = Math.abs(wasm.spectrum_hz(bin) - signal.hz);
    if (distance < closest) {
      closest = distance;
      locked = bin;
    }
  });
  bins.forEach((db, bin) => {
    const bar = heightOf(db, height);
    context.fillStyle = bin === locked ? colors.active : colors.idle;
    context.fillRect(bin * column, height - bar, Math.max(column - 1, 1), bar);
  });
}

function row(cells, warn) {
  const node = document.createElement("div");
  node.classList.toggle("warn", warn);
  node.append(...cells.map((text) => span("", text)));
  return node;
}

const LEAD_LABELS = {
  start: "block start",
  slot: "",
  break: "digit break",
  stray: "stray break",
  drowned: "drowned",
  known: "from list",
};

function renderStats() {
  const signal = state.latest;
  const value = (read) => (signal ? read(signal) : "-");
  const stats = [
    ["Tone", value((s) => `${s.hz.toFixed(0)} Hz`)],
    ["Tone level", value((s) => `${s.snr.toFixed(1)} dB`)],
    ["Threshold", value((s) => `${s.threshold.toFixed(1)} dB`)],
    ["Noise floor", value((s) => `${s.floor.toFixed(1)} dBFS`)],
    ["Audio level", value((s) => `${s.level.toFixed(1)} dBFS`)],
    ["Stream time", value((s) => formatTime(s.time))],
    ["Rejected", String(state.rejected)],
  ];
  ui.stats.replaceChildren(
    ...stats.map(([label, text]) => {
      const line = document.createElement("div");
      const term = document.createElement("dt");
      const detail = document.createElement("dd");
      term.textContent = label;
      detail.textContent = text;
      line.append(term, detail);
      return line;
    }),
  );
  ui.beeps.replaceChildren(
    ...state.recent
      .toReversed()
      .map((beep) =>
        row(
          [
            formatTime(beep.start),
            beep.symbol,
            `${beep.duration} ms`,
            `${beep.gap} ms`,
            LEAD_LABELS[beep.lead],
          ],
          beep.lead === "stray" || beep.lead === "drowned",
        ),
      ),
  );
  ui.blocks.replaceChildren(
    ...state.log.map((block) => {
      const kind = { complete: "ok", tail: "tail", damaged: "damaged" }[block.kind];
      const count = `${kind} ${block.elements}/${ELEMENTS_PER_BLOCK}`;
      const known = block.known ? `, ${block.known} from list` : "";
      const flags = block.flagged ? `, ${block.flagged} flagged` : "";
      return row(
        [formatTime(block.start), block.digits, count + known + flags],
        block.kind === "damaged" || block.flagged > 0,
      );
    }),
  );
}

function renderReceiving() {
  const aligned = state.latest?.aligned;
  const symbols = state.received.map((beep) => beep.symbol);
  if (symbols.length === 0) {
    ui.receiving.textContent = aligned
      ? "Waiting for the next block"
      : "Waiting for a pause to line up on a block";
  } else if (aligned) {
    ui.receiving.textContent = `Receiving ${symbols.length}/${ELEMENTS_PER_BLOCK}: ${digitsSoFar(symbols)}`;
  } else {
    ui.receiving.textContent = `Joined mid-block, ${symbols.length} beeps heard. Digits start after the next pause.`;
  }
  ui.symbols.textContent = formatGroups(symbols);
}

function draw() {
  const signal = state.latest;
  const beeping = Boolean(signal?.on) && isRunning();
  ui.lamp.classList.toggle("on", beeping);
  ui.mastLight.classList.toggle("on", beeping);
  const level = signal
    ? (signal.level - METER_FLOOR_DB) / -METER_FLOOR_DB
    : 0;
  const tone = signal ? signal.peak / FULL_SCALE_DB : 0;
  ui.level.style.width = `${Math.min(Math.max(level, 0), 1) * 100}%`;
  ui.tone.style.width = `${Math.min(Math.max(tone, 0), 1) * 100}%`;
  ui.thresholdValue.textContent = ui.thresholdAuto.checked
    ? signal
      ? `${signal.threshold.toFixed(0)} dB`
      : "-"
    : `${ui.threshold.value} dB`;
  drawScope();
  renderReceiving();
  if (!ui.debug.hidden) {
    drawSpectrum();
    renderStats();
  }
  if (
    state.capture &&
    state.silentSince !== null &&
    signal &&
    signal.time - state.silentSince >= SILENCE_HINT_MS
  ) {
    showNotice(
      "The share is silent. Share the entire screen with system audio, and check that DayZ isn't muted.",
    );
  }
}

const isRunning = () => Boolean(state.capture) || state.readingFile;

function setRunning(status) {
  ui.status.textContent = status;
  ui.start.textContent = state.capture ? "Stop" : "Start";
  ui.start.classList.toggle("primary", !state.capture);
  ui.start.classList.toggle("stop", Boolean(state.capture));
  ui.start.disabled = state.readingFile;
  ui.file.disabled = Boolean(state.capture) || state.readingFile;
  requestDraw();
}

async function requestShare() {
  return navigator.mediaDevices.getDisplayMedia({
    // Video is required by the browser and not used, so keep it cheap.
    video: { frameRate: 1, width: 320, height: 180 },
    audio: {
      // These are meant for voice and would filter the beeps out.
      echoCancellation: false,
      noiseSuppression: false,
      autoGainControl: false,
      suppressLocalAudioPlayback: false,
    },
    systemAudio: "include",
    windowAudio: "window",
    selfBrowserSurface: "exclude",
  });
}

function shareFailure(error) {
  if (error.name === "NotAllowedError") {
    return "Sharing was cancelled or blocked. Press Start to try again in Chrome or Edge, in a normal browser window.";
  }
  return `Could not share audio: ${error.message}`;
}

async function start() {
  showNotice(null);
  if (!navigator.mediaDevices?.getDisplayMedia) {
    showNotice(
      "This browser can't share audio. Use Chrome or Edge on a computer, on an https page.",
    );
    return;
  }

  let stream;
  try {
    stream = await requestShare();
  } catch (error) {
    showNotice(shareFailure(error));
    return;
  }

  const [audioTrack] = stream.getAudioTracks();
  if (!audioTrack) {
    stream.getTracks().forEach((track) => track.stop());
    showNotice(
      "That share has no audio. Press Start, choose Entire screen, and turn on the share audio option.",
    );
    return;
  }

  try {
    const context = new AudioContext({ latencyHint: "playback" });
    await context.audioWorklet.addModule(
      new URL("capture-worklet.js", import.meta.url),
    );
    const source = context.createMediaStreamSource(new MediaStream([audioTrack]));
    const worklet = new AudioWorkletNode(context, "capture");
    // The graph must reach the output to keep running, so it ends in silence.
    const muted = context.createGain();
    muted.gain.value = 0;
    source.connect(worklet).connect(muted).connect(context.destination);

    const session = sessionFor(context.sampleRate);
    worklet.port.onmessage = ({ data }) => handle(session.feed(data));
    audioTrack.addEventListener("ended", stop);

    state.capture = { context, stream, worklet };
    state.silentSince = null;
    ui.help.open = false;
    await context.resume();
    setRunning(`Listening to ${audioTrack.label || "shared audio"}`);
  } catch (error) {
    stream.getTracks().forEach((track) => track.stop());
    showNotice(`Could not start listening: ${error.message}`);
  }
}

function stop() {
  const capture = state.capture;
  if (!capture) return;
  state.capture = null;
  capture.worklet.port.onmessage = null;
  capture.stream.getTracks().forEach((track) => track.stop());
  capture.context.close();
  handle(state.session.finish());
  setRunning("Stopped");
}

async function readFile(file) {
  showNotice(null);
  state.readingFile = true;
  setRunning(`Reading ${file.name}`);
  try {
    const bytes = await file.arrayBuffer();
    // An offline context decodes and resamples without ever playing it.
    const audio = await new OfflineAudioContext(1, 1, FILE_RATE).decodeAudioData(bytes);
    const mono = new Float32Array(audio.length);
    for (let channel = 0; channel < audio.numberOfChannels; channel++) {
      const samples = audio.getChannelData(channel);
      for (let i = 0; i < mono.length; i++) {
        mono[i] += samples[i] / audio.numberOfChannels;
      }
    }

    state.session?.free();
    state.session = null;
    state.readings = [];
    clearResults();
    const session = sessionFor(audio.sampleRate);
    const chunk = FILE_CHUNK_SECONDS * audio.sampleRate;
    for (let at = 0; at < mono.length; at += chunk) {
      handle(session.feed(mono.subarray(at, at + chunk)));
      await new Promise((resolve) => setTimeout(resolve));
    }
    handle(session.finish());
    state.readingFile = false;
    setRunning(`Finished ${file.name}`);
  } catch (error) {
    state.readingFile = false;
    showNotice(`Could not read that file: ${error.message}`);
    setRunning("Stopped");
  }
}

ui.start.addEventListener("click", () => (state.capture ? stop() : start()));
ui.reset.addEventListener("click", () => {
  state.session?.reset();
  clearResults();
  requestDraw();
});
ui.debugToggle.addEventListener("change", () => {
  ui.debug.hidden = !ui.debugToggle.checked;
  requestDraw();
});
ui.threshold.addEventListener("input", () => {
  applyThreshold();
  requestDraw();
});
ui.thresholdAuto.addEventListener("change", () => {
  const auto = ui.thresholdAuto.checked;
  ui.threshold.disabled = auto;
  if (!auto && state.latest) {
    // Starts where the line already is, so nothing jumps.
    ui.threshold.value = Math.round(state.latest.threshold);
  }
  applyThreshold();
  requestDraw();
});
ui.file.addEventListener("change", () => {
  const [file] = ui.file.files;
  ui.file.value = "";
  if (file) readFile(file);
});
window.addEventListener("resize", requestDraw);

renderBunkers();
requestDraw();
