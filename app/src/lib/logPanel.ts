// Sizes and limits of the log panel, and how the shown lines are picked
// from the log buffer. Pure: the component and tests share it.
import type { LogEntry } from "./messages";

/** Height of the lines area (CSS px) unless the user chose one. */
export const DEFAULT_LOG_HEIGHT = 150;
/** Smallest lines area: about three lines (the Rust side accepts it too:
 *  `MIN_LOG_PANEL_HEIGHT` in settings.rs). */
export const MIN_LOG_HEIGHT = 64;
/** Largest stored height (`MAX_SIDE` in settings.rs). */
export const MAX_LOG_HEIGHT = 16_384;
/** One arrow key press on the splitter: about one line. */
export const LOG_STEP = 16;
/** The device table above keeps at least this much (CSS px). */
export const MIN_TABLE_HEIGHT = 120;
/** Most lines rendered at once (the newest); the export has every kept line. */
export const MAX_SHOWN = 2000;

/** `h` within [MIN_LOG_HEIGHT, max] (max never below the minimum), rounded. */
export function clampLogHeight(h: number, max: number = MAX_LOG_HEIGHT): number {
  const top = Math.max(MIN_LOG_HEIGHT, Math.min(max, MAX_LOG_HEIGHT));
  return Math.round(Math.min(Math.max(h, MIN_LOG_HEIGHT), top));
}

/** The lines to show, newest first: of `lines` (line `i` is number
 *  `first + i`), those numbered below `upto`, of device `mac` if given,
 *  at most `max`. */
export function newestFirst(lines: readonly LogEntry[], first: number, upto: number, mac: string, max = MAX_SHOWN): LogEntry[] {
  const out: LogEntry[] = [];
  const end = Math.min(lines.length, Math.max(0, upto - first));
  for (let i = end - 1; i >= 0 && out.length < max; i--) {
    const e = lines[i]!;
    if (!mac || e.mac === mac) out.push(e);
  }
  return out;
}
