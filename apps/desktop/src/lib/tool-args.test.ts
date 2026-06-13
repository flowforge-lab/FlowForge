import { describe, it, expect } from "vitest";
import { formatArgs } from "@/lib/tool-args";

describe("formatArgs", () => {
  it("pretty-prints small args unchanged", () => {
    expect(formatArgs({ path: "f.txt", content: "hi" })).toBe(
      '{\n  "path": "f.txt",\n  "content": "hi"\n}',
    );
  });

  it("truncates a long string value with a char count", () => {
    const content = "x".repeat(1000);
    const out = formatArgs({ path: "big.rs", content });
    expect(out).toContain("… (1000 chars)");
    expect(out).not.toContain("x".repeat(1000));
    expect(out).toContain('"path": "big.rs"');
  });

  it("truncates long strings nested in arrays and objects", () => {
    const long = "a".repeat(500);
    const out = formatArgs({ items: [long], nested: { v: long } });
    expect(out).toContain("(500 chars)");
    expect(out).not.toContain("a".repeat(500));
  });

  it("passes through non-string values", () => {
    expect(formatArgs({ replace_all: true, n: 3 })).toBe(
      '{\n  "replace_all": true,\n  "n": 3\n}',
    );
  });

  it("falls back to String() on non-serializable input", () => {
    const circular: Record<string, unknown> = {};
    circular.self = circular;
    expect(formatArgs(circular)).toBe(String(circular));
  });
});
