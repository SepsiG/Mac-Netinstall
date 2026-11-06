// Numbers, sizes and times for display.
import type { Lang } from "./i18n";

/** Whole percent of `done` / `total` (0 without a total), at most 100. */
export function percent(done: number, total: number | null | undefined): number {
  if (!total || total <= 0) return 0;
  return Math.min(100, Math.floor((done * 100) / total));
}

/** `1.5 MB` / `1,5 MB`. */
export function bytes(n: number, lang: Lang): string {
  const units = ["B", "KB", "MB", "GB"];
  let v = n;
  let i = 0;
  while (v >= 1000 && i < units.length - 1) {
    v /= 1000;
    i++;
  }
  const digits = i === 0 || v >= 100 ? 0 : 1;
  const num = new Intl.NumberFormat(lang, { maximumFractionDigits: digits }).format(v);
  return `${num} ${units[i]}`;
}

/** `2 minutes ago` / `2 perccel ezelőtt`, from Unix milliseconds. */
export function ago(thenMs: number, nowMs: number, lang: Lang): string {
  const secs = Math.round((thenMs - nowMs) / 1000);
  const rtf = new Intl.RelativeTimeFormat(lang, { numeric: "auto" });
  if (Math.abs(secs) < 60) return rtf.format(secs, "second");
  const mins = Math.round(secs / 60);
  if (Math.abs(mins) < 60) return rtf.format(mins, "minute");
  return rtf.format(Math.round(mins / 60), "hour");
}

/** `14:03:07` in the given language's conventions. */
export function clock(ms: number, lang: Lang): string {
  return new Intl.DateTimeFormat(lang, {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  }).format(ms);
}

const IPV4 = /^(25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)(\.(25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)){3}$/;

export function isIpv4(s: string): boolean {
  return IPV4.test(s);
}
