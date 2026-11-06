import { fireEvent, render, screen } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import ConfirmDialog from "../src/components/ConfirmDialog.svelte";
import ProfileEditor from "../src/components/ProfileEditor.svelte";
import SettingsDialog from "../src/components/SettingsDialog.svelte";
import { backendContext } from "../src/lib/context";
import { initDialogSizes } from "../src/lib/dialogSizes";
import { locale } from "../src/lib/i18n";
import { mockBackend, SETTINGS } from "./mock";

function open() {
  const outside = document.createElement("button");
  outside.textContent = "outside";
  document.body.appendChild(outside);
  outside.focus();
  const oncancel = vi.fn();
  const onconfirm = vi.fn();
  const result = render(ConfirmDialog, {
    props: { title: "Sure?", body: "Really.", confirmLabel: "Do it", cancelLabel: "Keep", destructive: true, onconfirm, oncancel },
  });
  return { outside, oncancel, onconfirm, ...result };
}

afterEach(() => document.querySelectorAll("body > button").forEach((b) => b.remove()));

describe("Modal focus", () => {
  it("focuses the safe button and keeps Tab inside", async () => {
    open();
    const keep = screen.getByRole("button", { name: "Keep" });
    const doIt = screen.getByRole("button", { name: "Do it" });
    expect(keep).toHaveFocus();
    await userEvent.tab();
    expect(doIt).toHaveFocus();
    await userEvent.tab();
    expect(keep).toHaveFocus();
    await userEvent.tab({ shift: true });
    expect(doIt).toHaveFocus();
  });

  it("closes on Escape without a click first", async () => {
    const { oncancel, onconfirm } = open();
    await userEvent.keyboard("{Escape}");
    expect(oncancel).toHaveBeenCalledTimes(1);
    expect(onconfirm).not.toHaveBeenCalled();
  });

  it("makes the rest of the window inert and restores focus when closed", () => {
    const { outside, unmount } = open();
    expect(outside).toHaveAttribute("inert");
    unmount();
    expect(outside).not.toHaveAttribute("inert");
    expect(outside).toHaveFocus();
  });

  it("lets only the topmost dialog react to Escape", async () => {
    const first = open();
    const second = open();
    await userEvent.keyboard("{Escape}");
    expect(second.oncancel).toHaveBeenCalledTimes(1);
    expect(first.oncancel).not.toHaveBeenCalled();
  });
});

describe("Modal size", () => {
  const { innerWidth, innerHeight } = window;
  beforeEach(() => {
    locale.set("en");
    initDialogSizes(null, null);
  });
  afterEach(() => {
    Object.assign(window, { innerWidth, innerHeight });
  });

  const sheet = () => screen.getByRole("dialog");
  const handle = () => screen.getByTestId("resize-handle");
  /** Drag the grip from (100, 100) by (dx, dy) and let go. */
  async function drag(dx: number, dy: number) {
    await fireEvent.pointerDown(handle(), { button: 0, pointerId: 1, clientX: 100, clientY: 100 });
    await fireEvent.pointerMove(handle(), { pointerId: 1, clientX: 100 + dx, clientY: 100 + dy });
    await fireEvent.pointerUp(handle(), { pointerId: 1, clientX: 100 + dx, clientY: 100 + dy });
  }

  it("every dialog has a resize grip", () => {
    const backend = mockBackend();
    const a = render(ConfirmDialog, {
      props: { title: "t", body: "b", confirmLabel: "OK", cancelLabel: "No", onconfirm: vi.fn(), oncancel: vi.fn() },
    });
    expect(handle()).toBeInTheDocument();
    a.unmount();
    const b = render(ProfileEditor, { props: { profile: null, onclose: vi.fn() }, context: backendContext(backend) });
    expect(handle()).toBeInTheDocument();
    b.unmount();
    render(SettingsDialog, { props: { settings: { ...SETTINGS }, onclose: vi.fn() }, context: backendContext(backend) });
    expect(handle()).toBeInTheDocument();
  });

  it("starts with a width that scales with the window and the content's height", () => {
    Object.assign(window, { innerWidth: 1560, innerHeight: 900 });
    open();
    expect(sheet().style.width).toBe("630px"); // 420 at 1040 wide
    expect(sheet().style.height).toBe("");
  });

  it("is resized with the grip, never below its minimum nor beyond the window", async () => {
    Object.assign(window, { innerWidth: 1000, innerHeight: 700 });
    open();
    await drag(-2000, -2000);
    expect([sheet().style.width, sheet().style.height]).toEqual(["300px", "140px"]);
    await drag(5000, 5000);
    expect([sheet().style.width, sheet().style.height]).toEqual([`${1000 - 32}px`, `${700 - 64}px`]);
    // The window shrinking clamps it again.
    Object.assign(window, { innerWidth: 800, innerHeight: 500 });
    await fireEvent(window, new Event("resize"));
    expect([sheet().style.width, sheet().style.height]).toEqual([`${800 - 32}px`, `${500 - 64}px`]);
    // The focus trap and Escape still work.
    expect(screen.getByRole("button", { name: "Keep" })).toHaveFocus();
  });

  it("remembers its size through the backend and opens at it again", async () => {
    Object.assign(window, { innerWidth: 1200, innerHeight: 800 });
    const backend = mockBackend();
    const settings = { ...SETTINGS, dialog_sizes: { confirm: { width: 500, height: 300 } } };
    initDialogSizes(settings.dialog_sizes, (id, s) => backend.setDialogSize(id, s.width, s.height));
    const first = open();
    expect([sheet().style.width, sheet().style.height]).toEqual(["500px", "300px"]);
    await drag(10, 20);
    expect(backend.setDialogSize).toHaveBeenCalledWith("confirm", 520, 320);
    first.unmount();
    const second = open();
    expect([sheet().style.width, sheet().style.height]).toEqual(["520px", "320px"]);
    second.unmount();
    // A stored size larger than the window is shown clamped.
    initDialogSizes({ confirm: { width: 5000, height: 5000 } }, null);
    open();
    expect([sheet().style.width, sheet().style.height]).toEqual([`${1200 - 32}px`, `${800 - 64}px`]);
  });
});
