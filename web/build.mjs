// Builds the WebAssembly decoder into `public/`, which is the whole site.
//   node web/build.mjs

import { execFileSync } from "node:child_process";
import { copyFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const web = dirname(fileURLToPath(import.meta.url));

execFileSync(
  "cargo",
  ["build", "--release", "--locked", "--target", "wasm32-unknown-unknown"],
  {
    cwd: web,
    stdio: "inherit",
    env: {
      ...process.env,
      // The binary is public, and panic messages embed source paths.
      CARGO_ENCODED_RUSTFLAGS: `--remap-path-prefix=${homedir()}=~`,
    },
  },
);
copyFileSync(
  join(web, "target/wasm32-unknown-unknown/release/bunker_morse_web.wasm"),
  join(web, "public/bunker_morse_web.wasm"),
);
console.log("web/public is ready to host");
