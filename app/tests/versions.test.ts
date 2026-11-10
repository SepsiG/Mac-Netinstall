import { describe, expect, it } from "vitest";
import { acceptedVersion, compareVersions, isV6, listAgeHours, supportsModeScript, versionGroups } from "../src/lib/versions";
import { VERSIONS } from "./mock";

describe("versions", () => {
  it("compare semantically", () => {
    const sorted = ["7.9", "7.25.1", "6.49.23", "7.26rc1", "7.26", "7.26beta5", "7.10"].sort(compareVersions);
    expect(sorted).toEqual(["6.49.23", "7.9", "7.10", "7.25.1", "7.26beta5", "7.26rc1", "7.26"]);
  });

  it("accept 7.x and 6.x from 6.30", () => {
    expect(["7.1", "8.0", "6.30", "6.49.23"].every(acceptedVersion)).toBe(true);
    expect(["6.29", "5.26", "7", "x", "6.49.x"].some(acceptedVersion)).toBe(false);
  });

  it("know v6 and the mode-script floor", () => {
    expect(isV6("v6-long-term") && isV6("6.49.23")).toBe(true);
    expect(isV6("7.25.1") || isV6("long-term")).toBe(false);
    expect(supportsModeScript("stable")).toBe(true);
    expect(supportsModeScript("7.22")).toBe(true);
    expect(supportsModeScript("7.21.3")).toBe(false);
    expect(supportsModeScript("7.22rc1")).toBe(false);
    expect(supportsModeScript("v6-long-term")).toBe(false);
  });

  it("group and search", () => {
    const all = versionGroups(VERSIONS);
    expect(all.channels.map((c) => c.channel)).toEqual(["stable", "long-term", "testing", "development", "v6-long-term"]);
    expect(all.v7.map((l) => l.version)).toEqual(["7.25.1", "7.24.6", "7.24.5"]);
    expect(all.v6.map((l) => l.version)).toEqual(["6.49.23", "6.48.7"]);
    const lt = versionGroups(VERSIONS, "long-term");
    expect(lt.v7.map((l) => l.version)).toEqual(["7.24.6"]);
    expect(lt.channels.map((c) => c.channel)).toEqual(["long-term", "v6-long-term"]);
    const translated = versionGroups(VERSIONS, "stabil", (c) => (c === "stable" ? "Stabil" : c));
    expect(translated.channels.map((c) => c.channel)).toEqual(["stable"]);
    expect(versionGroups(VERSIONS, "2026-09").v7.map((l) => l.version)).toEqual(["7.24.5"]);
    expect(versionGroups(VERSIONS, "nothing")).toEqual({ channels: [], v7: [], v6: [] });
  });

  it("tell the list's age", () => {
    expect(listAgeHours(VERSIONS, (VERSIONS.fetched_unix + 7200) * 1000)).toBe(2);
    expect(listAgeHours({ ...VERSIONS, fetched_unix: 0 }, Date.now())).toBeNull();
  });
});
