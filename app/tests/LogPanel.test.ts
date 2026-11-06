import { render, screen } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import LogPanel from "../src/components/LogPanel.svelte";
import { backendContext } from "../src/lib/context";
import { locale } from "../src/lib/i18n";
import { logEntry, type LogEntry } from "../src/lib/messages";
import { mockBackend } from "./mock";

const A = "48:A9:8A:00:00:0A";
const B = "48:A9:8A:00:00:0B";

function entries(): LogEntry[] {
  return [
    logEntry({ event: "kernels_ready", version: "7.25.1" }, 0)!,
    logEntry({ event: "device", mac: A, state: "done" }, 1000)!,
    logEntry({ event: "device", mac: B, state: "failed", code: "cancelled" }, 2000)!,
  ];
}

beforeEach(() => locale.set("en"));

describe("LogPanel", () => {
  it("lists entries and filters by device", async () => {
    render(LogPanel, {
      props: { entries: entries(), open: true, onclear: () => {}, onerror: () => {} },
      context: backendContext(mockBackend()),
    });
    expect(screen.getAllByRole("listitem")).toHaveLength(3);
    await userEvent.selectOptions(screen.getByRole("combobox"), B);
    const items = screen.getAllByRole("listitem");
    expect(items).toHaveLength(1);
    expect(items[0]).toHaveTextContent(`${B} ✖ Failed: Cancelled.`);
  });

  it("exports the shown lines as text, in the current language", async () => {
    locale.set("hu");
    const backend = mockBackend();
    render(LogPanel, {
      props: { entries: entries(), open: true, onclear: () => {}, onerror: () => {} },
      context: backendContext(backend),
    });
    await userEvent.click(screen.getByRole("button", { name: "Exportálás…" }));
    const text = vi.mocked(backend.exportLog).mock.calls[0]![0];
    expect(text.split("\n")).toHaveLength(4);
    expect(text).toContain(`${A}  Kész`);
    expect(text).toContain("A boot kernelek készen állnak (7.25.1)");
  });

  it("collapses", async () => {
    render(LogPanel, {
      props: { entries: entries(), open: true, onclear: () => {}, onerror: () => {} },
      context: backendContext(mockBackend()),
    });
    await userEvent.click(screen.getByRole("button", { name: /Log/ }));
    expect(screen.queryAllByRole("listitem")).toHaveLength(0);
    expect(screen.getByRole("button", { name: /Log/ })).toHaveAttribute("aria-expanded", "false");
  });
});
