// Renders the app icon from its hand-written SVG sources (icon/icon.svg,
// and icon/icon-small.svg for 16-64 px, simplified for legibility) to the
// PNGs the bundle uses. resvg (WebAssembly) is deterministic: the same SVG
// gives the same bytes on every machine, so the PNGs are committed and a
// test checks they match the SVGs. The bundler builds the .icns from them
// (file names ending in "@2x.png" are Retina variants of half the size).
import { initWasm, Resvg } from "@resvg/resvg-wasm";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const root = new URL("../", import.meta.url);

/** [file under src-tauri/icons, pixel size, SVG source under icon/]. */
export const ICONS = [
  // tauri.conf.json lists it first, so it is also the window icon.
  ["icon.png", 512, "icon.svg"],
  ["16x16.png", 16, "icon-small.svg"],
  ["16x16@2x.png", 32, "icon-small.svg"],
  ["32x32.png", 32, "icon-small.svg"],
  ["32x32@2x.png", 64, "icon-small.svg"],
  ["128x128.png", 128, "icon.svg"],
  ["128x128@2x.png", 256, "icon.svg"],
  ["256x256.png", 256, "icon.svg"],
  ["256x256@2x.png", 512, "icon.svg"],
  ["512x512@2x.png", 1024, "icon.svg"],
];

let ready;

/** Render `svg` (a file under icon/) to a `size` x `size` PNG. */
export async function render(svg, size) {
  ready ??= initWasm(readFileSync(require.resolve("@resvg/resvg-wasm/index_bg.wasm")));
  await ready;
  const source = readFileSync(new URL(`icon/${svg}`, root), "utf8");
  const resvg = new Resvg(source, {
    fitTo: { mode: "width", value: size },
    font: { loadSystemFonts: false },
  });
  return Buffer.from(resvg.render().asPng());
}

/** The committed PNG `name` under src-tauri/icons. */
export function committed(name) {
  return readFileSync(new URL(`src-tauri/icons/${name}`, root));
}

/** Every bundle icon: [file name, PNG bytes]. */
export async function renderAll() {
  const out = [];
  for (const [name, size, svg] of ICONS) out.push([name, await render(svg, size)]);
  return out;
}
