import { render, screen, waitFor } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import App from "../src/App.svelte";
import type { Backend } from "../src/lib/api";
import { AVAILABLE, IDLE, mockBackend } from "./mock";

const A = "48:A9:8A:00:00:0A";

function app(overrides: Partial<Backend> = {}, delay = 0) {
  const backend = mockBackend({ checkForUpdates: vi.fn(async () => structuredClone(AVAILABLE)), ...overrides });
  render(App, { props: { backend, updateCheckDelayMs: delay } });
  return backend;
}

afterEach(() => vi.useRealTimers());

describe("update notice in the main window", () => {
  it("checks automatically only after the start-up delay", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const backend = app({}, 10_000);
    await waitFor(() => expect(backend.preflight).toHaveBeenCalled());
    expect(backend.checkForUpdates).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(10_000);
    await waitFor(() => expect(backend.checkForUpdates).toHaveBeenCalledWith(false));
    expect(await screen.findByText("A new version is available: 0.1.3")).toBeInTheDocument();
  });

  it("shows nothing when there is no newer version", async () => {
    const backend = app({ checkForUpdates: vi.fn(async () => ({ ...AVAILABLE, available: false })) });
    await waitFor(() => expect(backend.checkForUpdates).toHaveBeenCalled());
    expect(screen.queryByText(/A new version is available/)).toBeNull();
  });

  it("waits while devices are being prepared or flashed", async () => {
    const backend = app({
      snapshot: vi.fn(async () => ({ ...IDLE, running: true })),
    });
    await waitFor(() => expect(backend.checkForUpdates).toHaveBeenCalled());
    expect(await screen.findByText(/A new version is available/)).toBeInTheDocument();
    backend.emit({ event: "device", mac: A, state: "flashing", version: "7.25.1", phase: "formatting", sent: 0, total: 100 });
    // Hidden (kept mounted, so a running download keeps its progress).
    await waitFor(() => expect(screen.getByText(/A new version is available/)).not.toBeVisible());
    backend.emit({ event: "device", mac: A, state: "done" });
    await waitFor(() => expect(screen.getByText(/A new version is available/)).toBeVisible());
  });

  it("later hides it for this session", async () => {
    app();
    await userEvent.click(await screen.findByRole("button", { name: "Later" }));
    expect(screen.queryByText(/A new version is available/)).toBeNull();
  });

  it("a check from Settings shows the notice too", async () => {
    let first = true;
    const backend = app({
      checkForUpdates: vi.fn(async (manual: boolean) => {
        const v = { ...structuredClone(AVAILABLE), available: !first && manual };
        first = false;
        return v;
      }),
    });
    await waitFor(() => expect(backend.checkForUpdates).toHaveBeenCalledWith(false));
    expect(screen.queryByText(/A new version is available/)).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: "Settings" }));
    await userEvent.click(screen.getByRole("button", { name: "Check for updates now" }));
    expect(backend.checkForUpdates).toHaveBeenLastCalledWith(true);
    expect(await screen.findByText(/A new version is available: 0.1.3/)).toBeInTheDocument();
  });
});
