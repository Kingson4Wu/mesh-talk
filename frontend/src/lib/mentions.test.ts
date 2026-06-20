import { describe, it, expect } from "vitest";
import { mentionSegments, mentionsName } from "./mentions";

describe("mentionSegments", () => {
  it("splits a mention out of surrounding text", () => {
    expect(mentionSegments("hi @alice there")).toEqual([
      { text: "hi ", mention: false },
      { text: "@alice", mention: true },
      { text: " there", mention: false },
    ]);
  });
  it("plain text is one non-mention segment", () => {
    expect(mentionSegments("just text")).toEqual([
      { text: "just text", mention: false },
    ]);
  });
  it("handles adjacent mentions without empty segments", () => {
    expect(mentionSegments("@a @b")).toEqual([
      { text: "@a", mention: true },
      { text: " ", mention: false },
      { text: "@b", mention: true },
    ]);
  });
});

describe("mentionsName", () => {
  it("matches a whole-word mention", () => {
    expect(mentionsName("hey @alice!", "alice")).toBe(true);
  });
  it("is case-insensitive", () => {
    expect(mentionsName("@Alice", "alice")).toBe(true);
  });
  it("respects a word boundary (no partial match)", () => {
    expect(mentionsName("@aliced", "alice")).toBe(false);
  });
  it("empty name never matches", () => {
    expect(mentionsName("@x", "")).toBe(false);
  });
  it("escapes regex metacharacters in the name", () => {
    expect(mentionsName("@a.b", "a.b")).toBe(true);
    expect(mentionsName("@axb", "a.b")).toBe(false);
  });
});
