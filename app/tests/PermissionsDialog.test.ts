import { render, screen, waitFor, within } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import PermissionsDialog from "../src/components/PermissionsDialog.svelte";
import { backendContext } from "../src/lib/context";
import { locale } from "../src/lib/i18n";
import type { PermissionsView } from "../src/lib/types";
import { mockBackend } from "./mock";

function show(result: PermissionsView, more: Record<string, unknown> = {}) {
  const backend = mockBackend({ requestPermissions: vi.fn(async () => result) });
  const onclose = vi.fn();
  const onrequested = vi.fn();
  render(PermissionsDialog, {
    props: { onclose, onrequested, ...more },
    context: backendContext(backend),
  });
  return { backend, onclose, onrequested };
}

beforeEach(() => locale.set("en"));

describe("PermissionsDialog", () => {
  it("explains both permissions and asks for them on request", async () => {
    const { backend, onrequested } = show({ local_network: "granted", firewall: "allowed" });
    const dialog = screen.getByRole("dialog");
    expect(dialog).toHaveTextContent("Local network");
    expect(dialog).toHaveTextContent("Incoming connections (firewall)");
    expect(within(dialog).getAllByText("Not checked yet")).toHaveLength(2);
    expect(backend.requestPermissions).not.toHaveBeenCalled();
    await userEvent.click(screen.getByRole("button", { name: "Request permissions" }));
    expect(backend.requestPermissions).toHaveBeenCalledOnce();
    await waitFor(() => expect(within(dialog).getAllByText(/✓ Allowed/)).toHaveLength(2));
    expect(onrequested).toHaveBeenCalledOnce();
    // Nothing to fix: no settings buttons.
    expect(screen.queryByRole("button", { name: /settings/ })).toBeNull();
    expect(screen.getByRole("button", { name: "Check again" })).toBeInTheDocument();
  });

  it("offers System Settings for what is not allowed", async () => {
    const { backend, onclose } = show({ local_network: "denied", firewall: "blocked" });
    await userEvent.click(screen.getByRole("button", { name: "Request permissions" }));
    expect(await screen.findByText(/✗ Not allowed/)).toBeInTheDocument();
    expect(screen.getByText(/✗ Blocked/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Open Local Network settings" }));
    expect(backend.openSystemSettings).toHaveBeenLastCalledWith("local_network");
    await userEvent.click(screen.getByRole("button", { name: "Open firewall settings" }));
    expect(backend.openSystemSettings).toHaveBeenLastCalledWith("firewall");
    await userEvent.click(screen.getByRole("button", { name: "Done" }));
    expect(onclose).toHaveBeenCalled();
  });

  it("says when the firewall is off, in Hungarian", async () => {
    locale.set("hu");
    show({ local_network: "granted", firewall: "off" });
    await userEvent.click(screen.getByRole("button", { name: "Engedélyek kérése" }));
    expect(await screen.findByText(/A tűzfal ki van kapcsolva/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Kész" })).toBeInTheDocument();
  });

  it("checks at once when opened because sending was refused", async () => {
    const { backend } = show({ local_network: "denied", firewall: "off" }, { refused: true });
    expect(screen.getByText(/macOS refused to send to the devices/)).toBeInTheDocument();
    await waitFor(() => expect(backend.requestPermissions).toHaveBeenCalledOnce());
    expect(await screen.findByRole("button", { name: "Open Local Network settings" })).toBeInTheDocument();
  });

  it("can be put off: Later closes without asking", async () => {
    const { backend, onclose, onrequested } = show({ local_network: "granted", firewall: "off" });
    await userEvent.click(screen.getByRole("button", { name: "Later" }));
    expect(onclose).toHaveBeenCalled();
    expect(onrequested).not.toHaveBeenCalled();
    expect(backend.requestPermissions).not.toHaveBeenCalled();
  });
});
