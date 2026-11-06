import { render, screen, waitFor, within } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import ProfileEditor from "../src/components/ProfileEditor.svelte";
import { backendContext } from "../src/lib/context";
import { locale } from "../src/lib/i18n";
import { formErrors, fromForm, toForm, validName, validVersion } from "../src/lib/profile";
import type { Profile } from "../src/lib/types";
import { mockBackend, profile } from "./mock";

beforeEach(() => locale.set("en"));

function open(profile: Profile | null, backend = mockBackend()) {
  const onclose = vi.fn();
  render(ProfileEditor, { props: { profile, onclose }, context: backendContext(backend) });
  return { backend, onclose };
}

describe("profile rules", () => {
  it("validate names and versions like the engine", () => {
    expect(validName("hAP ax2 stable")).toBe(true);
    expect(validName("a+b_c-d.e")).toBe(true);
    for (const bad of ["", ".hidden", " lead", "trail ", "a/b", "x".repeat(65), "é"]) {
      expect(validName(bad), bad).toBe(false);
    }
    expect(validVersion("7.25.1")).toBe(true);
    expect(validVersion("7.26rc1")).toBe(true);
    expect(validVersion("7.x")).toBe(false);
  });

  it("round-trips profiles through the form", () => {
    const p: Profile = profile("lab", {
      version: "7.25.1",
      extra_packages: ["wireless"],
      config: { mode: "script", local_name: "setup.rsc" },
      mode_script: "mode.rsc",
      remove_branding: true,
      license_key: "KEY",
      after_install: "shutdown",
    });
    expect(fromForm(toForm(p))).toEqual(p);
    for (const mode of ["keep", "default", "empty"] as const) {
      const q = profile("x", { config: { mode } });
      expect(fromForm(toForm(q))).toEqual(q);
    }
    const f = toForm(null);
    f.config = "script";
    f.useModeScript = true;
    expect(formErrors(f)).toEqual({
      name: "profile.err_name",
      script: "profile.err_script",
      modeScript: "profile.err_mode_script",
    });
    f.config = "keep";
    expect(formErrors(f).modeScript).toBe("profile.err_mode_keep");
  });
});

