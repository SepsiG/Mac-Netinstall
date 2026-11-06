// The engine as the window sees it: built from snapshots and kept current
// by events. Pure state, no Tauri: tests feed it events directly.
import { logEntry, type LogEntry } from "./messages";
import type {
  DeviceInfoView,
  DeviceState,
  DeviceView,
  EngineEvent,
  KernelState,
  Snapshot,
  StartInfo,
} from "./types";

/** Most log entries kept (oldest dropped first). */
export const MAX_LOG = 2000;

export interface DeviceRow {
  mac: string;
  ip: string | null;
  boot_arch: string | null;
  info: DeviceInfoView | null;
  version: string | null;
  attempt: number;
  updated_ms: number;
  state: DeviceState;
}

function row(d: DeviceView): DeviceRow {
  return {
    mac: d.mac,
    ip: d.ip,
    boot_arch: d.boot_arch,
    info: d.info,
    version: d.version,
    attempt: d.attempt,
    updated_ms: d.updated_ms,
    state: d.state,
  };
}

export class EngineState {
  running = $state(false);
  started = $state<StartInfo | null>(null);
  kernels = $state<KernelState>({ state: "idle" });
  autoRepeat = $state(false);
  profile = $state<string | null>(null);
  onlyMac = $state<string | null>(null);
  linkDown = $state<string | null>(null);
  devices = $state<Record<string, DeviceRow>>({});
  log = $state<LogEntry[]>([]);

  /** Devices sorted by MAC. */
  get rows(): DeviceRow[] {
    return Object.values(this.devices).sort((a, b) => a.mac.localeCompare(b.mac));
  }

  /** Devices being prepared or flashed. */
  get busy(): number {
    return this.rows.filter((d) => d.state.state === "preparing" || d.state.state === "flashing")
      .length;
  }

  applySnapshot(s: Snapshot): void {
    this.running = s.running;
    this.started = s.started;
    this.kernels = s.kernels;
    this.autoRepeat = s.auto_repeat;
    this.profile = s.profile;
    this.onlyMac = s.only_mac;
    this.linkDown = s.link_down;
    const devices: Record<string, DeviceRow> = {};
    for (const d of s.devices) devices[d.mac] = row(d);
    this.devices = devices;
  }

  private device(mac: string): DeviceRow {
    let d = this.devices[mac];
    if (!d) {
      d = {
        mac,
        ip: null,
        boot_arch: null,
        info: null,
        version: null,
        attempt: 0,
        updated_ms: Date.now(),
        state: { state: "discovered" },
      };
      this.devices[mac] = d;
    }
    return d;
  }

  apply(e: EngineEvent, now: number = Date.now()): void {
    const entry = logEntry(e, now);
    if (entry) {
      this.log.push(entry);
      if (this.log.length > MAX_LOG) this.log.splice(0, this.log.length - MAX_LOG);
    }
    switch (e.event) {
      case "started": {
        const { event: _e, ...info } = e;
        this.running = true;
        this.started = info;
        this.devices = {};
        this.linkDown = null;
        break;
      }
      case "stopped":
        this.running = false;
        this.kernels = { state: "idle" };
        this.linkDown = null;
        break;
      case "kernels": {
        const { event: _e, ...state } = e;
        this.kernels = state as KernelState;
        break;
      }
      case "device": {
        const { event: _e, mac, ...state } = e;
        const d = this.device(mac);
        const s = state as DeviceState;
        if (s.state === "preparing" && d.state.state !== "preparing") d.attempt += 1;
        if (s.state === "bootp_seen") {
          d.ip = s.ip;
          d.boot_arch = s.boot_arch;
        }
        if ((s.state === "preparing" && s.version) || s.state === "flashing") {
          d.version = s.version ?? d.version;
        }
        d.state = s;
        d.updated_ms = now;
        break;
      }
      case "discovered": {
        const { event: _e, mac, ...info } = e;
        this.device(mac).info = info;
        break;
      }
      case "warning":
        if (e.code === "link_down") this.linkDown = e.params?.reason ?? "no_ip";
        if (e.code === "link_up") this.linkDown = null;
        break;
      default:
        break;
    }
  }

  clearLog(): void {
    this.log = [];
  }
}
