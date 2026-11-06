import { parse, type MessageFormatElement } from "@formatjs/icu-messageformat-parser";
import { get } from "svelte/store";
import { describe, expect, it } from "vitest";
import codes from "../src/lib/codes.json";
import { format, locale, pickLanguage, t } from "../src/lib/i18n";
import en from "../src/locales/en.json";
import hu from "../src/locales/hu.json";

const EN: Record<string, string> = en;
const HU: Record<string, string> = hu;

/** Argument names used by an ICU message (recursing into plurals). */
function args(elements: MessageFormatElement[], out = new Set<string>()): Set<string> {
  for (const el of elements) {
    if ("value" in el && el.type !== 0 && typeof el.value === "string") out.add(el.value);
    if ("options" in el) {
      for (const opt of Object.values(el.options)) args(opt.value, out);
    }
  }
  return out;
}

/** Codes the frontend produces itself (`toFailure`, unknown codes). */
const FRONTEND_CODES = ["internal", "unknown"];

describe("locales", () => {
  it("Hungarian counts use plurals and distinct words for actions and states", () => {
    for (const key of ["confirm.quit_body", "confirm.stop_body"]) {
      const el = parse(HU[key]!).find((e) => "options" in e);
      expect(el, key).toBeDefined();
    }
    expect(HU["confirm.quit_ok"]).toBe("Kilépés így is");
    const words = new Set([HU["action.flash"], HU["state.flashing"], HU["settings.flashing"]]);
    expect(words.size).toBe(3);
  });

  it("tells to hold the reset button while powering on, in natural Hungarian", () => {
    expect(HU["code.kernel_version_unknown"]).toMatch(/kapcsold be úgy, hogy közben nyomva tartod a reset gombot/);
    for (const [key, text] of Object.entries(HU)) {
      expect(text, key).not.toMatch(/gombot nyomva tartva/);
    }
  });

  it("busy devices are described the same way everywhere", () => {
    for (const key of ["confirm.quit_body", "confirm.stop_body"]) {
      expect(EN[key], key).toMatch(/being prepared or flashed/);
      expect(HU[key], key).toMatch(/előkészítés vagy telepítés/);
    }
  });

  it("hu has exactly the keys of en", () => {
    expect(Object.keys(HU).sort()).toEqual(Object.keys(EN).sort());
  });

  it("every message parses and hu uses the same placeholders as en", () => {
    for (const [key, text] of Object.entries(EN)) {
      const want = [...args(parse(text))].sort();
      const have = [...args(parse(HU[key] ?? ""))].sort();
      expect(have, key).toEqual(want);
    }
  });

  it("no translation is left empty or untranslated by accident", () => {
    for (const [key, text] of Object.entries(HU)) {
      expect(text.trim(), key).not.toBe("");
    }
    // Spot checks that hu is really Hungarian.
    expect(HU["toolbar.start"]).toBe("Indítás");
    expect(HU["state.done"]).toBe("Kész");
  });

  it("every engine and bridge code has a message", () => {
    const need: string[] = [
      ...[...codes.failures, ...codes.app_failures, ...FRONTEND_CODES].map((c) => `code.${c}`),
      ...codes.asset_causes.map((c) => `cause.${c}`),
      ...codes.ignored_reasons.map((c) => `ignored.${c}`),
      ...codes.link_reasons.map((c) => `link.${c}`),
      ...codes.device_states.map((c) => `state.${c}`),
      ...codes.wait_reasons.map((c) => `wait.${c}`),
      ...codes.kernel_states.map((c) => `kernel.${c}`),
      ...codes.phases.map((c) => `phase.${c}`),
      ...codes.interface_kinds.map((c) => `kind.${c}`),
      ...codes.services.map((c) => `service.${c}`),
    ];
    const missing = need.filter((k) => !(k in EN));
    expect(missing).toEqual([]);
  });

  it("hints exist only for real codes", () => {
    const known = new Set([...codes.failures, ...codes.app_failures]);
    for (const key of Object.keys(EN).filter((k) => k.startsWith("hint."))) {
      expect(known.has(key.slice(5)), key).toBe(true);
    }
  });
});

describe("format", () => {
  it("fills placeholders and plurals", () => {
    expect(format("en", "code.port_in_use", { port: 67, service: "BOOTP" })).toBe(
      "Port 67 (BOOTP) is used by another program.",
    );
    expect(format("en", "confirm.quit_body", { count: 1 })).toMatch(/^1 device is being/);
    expect(format("en", "confirm.quit_body", { count: 3 })).toMatch(/^3 devices are being/);
    expect(format("hu", "toolbar.start")).toBe("Indítás");
  });

  it("falls back to the key, and to the raw text without values", () => {
    expect(format("hu", "no.such.key")).toBe("no.such.key");
    expect(format("en", "code.port_in_use")).toContain("{port}");
  });

  it("the t store follows the locale", () => {
    locale.set("hu");
    expect(get(t)("toolbar.stop")).toBe("Leállítás");
    locale.set("en");
    expect(get(t)("toolbar.stop")).toBe("Stop");
  });
});

describe("pickLanguage", () => {
  it("prefers the setting, then the first macOS language", () => {
    expect(pickLanguage("hu", ["en-US"])).toBe("hu");
    expect(pickLanguage("en", ["hu-HU"])).toBe("en");
    expect(pickLanguage(null, ["hu-HU", "en-US"])).toBe("hu");
    expect(pickLanguage(null, ["HU"])).toBe("hu");
    expect(pickLanguage(null, ["de-DE", "hu-HU"])).toBe("en");
    expect(pickLanguage(null, [])).toBe("en");
    expect(pickLanguage("xx", ["hu"])).toBe("hu");
  });
});
