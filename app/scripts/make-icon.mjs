// Draws the app icon source (1024x1024 PNG) without any image library:
// a blue rounded square with a white "install" glyph (an arrow into a
// tray). `npx tauri icon app-icon.png` turns it into the bundle icons.
// Usage: node scripts/make-icon.mjs app-icon.png
import { writeFileSync } from "node:fs";
import { deflateSync } from "node:zlib";

const N = 1024;
const px = new Uint8Array(N * N * 4);

// Signed distance to a rounded box centred at (cx, cy).
function roundBox(x, y, cx, cy, hw, hh, r) {
  const qx = Math.abs(x - cx) - hw + r;
  const qy = Math.abs(y - cy) - hh + r;
  const ox = Math.max(qx, 0);
  const oy = Math.max(qy, 0);
  return Math.hypot(ox, oy) + Math.min(Math.max(qx, qy), 0) - r;
}

// Distance to the segment (ax, ay)-(bx, by).
function segment(x, y, ax, ay, bx, by) {
  const dx = bx - ax;
  const dy = by - ay;
  const t = Math.max(0, Math.min(1, ((x - ax) * dx + (y - ay) * dy) / (dx * dx + dy * dy)));
  return Math.hypot(x - ax - t * dx, y - ay - t * dy);
}

const cover = (d) => Math.max(0, Math.min(1, 0.5 - d)); // 1 px anti-aliasing

for (let y = 0; y < N; y++) {
  for (let x = 0; x < N; x++) {
    const i = (y * N + x) * 4;
    const bg = cover(roundBox(x + 0.5, y + 0.5, 512, 512, 412, 412, 92));
    if (bg === 0) continue;
    // Vertical gradient #2D7FF9 -> #0B4FC2.
    const t = y / N;
    let r = 0x2d + (0x0b - 0x2d) * t;
    let g = 0x7f + (0x4f - 0x7f) * t;
    let b = 0xf9 + (0xc2 - 0xf9) * t;
    const p = [x + 0.5, y + 0.5];
    const w = 34; // half stroke width
    const glyph = Math.min(
      segment(...p, 512, 250, 512, 590) - w, // arrow shaft
      segment(...p, 512, 600, 392, 480) - w, // arrow head, left
      segment(...p, 512, 600, 632, 480) - w, // arrow head, right
      segment(...p, 300, 640, 300, 760) - w, // tray, left
      segment(...p, 300, 760, 724, 760) - w, // tray, bottom
      segment(...p, 724, 760, 724, 640) - w, // tray, right
    );
    const a = cover(glyph);
    r += (255 - r) * a;
    g += (255 - g) * a;
    b += (255 - b) * a;
    px[i] = Math.round(r);
    px[i + 1] = Math.round(g);
    px[i + 2] = Math.round(b);
    px[i + 3] = Math.round(255 * bg);
  }
}

function crc32(buf) {
  let c = ~0;
  for (const byte of buf) {
    c ^= byte;
    for (let k = 0; k < 8; k++) c = c & 1 ? (c >>> 1) ^ 0xedb88320 : c >>> 1;
  }
  return ~c >>> 0;
}

function chunk(type, data) {
  const head = Buffer.alloc(8);
  head.writeUInt32BE(data.length, 0);
  head.write(type, 4, "ascii");
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(Buffer.concat([head.subarray(4), data])), 0);
  return Buffer.concat([head, data, crc]);
}

const raw = Buffer.alloc(N * (N * 4 + 1));
for (let y = 0; y < N; y++) {
  raw[y * (N * 4 + 1)] = 0; // filter: none
  Buffer.from(px.buffer, y * N * 4, N * 4).copy(raw, y * (N * 4 + 1) + 1);
}
const ihdr = Buffer.alloc(13);
ihdr.writeUInt32BE(N, 0);
ihdr.writeUInt32BE(N, 4);
ihdr.set([8, 6, 0, 0, 0], 8); // 8-bit RGBA
const png = Buffer.concat([
  Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
  chunk("IHDR", ihdr),
  chunk("IDAT", deflateSync(raw, { level: 9 })),
  chunk("IEND", Buffer.alloc(0)),
]);
writeFileSync(process.argv[2] ?? "app-icon.png", png);
