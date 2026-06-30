// @vitest-environment jsdom

import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { ModelWindowInfo } from "@/components/model-window-info";
import { formatContextWindow } from "@/lib/served-window";

afterEach(() => cleanup());

describe("formatContextWindow (#602)", () => {
  it("formats power-of-two windows as clean k", () => {
    expect(formatContextWindow(131072)).toBe("128k");
    expect(formatContextWindow(262144)).toBe("256k");
  });

  it("falls back to decimal-rounded k and exact below 1k", () => {
    expect(formatContextWindow(32000)).toBe("32k");
    expect(formatContextWindow(200000)).toBe("200k");
    expect(formatContextWindow(512)).toBe("512");
  });
});

describe("ModelWindowInfo (#602)", () => {
  it("renders the served + trained windows and the auto-detected source", () => {
    render(
      <ModelWindowInfo
        info={{ window: 131072, trained: 262144, source: "served" }}
      />,
    );
    expect(screen.getByText(/serving 128k/i)).not.toBeNull();
    expect(screen.getByText(/trained 256k/i)).not.toBeNull();
    expect(screen.getByText(/auto-detected from server/i)).not.toBeNull();
  });

  it("labels the explicit env-var source", () => {
    render(
      <ModelWindowInfo
        info={{ window: 131072, trained: 262144, source: "explicit" }}
      />,
    );
    expect(screen.getByText(/from flowforge_ollama_num_ctx/i)).not.toBeNull();
  });

  it("flags the conservative-default fallback with amber emphasis", () => {
    render(
      <ModelWindowInfo
        info={{ window: 32000, trained: null, source: "default" }}
      />,
    );
    const line = screen.getByText(/window not detected/i);
    expect(line).not.toBeNull();
    // Amber emphasis marks the under-fill; trained is omitted when unknown.
    expect(line.closest("div")?.className).toContain("text-amber-500");
    expect(screen.queryByText(/trained/i)).toBeNull();
  });
});
