import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { flushSync } from "svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import LogPanel from "../src/components/LogPanel.svelte";
import { backendContext } from "../src/lib/context";
import { locale } from "../src/lib/i18n";
import { DEFAULT_LOG_HEIGHT, LOG_STEP, MAX_SHOWN, MIN_LOG_HEIGHT, MIN_TABLE_HEIGHT } from "../src/lib/logPanel";
import { logEntry, type LogEntry } from "../src/lib/messages";
import { LogBuffer } from "../src/lib/state.svelte";
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

/** Line `n`: "Boot kernels v<n> ready; …". */
function numbered(n: number): LogEntry {
  return logEntry({ event: "kernels_ready", version: `v${n}` }, n)!;
}

function buffer(list: LogEntry[] = entries()): LogBuffer {
  const b = new LogBuffer();
  for (const e of list) b.push(e);
  return b;
}

function show(log: LogBuffer, more: Record<string, unknown> = {}, target?: HTMLElement) {
  const backend = mockBackend();
  const onclear = vi.fn(() => log.clear());
  const r = render(LogPanel, {
    props: { log, open: true, onclear, onerror: () => {}, ...more },
    context: backendContext(backend),
    ...(target ? { target } : {}),
  });
  return { backend, onclear, ...r };
}

const items = () => screen.getAllByRole("listitem");
const list = () => screen.getByRole("list");
const texts = () => items().map((li) => li.textContent ?? "");

beforeEach(() => locale.set("en"));
afterEach(() => vi.useRealTimers());

describe("LogPanel order", () => {
  it("lists the newest line first and filters by device", async () => {
    show(buffer());
    const all = items();
    expect(all).toHaveLength(3);
    expect(all[0]).toHaveTextContent(`${B} ✖ Failed: Cancelled.`);
    expect(all[2]).toHaveTextContent("Boot kernels 7.25.1 ready");
    await userEvent.selectOptions(screen.getByRole("combobox"), B);
    const one = items();
    expect(one).toHaveLength(1);
    expect(one[0]).toHaveTextContent(`${B} ✖ Failed: Cancelled.`);
  });

  it("exports oldest first, in the current language, filtered like the list", async () => {
    locale.set("hu");
    const log = buffer();
    const { backend } = show(log);
    const exportButton = screen.getByRole("button", { name: "Exportálás…" });
    expect(exportButton.title).toMatch(/legrégebbi/);
    await userEvent.click(exportButton);
    const text = vi.mocked(backend.exportLog).mock.calls[0]![0];
    const lines = text.split("\n");
    expect(lines).toHaveLength(4);
    expect(lines[0]).toContain("A boot kernelek készen állnak (7.25.1)");
    expect(lines[1]).toContain(`${A}  Kész`);
    expect(lines[2]).toContain(B);
    await userEvent.selectOptions(screen.getByRole("combobox"), A);
    await userEvent.click(exportButton);
    const filtered = vi.mocked(backend.exportLog).mock.calls[1]![0];
    expect(filtered.trim().split("\n")).toHaveLength(1);
    expect(filtered).toContain(A);
  });

  it("a new line arrives at the top and the view stays at the top", async () => {
    const log = buffer();
    show(log);
    list().scrollTop = 0;
    log.push(numbered(1));
    await waitFor(() => expect(items()[0]).toHaveTextContent("Boot kernels v1 ready"));
    expect(items()).toHaveLength(4);
    expect(list().scrollTop).toBe(0);
  });

  it("shows at most the newest MAX_SHOWN lines but exports all", async () => {
    const log = buffer(Array.from({ length: MAX_SHOWN + 10 }, (_, i) => numbered(i)));
    const { backend } = show(log);
    expect(items()).toHaveLength(MAX_SHOWN);
    expect(items()[0]).toHaveTextContent(`v${MAX_SHOWN + 9} `);
    await fireEvent.click(screen.getByRole("button", { name: "Export…" }));
    const text = vi.mocked(backend.exportLog).mock.calls[0]![0];
    const lines = text.trim().split("\n");
    expect(lines).toHaveLength(MAX_SHOWN + 10);
    expect(lines[0]).toContain("v0 ");
  });
});

