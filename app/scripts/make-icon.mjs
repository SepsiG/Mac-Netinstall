// Writes the bundle icons (src-tauri/icons/*.png) from icon/*.svg.
// Usage (in app/): node scripts/make-icon.mjs [output dir]
// See scripts/icons.mjs; tests/icons.test.ts checks the committed PNGs.
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { renderAll } from "./icons.mjs";

const dir = process.argv[2] ?? fileURLToPath(new URL("../src-tauri/icons/", import.meta.url));
mkdirSync(dir, { recursive: true });
for (const [name, png] of await renderAll()) {
  writeFileSync(join(dir, name), png);
  console.log(`${name}\t${png.length} bytes`);
}
