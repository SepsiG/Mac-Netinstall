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

/** One shown row: `count` identical consecutive lines, `last` the
 *  newest of them (its time is shown), `first` the oldest (the row's key:
 *  it stays the same while the row counts up). */
export interface LogRow {
  first: LogEntry;
  last: LogEntry;
  count: number;
}

const signatures = new WeakMap<LogEntry, string>();

/** What makes two lines the same (all but the time). */
function signature(e: LogEntry): string {
  let s = signatures.get(e);
  if (s === undefined) {
    s = JSON.stringify([e.level, e.mac ?? "", e.key ?? "", e.values ?? null, e.failure ?? null, e.state ?? null]);
    signatures.set(e, s);
  }
  return s;
}

/** The last row made for each `first`: reused while unchanged, so the
 *  list does not redraw rows that did not change. */
const rows = new WeakMap<LogEntry, LogRow>();

function row(first: LogEntry, last: LogEntry, count: number): LogRow {
  const had = rows.get(first);
  if (had && had.last === last && had.count === count) return had;
  const r = { first, last, count };
  rows.set(first, r);
  return r;
}

/** The rows to show, newest first: of `lines` (line `i` is number
 *  `first + i`), those numbered below `upto`, of device `mac` if given,
 *  identical consecutive ones as one row; at most `max` rows. */
export function newestFirst(lines: readonly LogEntry[], first: number, upto: number, mac: string, max = MAX_SHOWN): LogRow[] {
  const out: LogRow[] = [];
  const end = Math.min(lines.length, Math.max(0, upto - first));
  let group: { last: LogEntry; first: LogEntry; count: number; sig: string } | null = null;
  for (let i = end - 1; i >= 0; i--) {
    const e = lines[i]!;
    if (mac && e.mac !== mac) continue;
    const sig = signature(e);
    if (group && group.sig === sig) {
      group.first = e;
      group.count++;
      continue;
    }
    if (group) out.push(row(group.first, group.last, group.count));
    if (out.length >= max) return out;
    group = { last: e, first: e, count: 1, sig };
  }
  if (group && out.length < max) out.push(row(group.first, group.last, group.count));
  return out;
}
