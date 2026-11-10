import { describe, expect, it } from "vitest";
import { noteBlocks } from "../src/lib/releaseNotes";

describe("release notes", () => {
  it("keeps headings, list items and paragraphs as plain text", () => {
    const md = [
      "## Added",
      "- **Update check** with `sha256` [docs](https://evil.example/x)",
      "* second",
      "",
      "Some text",
      "continued.",
      "<script>alert(1)</script>",
      "<img src=x onerror=alert(1)>",
    ].join("\r\n");
    expect(noteBlocks(md)).toEqual([
      { kind: "heading", text: "Added" },
      { kind: "item", text: "Update check with sha256 docs" },
      { kind: "item", text: "second" },
      { kind: "text", text: "Some text continued." },
      { kind: "text", text: "alert(1)" },
    ]);
  });

  it("is empty for empty notes and bounded for huge ones", () => {
    expect(noteBlocks("")).toEqual([]);
    expect(noteBlocks("   \n\n")).toEqual([]);
    expect(noteBlocks("- x\n".repeat(5000)).length).toBe(500);
  });
});
