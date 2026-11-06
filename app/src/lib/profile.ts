// Profile form rules, mirroring ni-engine's validation so most mistakes
// are shown before saving (the backend checks again).
import { CHANNELS, type AfterInstall, type Profile } from "./types";

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

export function validVersion(v: string): boolean {
  return VERSION.test(v);
}

export function isChannel(v: string): boolean {
  return (CHANNELS as readonly string[]).includes(v);
}

export interface ProfileForm {
  name: string;
  useChannel: boolean;
  channel: string;
  exact: string;
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
      useChannel: true,
      channel: "stable",
      exact: "",
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
  const channel = isChannel(p.version);
  return {
    name: p.name,
    useChannel: channel,
    channel: channel ? p.version : "stable",
    exact: channel ? "" : p.version,
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

type Field = "name" | "version" | "script" | "modeScript";

/** Message keys of the form's problems, by field. */
export function formErrors(f: ProfileForm): Partial<Record<Field, string>> {
  const out: Partial<Record<Field, string>> = {};
  if (!validName(f.name)) out.name = "profile.err_name";
  if (!f.useChannel && !validVersion(f.exact.trim())) out.version = "profile.err_version";
  if (f.config === "script" && !f.script) out.script = "profile.err_script";
  if (f.useModeScript && f.config === "keep") out.modeScript = "profile.err_mode_keep";
  else if (f.useModeScript && !f.modeScript) out.modeScript = "profile.err_mode_script";
  return out;
}

export function fromForm(f: ProfileForm): Profile {
  const config: Profile["config"] =
    f.config === "script" ? { mode: "script", local_name: f.script ?? "" } : { mode: f.config };
  return {
    name: f.name,
    version: f.useChannel ? f.channel : f.exact.trim(),
    extra_packages: [...f.packages],
    config,
    mode_script: f.useModeScript ? f.modeScript : null,
    remove_branding: f.removeBranding,
    license_key: f.licenseKey,
    after_install: f.afterInstall,
  };
}
