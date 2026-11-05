// A thin wrapper over the WebAssembly decoder, shared by the page and its test.

export async function loadDecoder(source) {
  const bytes =
    source instanceof Uint8Array
      ? source
      : await (await fetch(source)).arrayBuffer();
  const { instance } = await WebAssembly.instantiate(bytes, {});
  return instance.exports;
}

export class Session {
  #wasm;
  #handle;
  #text = new TextDecoder();

  constructor(wasm, sampleRate) {
    this.#wasm = wasm;
    this.#handle = wasm.session_new(sampleRate);
    this.sampleRate = sampleRate;
  }

  /** Decodes mono samples and returns what happened, oldest first. */
  feed(samples) {
    const wasm = this.#wasm;
    const pointer = wasm.session_input(this.#handle, samples.length);
    // The view must be made after the call, which may have grown memory.
    new Float32Array(wasm.memory.buffer, pointer, samples.length).set(samples);
    return this.#read(wasm.session_feed(this.#handle));
  }

  /** Ends the stream, reporting any block in progress. */
  finish() {
    return this.#read(this.#wasm.session_finish(this.#handle));
  }

  /** Sets a fixed threshold in dB, or null to follow the beeps. */
  setThreshold(db) {
    this.#wasm.session_set_threshold(this.#handle, db ?? 0);
  }

  /** Forgets the decoded codes. */
  reset() {
    this.#wasm.session_reset(this.#handle);
  }

  free() {
    this.#wasm.session_free(this.#handle);
  }

  #read(pointer) {
    const length = this.#wasm.session_output_length(this.#handle);
    const bytes = new Uint8Array(this.#wasm.memory.buffer, pointer, length);
    return JSON.parse(this.#text.decode(bytes));
  }
}
