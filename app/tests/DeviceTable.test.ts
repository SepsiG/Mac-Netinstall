import { render, screen, within } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import DeviceTable from "../src/components/DeviceTable.svelte";
import { backendContext } from "../src/lib/context";
import { locale } from "../src/lib/i18n";
import type { DeviceRow } from "../src/lib/state.svelte";
import type { DeviceState } from "../src/lib/types";
import { mockBackend } from "./mock";

const A = "48:A9:8A:00:00:0A";

function dev(state: DeviceState, extra: Partial<DeviceRow> = {}): DeviceRow {
  return {
    mac: A,
    ip: "192.168.88.2",
    boot_arch: "arm64",
    info: { model: "RB5009UG+S+", arch: "arm64", license_id: "X", min_os: "7" },
    version: "7.25.1",
    attempt: 1,
    updated_ms: Date.now(),
    state,
    ...extra,
  };
}

function show(rows: DeviceRow[], props: Record<string, unknown> = {}) {
  const backend = mockBackend();
  const onerror = vi.fn();
  render(DeviceTable, {
    props: { rows, running: true, profile: "lab", onlyMac: null, onerror, ...props },
    context: backendContext(backend),
  });
  return { backend, onerror };
}

const row = () => screen.getByText(A).closest("tr")!;

beforeEach(() => locale.set("en"));

describe("DeviceTable", () => {
  it("shows an explanation when empty", () => {
    show([]);
    expect(screen.getByText(/No devices yet/)).toBeInTheDocument();
  });

  it.each<[DeviceState, string]>([
    [{ state: "bootp_seen", boot_arch: "arm64", ip: "1.2.3.4" }, "Booting"],
    [{ state: "kernel_serving", sent: 10, total: 40 }, "Sending boot kernel"],
    [{ state: "discovered" }, "Found"],
    [{ state: "waiting", reason: "no_profile" }, "Waiting for a profile"],
    [{ state: "waiting", reason: "concurrency" }, "Waiting for a free slot"],
    [{ state: "preparing" }, "Preparing"],
    [{ state: "flashing", version: "7.25.1", phase: "formatting", sent: 0, total: 9 }, "Formatting"],
    [{ state: "done" }, "Done now"],
    [{ state: "failed", code: "flash_timeout", params: { phase: "uploading" } }, "The device stopped answering (step: Uploading)."],
  ])("renders %o as %s", (state, text) => {
    show([dev(state)]);
    expect(within(row()).getByText(text)).toBeInTheDocument();
    expect(within(row()).getByText("RB5009UG+S+")).toBeInTheDocument();
  });

  it("shows progress for kernel transfer and flashing", () => {
    show([dev({ state: "flashing", version: "7.25.1", phase: "uploading", sent: 72, total: 100 })]);
    const bar = within(row()).getByRole("progressbar");
    expect(bar).toHaveAttribute("value", "72");
    expect(within(row()).getByText("72%")).toBeInTheDocument();
  });

  it("offers the right actions per state", () => {
    show([dev({ state: "waiting", reason: "no_profile" })]);
    const names = within(row())
      .getAllByRole("button")
      .map((b) => b.textContent?.trim());
    expect(names).toEqual(["Flash", "Forget", "Only this"]);
  });

  it("flashes with the selected profile and retries failures", async () => {
    const { backend } = show([
      dev({ state: "waiting", reason: "no_profile" }),
      dev({ state: "failed", code: "cancelled" }, { mac: "48:A9:8A:00:00:0B" }),
    ]);
    await userEvent.click(screen.getByRole("button", { name: "Flash" }));
    expect(backend.flash).toHaveBeenCalledWith(A, "lab");
    await userEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(backend.retry).toHaveBeenCalledWith("48:A9:8A:00:00:0B");
  });

  it("asks before cancelling, and warns harder while flashing", async () => {
    const { backend } = show([dev({ state: "flashing", version: "7.25.1", phase: "uploading", sent: 1, total: 9 })]);
    await userEvent.click(within(row()).getByRole("button", { name: "Cancel" }));
    const dialog = screen.getByRole("dialog");
    expect(dialog).toHaveTextContent("Stop installing on 48:A9:8A:00:00:0A?");
    expect(dialog).toHaveTextContent("without a working system");
    await userEvent.click(within(dialog).getByRole("button", { name: "Keep going" }));
    expect(backend.cancel).not.toHaveBeenCalled();
    expect(screen.queryByRole("dialog")).toBeNull();
    await userEvent.click(within(row()).getByRole("button", { name: "Cancel" }));
    await userEvent.click(screen.getByRole("button", { name: "Stop install" }));
    expect(backend.cancel).toHaveBeenCalledWith(A);
  });

  it("reports command failures", async () => {
    const backend = mockBackend({
      forget: vi.fn(async () => Promise.reject({ code: "device_busy", params: { mac: A } })),
    });
    const onerror = vi.fn();
    render(DeviceTable, {
      props: { rows: [dev({ state: "done" })], running: true, profile: null, onlyMac: null, onerror },
      context: backendContext(backend),
    });
    await userEvent.click(screen.getByRole("button", { name: "Forget" }));
    expect(onerror).toHaveBeenCalledWith({ code: "device_busy", params: { mac: A } });
  });

  it("does not offer Forget for a device in its cooldown", () => {
    show([dev({ state: "done" }, { updated_ms: Date.now() - 10_000 })], { cooldownSecs: 300 });
    const forget = within(row()).getByRole("button", { name: "Forget" });
    expect(forget).toBeDisabled();
    expect(forget.closest("[title]")).toHaveAttribute("title", expect.stringMatching(/cooldown/));
  });

  it("offers Forget again once the cooldown is over, and for failed devices", () => {
    show(
      [
        dev({ state: "done" }, { updated_ms: Date.now() - 400_000 }),
        dev({ state: "failed", code: "cancelled" }, { mac: "48:A9:8A:00:00:0B" }),
      ],
      { cooldownSecs: 300 },
    );
    for (const b of screen.getAllByRole("button", { name: "Forget" })) expect(b).toBeEnabled();
  });

  it("toggles serving only one device", async () => {
    const { backend } = show([dev({ state: "done" })]);
    await userEvent.click(screen.getByRole("button", { name: "Only this" }));
    expect(backend.setOnlyMac).toHaveBeenCalledWith(A);
  });

  it("is translated", () => {
    locale.set("hu");
    show([dev({ state: "waiting", reason: "concurrency" })]);
    expect(screen.getByText("Szabad helyre vár")).toBeInTheDocument();
    expect(screen.getByRole("columnheader", { name: "Állapot" })).toBeInTheDocument();
  });
});
