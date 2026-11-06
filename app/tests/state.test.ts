import { describe, expect, it } from "vitest";
import { EngineState, MAX_LOG } from "../src/lib/state.svelte";
import type { Snapshot } from "../src/lib/types";

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
    expect(s.log.length).toBe(5); // started, bootp_seen, discovered, preparing, done
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

  it("keeps the log bounded", () => {
    const s = new EngineState();
    for (let i = 0; i < MAX_LOG + 5; i++) s.apply({ event: "ignored", mac: A, reason: "cooldown" }, i);
    expect(s.log.length).toBe(MAX_LOG);
    expect(s.log[0]!.at).toBe(5);
    s.clearLog();
    expect(s.log).toEqual([]);
  });
});
