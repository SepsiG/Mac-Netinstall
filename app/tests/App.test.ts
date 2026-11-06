import { render, screen, waitFor, within } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import App from "../src/App.svelte";
import type { Backend } from "../src/lib/api";
import type { DeviceState, DeviceView, Profile, Snapshot } from "../src/lib/types";
import { mockBackend, SETTINGS, IDLE } from "./mock";

const A = "48:A9:8A:00:00:0A";
const B = "48:A9:8A:00:00:0B";
const LAB: Profile = { name: "lab", version: "stable", extra_packages: [], config: { mode: "default" } };
const OTHER: Profile = { ...LAB, name: "other" };
const FORMATTING: DeviceState = { state: "flashing", version: "7.25.1", phase: "formatting", sent: 0, total: 100 };

function view(mac: string, state: DeviceState): DeviceView {
  return {
    mac,
    ip: "192.168.88.2",
    boot_arch: "arm64",
    info: { model: "RB5009UG+S+", arch: "arm64", license_id: "X", min_os: "7" },
    profile: "lab",
    version: "7.25.1",
    attempt: 1,
    first_seen_ms: Date.now(),
    updated_ms: Date.now(),
    state,
  };
}

/** The app attached to a running engine (snapshot fields in `snap`). */
async function running(overrides: Partial<Backend> & Partial<Snapshot> = {}) {
  const { devices = [], kernels = { state: "ready", version: "7.25.1", offline: false }, ...rest } = overrides;
  const backend = mockBackend({
    snapshot: vi.fn(async () => ({
      ...IDLE,
      running: true,
      started: { interface: "en7", server_ip: "192.168.88.1", bootp: "", tftp: "", netinstall: "" },
      kernels,
      profile: "lab",
      auto_repeat: true,
      max_parallel: 4,
      cooldown_secs: 300,
      devices,
    })),
    ...(rest as Partial<Backend>),
  });
  render(App, { props: { backend } });
  await screen.findByRole("button", { name: "Stop" });
  return backend;
}

async function start(overrides = {}) {
  const backend = mockBackend(overrides);
  render(App, { props: { backend } });
  await waitFor(() => expect(backend.preflight).toHaveBeenCalled());
  return backend;
}

