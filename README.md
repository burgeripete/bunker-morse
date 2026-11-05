# Bunker Morse

Listens to DayZ's audio and decodes the Morse bunker broadcast into coordinates
and keypad codes. It runs as an ordinary window with no overlay, and it never
plays any sound.

```text
game audio -> WASAPI process loopback -> tone detector -> Morse decoder -> bunker cards
              (no injection)              (988 Hz beep)    (50 elements)    (vote across passes)
```

## Web version

`web/public` is a static site with the same decoder, compiled to WebAssembly. It
listens to a screen share instead of capturing the game directly, so there is
nothing to install. Nothing leaves the page, and it never plays sound.

Hosted at <https://burgeripete.github.io/bunker-morse/>.

The site is deployed by `.github/workflows/pages.yml`, which builds the decoder,
runs its tests and publishes `web/public` on every push to `main`. To set it up
on a new repository, push it, then set **Settings > Pages > Source** to
**GitHub Actions** and re-run the workflow from the **Actions** tab. On a free
GitHub plan the repository must be public.

Any other static host works too: run `node web/build.mjs` and publish
`web/public`. It must be served over https, because browsers only allow screen
sharing and audio worklets on secure pages.

Use Chrome or Edge on a computer. Other browsers may not share audio at all.
Press **Start**, then pick the DayZ window if the dialog offers to share its
audio, or **Entire screen** otherwise, and turn on the share audio option. Sharing
the entire screen hears everything on the PC, so the desktop app is the cleaner
option where you can install it: it hears only the game.

To work on it:

```bash
node web/build.mjs
node --test web/test.mjs
python -m http.server 8765 --bind 127.0.0.1
```

`build.mjs` compiles `web/src` into `web/public`, and needs Rust's
`wasm32-unknown-unknown` target. The built `.wasm` is not kept in Git. Run the
server from the repository root and open `http://127.0.0.1:8765/web/public/`.

## The broadcast

Measured from a recording. Each block is one bunker: ten digits of five
elements in international Morse. The first six digits are coordinates and the
last four are the keypad code. The broadcast loops through its three bunkers,
about 80 seconds each, so a full cycle takes about four minutes.

| Element | Tone | Start to next start |
| --- | --- | --- |
| Dot | 120 ms | 900 ms |
| Dash | 500 ms | 1900 ms |
| Between digits | | 400 ms more |
| Between blocks | | 6000 ms more |

Digits have to be counted rather than split on gaps. A gap inside a digit after
a dash (1.4 s) is longer than a gap between digits after a dot (1.2 s), and the
break between digits 6 and 7 is missing altogether.

## Usage

```bash
cargo run --release
```

Rust stable and Visual Studio 2022 with the C++ tools are needed to build, and
Windows 10 2004 or later to capture a single process.

1. Press **Start**. It waits for `DayZ_x64.exe` if the game is not running yet.
   Pick another process under **Listen to** if needed.
2. Go somewhere you can hear the broadcast.
3. Each complete block fills a bunker card. Coordinates appear both raw, as
   `057074`, and as a GPS reads them, as `X: 0.57  Y: 0.74`.
4. Click a card's coordinates to open them on the iZurvive Nasdara map in your
   browser. The link appears once all six coordinate digits are known.

Digits are white when heard once and green when a later pass agrees. Amber means
passes disagree, and the most common value is shown. Bunker numbers are simply
the order they were first heard. If you join partway through a block, its digits
start after the next pause.

Press **Reset** after a server restart. A code that fits no bunker raises a
notice rather than replacing one.

## Reading the signal

The lamp beside the title lights on each beep. The strip plots 30 seconds of the
tone's level above the noise, with the threshold in amber and detected beeps in
green along the bottom. **Audio level** shows whether any audio is arriving at
all, and **Beep tone** shows the tone itself.

**Threshold** sets how far above the noise a sound must be to count as a beep,
and it is the yellow line on the plot. **Auto** keeps it at 70% of the loudest
recent beep. To set it yourself, turn **Auto** off and drag the slider until the
line sits above any ambient sound and well below the beeps. Changes apply at
once. The setting is not saved when the app closes.

If nothing decodes:

- **Audio level is empty:** the wrong process is selected, or the game is silent.
- **Audio moves but no beeps show:** look at the **Spectrum** in the debug panel.
  The tone's bin is green. The detector locks onto anything from 600 to 1500 Hz.
- **Ambient sound turns green:** raise **Threshold** above it.
- **Beeps show but digits are wrong:** the latest-beeps list gives each beep's
  length and the silence before it. Compare them with the table above.

Noise that clips or hides a beep is recovered from the timing around it where
possible. What cannot be recovered shows as `-`, and a later pass fills it in.
A block that does not come to 50 elements is logged as damaged and never reaches
the cards. On the sample clip, decoding stays clean with steady white noise up to
about 7 dB louder than the tone.

## Testing without a game

Nothing here is audible.

```bash
cargo test
```

`bunker-morse-cli` drives the decoder without the UI:

```bash
cargo run --bin bunker-morse-cli -- decode recording.mkv --elements
cargo run --bin bunker-morse-cli -- listen DayZ_x64.exe --elements
cargo run --bin bunker-morse-cli -- loopback-test
```

`decode` reads an OBS recording faster than real time and prints every block,
and every beep with `--elements`. `loopback-test` renders a synthetic block at
-70 dB in its own process and decodes it back through the real capture path,
which takes about 90 seconds.

To run the whole UI on a recording, set `BUNKER_MORSE_TEST_FILE`. The app then
starts by itself and reads that file silently. `BUNKER_MORSE_TEST_SPEED` sets a
multiple of real time.

`assets/broadcast-clip.wav` is 95 seconds of the real broadcast, used by the
tests in `tests/recording.rs`.
