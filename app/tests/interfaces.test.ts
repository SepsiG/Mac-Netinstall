import { describe, expect, it } from "vitest";
import { defaultInterface, orderInterfaces, visibleInterfaces } from "../src/lib/interfaces";
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
    thunderbolt: false,
    ...extra,
  };
}

const ip = [{ address: "192.168.88.1", prefix: 24 }];
const TB1 = iface("en1", { display_name: "Thunderbolt 1", thunderbolt: true, ipv4: ip });
const TB2 = iface("en4", { display_name: "Thunderbolt 2", thunderbolt: true });
const WIFI = iface("en0", { kind: "wifi", ipv4: ip });
const BRIDGE = iface("bridge0", { kind: "bridge" });
const USB = iface("en7", { display_name: "USB 10/100/1000 LAN" });
const USB_IP = iface("en8", { display_name: "USB 10/100/1000 LAN", ipv4: ip });

describe("interface order", () => {
  it("puts Ethernet adapters first and Thunderbolt ports last", () => {
    const list = [TB1, WIFI, BRIDGE, TB2, USB, USB_IP];
    expect(orderInterfaces(list).map((i) => i.name)).toEqual(["en7", "en8", "en0", "bridge0", "en1", "en4"]);
    expect(visibleInterfaces(list, false, null).map((i) => i.name)).toEqual(["en7", "en8", "en1", "en4"]);
  });
});

describe("default interface", () => {
  it("prefers the last used one, if it is listed", () => {
    expect(defaultInterface([USB, USB_IP, TB1], "en1")).toBe("en1");
    expect(defaultInterface([USB, USB_IP], "en9")).toBe("en8");
  });

  it("then an Ethernet adapter with an address, then any adapter", () => {
    expect(defaultInterface([TB1, USB, USB_IP], null)).toBe("en8");
    expect(defaultInterface([TB1, WIFI, USB], null)).toBe("en7");
  });

  it("never picks Wi-Fi; a Thunderbolt port only if nothing else is there", () => {
    expect(defaultInterface([WIFI, BRIDGE, TB1], null)).toBe("en1");
    expect(defaultInterface([WIFI, BRIDGE], null)).toBeNull();
    expect(defaultInterface([], null)).toBeNull();
  });
});
