// TypeScript mirrors of the Rust (serde) types the bridge exchanges.
// Field names are the JSON names: snake_case, as Rust sends them.

export type Params = Record<string, string>;

/** A stable code with parameters (`ni_engine::Failure`). */
export interface Failure {
  code: string;
  params?: Params;
}

export type WaitReason = "no_profile" | "concurrency";

export type DeviceState =
  | { state: "bootp_seen"; boot_arch: string; ip: string }
  | { state: "kernel_serving"; sent: number; total: number }
  | { state: "discovered" }
  | { state: "waiting"; reason: WaitReason }
  | { state: "preparing"; version?: string; config?: string }
  | { state: "flashing"; version: string; phase: string; sent: number; total: number }
  | { state: "done" }
  | { state: "failed"; code: string; params?: Params };

export type KernelState =
  | { state: "idle" }
  | { state: "downloading"; spec: string; done: number; total: number | null }
  | { state: "ready"; version: string; offline: boolean }
  | { state: "unavailable"; code: string; params?: Params };

export interface DeviceInfoView {
  model: string;
  arch: string;
  license_id: string;
  min_os: string;
}

export interface StartInfo {
  interface: string;
  server_ip: string;
  bootp: string;
  tftp: string;
  netinstall: string;
}

export interface NetPlan {
  server_ip: string;
  netmask: string;
  pool_start: string;
  pool_end: string;
}

export interface DeviceView {
  mac: string;
  ip: string | null;
  boot_arch: string | null;
  info: DeviceInfoView | null;
  profile: string | null;
  /** The profile's configuration mode: keep, default, empty or script. */
  config: string | null;
  version: string | null;
  attempt: number;
  first_seen_ms: number;
  updated_ms: number;
  state: DeviceState;
}

export interface Snapshot {
  running: boolean;
  started: StartInfo | null;
  network: NetPlan | null;
  kernel_version: string | null;
  kernels: KernelState;
  auto_repeat: boolean;
  profile: string | null;
  only_mac: string | null;
  max_parallel: number;
  cooldown_secs: number;
  link_down: string | null;
  devices: DeviceView[];
}

export type EngineEvent =
  | ({ event: "started" } & StartInfo)
  | { event: "stopped" }
  | ({ event: "kernels" } & KernelState)
  | { event: "kernels_ready"; version: string }
  | { event: "download"; file: string; done: number; total: number | null }
  | ({ event: "device"; mac: string } & DeviceState)
  | ({ event: "discovered"; mac: string } & DeviceInfoView)
  | { event: "ignored"; mac: string; reason: string }
  | { event: "forgotten"; mac: string }
  | { event: "only_mac_changed"; mac: string | null }
  | ({ event: "error" } & Failure)
  | ({ event: "warning" } & Failure);

export interface Finding extends Failure {
  severity: "error" | "warning";
}

export interface PreflightReport {
  findings: Finding[];
  network: NetPlan | null;
}

export interface Ipv4View {
  address: string;
  prefix: number;
}

export interface InterfaceView {
  name: string;
  display_name: string | null;
  kind: string;
  mac: string | null;
  ipv4: Ipv4View[];
  up: boolean;
  running: boolean;
  default_route: boolean;
  /** A real wired Ethernet NIC: the only interfaces offered. */
  netinstall_capable: boolean;
  /** How macOS is set up to address it (System Settings → TCP/IP). */
  configured_ipv4: { method: string; address: string | null; prefix: number | null } | null;
  /** The automatic DHCP pool for its manual address. */
  auto_pool: { start: string; end: string } | null;
}

/** The device's configuration after the install (`ni_engine::ConfigMode`). */
export type ConfigMode =
  | { mode: "keep" }
  | { mode: "default" }
  | { mode: "empty" }
  | { mode: "script"; local_name: string };

export const AFTER_INSTALL = ["reboot", "reboot_message", "shutdown"] as const;
export type AfterInstall = (typeof AFTER_INSTALL)[number];

export interface Profile {
  name: string;
  /** A channel (`stable`, `long-term`, `testing`, `development`) or a version. */
  version: string;
  extra_packages: string[];
  config: ConfigMode;
  /** Registered `.rsc` sent as `mode.scr` (not with `keep`). */
  mode_script: string | null;
  remove_branding: boolean;
  /** Key file contents without line breaks. */
  license_key: string | null;
  after_install: AfterInstall;
}

export interface ProfilesView {
  profiles: Profile[];
  broken: { file: string; code: string }[];
}

export interface PackagesView {
  version: string;
  arch: string;
  packages: string[];
}

export interface ScriptView {
  name: string;
  size: number;
}

export interface LicenseKeyView {
  key: string;
}

export type Integrity = "verified" | "unpublished" | "unchecked" | "corrupt" | "missing" | "partial";

/** One cached item (`ni_assets::CachedItem`); `id` is path-free. */
export interface CachedItem {
  id: string;
  kind: "kernels" | "package" | "other" | "partial" | "local";
  name: string;
  arch: string | null;
  size: number;
  integrity: Integrity;
  files: number;
}

export interface CachedVersion {
  version: string;
  size: number;
  items: CachedItem[];
}

export interface CacheView {
  /** For display only (`~/…`). */
  location: string;
  inventory: {
    versions: CachedVersion[];
    partial: CachedItem[];
    local: CachedItem[];
    total: number;
  };
}

export type DeleteScope =
  | { scope: "item"; id: string }
  | { scope: "version"; version: string }
  | { scope: "all" }
  | { scope: "damaged" };

export function damaged(i: CachedItem): boolean {
  return i.integrity === "corrupt" || i.integrity === "missing" || i.integrity === "partial";
}

export interface StartRequest {
  interface: string;
  profile: string | null;
  auto_repeat: boolean;
}

export interface Settings {
  /** Layout version of `settings.json` (Rust writes the current one). */
  version: number;
  language: string | null;
  last_interface: string | null;
  pool_start: string | null;
  pool_end: string | null;
  max_parallel: number;
  cooldown_secs: number;
  auto_repeat: boolean;
  last_profile: string | null;
  download_timeout_secs: number;
}

export const CHANNELS = ["stable", "long-term", "testing", "development"] as const;
export const ACTIVE_STATES = ["preparing", "flashing"] as const;

export function isActive(s: DeviceState): boolean {
  return s.state === "preparing" || s.state === "flashing";
}
