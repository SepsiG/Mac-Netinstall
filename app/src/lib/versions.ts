// The version picker's rules: grouping, search, version checks. Pure, so
// they are tested without a component.
import type { ChannelView, ListedVersion, VersionsView } from "./types";

const RELEASE = /^(\d+)\.(\d+)(?:\.(\d+))?((?:alpha|beta|rc)\d+)?$/;

/** Numbers and pre-release rank of a version (`null`: not a version). */
function key(v: string): [number[], number, number] | null {
  const m = RELEASE.exec(v.trim());
  if (!m) return null;
  const nums = [Number(m[1]), Number(m[2]), Number(m[3] ?? 0)];
  const pre = m[4];
  if (!pre) return [nums, 3, 0];
  const rank = pre.startsWith("rc") ? 2 : pre.startsWith("beta") ? 1 : 0;
  return [nums, rank, Number(pre.replace(/^\D+/, ""))];
}

/** Compare two versions (`7.9 < 7.25`, `7.26rc1 < 7.26`). */
export function compareVersions(a: string, b: string): number {
  const ka = key(a);
  const kb = key(b);
  if (!ka || !kb) return a.localeCompare(b);
  for (let i = 0; i < 3; i++) {
    const d = ka[0][i]! - kb[0][i]!;
    if (d !== 0) return d;
  }
  return ka[1] - kb[1] || ka[2] - kb[2];
}

/** A version the app accepts: 7.x and later, or 6.x from 6.30. */
export function acceptedVersion(v: string): boolean {
  // One spelling per version: no leading zeros (`06.49`, `7.025`).
  if (/(^|\D)0\d/.test(v.trim())) return false;
  const k = key(v);
  if (!k) return false;
  const [major, minor] = k[0];
  return major! >= 7 || (major === 6 && minor! >= 30);
}

/** A spec asking for RouterOS v6 (the pseudo-channel or a 6.x version). */
export function isV6(spec: string): boolean {
  return spec === "v6-long-term" || /^6\./.test(spec.trim());
}

/** Whether the mode script can be used with `spec`: RouterOS 7.22 and
 *  later (v7 channels are newer already). */
export function supportsModeScript(spec: string): boolean {
  if (isV6(spec)) return false;
  if (!key(spec)) return true; // a v7 channel
  return compareVersions(spec, "7.22") >= 0;
}

export interface VersionGroups {
  channels: ChannelView[];
  v7: ListedVersion[];
  v6: ListedVersion[];
}

/** Split the list for the picker: channels, 7.x and the 6.x archive, each
 *  newest first, filtered by `query` (matches the version, a channel tag or
 *  the date; channels also by their current version). */
export function versionGroups(view: VersionsView, query = "", channelName: (c: string) => string = (c) => c): VersionGroups {
  const q = query.trim().toLowerCase();
  const hit = (...texts: (string | null | undefined)[]) =>
    q === "" || texts.some((t) => t != null && t.toLowerCase().includes(q));
  const sorted = [...view.versions].sort((a, b) => compareVersions(b.version, a.version));
  const listed = sorted.filter((l) => hit(l.version, l.date, ...l.channels));
  return {
    channels: view.channels.filter((c) => hit(c.channel, channelName(c.channel), c.version)),
    v7: listed.filter((l) => !isV6(l.version)),
    v6: listed.filter((l) => isV6(l.version)),
  };
}

/** Age of the list in whole hours (`null` for the built-in list). */
export function listAgeHours(view: VersionsView, nowMs: number): number | null {
  if (view.fetched_unix === 0) return null;
  return Math.max(0, Math.floor((nowMs / 1000 - view.fetched_unix) / 3600));
}
