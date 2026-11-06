import { render, screen } from "@testing-library/svelte";
import { beforeEach, describe, expect, it } from "vitest";
import StatusBar from "../src/components/StatusBar.svelte";
import InterfacePicker from "../src/components/InterfacePicker.svelte";
import { locale } from "../src/lib/i18n";
import { visibleInterfaces } from "../src/lib/interfaces";
import type { InterfaceView, KernelState, PreflightReport } from "../src/lib/types";

const base = {
  running: false,
  started: null,
  kernels: { state: "idle" } as KernelState,
  linkDown: null,
  onlyMac: null,
  report: null as PreflightReport | null,
  checking: false,
};

beforeEach(() => locale.set("en"));

describe("StatusBar", () => {
  it("shows preflight findings with advice", () => {
    render(StatusBar, {
      props: {
        ...base,
        report: {
          network: null,
          findings: [
            { severity: "error", code: "port_in_use", params: { service: "bootp", port: "67" } },
            { severity: "warning", code: "link_down", params: { interface: "en7", reason: "no_ip" } },
          ],
        },
      },
    });
    expect(screen.getByText("Port 67 (BOOTP) is used by another program.")).toBeInTheDocument();
    expect(screen.getByText(/most likely Internet Sharing/)).toBeInTheDocument();
    expect(screen.getByText("en7: no link (no IPv4 address (no cable?)).")).toBeInTheDocument();
    expect(screen.queryByText(/Ready to start/)).toBeNull();
  });

  it("says ready when nothing was found, in Hungarian too", () => {
    locale.set("hu");
    render(StatusBar, { props: { ...base, report: { findings: [], network: null } } });
    expect(screen.getByText(/Indításra kész/)).toBeInTheDocument();
  });

  it("shows the running state, link and kernels", () => {
    render(StatusBar, {
      props: {
        ...base,
        running: true,
        started: { interface: "en7", server_ip: "192.168.88.1", bootp: "", tftp: "", netinstall: "" },
        kernels: { state: "downloading", spec: "stable", done: 25, total: 100 },
        linkDown: "no_carrier",
        onlyMac: "48:A9:8A:00:00:0A",
      },
    });
    expect(screen.getByText("Serving on en7 (192.168.88.1)")).toBeInTheDocument();
    expect(screen.getByText(/No link: no cable/)).toBeInTheDocument();
    expect(screen.getByText(/fetching stable… 25%/)).toBeInTheDocument();
    expect(screen.getByText(/Serving only 48:A9:8A:00:00:0A/)).toBeInTheDocument();
  });

  it("explains missing kernels", () => {
    render(StatusBar, {
      props: { ...base, running: true, kernels: { state: "unavailable", code: "kernels_unavailable", params: { cause: "network" } } },
    });
    expect(screen.getByText(/No boot kernels: no connection to mikrotik.com/)).toBeInTheDocument();
    expect(screen.getByText(/Connect to the internet once/)).toBeInTheDocument();
  });
});

const IFACES: InterfaceView[] = [
  { name: "en7", display_name: "USB LAN", kind: "ethernet", mac: null, ipv4: [{ address: "192.168.88.1", prefix: 24 }], up: true, running: true, default_route: false },
  { name: "en0", display_name: "Wi-Fi", kind: "wifi", mac: null, ipv4: [], up: true, running: true, default_route: true },
  { name: "bridge0", display_name: null, kind: "bridge", mac: null, ipv4: [], up: true, running: true, default_route: false },
];

describe("InterfacePicker", () => {
  it("hides Wi-Fi, bridges and the like unless asked", () => {
    expect(visibleInterfaces(IFACES, false, null).map((i) => i.name)).toEqual(["en7"]);
    expect(visibleInterfaces(IFACES, false, "en0").map((i) => i.name)).toEqual(["en7", "en0"]);
    expect(visibleInterfaces(IFACES, true, null)).toHaveLength(3);
  });

  it("labels interfaces with name, address and kind", () => {
    render(InterfacePicker, {
      props: { interfaces: IFACES, selected: "en7", showAll: true, onrefresh: () => {} },
    });
    const options = screen.getAllByRole("option").map((o) => o.textContent);
    expect(options).toEqual([
      "en7 – USB LAN · 192.168.88.1/24",
      "en0 – Wi-Fi · no IPv4 address · Wi-Fi",
      "bridge0 · no IPv4 address · bridge",
    ]);
  });
});
