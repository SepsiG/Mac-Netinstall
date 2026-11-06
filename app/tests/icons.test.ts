// @vitest-environment node
// The committed bundle icons are exactly what scripts/make-icon.mjs renders
// from icon/*.svg (rerun it after editing an SVG).
import { describe, expect, it } from "vitest";
import { committed, ICONS, renderAll } from "../scripts/icons.mjs";
import conf from "../src-tauri/tauri.conf.json";

const same = (a: Uint8Array, b: Uint8Array) =>
  a.length === b.length && a.every((byte, i) => byte === b[i]);

describe("app icon", () => {
  it("renders every icon the bundle lists", () => {
    expect(ICONS.map(([name]) => `icons/${name}`)).toEqual(conf.bundle.icon);
  });

  it("committed PNGs match the SVG sources", async () => {
    for (const [name, png] of await renderAll()) {
      expect(same(committed(name), png), `${name} is stale: run node scripts/make-icon.mjs`).toBe(
        true,
      );
    }
  }, 60_000);

  it("rendering is deterministic", async () => {
    const [a, b] = [await renderAll(), await renderAll()];
    expect(a.map(([name]) => name)).toEqual(b.map(([name]) => name));
    a.forEach(([name, png], i) => expect(same(png, b[i]![1]), name).toBe(true));
  }, 60_000);
});