describe("LogPanel pause", () => {
  it("freezes the list, counts new lines and merges them on resume", async () => {
    const log = buffer();
    const { backend } = show(log);
    await userEvent.click(screen.getByRole("button", { name: "Pause" }));
    const before = texts();
    for (let i = 1; i <= 3; i++) log.push(numbered(i));
    await waitFor(() => expect(screen.getByText("+3 new lines")).toBeInTheDocument());
    expect(texts()).toEqual(before);
    log.push(numbered(4));
    await waitFor(() => expect(screen.getByText("+4 new lines")).toBeInTheDocument());
    expect(texts()).toEqual(before);
    // Nothing is lost: the export has them already.
    await userEvent.click(screen.getByRole("button", { name: "Export…" }));
    expect(vi.mocked(backend.exportLog).mock.calls[0]![0].trim().split("\n")).toHaveLength(7);

    list().scrollTop = 80;
    await userEvent.click(screen.getByRole("button", { name: "Resume" }));
    await waitFor(() => expect(items()).toHaveLength(7));
    expect(items()[0]).toHaveTextContent("v4 ");
    expect(items()[3]).toHaveTextContent("v1 ");
    expect(list().scrollTop).toBe(0);
    expect(screen.queryByText(/new line/)).toBeNull();
    expect(screen.getByRole("button", { name: "Pause" })).toBeInTheDocument();
  });

  it("one new line is counted in the singular", async () => {
    const log = buffer();
    show(log);
    await userEvent.click(screen.getByRole("button", { name: "Pause" }));
    log.push(numbered(1));
    await waitFor(() => expect(screen.getByText("+1 new line")).toBeInTheDocument());
  });

  it("scrolling away from the top pauses; scrolling back resumes", async () => {
    const log = buffer();
    show(log);
    list().scrollTop = 120;
    await fireEvent.scroll(list());
    expect(screen.getByRole("button", { name: "Resume" })).toBeInTheDocument();
    log.push(numbered(1));
    await waitFor(() => expect(screen.getByText("+1 new line")).toBeInTheDocument());
    expect(items()).toHaveLength(3);
    list().scrollTop = 0;
    await fireEvent.scroll(list());
    await waitFor(() => expect(items()[0]).toHaveTextContent("v1 "));
    expect(screen.getByRole("button", { name: "Pause" })).toBeInTheDocument();
  });

  it("a pause by the button is not undone by scrolling", async () => {
    const log = buffer();
    show(log);
    await userEvent.click(screen.getByRole("button", { name: "Pause" }));
    list().scrollTop = 0;
    await fireEvent.scroll(list());
    expect(screen.getByRole("button", { name: "Resume" })).toBeInTheDocument();
  });

  it("Clear empties everything, including what arrived while paused", async () => {
    const log = buffer();
    const { onclear } = show(log);
    await userEvent.click(screen.getByRole("button", { name: "Pause" }));
    log.push(numbered(1));
    await waitFor(() => expect(screen.getByText("+1 new line")).toBeInTheDocument());
    await userEvent.click(screen.getByRole("button", { name: "Clear" }));
    expect(onclear).toHaveBeenCalledOnce();
    await waitFor(() => expect(texts()).toEqual(["Nothing logged yet."]));
    expect(screen.queryByText(/new line/)).toBeNull();
    expect(screen.getByRole("button", { name: "Pause" })).toBeInTheDocument();
    log.push(numbered(2));
    await waitFor(() => expect(items()[0]).toHaveTextContent("v2 "));
    expect(items()).toHaveLength(1);
  });
});

describe("LogPanel bursts", () => {
  it("renders a burst of 5000 lines in one frame and loses none", async () => {
    vi.useFakeTimers({ toFake: ["requestAnimationFrame", "cancelAnimationFrame"] });
    const log = buffer([]);
    const { backend } = show(log);
    let changes = 0;
    let top = list().firstElementChild?.textContent;
    for (let i = 0; i < 5000; i++) {
      log.push(numbered(i));
      flushSync();
      const now = list().firstElementChild?.textContent;
      if (now !== top) changes++;
      top = now;
    }
    expect(changes).toBe(0);
    vi.advanceTimersToNextFrame();
    flushSync();
    expect(items()).toHaveLength(MAX_SHOWN);
    expect(items()[0]).toHaveTextContent("v4999 ");
    vi.useRealTimers();
    await fireEvent.click(screen.getByRole("button", { name: "Export…" }));
    const lines = vi.mocked(backend.exportLog).mock.calls[0]![0].trim().split("\n");
    expect(lines).toHaveLength(5000);
    expect(lines[0]).toContain("v0 ");
    expect(lines[4999]).toContain("v4999 ");
  });
});

