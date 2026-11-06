import { describe, expect, it } from "vitest";
import { capableInterfaces, defaultInterface } from "../src/lib/interfaces";
import type { InterfaceView } from "../src/lib/types";

function iface(name: string, extra: Partial<InterfaceView> = {}): InterfaceView {
  return {
    name,
    display_name: null,
    kind: "ethernet",
    mac: null,
    ipv4: [],
    up: true,
    running: true,
    default_route: false,
    netinstall_capable: true,
    ...extra,
  };
}

const ip = [{ address: "192.168.88.1", prefix: 24 }];
const TB_PORT = iface("en1", { display_name: "Thunderbolt 1", kind: "thunderbolt_ip", netinstall_capable: false, ipv4: ip });
const WIFI = iface("en0", { kind: "wifi", netinstall_capable: false, ipv4: ip });
const BRIDGE = iface("bridge0", { kind: "bridge", netinstall_capable: false });
const USB_DEVICE_MODE = iface("en2", { display_name: "Ethernet Adapter (en2)", kind: "virtual", netinstall_capable: false });
const USB = iface("en7", { display_name: "USB 10/100/1000 LAN" });
const TB_ADAPTER = iface("en8", { display_name: "Thunderbolt Ethernet Slot 1", ipv4: ip });

describe("capable interfaces", () => {
  it("lists only real wired Ethernet, whatever the bus", () => {
    const list = [TB_PORT, WIFI, BRIDGE, USB_DEVICE_MODE, USB, TB_ADAPTER];
    expect(capableInterfaces(list).map((i) => i.name)).toEqual(["en7", "en8"]);
    expect(capableInterfaces([WIFI, TB_PORT])).toEqual([]);
  });
});

describe("default interface", () => {
  it("prefers the last used one while it is capable", () => {
    expect(defaultInterface([USB, TB_ADAPTER], "en7")).toBe("en7");
    expect(defaultInterface([USB, TB_ADAPTER, TB_PORT], "en1")).toBe("en8");
    expect(defaultInterface([USB, TB_ADAPTER], "en9")).toBe("en8");
  });

  it("then a capable adapter with an address, then any capable one", () => {
    expect(defaultInterface([TB_PORT, USB, TB_ADAPTER], null)).toBe("en8");
    expect(defaultInterface([TB_PORT, WIFI, USB], null)).toBe("en7");
  });

  it("is nothing without a capable interface", () => {
    expect(defaultInterface([WIFI, BRIDGE, TB_PORT, USB_DEVICE_MODE], "en0")).toBeNull();
    expect(defaultInterface([], null)).toBeNull();
  });
});
