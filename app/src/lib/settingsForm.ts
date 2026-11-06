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
  /** A custom DHCP range; otherwise automatic (from the adapter's subnet). */
  poolCustom: boolean;
  pool_start: string;
  pool_end: string;
  max_parallel: string;
  cooldown_secs: string;
  download_timeout_min: string;
}

export type FieldError = { key: string; values?: Record<string, number | string> };

export function toSettingsForm(s: Settings): SettingsForm {
  return {
    language: s.language ?? "",
    poolCustom: s.pool_start !== null || s.pool_end !== null,
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

function ipNumber(s: string): number {
  return s.split(".").reduce((n, part) => n * 256 + Number(part), 0);
}

/** The adapter's subnet, for checking a custom pool. */
export interface Subnet {
  address: string;
  prefix: number;
}

/** `172.30.0.0/22` for 172.30.0.2/22. */
export function subnetText(s: Subnet): string {
  const size = 2 ** (32 - s.prefix);
  const net = Math.floor(ipNumber(s.address) / size) * size;
  const parts = [24, 16, 8, 0].map((shift) => Math.floor(net / 2 ** shift) % 256);
  return `${parts.join(".")}/${s.prefix}`;
}

/** A host address of `s` (not its network or broadcast address). */
function inside(ip: string, s: Subnet): boolean {
  const size = 2 ** (32 - s.prefix);
  const net = Math.floor(ipNumber(s.address) / size) * size;
  const n = ipNumber(ip);
  return n > net && n < net + size - 1;
}

/** Problems by field name (message key + values). A custom pool must lie
 *  inside `subnet` (the adapter's configured one), if known. */
export function settingsErrors(f: SettingsForm, subnet: Subnet | null = null): Record<string, FieldError> {
  const out: Record<string, FieldError> = {};
  if (f.poolCustom) {
    for (const k of ["pool_start", "pool_end"] as const) {
      if (!isIpv4(f[k].trim())) out[k] = { key: "settings.err_ip" };
    }
    if (!out.pool_start && !out.pool_end && subnet) {
      if (!inside(f.pool_start.trim(), subnet) || !inside(f.pool_end.trim(), subnet)) {
        out.pool_end = { key: "settings.err_pool_subnet", values: { subnet: subnetText(subnet) } };
      }
    }
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
    pool_start: f.poolCustom ? ip(f.pool_start) : null,
    pool_end: f.poolCustom ? ip(f.pool_end) : null,
    max_parallel: Number(f.max_parallel.trim()),
    cooldown_secs: Number(f.cooldown_secs.trim()),
    download_timeout_secs: Number(f.download_timeout_min.trim()) * 60,
  };
}
