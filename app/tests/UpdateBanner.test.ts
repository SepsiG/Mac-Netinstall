import { render, screen, waitFor, within } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import UpdateBanner from "../src/components/UpdateBanner.svelte";
import { backendContext } from "../src/lib/context";
import { locale } from "../src/lib/i18n";
import type { DownloadedView, UpdateView } from "../src/lib/types";
import { AVAILABLE, mockBackend } from "./mock";

beforeEach(() => locale.set("en"));

function show(backend = mockBackend(), update: UpdateView = AVAILABLE, hidden = false) {
  const onlater = vi.fn();
  const onupdate = vi.fn();
  const r = render(UpdateBanner, {
    props: { update, hidden, onlater, onupdate },
    context: backendContext(backend),
  });
  return { backend, onlater, onupdate, ...r };
}

describe("UpdateBanner", () => {
  it("announces the new version with its actions", () => {
    show();
    const banner = screen.getByRole("status");
    expect(banner).toHaveTextContent("A new version is available: 0.1.3");
    for (const name of ["What's new?", "Download", "Later", "Skip this version"]) {
      expect(within(banner).getByRole("button", { name })).toBeInTheDocument();
    }
  });

  it("is hidden while devices are busy, but stays mounted", async () => {
    const { rerender } = show(mockBackend(), AVAILABLE, true);
    expect(screen.queryByRole("status")).toBeNull();
    await rerender({ hidden: false });
    expect(screen.getByRole("status")).toBeInTheDocument();
  });

  it("shows the release notes as text, never as HTML", async () => {
    const backend = mockBackend();
    show(backend, { ...AVAILABLE, latest: { ...AVAILABLE.latest!, notes: "## Fixed\n- a <b>bold</b> claim\n<img src=x onerror=alert(1)>" } });
    await userEvent.click(screen.getByRole("button", { name: "What's new?" }));
    const dialog = screen.getByRole("dialog", { name: "What's new in 0.1.3" });
    expect(dialog).toHaveTextContent("Fixed");
    expect(dialog).toHaveTextContent("a bold claim");
    expect(dialog.querySelector("img, b, script")).toBeNull();
    await userEvent.click(within(dialog).getByRole("button", { name: "Open on GitHub" }));
    expect(backend.openReleasePage).toHaveBeenCalledWith();
    await userEvent.click(within(dialog).getByRole("button", { name: "Close" }));
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("later and skip", async () => {
    const { backend, onlater, onupdate } = show();
    await userEvent.click(screen.getByRole("button", { name: "Later" }));
    expect(onlater).toHaveBeenCalled();
    await userEvent.click(screen.getByRole("button", { name: "Skip this version" }));
    expect(backend.skipUpdate).toHaveBeenCalledWith("0.1.3");
    await waitFor(() => expect(onupdate).toHaveBeenCalledWith(expect.objectContaining({ available: false, skipped: "0.1.3" })));
  });

  it("downloads with progress, then tells how to install", async () => {
    let finish: (v: DownloadedView) => void = () => {};
    const backend = mockBackend({
      downloadUpdate: vi.fn(() => new Promise<DownloadedView>((resolve) => (finish = resolve))),
    });
    show(backend);
    await userEvent.click(screen.getByRole("button", { name: "Download" }));
    expect(backend.downloadUpdate).toHaveBeenCalled();
    backend.emitUpdateProgress({ done: 3_000_000, total: 12_000_000 });
    const banner = screen.getByRole("status");
    await waitFor(() => expect(banner).toHaveTextContent("Downloading Mac Netinstall 0.1.3… 25%"));
    expect(within(banner).queryByRole("button", { name: "Download" })).toBeNull();
    finish({ file_name: "Mac-Netinstall_0.1.3_aarch64.dmg" });
    await waitFor(() => expect(banner).toHaveTextContent("Downloaded and verified: Mac-Netinstall_0.1.3_aarch64.dmg"));
    expect(banner).toHaveTextContent("Drag Mac Netinstall to the Applications folder");
    expect(banner).toHaveTextContent("Local Network permission");
    expect(banner).toHaveTextContent("Gatekeeper");
    // Idle: opened at once (once), and it can be opened again.
    await waitFor(() => expect(backend.openDownloadedUpdate).toHaveBeenCalledTimes(1));
    await userEvent.click(within(banner).getByRole("button", { name: "Open" }));
    expect(backend.openDownloadedUpdate).toHaveBeenCalledTimes(2);
    expect(banner).not.toHaveTextContent("Open it from the Downloads folder");
  });

  it("opens the DMG only once no device is busy", async () => {
    let finish: (v: DownloadedView) => void = () => {};
    const backend = mockBackend({
      downloadUpdate: vi.fn(() => new Promise<DownloadedView>((resolve) => (finish = resolve))),
    });
    const { rerender } = show(backend);
    await userEvent.click(screen.getByRole("button", { name: "Download" }));
    // Flashing started meanwhile: the download finishes while hidden.
    await rerender({ hidden: true });
    finish({ file_name: "Mac-Netinstall_0.1.3_aarch64.dmg" });
    await new Promise((r) => setTimeout(r, 20));
    expect(backend.openDownloadedUpdate).not.toHaveBeenCalled();
    await rerender({ hidden: false });
    await waitFor(() => expect(backend.openDownloadedUpdate).toHaveBeenCalledTimes(1));
  });

  it("says where the file is if it cannot be opened", async () => {
    const backend = mockBackend({
      openDownloadedUpdate: vi.fn(async () => {
        throw { code: "update_open_busy" };
      }),
    });
    show(backend);
    await userEvent.click(screen.getByRole("button", { name: "Download" }));
    expect(await screen.findByText("Open it from the Downloads folder.")).toBeInTheDocument();
  });

  it("hides the release notes while devices are busy", async () => {
    const { rerender } = show();
    await userEvent.click(screen.getByRole("button", { name: "What's new?" }));
    expect(screen.getByRole("dialog")).toBeInTheDocument();
    await rerender({ hidden: true });
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("shows a failed download as a coded error and allows a retry", async () => {
    const backend = mockBackend({
      downloadUpdate: vi.fn(async () => {
        throw { code: "update_download_failed", params: { problem: "checksum_mismatch" } };
      }),
    });
    show(backend);
    await userEvent.click(screen.getByRole("button", { name: "Download" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "The new version could not be downloaded: the checksum does not match, the file was deleted.",
    );
    expect(screen.getByRole("button", { name: "Download" })).toBeEnabled();
  });

  it("speaks Hungarian", () => {
    locale.set("hu");
    show();
    const banner = screen.getByRole("status");
    expect(banner).toHaveTextContent("Új verzió érhető el: 0.1.3");
    for (const name of ["Mi újság?", "Letöltés", "Később", "Ezt a verziót kihagyom"]) {
      expect(within(banner).getByRole("button", { name })).toBeInTheDocument();
    }
  });
});