describe("App", () => {
  it("picks the Ethernet interface and runs preflight", async () => {
    const backend = await start();
    expect(screen.getByLabelText("Interface")).toHaveValue("en7");
    expect(backend.preflight).toHaveBeenLastCalledWith({ interface: "en7", profile: null, auto_repeat: false });
    expect(await screen.findByText(/Ready to start/)).toBeInTheDocument();
    // Wi-Fi is never offered.
    expect(screen.queryByRole("option", { name: /en0/ })).toBeNull();
  });

  it("follows the macOS language and switches in Settings", async () => {
    const backend = await start({
      getSettings: vi.fn(async () => ({ ...SETTINGS, language: null })),
      preferredLanguages: vi.fn(async () => ["hu-HU", "en-US"]),
    });
    expect(await screen.findByRole("button", { name: "Indítás" })).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Beállítások" }));
    await userEvent.selectOptions(screen.getByLabelText("Nyelv"), "en");
    await userEvent.click(screen.getByRole("button", { name: "Mentés" }));
    expect(backend.setSettings).toHaveBeenCalledWith(expect.objectContaining({ language: "en" }));
    expect(await screen.findByRole("button", { name: "Start" })).toBeInTheDocument();
  });

  it("starts, shows devices from events, and stops", async () => {
    const backend = await start({
      getSettings: vi.fn(async () => ({ ...SETTINGS, last_profile: "lab", auto_repeat: true })),
    });
    await waitFor(() => expect(screen.getByLabelText("Profile")).toHaveValue("lab"));
    await userEvent.click(screen.getByRole("button", { name: "Start" }));
    expect(backend.start).toHaveBeenCalledWith({ interface: "en7", profile: "lab", auto_repeat: true });
    backend.emit({ event: "started", interface: "en7", server_ip: "192.168.88.1", bootp: "", tftp: "", netinstall: "" });
    backend.emit({ event: "kernels", state: "ready", version: "7.25.1", offline: false });
    backend.emit({ event: "device", mac: A, state: "flashing", version: "7.25.1", phase: "uploading", sent: 40, total: 100 });
    const row = (await screen.findByText(A)).closest("tr")!;
    expect(within(row).getByText("40%")).toBeInTheDocument();
    expect(screen.getByText(/RouterOS 7.25.1/)).toBeInTheDocument();
    backend.emit({ event: "device", mac: A, state: "done" });
    expect(await within(row).findByText(/Done/)).toBeInTheDocument();
    expect(screen.getAllByText(/Serving on en7/).length).toBeGreaterThan(0);
    await userEvent.click(screen.getByRole("button", { name: "Stop" }));
    expect(backend.stop).toHaveBeenCalled();
  });

  it("changing the profile while running tells the engine", async () => {
    const backend = mockBackend({
      snapshot: vi.fn(async () => ({ ...IDLE, running: true, profile: null })),
    });
    render(App, { props: { backend } });
    await waitFor(() => expect(screen.getByRole("button", { name: "Stop" })).toBeInTheDocument());
    await userEvent.selectOptions(screen.getByLabelText("Profile"), "lab");
    expect(backend.setActiveProfile).toHaveBeenCalledWith("lab");
  });

  it("shows preflight problems as things to do, and as errors once Start is tried", async () => {
    const backend = await start({
      preflight: vi.fn(async () => ({
        network: null,
        findings: [{ severity: "error" as const, code: "no_ipv4", params: { interface: "en7" } }],
      })),
    });
    const finding = async () => (await screen.findByText("en7 has no IPv4 address.")).closest(".finding")!;
    expect(await finding()).toHaveTextContent("To do:");
    expect(await finding()).not.toHaveClass("error");
    expect(screen.getByText(/Configure IPv4: Manually/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Start" }));
    expect(backend.start).not.toHaveBeenCalled();
    expect(await finding()).toHaveClass("error");
    expect(await finding()).not.toHaveTextContent("To do:");
  });

  it("shows command errors in a banner", async () => {
    await start({ start: vi.fn(async () => Promise.reject({ code: "port_in_use", params: { service: "tftp", port: "69" } })) });
    await userEvent.click(screen.getByRole("button", { name: "Start" }));
    const banner = await screen.findByRole("alert");
    expect(banner).toHaveTextContent("Port 69 (TFTP) is used by another program.");
    expect(banner).toHaveTextContent("Internet Sharing");
  });

  it("asks before quitting while devices are flashed", async () => {
    const backend = await start();
    backend.emitCloseRequested(2);
    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent("2 devices are being prepared or flashed right now.");
    expect(document.querySelector("main")).toHaveAttribute("inert");
    await userEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
    expect(backend.quit).not.toHaveBeenCalled();
    backend.emitCloseRequested(1);
    await userEvent.click(await screen.findByRole("button", { name: "Quit anyway" }));
    expect(backend.quit).toHaveBeenCalled();
  });

  it("reloads the snapshot after missed events", async () => {
    const backend = await start();
    const calls = vi.mocked(backend.snapshot).mock.calls.length;
    backend.emitLagged();
    await waitFor(() => expect(vi.mocked(backend.snapshot).mock.calls.length).toBe(calls + 1));
  });

  it("opens the profile editor and selects the saved profile", async () => {
    const backend = await start();
    await userEvent.click(screen.getByRole("button", { name: "New…" }));
    await userEvent.type(screen.getByLabelText("Name"), "fresh");
    vi.mocked(backend.listProfiles).mockResolvedValueOnce({
      profiles: [
        { name: "fresh", version: "stable", extra_packages: [], config: { mode: "default" } },
        { name: "lab", version: "stable", extra_packages: [], config: { mode: "default" } },
      ],
      broken: [],
    });
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(screen.getByLabelText("Profile")).toHaveValue("fresh"));
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("asks before stopping while devices are prepared or flashed", async () => {
    const backend = await running({ devices: [view(A, FORMATTING), view(B, { state: "done" })] });
    await userEvent.click(screen.getByRole("button", { name: "Stop" }));
    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent("1 device is being prepared or flashed right now.");
    expect(within(dialog).getByRole("listitem")).toHaveTextContent(`${A} · RB5009UG+S+`);
    expect(within(dialog).getByRole("button", { name: "Keep going" })).toHaveFocus();
    expect(backend.stop).not.toHaveBeenCalled();
    await userEvent.click(within(dialog).getByRole("button", { name: "Keep going" }));
    expect(backend.stop).not.toHaveBeenCalled();
    expect(screen.getByText(A)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Stop" }));
    await userEvent.click(within(await screen.findByRole("dialog")).getByRole("button", { name: "Stop anyway" }));
    expect(backend.stop).toHaveBeenCalled();
  });

  it("removes a forgotten device from the table", async () => {
    const backend = await running({ devices: [view(A, { state: "failed", code: "cancelled" })] });
    vi.mocked(backend.forget).mockImplementation(async (mac) => backend.emit({ event: "forgotten", mac }));
    const row = (await screen.findByText(A)).closest("tr")!;
    await userEvent.click(within(row).getByRole("button", { name: "Forget" }));
    expect(backend.forget).toHaveBeenCalledWith(A);
    await waitFor(() => expect(document.querySelector(`tr[data-mac="${A}"]`)).toBeNull());
  });

  it("shows when only one device is served", async () => {
    const backend = await running({ devices: [view(A, { state: "waiting", reason: "no_profile" })] });
    vi.mocked(backend.setOnlyMac).mockImplementation(async (mac) => backend.emit({ event: "only_mac_changed", mac }));
    const status = screen.getByRole("status");
    await userEvent.click(await screen.findByRole("button", { name: "Only this" }));
    expect(await within(status).findByText(`· Serving only ${A}`)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Serve all" }));
    await waitFor(() => expect(within(status).queryByText(/Serving only/)).toBeNull());
    expect(screen.getByRole("button", { name: "Only this" })).toBeInTheDocument();
  });

  it("keeps the previous profile when the engine refuses a new one", async () => {
    await running({
      listProfiles: vi.fn(async () => ({ profiles: [LAB, OTHER], broken: [] })),
      setActiveProfile: vi.fn(async () => Promise.reject({ code: "invalid_package_name", params: { name: "x" } })),
    });
    const picker = screen.getByLabelText("Profile");
    await waitFor(() => expect(picker).toHaveValue("lab"));
    await userEvent.selectOptions(picker, "other");
    await waitFor(() => expect(picker).toHaveValue("lab"));
    expect(await screen.findByRole("alert")).toBeInTheDocument();
  });

  it("saving another profile while running keeps the active one", async () => {
    const backend = await running({});
    await waitFor(() => expect(screen.getByLabelText("Profile")).toHaveValue("lab"));
    await userEvent.click(screen.getByRole("button", { name: "New…" }));
    await userEvent.type(screen.getByLabelText("Name"), "fresh");
    vi.mocked(backend.listProfiles).mockResolvedValue({ profiles: [{ ...LAB, name: "fresh" }, LAB], broken: [] });
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(screen.getByLabelText("Profile")).toHaveValue("lab");
    expect(backend.setActiveProfile).not.toHaveBeenCalled();
  });

  it("saving the active profile while running applies it again (also renamed)", async () => {
    const backend = await running({});
    await waitFor(() => expect(screen.getByLabelText("Profile")).toHaveValue("lab"));
    await userEvent.click(screen.getByRole("button", { name: "Edit…" }));
    await userEvent.clear(screen.getByLabelText("Name"));
    await userEvent.type(screen.getByLabelText("Name"), "lab 2");
    vi.mocked(backend.listProfiles).mockResolvedValue({ profiles: [{ ...LAB, name: "lab 2" }], broken: [] });
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(backend.setActiveProfile).toHaveBeenCalledWith("lab 2"));
    await waitFor(() => expect(screen.getByLabelText("Profile")).toHaveValue("lab 2"));
  });

  it("fetches unavailable kernels again on request", async () => {
    const backend = await running({
      kernels: { state: "unavailable", code: "kernels_unavailable", params: { cause: "network" } },
    });
    await userEvent.click(screen.getByRole("button", { name: "Retry download" }));
    expect(backend.setActiveProfile).toHaveBeenCalledWith("lab");
    backend.emit({ event: "kernels", state: "downloading", spec: "stable", done: 0, total: null });
    expect(await screen.findByText(/fetching stable/)).toBeInTheDocument();
  });

  it("keeps the last devices when a resync finds the engine stopped", async () => {
    const backend = await start();
    backend.emit({ event: "started", interface: "en7", server_ip: "192.168.88.1", bootp: "", tftp: "", netinstall: "" });
    backend.emit({ event: "device", mac: A, state: "done" });
    backend.emit({ event: "stopped" });
    const row = () => document.querySelector(`tr[data-mac="${A}"]`);
    await waitFor(() => expect(row()).not.toBeNull());
    const calls = vi.mocked(backend.snapshot).mock.calls.length;
    backend.emitLagged();
    await waitFor(() => expect(vi.mocked(backend.snapshot).mock.calls.length).toBe(calls + 1));
    expect(row()).not.toBeNull();
    expect(screen.getByText(/Last known state/)).toBeInTheDocument();
  });

  it("selects a capable adapter with an address; ports and Wi-Fi are not offered", async () => {
    const eth = (name: string, extra = {}) => ({
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
    });
    await start({
      listInterfaces: vi.fn(async () => [
        eth("en1", { display_name: "Thunderbolt 1", kind: "thunderbolt_ip", netinstall_capable: false, ipv4: [{ address: "169.254.1.1", prefix: 16 }] }),
        eth("en7"),
        eth("en8", { display_name: "Thunderbolt Ethernet Slot 1", ipv4: [{ address: "192.168.88.1", prefix: 24 }] }),
      ]),
    });
    const picker = screen.getByLabelText("Interface");
    await waitFor(() => expect(picker).toHaveValue("en8"));
    const names = within(picker).getAllByRole("option").map((o) => (o as HTMLOptionElement).value);
    expect(names).toEqual(["en7", "en8"]);
  });

  it("explains when there is no usable adapter", async () => {
    const backend = mockBackend({
      listInterfaces: vi.fn(async () => [
        { name: "en0", display_name: "Wi-Fi", kind: "wifi", mac: null, ipv4: [], up: true, running: true, default_route: true, netinstall_capable: false },
      ]),
    });
    render(App, { props: { backend } });
    expect(await screen.findByText(/No usable wired Ethernet interface/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Start" })).toBeDisabled();
    expect(backend.preflight).not.toHaveBeenCalled();
  });

  it("opens Settings with the stored settings, not a stale copy", async () => {
    const backend = await start();
    vi.mocked(backend.getSettings).mockResolvedValueOnce({ ...SETTINGS, cooldown_secs: 600 });
    await userEvent.click(screen.getByRole("button", { name: "Settings" }));
    expect(await screen.findByLabelText(/Ignore a flashed device/)).toHaveValue("600");
  });
});
