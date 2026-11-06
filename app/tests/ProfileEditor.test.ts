import { render, screen, waitFor, within } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import ProfileEditor from "../src/components/ProfileEditor.svelte";
import { backendContext } from "../src/lib/context";
import { locale } from "../src/lib/i18n";
import { formErrors, fromForm, toForm, validName, validVersion } from "../src/lib/profile";
import type { Profile } from "../src/lib/types";
import { mockBackend } from "./mock";

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
    const p: Profile = {
      name: "lab",
      version: "7.25.1",
      extra_packages: ["wireless"],
      config: { mode: "custom_script", local_name: "setup.rsc" },
    };
    expect(fromForm(toForm(p))).toEqual(p);
    const f = toForm(null);
    f.useScript = true;
    expect(formErrors(f)).toEqual({ name: "profile.err_name", script: "profile.err_script" });
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
      {
        name: "hAP lab",
        version: "stable",
        extra_packages: ["wireless"],
        config: { mode: "custom_script", local_name: "setup.rsc" },
      },
      null,
    );
    expect(onclose).toHaveBeenCalledWith({ saved: "hAP lab" });
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
    const { onclose } = open(
      { name: "lab", version: "7.24.6", extra_packages: ["gps"], config: { mode: "default" } },
      backend,
    );
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
    const { backend, onclose } = open({ name: "lab", version: "stable", extra_packages: [], config: { mode: "default" } });
    await userEvent.click(screen.getByRole("button", { name: "Delete…" }));
    const dialogs = screen.getAllByRole("dialog");
    await userEvent.click(within(dialogs[dialogs.length - 1]!).getByRole("button", { name: "Delete" }));
    expect(backend.deleteProfile).toHaveBeenCalledWith("lab");
    expect(onclose).toHaveBeenCalledWith({ deleted: "lab" });
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