describe("ProfileEditor", () => {
  it("creates a profile with packages and a script", async () => {
    const { backend, onclose } = open(null);
    await userEvent.type(screen.getByLabelText("Name"), "hAP lab");
    await waitFor(() => expect(screen.getByLabelText("wireless")).toBeInTheDocument());
    expect(backend.availablePackages).toHaveBeenCalledWith("stable", "arm64");
    await userEvent.click(screen.getByLabelText("wireless"));
    await userEvent.click(screen.getByLabelText("Run my script (.rsc)"));
    await userEvent.click(screen.getByRole("button", { name: "Choose…" }));
    expect(await screen.findByText("setup.rsc")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(backend.saveProfile).toHaveBeenCalledWith(
      profile("hAP lab", { extra_packages: ["wireless"], config: { mode: "script", local_name: "setup.rsc" } }),
      null,
    );
    expect(onclose).toHaveBeenCalledWith({ saved: "hAP lab" });
  });

  it("refuses an empty script and points to the empty configuration", async () => {
    const backend = mockBackend();
    backend.pickScript.mockResolvedValueOnce({ name: "empty.rsc", size: 0 });
    open(null, backend);
    await userEvent.type(screen.getByLabelText("Name"), "lab");
    await userEvent.click(screen.getByLabelText("Run my script (.rsc)"));
    await userEvent.click(screen.getByRole("button", { name: "Choose…" }));
    expect(
      await screen.findByText(
        "This script is empty. To install the device without any configuration, choose “Empty configuration”.",
      ),
    ).toBeInTheDocument();
    expect(screen.queryByText("empty.rsc")).not.toBeInTheDocument();
    expect(screen.getByText("No script chosen")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(backend.saveProfile).not.toHaveBeenCalled();
    // A script with content clears the error.
    await userEvent.click(screen.getByRole("button", { name: "Choose…" }));
    expect(await screen.findByText("setup.rsc")).toBeInTheDocument();
    expect(screen.queryByText(/This script is empty/)).not.toBeInTheDocument();
  });

  it("sets the configuration, what happens afterwards and the advanced options", async () => {
    const { backend } = open(null);
    await userEvent.type(screen.getByLabelText("Name"), "x86 lab");
    expect(screen.getByLabelText("RouterOS default configuration")).toBeChecked();
    await userEvent.click(screen.getByLabelText("Empty configuration"));
    await userEvent.selectOptions(screen.getByLabelText("After install"), "Shut down");
    await userEvent.click(screen.getByLabelText("Mode script (mode.scr)"));
    await userEvent.click(screen.getAllByRole("button", { name: "Choose…" }).at(-1)!);
    await userEvent.click(screen.getByLabelText("Remove branding"));
    await userEvent.click(screen.getByRole("button", { name: "Choose key file…" }));
    expect(await screen.findByText("key loaded (40 characters)")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(backend.saveProfile).toHaveBeenCalledWith(
      profile("x86 lab", {
        config: { mode: "empty" },
        mode_script: "setup.rsc",
        remove_branding: true,
        license_key: "-----BEGIN KEY-----AAAA-----END KEY-----",
        after_install: "shutdown",
      }),
      null,
    );
  });

  it("keeps a mode script away from keeping the configuration", async () => {
    const { backend } = open(profile("lab", { config: { mode: "empty" }, mode_script: "m.rsc", license_key: "K" }));
    expect(screen.getByText("m.rsc")).toBeInTheDocument();
    await userEvent.click(screen.getByLabelText("Keep the existing configuration"));
    expect(screen.getByRole("alert")).toHaveTextContent(
      "A mode script (mode.scr) cannot be used when keeping the existing configuration.",
    );
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(backend.saveProfile).not.toHaveBeenCalled();
    await userEvent.click(screen.getByLabelText("Mode script (mode.scr)"));
    expect(screen.getByLabelText("Mode script (mode.scr)")).toBeDisabled();
    expect(screen.queryByRole("alert")).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: "Remove" }));
    expect(screen.getByText("none")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(backend.saveProfile).toHaveBeenCalledWith(profile("lab", { config: { mode: "keep" } }), "lab");
  });

  it("shows validation messages instead of saving", async () => {
    const { backend } = open(null);
    await userEvent.type(screen.getByLabelText("Name"), ".bad");
    await userEvent.click(screen.getByRole("radio", { name: /Exact version/ }));
    await userEvent.type(screen.getByRole("textbox", { name: "Exact version" }), "seven");
    await userEvent.click(screen.getByLabelText("Run my script (.rsc)"));
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    const alerts = screen.getAllByRole("alert").map((a) => a.textContent);
    expect(alerts).toEqual([
      "Use 1–64 letters, digits, spaces or . _ - +, not starting with a dot or a space.",
      "Enter a version such as 7.25.1 or 7.26rc1.",
      "Choose a script file.",
    ]);
    expect(backend.saveProfile).not.toHaveBeenCalled();
  });

  it("edits an existing profile and reports backend errors", async () => {
    const backend = mockBackend({
      saveProfile: vi.fn(async () => Promise.reject({ code: "profile_exists", params: { name: "other" } })),
    });
    const { onclose } = open(profile("lab", { version: "7.24.6", extra_packages: ["gps"] }), backend);
    expect(screen.getByRole("heading")).toHaveTextContent("Edit profile");
    expect(screen.getByRole("textbox", { name: "Exact version" })).toHaveValue("7.24.6");
    // A selected package missing from the published list stays visible.
    await waitFor(() => expect(screen.getByLabelText("gps")).toBeChecked());
    expect(backend.availablePackages).toHaveBeenCalledWith("7.24.6", "arm64");
    await userEvent.clear(screen.getByLabelText("Name"));
    await userEvent.type(screen.getByLabelText("Name"), "other");
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(backend.saveProfile).toHaveBeenCalledWith(expect.objectContaining({ name: "other" }), "lab");
    expect(await screen.findByText("A profile named “other” already exists.")).toBeInTheDocument();
    expect(onclose).not.toHaveBeenCalled();
  });

  it("deletes after confirmation", async () => {
    const { backend, onclose } = open(profile("lab"));
    await userEvent.click(screen.getByRole("button", { name: "Delete…" }));
    const dialogs = screen.getAllByRole("dialog");
    await userEvent.click(within(dialogs[dialogs.length - 1]!).getByRole("button", { name: "Delete" }));
    expect(backend.deleteProfile).toHaveBeenCalledWith("lab");
    expect(onclose).toHaveBeenCalledWith({ deleted: "lab" });
  });

  it("waits for typing to pause before listing packages of an exact version", async () => {
    const { backend } = open(null);
    await waitFor(() => expect(backend.availablePackages).toHaveBeenCalledWith("stable", "arm64"));
    vi.mocked(backend.availablePackages).mockClear();
    await userEvent.click(screen.getByRole("radio", { name: /Exact version/ }));
    await userEvent.type(screen.getByRole("textbox", { name: "Exact version" }), "7.25.1");
    expect(backend.availablePackages).not.toHaveBeenCalled();
    await waitFor(() => expect(backend.availablePackages).toHaveBeenCalledWith("7.25.1", "arm64"));
    expect(backend.availablePackages).toHaveBeenCalledTimes(1);
  });

  it("asks before Escape discards changes", async () => {
    const { onclose } = open(profile("lab"));
    await userEvent.type(screen.getByLabelText("Name"), "2");
    await userEvent.keyboard("{Escape}");
    expect(onclose).not.toHaveBeenCalled();
    const ask = screen.getAllByRole("dialog").at(-1)!;
    expect(ask).toHaveTextContent("Discard your changes?");
    await userEvent.click(within(ask).getByRole("button", { name: "Keep editing" }));
    expect(screen.getAllByRole("dialog")).toHaveLength(1);
    await userEvent.keyboard("{Escape}");
    await userEvent.click(within(screen.getAllByRole("dialog").at(-1)!).getByRole("button", { name: "Discard" }));
    expect(onclose).toHaveBeenCalledWith({});
  });

  it("closes on Escape at once without changes", async () => {
    const { onclose } = open(profile("lab"));
    await userEvent.keyboard("{Escape}");
    expect(onclose).toHaveBeenCalledWith({});
  });

  it("shows why the package list is missing", async () => {
    const backend = mockBackend({
      availablePackages: vi.fn(async () => Promise.reject({ code: "packages_unavailable", params: { cause: "network" } })),
    });
    open(null, backend);
    expect(
      await screen.findByText("The package list is not available: no connection to mikrotik.com."),
    ).toBeInTheDocument();
  });
});
