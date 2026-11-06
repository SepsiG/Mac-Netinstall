// A Backend for tests: every method is a vi.fn with a sensible default,
// and `emit*` drive the event callbacks.
import { vi } from "vitest";
import type { Backend } from "../src/lib/api";
import type { CacheView, EngineEvent, Settings, Snapshot } from "../src/lib/types";

export const SETTINGS: Settings = {
  version: 1,
  language: "en",
  last_interface: null,
  pool_start: null,
  pool_end: null,
  max_parallel: 1,
  cooldown_secs: 300,
  auto_repeat: false,
  last_profile: null,
  download_timeout_secs: 1800,
};

export const CACHE: CacheView = {
  location: "~/Library/Caches/x",
  inventory: {
    versions: [
      {
        version: "7.25.1",
        size: 1_500_000,
        items: [
          { id: "7.25.1/kernels", kind: "kernels", name: "kernels", arch: null, size: 1_000_000, integrity: "verified", files: 8 },
          {
            id: "7.25.1/routeros-7.25.1-arm64.npk",
            kind: "package",
            name: "routeros-7.25.1-arm64.npk",
            arch: "arm64",
            size: 400_000,
            integrity: "verified",
            files: 1,
          },
          {
            id: "7.25.1/wireless-7.25.1-arm64.npk",
            kind: "package",
            name: "wireless-7.25.1-arm64.npk",
            arch: "arm64",
            size: 100_000,
            integrity: "corrupt",
            files: 1,
          },
        ],
      },
    ],
    partial: [],
    local: [{ id: "local/setup.rsc", kind: "local", name: "setup.rsc", arch: null, size: 30, integrity: "unpublished", files: 1 }],
    total: 1_500_030,
  },
};

export const IDLE: Snapshot = {
  running: false,
  started: null,
  network: null,
  kernel_version: null,
  kernels: { state: "idle" },
  auto_repeat: false,
  profile: null,
  only_mac: null,
  max_parallel: 0,
  cooldown_secs: 0,
  link_down: null,
  devices: [],
};

export interface MockBackend extends Backend {
  emit(e: EngineEvent): void;
  emitLagged(): void;
  emitCloseRequested(busy: number): void;
}

export function mockBackend(overrides: Partial<Backend> = {}): MockBackend {
  const events: ((e: EngineEvent) => void)[] = [];
  const lagged: (() => void)[] = [];
  const close: ((n: number) => void)[] = [];
  const ok = () => vi.fn(async () => {});
  const base: Backend = {
    listInterfaces: vi.fn(async () => [
      {
        name: "en7",
        display_name: "USB 10/100/1000 LAN",
        kind: "ethernet",
        mac: "00:E0:4C:68:00:01",
        ipv4: [{ address: "192.168.88.1", prefix: 24 }],
        up: true,
        running: true,
        default_route: false,
        netinstall_capable: true,
      },
      {
        name: "en0",
        display_name: "Wi-Fi",
        kind: "wifi",
        mac: null,
        ipv4: [{ address: "10.0.0.5", prefix: 24 }],
        up: true,
        running: true,
        default_route: true,
        netinstall_capable: false,
      },
    ]),
    preflight: vi.fn(async () => ({ findings: [], network: null })),
    checkInterface: vi.fn(async () => ({ findings: [], network: null })),
    start: vi.fn(async (req) => ({
      interface: req.interface,
      server_ip: "192.168.88.1",
      bootp: "192.168.88.1:67",
      tftp: "192.168.88.1:69",
      netinstall: "192.168.88.1:5000",
    })),
    stop: ok(),
    flash: ok(),
    retry: ok(),
    cancel: ok(),
    forget: ok(),
    setAutoRepeat: ok(),
    setActiveProfile: ok(),
    setOnlyMac: ok(),
    setMaxParallel: ok(),
    setCooldown: ok(),
    snapshot: vi.fn(async () => IDLE),
    listProfiles: vi.fn(async () => ({
      profiles: [{ name: "lab", version: "stable", extra_packages: [], config: { mode: "default" as const } }],
      broken: [],
    })),
    saveProfile: ok(),
    deleteProfile: ok(),
    availablePackages: vi.fn(async (version: string, arch: string) => ({
      version: version === "stable" ? "7.25.1" : version,
      arch,
      packages: ["container", "wireless"],
    })),
    pickScript: vi.fn(async () => ({ name: "setup.rsc", size: 30 })),
    getSettings: vi.fn(async () => ({ ...SETTINGS })),
    setSettings: vi.fn(async (s: Settings) => s),
    cacheInventory: vi.fn(async () => structuredClone(CACHE)),
    deleteCached: vi.fn(async () => 1_000_000),
    exportLog: vi.fn(async () => true),
    preferredLanguages: vi.fn(async () => ["en-US"]),
    quit: ok(),
    onEvent: vi.fn(async (cb) => {
      events.push(cb);
      return () => {};
    }),
    onLagged: vi.fn(async (cb) => {
      lagged.push(cb);
      return () => {};
    }),
    onCloseRequested: vi.fn(async (cb) => {
      close.push(cb);
      return () => {};
    }),
  };
  return {
    ...base,
    ...overrides,
    emit: (e) => events.forEach((cb) => cb(e)),
    emitLagged: () => lagged.forEach((cb) => cb()),
    emitCloseRequested: (n) => close.forEach((cb) => cb(n)),
  };
}
