// Profile form rules, mirroring ni-engine's validation so most mistakes
// are shown before saving (the backend checks again).
import { CHANNELS, type Profile } from "./types";

/** Architectures RouterOS packages are published for. */
export const ARCHES = ["arm64", "arm", "mipsbe", "mmips", "smips", "ppc", "tile"] as const;

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
  useScript: boolean;
  script: string | null;
}

export function toForm(p: Profile | null): ProfileForm {
  if (!p) {
    return { name: "", useChannel: true, channel: "stable", exact: "", packages: [], useScript: false, script: null };
  }
  const channel = isChannel(p.version);
  return {
    name: p.name,
    useChannel: channel,
    channel: channel ? p.version : "stable",
    exact: channel ? "" : p.version,
    packages: [...p.extra_packages],
    useScript: p.config.mode === "custom_script",
    script: p.config.mode === "custom_script" ? p.config.local_name : null,
  };
}

/** Message keys of the form's problems, by field. */
export function formErrors(f: ProfileForm): Partial<Record<"name" | "version" | "script", string>> {
  const out: Partial<Record<"name" | "version" | "script", string>> = {};
  if (!validName(f.name)) out.name = "profile.err_name";
  if (!f.useChannel && !validVersion(f.exact.trim())) out.version = "profile.err_version";
  if (f.useScript && !f.script) out.script = "profile.err_script";
  return out;
}

export function fromForm(f: ProfileForm): Profile {
  return {
    name: f.name,
    version: f.useChannel ? f.channel : f.exact.trim(),
    extra_packages: [...f.packages],
    config: f.useScript && f.script ? { mode: "custom_script", local_name: f.script } : { mode: "default" },
  };
}
