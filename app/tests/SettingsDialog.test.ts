import { render, screen, within } from "@testing-library/svelte";
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

  it("lists the downloaded files by version with their state", async () => {
    open();
    expect(await screen.findByText("RouterOS 7.25.1")).toBeInTheDocument();
    const section = screen.getByRole("region", { name: "Downloaded RouterOS files" });
    expect(section).toHaveTextContent("kept on this Mac, so they are not downloaded again for every flash");
    expect(section).toHaveTextContent("1.5 MB in ~/Library/Caches/x");
    const row = (name: string) => within(section).getByText(name).closest("tr")!;
    expect(row("Boot kernels (8)")).toHaveTextContent("1 MB");
    expect(row("Boot kernels (8)")).toHaveTextContent("✓ verified");
    expect(row("wireless-7.25.1-arm64.npk")).toHaveTextContent("arm64");
    expect(within(row("wireless-7.25.1-arm64.npk")).getByText("damaged")).toHaveClass("badge-warning");
    expect(within(section).getByText("Your files (scripts, packages)")).toBeInTheDocument();
    expect(row("setup.rsc")).toBeInTheDocument();
  });

  it("deletes one file after confirming, then shows the new list", async () => {
    const { backend } = open();
    const name = "wireless-7.25.1-arm64.npk";
    await userEvent.click(await screen.findByRole("button", { name: `Delete ${name}` }));
    let dialog = screen.getAllByRole("dialog").at(-1)!;
    expect(dialog).toHaveTextContent(`Delete ${name}?`);
    expect(dialog).toHaveTextContent("100 KB will be freed");
    await userEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
    expect(backend.deleteCached).not.toHaveBeenCalled();
    await userEvent.click(screen.getByRole("button", { name: `Delete ${name}` }));
    dialog = screen.getAllByRole("dialog").at(-1)!;
    await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
    expect(backend.deleteCached).toHaveBeenCalledWith({ scope: "item", id: `7.25.1/${name}` });
    expect(await screen.findByText("1 MB freed.")).toBeInTheDocument();
    expect(backend.cacheInventory).toHaveBeenCalledTimes(2);
  });

  it.each([
    ["Delete version", { scope: "version", version: "7.25.1" }, "RouterOS 7.25.1"],
    ["Delete damaged files", { scope: "damaged" }, "the damaged files"],
    ["Delete all", { scope: "all" }, "all downloaded files"],
  ])("%s asks first", async (button, scope, what) => {
    const { backend } = open();
    await userEvent.click(await screen.findByRole("button", { name: button }));
    const dialog = screen.getAllByRole("dialog").at(-1)!;
    expect(dialog).toHaveTextContent(`Delete ${what}?`);
    await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
    expect(backend.deleteCached).toHaveBeenCalledWith(scope);
  });

  it("says why a file cannot be deleted", async () => {
    const backend = mockBackend({
      deleteCached: vi.fn(async () => Promise.reject({ code: "asset_in_use", params: { version: "7.25.1" } })),
    });
    open(backend);
    await userEvent.click(await screen.findByRole("button", { name: "Delete all" }));
    await userEvent.click(within(screen.getAllByRole("dialog").at(-1)!).getByRole("button", { name: "Delete" }));
    expect(await screen.findByText(/RouterOS 7.25.1 is in use right now/)).toBeInTheDocument();
  });

  it("has nothing to delete when nothing is downloaded", async () => {
    const backend = mockBackend({
      cacheInventory: vi.fn(async () => ({
        location: "~/x",
        inventory: { versions: [], partial: [], local: [], total: 0 },
      })),
    });
    open(backend);
    expect(await screen.findByText("Nothing downloaded yet.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Delete all" })).toBeDisabled();
    expect(screen.queryByRole("button", { name: "Delete damaged files" })).toBeNull();
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

  it("explains that a newer app's settings file is left alone", async () => {
    const backend = mockBackend({
      setSettings: vi.fn(async () => Promise.reject({ code: "settings_newer", params: { version: "3", supported: "2" } })),
    });
    const { onclose } = open(backend);
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByText(/written by a newer version of Mac Netinstall/)).toBeInTheDocument();
    expect(onclose).not.toHaveBeenCalled();
  });
});
