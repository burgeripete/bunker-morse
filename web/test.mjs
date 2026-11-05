// Runs the built WebAssembly decoder over the real broadcast clip, the way the
// page feeds it. Nothing is played.
//   node web/test.mjs

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

import { Session, loadDecoder } from "./public/decoder.js";

const web = dirname(fileURLToPath(import.meta.url));
const wasm = await loadDecoder(
  new Uint8Array(readFileSync(join(web, "public/bunker_morse_web.wasm"))),
);

const CHUNK = 2048;
const WAV_HEADER = 44;

function clip() {
  const file = readFileSync(join(web, "../assets/broadcast-clip.wav"));
  const rate = file.readUInt32LE(24);
  const samples = new Float32Array((file.length - WAV_HEADER) / 2);
  for (let i = 0; i < samples.length; i++) {
    samples[i] = file.readInt16LE(WAV_HEADER + i * 2) / 32768;
  }
  return { rate, samples };
}

function feed(session, samples) {
  const events = [];
  for (let at = 0; at < samples.length; at += CHUNK) {
    events.push(...session.feed(samples.subarray(at, at + CHUNK)));
  }
  return events;
}

const ofType = (events, type) => events.filter((event) => event.type === type);

test("decodes the real broadcast", () => {
  const { rate, samples } = clip();
  const session = new Session(wasm, rate);
  const events = feed(session, samples);
  events.push(...session.finish());
  session.free();

  const complete = ofType(events, "block").filter((b) => b.kind === "complete");
  assert.deepEqual(complete.map((b) => b.digits), ["057074 6870"]);

  const [bunker] = ofType(events, "bunkers").at(-1).slots;
  assert.equal(bunker.raw, "057074");
  assert.equal(bunker.gps, "X: 0.57  Y: 0.74");
  assert.equal(bunker.url, "https://www.izurvive.com/nasdara/#location=5700;8984;6");
  assert.equal(bunker.confirmed, false);
});

test("reports the signal for the displays", () => {
  const { rate, samples } = clip();
  const session = new Session(wasm, rate);
  const signals = ofType(feed(session, samples), "signal");
  session.free();

  assert.ok(signals.some((s) => s.on));
  assert.ok(signals.every((s) => s.spectrum.length === 46));
  assert.ok(Math.abs(signals.at(-1).hz - 988) < 5);
});

test("a repeat of the broadcast confirms the bunker", () => {
  const { rate, samples } = clip();
  const session = new Session(wasm, rate);
  feed(session, samples);
  const events = feed(session, samples);
  session.free();

  const [bunker] = ofType(events, "bunkers").at(-1).slots;
  assert.equal(bunker.passes, 2);
  assert.equal(bunker.confirmed, true);
});

test("a fixed threshold applies, and Reset forgets the codes", () => {
  const { rate, samples } = clip();
  const session = new Session(wasm, rate);
  session.setThreshold(30);
  const events = feed(session, samples);
  assert.ok(ofType(events, "signal").every((s) => s.threshold === 30));
  assert.equal(ofType(events, "bunkers").at(-1).slots.length, 1);

  session.reset();
  const after = feed(session, samples.subarray(rate * 5));
  assert.equal(ofType(after, "bunkers").at(-1).slots[0].passes, 1);
  session.free();
});
