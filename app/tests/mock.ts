// A Backend for tests: every method is a vi.fn with a sensible default,
// and `emit*` drive the event callbacks.
import { vi } from "vitest";
import type { Backend } from "../src/lib/api";
import type {
  CacheView,
  EngineEvent,
  Profile,
  SettingsPane,
  Settings,
  Snapshot,
  UpdateProgress,
  UpdateView,
  VersionsView,
} from "../src/lib/types";

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
  window: null,
  dialog_sizes: {},
  log_panel_height: null,
  onboarding_done: true,
  keep_downloads: false,
  disclaimer_accepted_version: "1",
  check_updates: true,
  skipped_update_version: null,
};

/** A newer release found by the update check. */
export const AVAILABLE: UpdateView = {
  current: "0.1.2",
  latest: { version: "0.1.3", notes: "## Added\n- Update check", published_at: "2026-11-20T10:00:00Z", size: 12_000_000 },
  available: true,
  last_checked_unix: 1_793_600_000,
  skipped: null,
  auto: true,
};

/** The update check found nothing newer. */
export const UP_TO_DATE: UpdateView = {
  ...AVAILABLE,
  latest: { ...AVAILABLE.latest!, version: "0.1.2" },
  available: false,
};

export const VERSIONS: VersionsView = {
  channels: [
    { channel: "stable", version: "7.25.1" },
    { channel: "long-term", version: "7.24.6" },
    { channel: "testing", version: "7.26rc2" },
    { channel: "development", version: null },
    { channel: "v6-long-term", version: "6.49.23" },
  ],
  versions: [
    { version: "7.25.1", channels: ["stable"], date: "2026-11-02" },
    { version: "7.24.6", channels: ["long-term"], date: "2026-11-02" },
    { version: "7.24.5", channels: ["stable"], date: "2026-09-20" },
    { version: "6.49.23", channels: ["stable", "long-term"], date: "2026-03-15" },
    { version: "6.48.7", channels: ["long-term"], date: null },
  ],
  fetched_unix: 1_793_600_000,
  source: "cached",
  error: null,
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

/** A stored profile with the default options. */
export function profile(name: string, more: Partial<Profile> = {}): Profile {
  return {
    name,
    version: "stable",
    extra_packages: [],
    config: { mode: "default" },
    mode_script: null,
    remove_branding: false,
    license_key: null,
    after_install: "reboot",
    package_source: "download",
    local_packages: [],
    allow_below_minimum: false,
    ...more,
  };
}

export interface MockBackend extends Backend {
  emit(e: EngineEvent): void;
  emitLagged(): void;
  emitCloseRequested(busy: number): void;
  emitUpdateProgress(p: UpdateProgress): void;
}

export function mockBackend(overrides: Partial<Backend> = {}): MockBackend {
  const events: ((e: EngineEvent) => void)[] = [];
  const lagged: (() => void)[] = [];
  const close: ((n: number) => void)[] = [];
  const progress: ((p: UpdateProgress) => void)[] = [];
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
        configured_ipv4: { method: "manual", address: "192.168.88.1", prefix: 24 },
        auto_pool: { start: "192.168.88.2", end: "192.168.88.254" },
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
        configured_ipv4: { method: "dhcp", address: null, prefix: null },
        auto_pool: null,
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
      profiles: [profile("lab")],
      broken: [],
    })),
    saveProfile: ok(),
    deleteProfile: ok(),
    availablePackages: vi.fn(async (version: string, arch: string) => ({
      version: version === "stable" ? "7.25.1" : version,
      arch,
      packages: ["container", "wireless"],
    })),
    availableVersions: vi.fn(async (_refresh: boolean) => structuredClone(VERSIONS)),
    checkVersionExists: vi.fn(async (v: string) => v !== "6.49.99"),
    pickPackages: vi.fn(async () => [
      { name: "routeros-7.24.6-arm.npk", size: 12_000_000, package: "system", arch: "arm", version: "7.24.6" },
    ]),
    acceptDisclaimer: vi.fn(async () => ({ ...SETTINGS, disclaimer_accepted_version: "1" })),
    openLink: vi.fn(async () => {}),
    pickScript: vi.fn(async () => ({ name: "setup.rsc", size: 30 })),
    pickLicenseKey: vi.fn(async () => ({ key: "-----BEGIN KEY-----AAAA-----END KEY-----" })),
    getSettings: vi.fn(async () => ({ ...SETTINGS })),
    setSettings: vi.fn(async (s: Settings) => s),
    setDialogSize: vi.fn(async (_id: string, _w: number, _h: number) => {}),
    setLogPanelHeight: vi.fn(async (_height: number) => {}),
    setOnboardingDone: vi.fn(async (_done: boolean) => {}),
    requestPermissions: vi.fn(async () => ({ local_network: "granted" as const, firewall: "off" as const })),
    openSystemSettings: vi.fn(async (_pane: SettingsPane) => {}),
    cacheInventory: vi.fn(async () => structuredClone(CACHE)),
    deleteCached: vi.fn(async () => 1_000_000),
    exportLog: vi.fn(async () => true),
    preferredLanguages: vi.fn(async () => ["en-US"]),
    quit: ok(),
    updateStatus: vi.fn(async () => structuredClone(UP_TO_DATE)),
    checkForUpdates: vi.fn(async (_manual: boolean) => structuredClone(UP_TO_DATE)),
    skipUpdate: vi.fn(async (version: string) => ({ ...structuredClone(AVAILABLE), available: false, skipped: version })),
    clearSkippedUpdate: vi.fn(async () => ({ ...structuredClone(UP_TO_DATE), skipped: null })),
    downloadUpdate: vi.fn(async () => ({ file_name: "Mac-Netinstall_0.1.3_aarch64.dmg", opened: true })),
    openReleasePage: vi.fn(async () => {}),
    onUpdateProgress: vi.fn(async (cb) => {
      progress.push(cb);
      return () => {};
    }),
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
    emitUpdateProgress: (p) => progress.forEach((cb) => cb(p)),
  };
}
