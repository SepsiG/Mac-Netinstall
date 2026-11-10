import { describe, expect, it } from "vitest";
import { EngineState, MAX_LOG } from "../src/lib/state.svelte";
import type { Snapshot } from "../src/lib/types";
import { IDLE } from "./mock";

const A = "48:A9:8A:00:00:0A";
const B = "48:A9:8A:00:00:0B";

function snapshot(): Snapshot {
  return {
    running: true,
    started: null,
    network: null,
    kernel_version: "7.25.1",
    kernels: { state: "ready", version: "7.25.1", offline: false },
    auto_repeat: true,
    profile: "lab",
    only_mac: null,
    max_parallel: 4,
    cooldown_secs: 300,
    link_down: "no_ip",
    devices: [
      {
        mac: B,
        ip: "192.168.88.3",
        boot_arch: "arm64",
        info: null,
        profile: null,
        config: null,
        version: null,
        attempt: 1,
        first_seen_ms: 1,
        updated_ms: 2,
        state: { state: "done" },
      },
    ],
  };
}

describe("EngineState", () => {
  it("drops forgotten devices and follows the only-MAC selection", () => {
    const s = new EngineState();
    s.applySnapshot(snapshot());
    expect(s.cooldownSecs).toBe(300);
    s.apply({ event: "only_mac_changed", mac: B });
    expect(s.onlyMac).toBe(B);
    s.apply({ event: "forgotten", mac: B });
    expect(s.rows).toEqual([]);
    s.apply({ event: "only_mac_changed", mac: null });
    expect(s.onlyMac).toBeNull();
  });

  it("keeps the last rows (stale) when a snapshot of a stopped engine has none", () => {
    const s = new EngineState();
    s.applySnapshot(snapshot());
    s.apply({ event: "stopped" });
    expect(s.stale).toBe(true);
    s.applySnapshot({ ...snapshot(), running: false, devices: [] });
    expect(s.rows.map((r) => r.mac)).toEqual([B]);
    expect(s.stale).toBe(true);
    s.apply({ event: "started", interface: "en7", server_ip: "192.168.88.1", bootp: "", tftp: "", netinstall: "" });
    expect(s.rows).toEqual([]);
    expect(s.stale).toBe(false);
  });

  it("loads a snapshot", () => {
    const s = new EngineState();
    s.applySnapshot(snapshot());
    expect(s.running).toBe(true);
    expect(s.kernels).toEqual({ state: "ready", version: "7.25.1", offline: false });
    expect(s.linkDown).toBe("no_ip");
    expect(s.rows.map((d) => d.mac)).toEqual([B]);
  });

  it("follows a device through its states", () => {
    const s = new EngineState();
    s.apply({ event: "started", interface: "en7", server_ip: "192.168.88.1", bootp: "", tftp: "", netinstall: "" });
    s.apply({ event: "device", mac: A, state: "bootp_seen", boot_arch: "arm64", ip: "192.168.88.2" }, 10);
    s.apply({ event: "discovered", mac: A, model: "RB5009", arch: "arm64", license_id: "X", min_os: "7" });
    s.apply({ event: "device", mac: A, state: "preparing", version: "7.25.1", config: "script" });
    s.apply({ event: "device", mac: A, state: "flashing", version: "7.25.1", phase: "uploading", sent: 50, total: 100 }, 20);
    const d = s.devices[A]!;
    expect(d.ip).toBe("192.168.88.2");
    expect(d.info?.model).toBe("RB5009");
    expect(d.version).toBe("7.25.1");
    expect(d.config).toBe("script");
    expect(d.attempt).toBe(1);
    expect(d.updated_ms).toBe(20);
    expect(s.busy).toBe(1);
    s.apply({ event: "device", mac: A, state: "done" });
    expect(s.busy).toBe(0);
    expect(s.log.lines.length).toBe(5); // started, bootp_seen, discovered, preparing, done
  });

  it("keeps where the packages come from, and the version of local packages", () => {
    const s = new EngineState();
    s.apply({ event: "started", interface: "en7", server_ip: "192.168.88.1", bootp: "", tftp: "", netinstall: "" });
    s.apply({ event: "device", mac: A, state: "preparing", config: "default", source: "local" });
    expect(s.devices[A]!.source).toBe("local");
    s.apply({ event: "device", mac: A, state: "flashing", version: "7.24.6", phase: "offering", sent: 0, total: 9 });
    expect(s.devices[A]!.version).toBe("7.24.6");
    s.apply({ event: "device", mac: A, state: "bootp_seen", boot_arch: "arm64", ip: "192.168.88.2" });
    expect(s.devices[A]!.source).toBeNull();
    const snap = snapshot();
    snap.devices = [{ ...snap.devices[0]!, source: "download" }];
    s.applySnapshot(snap);
    expect(s.devices[B]!.source).toBe("download");
  });

  it("forgets the configuration of the previous round", () => {
    for (const next of ["bootp_seen", "discovered"] as const) {
      const s = new EngineState();
      s.apply({ event: "started", interface: "en7", server_ip: "192.168.88.1", bootp: "", tftp: "", netinstall: "" });
      s.apply({ event: "device", mac: A, state: "preparing", version: "7.25.1", config: "script" });
      s.apply({ event: "device", mac: A, state: "done" });
      expect(s.devices[A]!.config).toBe("script");
      if (next === "bootp_seen") {
        s.apply({ event: "device", mac: A, state: "bootp_seen", boot_arch: "arm64", ip: "192.168.88.2" });
      } else {
        s.apply({ event: "device", mac: A, state: "discovered" });
      }
      expect(s.devices[A]!.config, next).toBeNull();
    }
  });

  it("tracks kernels, link and stop", () => {
    const s = new EngineState();
    s.apply({ event: "started", interface: "en7", server_ip: "1.1.1.1", bootp: "", tftp: "", netinstall: "" });
    s.apply({ event: "kernels", state: "downloading", spec: "stable", done: 5, total: 10 });
    expect(s.kernels).toEqual({ state: "downloading", spec: "stable", done: 5, total: 10 });
    s.apply({ event: "warning", code: "link_down", params: { interface: "en7", reason: "no_carrier" } });
    expect(s.linkDown).toBe("no_carrier");
    s.apply({ event: "warning", code: "link_up", params: { interface: "en7" } });
    expect(s.linkDown).toBeNull();
    s.apply({ event: "stopped" });
    expect(s.running).toBe(false);
    expect(s.kernels.state).toBe("idle");
  });

  it("keeps the log bounded, counting every line ever added", () => {
    const s = new EngineState();
    const v0 = s.log.version;
    for (let i = 0; i < MAX_LOG + 5; i++) s.apply({ event: "ignored", mac: A, reason: "cooldown" }, i);
    expect(s.log.lines.length).toBe(MAX_LOG);
    expect(s.log.lines[0]!.at).toBe(5);
    expect(s.log.added).toBe(MAX_LOG + 5);
    expect(s.log.version).toBeGreaterThan(v0);
    const v1 = s.log.version;
    s.clearLog();
    expect(s.log.lines).toEqual([]);
    expect(s.log.added).toBe(MAX_LOG + 5);
    expect(s.log.version).toBeGreaterThan(v1);
  });

  it("keeps far more lines than a burst of 5000 for the export", () => {
    expect(MAX_LOG).toBeGreaterThanOrEqual(20_000);
  });

  it("knows when macOS blocks local network access", () => {
    const s = new EngineState();
    s.apply({ event: "started", interface: "en7", server_ip: "192.168.88.1", bootp: "", tftp: "", netinstall: "" });
    expect(s.localNetworkDenied).toBe(false);
    s.apply({ event: "warning", code: "local_network_denied", params: { service: "bootp" } });
    expect(s.localNetworkDenied).toBe(true);
    s.apply({ event: "warning", code: "local_network_allowed" });
    expect(s.localNetworkDenied).toBe(false);
    s.apply({ event: "warning", code: "local_network_denied", params: { service: "bootp" } });
    s.apply({ event: "stopped" });
    expect(s.localNetworkDenied).toBe(false);
    s.applySnapshot({ ...IDLE, running: true, local_network_denied: true });
    expect(s.localNetworkDenied).toBe(true);
  });
});
