// Codes and parameters → translated text. Rust sends only codes; this is
// the one place that turns them into sentences.
import { format, has, type Lang, type Values } from "./i18n";
import type { DeviceState, EngineEvent, Failure, Params } from "./types";

/** Parameter values that are codes themselves, by parameter name. */
const PARAM_DOMAINS: Record<string, string> = {
  cause: "cause",
  field: "field",
  kind: "kind",
  phase: "phase",
  reason: "link",
  service: "service",
  state: "state",
};

/** Message values for `params`, with coded values translated. */
export function values(lang: Lang, params: Params | undefined): Values {
  const out: Values = {};
  for (const [k, v] of Object.entries(params ?? {})) {
    const domain = PARAM_DOMAINS[k];
    out[k] = domain && has(`${domain}.${v}`) ? format(lang, `${domain}.${v}`) : v;
  }
  return out;
}

/** A failure (error, warning, finding, failed state) as a sentence. */
export function describe(lang: Lang, f: Failure): string {
  const key = `code.${f.code}`;
  if (!has(key)) return format(lang, "code.unknown", { code: f.code });
  return format(lang, key, values(lang, f.params));
}

/** What to do about it, if there is advice for this code. */
export function hint(lang: Lang, code: string): string | null {
  return has(`hint.${code}`) ? format(lang, `hint.${code}`) : null;
}

/** A device state as a short status text. */
export function stateText(lang: Lang, s: DeviceState): string {
  switch (s.state) {
    case "waiting":
      return format(lang, `wait.${s.reason}`);
    case "flashing":
      return format(lang, `phase.${s.phase}`);
    case "failed":
      return describe(lang, { code: s.code, params: s.params });
    default:
      return format(lang, `state.${s.state}`);
  }
}

export type Level = "info" | "warning" | "error";

/** One log line, kept untranslated so it follows language changes. */
export interface LogEntry {
  at: number;
  level: Level;
  mac?: string;
  /** A message key with its values, or a failure. */
  key?: string;
  values?: Values;
  failure?: Failure;
  /** For `device` events: the state (translated when shown). */
  state?: DeviceState;
}

/** The log entry for an event, or `null` for progress noise. */
export function logEntry(e: EngineEvent, at: number): LogEntry | null {
  switch (e.event) {
    case "started":
      return { at, level: "info", key: "log.started", values: { interface: e.interface, ip: e.server_ip } };
    case "stopped":
      return { at, level: "info", key: "log.stopped" };
    case "kernels_ready":
      return { at, level: "info", key: "log.kernels_ready", values: { version: e.version } };
    case "download":
      return e.done === 0 ? { at, level: "info", key: "log.download", values: { file: e.file } } : null;
    case "discovered":
      return {
        at,
        level: "info",
        mac: e.mac,
        key: "log.discovered",
        values: { model: e.model, arch: e.arch, license: e.license_id },
      };
    case "ignored":
      return { at, level: "info", mac: e.mac, key: "log.ignored", values: { reason: e.reason } };
    case "forgotten":
      return { at, level: "info", mac: e.mac, key: "log.forgotten" };
    case "only_mac_changed":
      return e.mac
        ? { at, level: "info", key: "log.only_mac", values: { mac: e.mac } }
        : { at, level: "info", key: "log.all_macs" };
    case "device": {
      const { event: _event, mac, ...state } = e;
      const s = state as DeviceState;
      // Progress updates of the same phase are noise in a log.
      if (s.state === "kernel_serving" && s.sent > 0) return null;
      if (s.state === "flashing" && s.sent > 0 && s.phase === "uploading") return null;
      const level: Level = s.state === "failed" ? "error" : "info";
      return { at, level, mac, state: s };
    }
    case "error":
      return { at, level: "error", failure: { code: e.code, params: e.params } };
    case "warning":
      return { at, level: "warning", failure: { code: e.code, params: e.params } };
    case "kernels":
      return null; // the status bar shows it; kernels_ready / error log it
  }
}

/** The text of a log entry in `lang`. */
export function logText(lang: Lang, entry: LogEntry): string {
  if (entry.failure) return describe(lang, entry.failure);
  if (entry.state) {
    if (entry.state.state === "failed") {
      return format(lang, "log.failed", { message: stateText(lang, entry.state) });
    }
    return stateText(lang, entry.state);
  }
  const vals = { ...entry.values };
  if (entry.key === "log.ignored" && typeof vals.reason === "string") {
    const k = `ignored.${vals.reason}`;
    if (has(k)) vals.reason = format(lang, k);
  }
  return format(lang, entry.key ?? "code.unknown", vals);
}
