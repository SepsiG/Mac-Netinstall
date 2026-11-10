// The only module that talks to Tauri. Components get a `Backend` (from
// context), so tests can pass a mock instead.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  CacheView,
  DeleteScope,
  EngineEvent,
  Failure,
  InterfaceView,
  LicenseKeyView,
  Link,
  PackageFileView,
  PackagesView,
  PermissionsView,
  PreflightReport,
  Profile,
  ProfilesView,
  ScriptView,
  Settings,
  SettingsPane,
  Snapshot,
  StartInfo,
  StartRequest,
  VersionsView,
} from "./types";

export type Unlisten = () => void;

export interface Backend {
  listInterfaces(): Promise<InterfaceView[]>;
  /** Full preflight: interface, ports, the profile's downloads. */
  preflight(req: StartRequest): Promise<PreflightReport>;
  /** Quick local check (interface, link, address): no ports, no network. */
  checkInterface(req: StartRequest): Promise<PreflightReport>;
  start(req: StartRequest): Promise<StartInfo>;
  stop(): Promise<void>;
  flash(mac: string, profile: string): Promise<void>;
  retry(mac: string): Promise<void>;
  cancel(mac: string): Promise<void>;
  forget(mac: string): Promise<void>;
  setAutoRepeat(on: boolean): Promise<void>;
  setActiveProfile(name: string | null): Promise<void>;
  setOnlyMac(mac: string | null): Promise<void>;
  setMaxParallel(n: number): Promise<void>;
  setCooldown(secs: number): Promise<void>;
  snapshot(): Promise<Snapshot>;
  listProfiles(): Promise<ProfilesView>;
  saveProfile(profile: Profile, previous: string | null): Promise<void>;
  deleteProfile(name: string): Promise<void>;
  availablePackages(version: string, arch: string): Promise<PackagesView>;
  /** Channels and the versions to pick from (`refresh`: fetch the list again). */
  availableVersions(refresh: boolean): Promise<VersionsView>;
  /** Whether MikroTik publishes this version. */
  checkVersionExists(version: string): Promise<boolean>;
  /** Opens a multi-select file dialog for `.npk` files (in Rust) and
   *  registers them; empty if cancelled. */
  pickPackages(): Promise<PackageFileView[]>;
  /** Accept the current disclaimer; returns the stored settings. */
  acceptDisclaimer(): Promise<Settings>;
  /** Open a fixed page (repository, disclaimer, license) in the browser. */
  openLink(link: Link): Promise<void>;
  /** Opens a file dialog (in Rust); `null` if cancelled. */
  pickScript(): Promise<ScriptView | null>;
  /** Opens a file dialog and reads the key file (in Rust); `null` if cancelled. */
  pickLicenseKey(): Promise<LicenseKeyView | null>;
  getSettings(): Promise<Settings>;
  setSettings(settings: Settings): Promise<Settings>;
  /** Remember a dialog's size (validated and stored in Rust). */
  setDialogSize(id: string, width: number, height: number): Promise<void>;
  /** Remember the log panel's height (CSS px of its lines). */
  setLogPanelHeight(height: number): Promise<void>;
  /** Remember that the permissions sheet was gone through. */
  setOnboardingDone(done: boolean): Promise<void>;
  /** Make macOS ask for its permissions now (if it has not), and check them. */
  requestPermissions(): Promise<PermissionsView>;
  /** Open System Settings at a permission's pane. */
  openSystemSettings(pane: SettingsPane): Promise<void>;
  /** `verify`: check every file's SHA-256 (slow); else a quick listing. */
  cacheInventory(verify: boolean): Promise<CacheView>;
  /** Bytes freed. */
  deleteCached(scope: DeleteScope): Promise<number>;
  /** Opens a save dialog (in Rust); `false` if cancelled. */
  exportLog(text: string): Promise<boolean>;
  preferredLanguages(): Promise<string[]>;
  quit(): Promise<void>;
  onEvent(cb: (e: EngineEvent) => void): Promise<Unlisten>;
  /** Events were missed (or the engine restarted): reload the snapshot. */
  onLagged(cb: () => void): Promise<Unlisten>;
  /** Quitting while devices are busy: payload is how many. */
  onCloseRequested(cb: (busy: number) => void): Promise<Unlisten>;
}

/** Errors from commands are `Failure`s; anything else becomes `internal`. */
export function toFailure(e: unknown): Failure {
  if (typeof e === "object" && e !== null && typeof (e as Failure).code === "string") {
    return e as Failure;
  }
  return { code: "internal", params: { detail: String(e) } };
}

async function on<T>(name: string, cb: (payload: T) => void): Promise<Unlisten> {
  const un: UnlistenFn = await listen<T>(name, (e) => cb(e.payload));
  return un;
}

/** The real backend: Tauri commands and events. */
export function tauriBackend(): Backend {
  return {
    listInterfaces: () => invoke("list_interfaces"),
    preflight: (req) => invoke("preflight", { req }),
    checkInterface: (req) => invoke("check_interface", { req }),
    start: (req) => invoke("start", { req }),
    stop: () => invoke("stop"),
    flash: (mac, profile) => invoke("flash", { mac, profile }),
    retry: (mac) => invoke("retry", { mac }),
    cancel: (mac) => invoke("cancel", { mac }),
    forget: (mac) => invoke("forget", { mac }),
    setAutoRepeat: (on) => invoke("set_auto_repeat", { on }),
    setActiveProfile: (name) => invoke("set_active_profile", { name }),
    setOnlyMac: (mac) => invoke("set_only_mac", { mac }),
    setMaxParallel: (n) => invoke("set_max_parallel", { n }),
    setCooldown: (secs) => invoke("set_cooldown", { secs }),
    snapshot: () => invoke("snapshot"),
    listProfiles: () => invoke("list_profiles"),
    saveProfile: (profile, previous) => invoke("save_profile", { profile, previous }),
    deleteProfile: (name) => invoke("delete_profile", { name }),
    availablePackages: (version, arch) => invoke("available_packages", { version, arch }),
    availableVersions: (refresh) => invoke("available_versions", { refresh }),
    checkVersionExists: (version) => invoke("check_version_exists", { version }),
    pickPackages: () => invoke("pick_packages"),
    acceptDisclaimer: () => invoke("accept_disclaimer"),
    openLink: (link) => invoke("open_link", { link }),
    pickScript: () => invoke("pick_script"),
    pickLicenseKey: () => invoke("pick_license_key"),
    getSettings: () => invoke("get_settings"),
    setSettings: (settings) => invoke("set_settings", { settings }),
    setDialogSize: (id, width, height) => invoke("set_dialog_size", { id, width, height }),
    setLogPanelHeight: (height) => invoke("set_log_panel_height", { height }),
    setOnboardingDone: (done) => invoke("set_onboarding_done", { done }),
    requestPermissions: () => invoke("request_permissions"),
    openSystemSettings: (pane) => invoke("open_system_settings", { pane }),
    cacheInventory: (verify) => invoke("cache_inventory", { verify }),
    deleteCached: (scope) => invoke("delete_cached", { scope }),
    exportLog: (text) => invoke("export_log", { text }),
    preferredLanguages: () => invoke("preferred_languages"),
    quit: () => invoke("quit"),
    onEvent: (cb) => on<EngineEvent>("engine-event", cb),
    onLagged: (cb) => on<number>("engine-lagged", () => cb()),
    onCloseRequested: (cb) => on<number>("close-requested", cb),
  };
}
