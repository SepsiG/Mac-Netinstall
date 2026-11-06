import { render, screen } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import StatusBar from "../src/components/StatusBar.svelte";
import InterfacePicker from "../src/components/InterfacePicker.svelte";
import { locale } from "../src/lib/i18n";
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
    expect(screen.getByText(/Cable not connected or the device is off/)).toBeInTheDocument();
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

  it("offers to fetch unavailable kernels again", async () => {
    const onretry = vi.fn();
    render(StatusBar, {
      props: {
        ...base,
        running: true,
        kernels: { state: "unavailable", code: "kernels_unavailable", params: { cause: "network" } },
        onretry,
      },
    });
    await userEvent.click(screen.getByRole("button", { name: "Retry download" }));
    expect(onretry).toHaveBeenCalled();
  });

  it("shows blocking findings as things to do until Start is tried", () => {
    const report: PreflightReport = {
      network: null,
      findings: [{ severity: "error", code: "no_ipv4", params: { interface: "en7" } }],
    };
    const { rerender } = render(StatusBar, { props: { ...base, report } });
    const finding = () => screen.getByText("en7 has no IPv4 address.").closest(".finding")!;
    expect(finding()).toHaveClass("todo");
    expect(finding()).not.toHaveClass("error");
    expect(finding()).toHaveTextContent("To do:");
    rerender({ ...base, report, attempted: true });
    expect(finding()).toHaveClass("error");
    expect(finding()).not.toHaveTextContent("To do:");
  });

  it("gives exact steps when the adapter has no manual IPv4 in macOS", () => {
    const report: PreflightReport = {
      network: null,
      findings: [{ severity: "error", code: "no_manual_ipv4", params: { interface: "en7", service: "USB 10/100/1000 LAN" } }],
    };
    render(StatusBar, { props: { ...base, report } });
    const finding = screen.getByText(/has no manual IPv4 address/).closest(".finding")!;
    expect(finding).toHaveClass("todo");
    expect(finding).toHaveTextContent(
      "System Settings → Network → USB 10/100/1000 LAN → Details → TCP/IP, set Configure IPv4 to Manually, IP address 192.168.88.1, subnet mask 255.255.255.0",
    );
    expect(finding).not.toHaveTextContent(/server IP/);
  });

  it("asks to switch a DHCP adapter to a manual address, in Hungarian too", () => {
    locale.set("hu");
    const report: PreflightReport = {
      network: null,
      findings: [{ severity: "error", code: "ipv4_dhcp", params: { interface: "en7", service: "USB LAN" } }],
    };
    render(StatusBar, { props: { ...base, report } });
    const finding = screen.getByText(/DHCP-n keresztül kap IPv4-címet/).closest(".finding")!;
    expect(finding).toHaveTextContent("Teendő:");
    expect(finding).toHaveTextContent("IPv4 konfigurálása: Kézzel");
  });

  it("shows a missing link as information: Start is possible", () => {
    const report: PreflightReport = {
      network: null,
      findings: [{ severity: "warning", code: "link_down", params: { interface: "en7", reason: "no_ip" } }],
    };
    render(StatusBar, { props: { ...base, report } });
    const finding = screen.getByText(/Cable not connected or the device is off/).closest(".finding")!;
    expect(finding).toHaveClass("info");
    expect(finding).not.toHaveClass("warning");
    expect(finding).toHaveTextContent(/you can start now/);
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
  { name: "en7", display_name: "USB LAN", kind: "ethernet", mac: null, ipv4: [{ address: "192.168.88.1", prefix: 24 }], up: true, running: true, default_route: false, netinstall_capable: true, configured_ipv4: null, auto_pool: null },
  { name: "en8", display_name: "Thunderbolt Ethernet Slot 1", kind: "ethernet", mac: null, ipv4: [], up: true, running: true, default_route: false, netinstall_capable: true, configured_ipv4: null, auto_pool: null },
  { name: "en0", display_name: "Wi-Fi", kind: "wifi", mac: null, ipv4: [], up: true, running: true, default_route: true, netinstall_capable: false, configured_ipv4: null, auto_pool: null },
  { name: "en1", display_name: "Thunderbolt 1", kind: "thunderbolt_ip", mac: null, ipv4: [], up: true, running: true, default_route: false, netinstall_capable: false, configured_ipv4: null, auto_pool: null },
  { name: "bridge0", display_name: null, kind: "bridge", mac: null, ipv4: [], up: true, running: true, default_route: false, netinstall_capable: false, configured_ipv4: null, auto_pool: null },
];

describe("InterfacePicker", () => {
  it("has a translated label on the refresh button", () => {
    locale.set("hu");
    render(InterfacePicker, { props: { interfaces: IFACES, selected: "en7", onrefresh: () => {} } });
    expect(screen.getByRole("button", { name: "Csatolólista frissítése" })).toBeInTheDocument();
  });

  it("lists only wired Ethernet adapters, with name and address", () => {
    render(InterfacePicker, { props: { interfaces: IFACES, selected: "en7", onrefresh: () => {} } });
    const options = screen.getAllByRole("option").map((o) => o.textContent);
    expect(options).toEqual(["en7 – USB LAN · 192.168.88.1/24", "en8 – Thunderbolt Ethernet Slot 1 · no IPv4 address"]);
    expect(screen.queryByLabelText(/Show all/)).toBeNull();
  });

  it("says so when there is no usable adapter, and can look again", async () => {
    locale.set("hu");
    const onrefresh = vi.fn();
    render(InterfacePicker, { props: { interfaces: IFACES.slice(2), selected: null, onrefresh } });
    expect(screen.queryByRole("combobox")).toBeNull();
    expect(
      screen.getByText("Nincs használható vezetékes Ethernet-csatoló. Csatlakoztass egy USB vagy Thunderbolt Ethernet-adaptert."),
    ).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Csatolólista frissítése" }));
    expect(onrefresh).toHaveBeenCalled();
  });

  const EN6: InterfaceView = {
    name: "en6",
    display_name: "USB 10/100/1G/2.5G LAN",
    kind: "ethernet",
    mac: null,
    ipv4: [],
    up: true,
    running: true,
    default_route: false,
    netinstall_capable: true,
    configured_ipv4: { method: "manual", address: "172.30.0.2", prefix: 22 },
    auto_pool: { start: "172.30.0.3", end: "172.30.3.254" },
  };

  it("shows the configured address when the cable is out, and the link state", () => {
    locale.set("hu");
    render(InterfacePicker, { props: { interfaces: [EN6], selected: "en6", onrefresh: () => {} } });
    expect(screen.getByRole("option").textContent).toBe("en6 – USB 10/100/1G/2.5G LAN · 172.30.0.2/22");
    const state = screen.getByTestId("ipv4-state");
    expect(state).toHaveTextContent("Kézi: 172.30.0.2/22");
    expect(state).toHaveTextContent("nincs link – nincs bedugva a kábel?");
  });

  it.each<[InterfaceView["configured_ipv4"], string[], string]>([
    [{ method: "manual", address: "172.30.0.2", prefix: 22 }, ["172.30.0.2"], "Manual: 172.30.0.2/22 · link up"],
    [{ method: "dhcp", address: null, prefix: null }, [], "DHCP – set it to manual · no link – cable not plugged in?"],
    [null, [], "No IPv4 configured · no link – cable not plugged in?"],
  ])("describes %o", (configured, live, text) => {
    const i: InterfaceView = { ...EN6, configured_ipv4: configured, ipv4: live.map((address) => ({ address, prefix: 22 })) };
    render(InterfacePicker, { props: { interfaces: [i], selected: "en6", onrefresh: () => {} } });
    expect(screen.getByTestId("ipv4-state")).toHaveTextContent(text);
  });
});
