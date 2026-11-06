import { render, screen } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import SettingsDialog from "../src/components/SettingsDialog.svelte";
import { backendContext } from "../src/lib/context";
import { locale } from "../src/lib/i18n";
import { fromSettingsForm, settingsErrors, toSettingsForm } from "../src/lib/settingsForm";
import { mockBackend, SETTINGS } from "./mock";

beforeEach(() => locale.set("en"));

function open(backend = mockBackend()) {
  const onclose = vi.fn();
  render(SettingsDialog, { props: { settings: { ...SETTINGS }, onclose }, context: backendContext(backend) });
  return { backend, onclose };
}

describe("settings form", () => {
  it("round-trips and checks fields", () => {
    const f = toSettingsForm(SETTINGS);
    expect(f.download_timeout_min).toBe("30");
    expect(settingsErrors(f)).toEqual({});
    expect(fromSettingsForm(f, SETTINGS)).toEqual(SETTINGS);
    const bad = { ...f, pool_start: "192.168.88.10", pool_end: "300.1.1.1", max_parallel: "0", cooldown_secs: "10" };
    expect(Object.keys(settingsErrors(bad)).sort()).toEqual(["cooldown_secs", "max_parallel", "pool_end"]);
    expect("server_ip" in f).toBe(false);
  });
});

describe("SettingsDialog", () => {
  it("saves changed settings", async () => {
    const { backend, onclose } = open();
    await userEvent.selectOptions(screen.getByLabelText("Language"), "hu");
    await userEvent.clear(screen.getByLabelText("Devices at once"));
    await userEvent.type(screen.getByLabelText("Devices at once"), "2");
    await userEvent.click(screen.getByText("Advanced"));
    await userEvent.type(screen.getByLabelText("Address pool 1"), "192.168.88.10");
    await userEvent.type(screen.getByLabelText("Address pool 2"), "192.168.88.50");
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    const saved = { ...SETTINGS, language: "hu", max_parallel: 2, pool_start: "192.168.88.10", pool_end: "192.168.88.50" };
    expect(backend.setSettings).toHaveBeenCalledWith(saved);
    expect(onclose).toHaveBeenCalledWith(saved);
  });

  it("shows field errors and does not save", async () => {
    const { backend } = open();
    await userEvent.click(screen.getByText("Advanced"));
    await userEvent.type(screen.getByLabelText("Address pool 1"), "192.168.88");
    await userEvent.type(screen.getByLabelText("Address pool 2"), "192.168.88.50");
    await userEvent.clear(screen.getByLabelText("Ignore a flashed device for (seconds)"));
    await userEvent.type(screen.getByLabelText("Ignore a flashed device for (seconds)"), "5");
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    const alerts = screen.getAllByRole("alert").map((a) => a.textContent);
    expect(alerts).toEqual([
      "Enter a whole number from 30 to 86400.",
      "Enter an IPv4 address such as 192.168.88.1.",
    ]);
    expect(backend.setSettings).not.toHaveBeenCalled();
  });

  it("shows the cache and clears it, or says why not", async () => {
    const backend = mockBackend();
    open(backend);
    expect(await screen.findByText("1.5 MB in ~/Library/Caches/x")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Clear downloads" }));
    expect(backend.clearCache).toHaveBeenCalled();
    expect(await screen.findByText("Downloads cleared.")).toBeInTheDocument();
    vi.mocked(backend.clearCache).mockRejectedValueOnce({ code: "engine_running" });
    await userEvent.click(screen.getByRole("button", { name: "Clear downloads" }));
    expect(await screen.findByText("Stop first.")).toBeInTheDocument();
  });

  it("reports backend validation", async () => {
    const backend = mockBackend({
      setSettings: vi.fn(async () => Promise.reject({ code: "invalid_setting", params: { field: "netmask" } })),
    });
    open(backend);
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByText("Invalid setting: subnet mask.")).toBeInTheDocument();
  });

  it("has no server IP; the pool and the download timeout are under Advanced", async () => {
    open();
    expect(screen.queryByLabelText("Server IP")).toBeNull();
    expect(screen.queryByLabelText(/Subnet mask/)).toBeNull();
    const advanced = screen.getByText("Advanced").closest("details")!;
    expect(advanced).not.toHaveAttribute("open");
    expect(advanced).toContainElement(screen.getByLabelText("Address pool 1"));
    expect(advanced).toContainElement(screen.getByLabelText("Download timeout (minutes)"));
    expect(advanced).toHaveTextContent("Only needs increasing on very slow connections.");
  });
});
