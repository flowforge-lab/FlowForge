// @vitest-environment jsdom

import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { ThinkingIndicator } from "@/components/thinking-indicator";

describe("ThinkingIndicator", () => {
  it("exposes an accessible status with three animated dots", () => {
    render(<ThinkingIndicator />);
    const status = screen.getByRole("status", { name: "Thinking" });
    expect(status).toBeTruthy();
    expect(status.querySelectorAll(".ff-thinking-dot")).toHaveLength(3);
    expect(screen.getByText("Thinking…")).toBeTruthy();
  });
});
