// Profile form rules, mirroring ni-engine's validation so most mistakes
// are shown before saving (the backend checks again).
import { CHANNELS, type AfterInstall, type PackageFileView, type PackageSource, type Profile } from "./types";
import { acceptedVersion, supportsModeScript } from "./versions";

/** Architectures RouterOS packages are published for. */
export const ARCHES = ["arm64", "arm", "mipsbe", "mmips", "smips", "ppc", "tile"] as const;

/** The configuration choices, in the order the editor shows them. */
export const CONFIG_CHOICES = ["keep", "default", "empty", "script"] as const;
export type ConfigChoice = (typeof CONFIG_CHOICES)[number];

const NAME = /^[A-Za-z0-9 ._+-]{1,64}$/;
const VERSION = /^\d+\.\d+(\.\d+)?((alpha|beta|rc)\d+)?$/;

export function validName(name: string): boolean {
  return NAME.test(name) && !name.startsWith(".") && !name.startsWith(" ") && !name.endsWith(" ");
}

/** A well-formed version this app accepts (7.x, or 6.x from 6.30). */
export function validVersion(v: string): boolean {
  return VERSION.test(v) && acceptedVersion(v);
}

export function isChannel(v: string): boolean {
  return (CHANNELS as readonly string[]).includes(v);
}

export interface ProfileForm {
  name: string;
  /** The picked channel or version (when not typing one). */
  version: string;
  /** A version typed by hand ("Custom version…"). */
  custom: boolean;
  exact: string;
  source: PackageSource;
  localPackages: string[];
  /** What the headers of files picked in this session say, by name. */
  localInfo: Record<string, PackageFileView>;
  allowBelowMinimum: boolean;
  packages: string[];
  config: ConfigChoice;
  /** The script for `config === "script"`. */
  script: string | null;
  useModeScript: boolean;
  modeScript: string | null;
  removeBranding: boolean;
  licenseKey: string | null;
  afterInstall: AfterInstall;
}

export function toForm(p: Profile | null): ProfileForm {
  if (!p) {
    return {
      name: "",
      version: "stable",
      custom: false,
      exact: "",
      source: "download",
      localPackages: [],
      localInfo: {},
      allowBelowMinimum: false,
      packages: [],
      config: "default",
      script: null,
      useModeScript: false,
      modeScript: null,
      removeBranding: false,
      licenseKey: null,
      afterInstall: "reboot",
    };
  }
  return {
    name: p.name,
    version: p.version,
    custom: false,
    exact: "",
    source: p.package_source ?? "download",
    localPackages: [...(p.local_packages ?? [])],
    localInfo: {},
    allowBelowMinimum: p.allow_below_minimum ?? false,
    packages: [...p.extra_packages],
    config: p.config.mode,
    script: p.config.mode === "script" ? p.config.local_name : null,
    useModeScript: p.mode_script !== null,
    modeScript: p.mode_script,
    removeBranding: p.remove_branding,
    licenseKey: p.license_key,
    afterInstall: p.after_install,
  };
}

type Field = "name" | "version" | "script" | "modeScript" | "local";

/** The version spec the form describes. */
export function formSpec(f: ProfileForm): string {
  return f.custom ? f.exact.trim() : f.version;
}

/** Message keys of the form's problems, by field. */
export function formErrors(f: ProfileForm): Partial<Record<Field, string>> {
  const out: Partial<Record<Field, string>> = {};
  if (!validName(f.name)) out.name = "profile.err_name";
  const local = f.source === "local";
  // Local packages carry their own version: a half-typed one is ignored.
  if (!local && f.custom && !validVersion(f.exact.trim())) out.version = "profile.err_version";
  if (local && f.localPackages.length === 0) out.local = "profile.err_local_none";
  else if (local) {
    const infos = f.localPackages.map((n) => f.localInfo[n]);
    if (infos.every((i) => i !== undefined) && !infos.some((i) => i!.package === "system")) {
      out.local = "profile.err_local_system";
    }
  }
  if (f.config === "script" && !f.script) out.script = "profile.err_script";
  if (f.useModeScript && f.config === "keep") out.modeScript = "profile.err_mode_keep";
  else if (f.useModeScript && !local && !out.version && !supportsModeScript(formSpec(f))) {
    out.modeScript = "profile.err_mode_7_22";
  } else if (f.useModeScript && !f.modeScript) out.modeScript = "profile.err_mode_script";
  return out;
}

export function fromForm(f: ProfileForm): Profile {
  const config: Profile["config"] =
    f.config === "script" ? { mode: "script", local_name: f.script ?? "" } : { mode: f.config };
  return {
    name: f.name,
    version: f.source === "local" && f.custom && !validVersion(f.exact.trim()) ? f.version : formSpec(f),
    extra_packages: [...f.packages],
    config,
    mode_script: f.useModeScript ? f.modeScript : null,
    remove_branding: f.removeBranding,
    license_key: f.licenseKey,
    after_install: f.afterInstall,
    package_source: f.source,
    local_packages: [...f.localPackages],
    allow_below_minimum: f.allowBelowMinimum,
  };
}
