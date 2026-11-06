import { render, screen } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import ConfirmDialog from "../src/components/ConfirmDialog.svelte";

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
