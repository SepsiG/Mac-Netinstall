// The settings form: strings in, `Settings` out, with field checks.
import { isIpv4 } from "./format";
import type { Settings } from "./types";

export const LIMITS = {
  max_parallel: [1, 64],
  cooldown_secs: [30, 86_400],
  download_timeout_min: [1, 1440],
} as const;

export interface SettingsForm {
  language: string; // "" = follow macOS
  pool_start: string;
  pool_end: string;
  max_parallel: string;
  cooldown_secs: string;
  download_timeout_min: string;
}

export type FieldError = { key: string; values?: Record<string, number> };

export function toSettingsForm(s: Settings): SettingsForm {
  return {
    language: s.language ?? "",
    pool_start: s.pool_start ?? "",
    pool_end: s.pool_end ?? "",
    max_parallel: String(s.max_parallel),
    cooldown_secs: String(s.cooldown_secs),
    download_timeout_min: String(Math.round(s.download_timeout_secs / 60)),
  };
}

function int(s: string, [min, max]: readonly [number, number]): number | null {
  if (!/^\d+$/.test(s.trim())) return null;
  const n = Number(s.trim());
  return n >= min && n <= max ? n : null;
}

/** Problems by field name (message key + values). */
export function settingsErrors(f: SettingsForm): Record<string, FieldError> {
  const out: Record<string, FieldError> = {};
  for (const k of ["pool_start", "pool_end"] as const) {
    if (f[k].trim() !== "" && !isIpv4(f[k].trim())) out[k] = { key: "settings.err_ip" };
  }
  if ((f.pool_start.trim() === "") !== (f.pool_end.trim() === "")) {
    out.pool_end ??= { key: "settings.err_pool" };
  }
  for (const k of ["max_parallel", "cooldown_secs", "download_timeout_min"] as const) {
    const [min, max] = LIMITS[k];
    if (int(f[k], LIMITS[k]) === null) out[k] = { key: "settings.err_range", values: { min, max } };
  }
  return out;
}

/** The settings the form describes (`base` keeps the fields it lacks). */
export function fromSettingsForm(f: SettingsForm, base: Settings): Settings {
  const ip = (s: string) => (s.trim() === "" ? null : s.trim());
  return {
    ...base,
    language: f.language === "" ? null : f.language,
    pool_start: ip(f.pool_start),
    pool_end: ip(f.pool_end),
    max_parallel: Number(f.max_parallel.trim()),
    cooldown_secs: Number(f.cooldown_secs.trim()),
    download_timeout_secs: Number(f.download_timeout_min.trim()) * 60,
  };
}
