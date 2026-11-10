import { describe, expect, it } from "vitest";
import { describe as text, hint, logEntry, logText, stateText, values } from "../src/lib/messages";
import { ago, bytes, isIpv4, percent } from "../src/lib/format";

const MAC = "48:A9:8A:00:00:0A";

describe("messages", () => {
  it("translates coded parameter values", () => {
    expect(values("hu", { service: "bootp", cause: "network", port: "67" })).toEqual({
      service: "BOOTP",
      cause: "nincs kapcsolat a mikrotik.com-mal",
      port: "67",
    });
    expect(text("en", { code: "assets_failed", params: { cause: "not_found", detail: "x" } })).toBe(
      "Could not get the RouterOS packages: not found on mikrotik.com.",
    );
    expect(text("hu", { code: "link_down", params: { interface: "en7", reason: "no_carrier" } })).toBe(
      "en7: nincs kapcsolat (nincs kábel).",
    );
    expect(text("en", { code: "brand_new" })).toBe("Error: brand_new");
  });

  it("explains the new safety refusals", () => {
    expect(text("en", { code: "version_below_minimum", params: { version: "6.49.23", min_os: "7.1" } })).toMatch(
      /6\.49\.23 is older than this device's minimum version 7\.1/,
    );
    expect(text("hu", { code: "package_not_published", params: { version: "6.30.4", arch: "mmips" } })).toBe(
      "A RouterOS 6.30.4 nem jelent meg mmips architektúrára. Válassz újabb verziót. Semmi sem települt.",
    );
    expect(text("en", { code: "disclaimer_not_accepted", params: { version: "1" } })).toBe(
      "Accept the disclaimer before starting.",
    );
  });

  it("explains why an unknown version is refused", () => {
    expect(text("en", { code: "version_unknown", params: { side: "device", version: "7.25.1", min_os: "x" } })).toMatch(
      /minimum version \(“x”\).*Allow below the device's minimum version/,
    );
    expect(text("hu", { code: "version_unknown", params: { side: "package", version: "?", min_os: "7.1" } })).toMatch(
      /nem olvasható ki.*Telepítés az eszköz minimális verziója alá is/,
    );
  });

  it("has hints for actionable codes only", () => {
    expect(hint("en", "port_in_use")).toContain("Internet Sharing");
    expect(hint("hu", "no_ipv4")).toContain("Rendszerbeállítások");
    expect(hint("en", "cancelled")).toBeNull();
  });

  it("describes device states", () => {
    expect(stateText("en", { state: "waiting", reason: "concurrency" })).toBe("Waiting for a free slot");
    expect(
      stateText("hu", { state: "flashing", version: "7.25.1", phase: "uploading", sent: 1, total: 2 }),
    ).toBe("Feltöltés");
    expect(stateText("en", { state: "failed", code: "flash_timeout", params: { phase: "formatting" } })).toBe(
      "The device stopped answering (step: Formatting).",
    );
    expect(stateText("en", { state: "bootp_seen", boot_arch: "arm64", ip: "1.2.3.4" })).toBe("Booting");
  });

  it("logs events without progress noise", () => {
    const at = 1000;
    expect(logEntry({ event: "device", mac: MAC, state: "kernel_serving", sent: 5, total: 9 }, at)).toBeNull();
    expect(
      logEntry({ event: "device", mac: MAC, state: "flashing", version: "7", phase: "uploading", sent: 5, total: 9 }, at),
    ).toBeNull();
    expect(logEntry({ event: "kernels", state: "idle" }, at)).toBeNull();
    expect(logEntry({ event: "download", file: "f", done: 5, total: 9 }, at)).toBeNull();
    const failed = logEntry({ event: "device", mac: MAC, state: "failed", code: "cancelled" }, at)!;
    expect(failed.level).toBe("error");
    expect(logText("en", failed)).toBe("Failed: Cancelled.");
    const ignored = logEntry({ event: "ignored", mac: MAC, reason: "cooldown" }, at)!;
    expect(logText("hu", ignored)).toBe("Nincs kiszolgálva: nemrég telepítve (várakozási idő)");
    const warn = logEntry({ event: "warning", code: "link_up", params: { interface: "en7" } }, at)!;
    expect([warn.level, logText("en", warn)]).toEqual(["warning", "en7: link is back."]);
    const resolved = logEntry(
      { event: "info", code: "arch_resolved", params: { mac: MAC, arch: "xmips", download_arch: "smips" } },
      at,
    )!;
    expect(resolved.level).toBe("info");
    expect(logText("en", resolved)).toContain("xmips");
    expect(logText("en", resolved)).toContain("smips");
    const found = logEntry(
      { event: "discovered", mac: MAC, model: "hEX", arch: "mmips", license_id: "AB", min_os: "7" },
      at,
    )!;
    expect(logText("en", found)).toBe("Found hEX (mmips), license AB, minimum version 7");
    const odd = logEntry({ event: "discovered", mac: MAC, model: "hEX", arch: "mmips", license_id: "AB", min_os: "" }, at)!;
    expect(logText("hu", odd)).toBe("Új eszköz: hEX (mmips), licenc: AB, min. verzió: ismeretlen");
  });
});

describe("format helpers", () => {
  it("percent, bytes, ago, ipv4", () => {
    expect([percent(50, 200), percent(5, 0), percent(5, null), percent(300, 200)]).toEqual([25, 0, 0, 100]);
    expect(bytes(999, "en")).toBe("999 B");
    expect(bytes(1_500_000, "en")).toBe("1.5 MB");
    expect(bytes(1_500_000, "hu")).toBe("1,5 MB");
    expect(ago(0, 120_000, "en")).toBe("2 minutes ago");
    expect(ago(0, 7_200_000, "hu")).toBe("2 órával ezelőtt");
    expect(isIpv4("192.168.88.1")).toBe(true);
    expect(isIpv4("192.168.88.256")).toBe(false);
    expect(isIpv4("1.2.3")).toBe(false);
  });
});
