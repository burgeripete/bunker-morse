// Runs on the audio thread: mixes the shared audio to mono and hands it to the
// page in batches. It writes nothing to its output, so nothing is ever heard.

const BATCH = 2048;
const RENDER_QUANTUM = 128;

class CaptureProcessor extends AudioWorkletProcessor {
  batch = new Float32Array(BATCH);
  filled = 0;

  process(inputs) {
    const channels = inputs[0];
    // An idle input has no channels. Its time still counts, because beeps are
    // timed by the stream's own clock.
    const frames = channels[0]?.length ?? RENDER_QUANTUM;
    for (let frame = 0; frame < frames; frame++) {
      let sum = 0;
      for (const channel of channels) {
        sum += channel[frame];
      }
      this.batch[this.filled++] = channels.length ? sum / channels.length : 0;
      if (this.filled === BATCH) {
        this.port.postMessage(this.batch, [this.batch.buffer]);
        this.batch = new Float32Array(BATCH);
        this.filled = 0;
      }
    }
    return true;
  }
}

registerProcessor("capture", CaptureProcessor);
