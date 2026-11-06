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
  | { state: "preparing"; version?: string }
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
  server_mac: string;
}

export interface DeviceView {
  mac: string;
  ip: string | null;
  boot_arch: string | null;
  info: DeviceInfoView | null;
  profile: string | null;
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
  /** A Thunderbolt networking port (`Thunderbolt 1`), not an adapter. */
  thunderbolt: boolean;
}

export type ConfigMode = { mode: "default" } | { mode: "custom_script"; local_name: string };

export interface Profile {
  name: string;
  /** A channel (`stable`, `long-term`, `testing`, `development`) or a version. */
  version: string;
  extra_packages: string[];
  config: ConfigMode;
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

export interface CacheInfo {
  location: string;
  bytes: number;
}

export interface StartRequest {
  interface: string;
  profile: string | null;
  auto_repeat: boolean;
}

export interface Settings {
  language: string | null;
  last_interface: string | null;
  show_all_interfaces: boolean;
  server_ip: string | null;
  netmask: string | null;
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