describe("LogPanel splitter", () => {
  const height = () => parseFloat(list().style.height);

  it("is an accessible separator that the arrow keys move", async () => {
    const { backend } = show(buffer());
    const sep = screen.getByRole("separator");
    expect(sep).toHaveAttribute("aria-orientation", "horizontal");
    expect(sep).toHaveAttribute("tabindex", "0");
    expect(sep).toHaveAttribute("aria-valuenow", String(DEFAULT_LOG_HEIGHT));
    expect(height()).toBe(DEFAULT_LOG_HEIGHT);
    sep.focus();
    await userEvent.keyboard("{ArrowUp}{ArrowUp}");
    expect(height()).toBe(DEFAULT_LOG_HEIGHT + 2 * LOG_STEP);
    await userEvent.keyboard("{ArrowDown}");
    expect(height()).toBe(DEFAULT_LOG_HEIGHT + LOG_STEP);
    await waitFor(() => expect(backend.setLogPanelHeight).toHaveBeenLastCalledWith(DEFAULT_LOG_HEIGHT + LOG_STEP));
    for (let i = 0; i < 40; i++) await fireEvent.keyDown(sep, { key: "ArrowDown" });
    expect(height()).toBe(MIN_LOG_HEIGHT);
  });

  it("drags within the room the device table leaves; double-click resets", async () => {
    const box = document.createElement("div");
    const table = document.createElement("div");
    let tableHeight = 300;
    table.style.flexGrow = "1";
    table.getBoundingClientRect = () => ({ height: tableHeight }) as DOMRect;
    box.append(table);
    document.body.append(box);
    const { backend } = show(buffer(), {}, box);
    const sep = screen.getByRole("separator");
    await fireEvent.pointerDown(sep, { clientY: 400, button: 0, pointerId: 1 });
    await fireEvent.pointerMove(window, { clientY: 350, pointerId: 1 });
    expect(height()).toBe(DEFAULT_LOG_HEIGHT + 50);
    tableHeight = 250; // the table gave up what the log took
    await fireEvent.pointerMove(window, { clientY: -1000, pointerId: 1 });
    expect(height()).toBe(DEFAULT_LOG_HEIGHT + 50 + 250 - MIN_TABLE_HEIGHT);
    await fireEvent.pointerMove(window, { clientY: 2000, pointerId: 1 });
    expect(height()).toBe(MIN_LOG_HEIGHT);
    expect(backend.setLogPanelHeight).not.toHaveBeenCalled();
    await fireEvent.pointerUp(window, { clientY: 2000, pointerId: 1 });
    expect(backend.setLogPanelHeight).toHaveBeenCalledWith(MIN_LOG_HEIGHT);
    await fireEvent.dblClick(sep);
    expect(height()).toBe(DEFAULT_LOG_HEIGHT);
    expect(backend.setLogPanelHeight).toHaveBeenLastCalledWith(DEFAULT_LOG_HEIGHT);
    box.remove();
  });

  it("shrinks when the window leaves too little room, and grows back", async () => {
    const box = document.createElement("div");
    const table = document.createElement("div");
    let tableHeight = 300;
    table.style.flexGrow = "1";
    table.getBoundingClientRect = () => ({ height: tableHeight }) as DOMRect;
    box.append(table);
    document.body.append(box);
    show(buffer(), { height: 250 }, box);
    expect(height()).toBe(250);
    tableHeight = MIN_TABLE_HEIGHT - 50;
    await fireEvent(window, new Event("resize"));
    expect(height()).toBe(200);
    tableHeight = 500;
    await fireEvent(window, new Event("resize"));
    expect(height()).toBe(250);
    box.remove();
  });

  it("starts at the stored height", () => {
    show(buffer(), { height: 260 });
    expect(height()).toBe(260);
  });

  it("collapses to the header", async () => {
    show(buffer());
    await userEvent.click(screen.getByRole("button", { name: /Log/ }));
    expect(screen.queryAllByRole("listitem")).toHaveLength(0);
    expect(screen.queryByRole("separator")).toBeNull();
    expect(screen.queryByRole("button", { name: "Pause" })).toBeNull();
    expect(screen.getByRole("button", { name: /Log/ })).toHaveAttribute("aria-expanded", "false");
    await userEvent.click(screen.getByRole("button", { name: /Log/ }));
    expect(items()).toHaveLength(3);
  });
});
