import { describe, expect, it } from "vitest";
import { toFailure } from "../src/lib/api";
import { isActive } from "../src/lib/types";
import { mockBackend } from "./mock";

describe("api", () => {
  it("passes command failures through and wraps anything else", () => {
    expect(toFailure({ code: "port_in_use", params: { port: "67" } })).toEqual({
      code: "port_in_use",
      params: { port: "67" },
    });
    expect(toFailure(new Error("boom"))).toEqual({ code: "internal", params: { detail: "Error: boom" } });
    expect(toFailure("x")).toEqual({ code: "internal", params: { detail: "x" } });
  });

  it("the mock backend delivers events to subscribers", async () => {
    const b = mockBackend();
    const seen: string[] = [];
    await b.onEvent((e) => seen.push(e.event));
    b.emit({ event: "stopped" });
    expect(seen).toEqual(["stopped"]);
    expect(await b.snapshot()).toMatchObject({ running: false, kernels: { state: "idle" } });
  });

  it("knows which states are active", () => {
    expect(isActive({ state: "preparing" })).toBe(true);
    expect(isActive({ state: "done" })).toBe(false);
  });
});
