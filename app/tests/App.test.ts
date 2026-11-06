import { render, screen, waitFor, within } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import App from "../src/App.svelte";
import { mockBackend, SETTINGS, IDLE } from "./mock";

const A = "48:A9:8A:00:00:0A";

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
    // Wi-Fi is hidden until "Show all".
    expect(screen.queryByRole("option", { name: /en0/ })).toBeNull();
    await userEvent.click(screen.getByLabelText("Show all"));
    expect(screen.getByRole("option", { name: /en0/ })).toBeInTheDocument();
    await waitFor(() =>
      expect(backend.setSettings).toHaveBeenCalledWith(expect.objectContaining({ show_all_interfaces: true })),
    );
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

  it("blocks Start on preflight errors and shows the advice", async () => {
    await start({
      preflight: vi.fn(async () => ({
        network: null,
        findings: [{ severity: "error" as const, code: "no_ipv4", params: { interface: "en7" } }],
      })),
    });
    expect(await screen.findByText("en7 has no IPv4 address.")).toBeInTheDocument();
    expect(screen.getByText(/Configure IPv4: Manually/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Start" })).toBeDisabled();
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
    expect(dialog).toHaveTextContent("2 devices are being flashed right now.");
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
});
