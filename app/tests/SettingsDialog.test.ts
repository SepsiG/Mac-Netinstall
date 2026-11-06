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
    const bad = { ...f, server_ip: "300.1.1.1", pool_start: "192.168.88.10", max_parallel: "0", cooldown_secs: "10" };
    expect(Object.keys(settingsErrors(bad)).sort()).toEqual(["cooldown_secs", "max_parallel", "pool_end", "server_ip"]);
  });
});

describe("SettingsDialog", () => {
  it("saves changed settings", async () => {
    const { backend, onclose } = open();
    await userEvent.selectOptions(screen.getByLabelText("Language"), "hu");
    await userEvent.type(screen.getByLabelText("Server IP"), "192.168.88.1");
    await userEvent.clear(screen.getByLabelText("Devices at once"));
    await userEvent.type(screen.getByLabelText("Devices at once"), "2");
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    const saved = { ...SETTINGS, language: "hu", server_ip: "192.168.88.1", max_parallel: 2 };
    expect(backend.setSettings).toHaveBeenCalledWith(saved);
    expect(onclose).toHaveBeenCalledWith(saved);
  });

  it("shows field errors and does not save", async () => {
    const { backend } = open();
    await userEvent.type(screen.getByLabelText("Server IP"), "192.168.88");
    await userEvent.clear(screen.getByLabelText("Ignore a flashed device for (seconds)"));
    await userEvent.type(screen.getByLabelText("Ignore a flashed device for (seconds)"), "5");
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    const alerts = screen.getAllByRole("alert").map((a) => a.textContent);
    expect(alerts).toEqual([
      "Enter an IPv4 address such as 192.168.88.1.",
      "Enter a whole number from 30 to 86400.",
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
});
